//! Rewriting bytes on their way to the target.
//!
//! The motivating case is a header that has to change for the far side to accept the
//! connection at all — `Host: note:8080` becoming `Host: google.com.br` when a local
//! client is pointed at a real site through the tunnel. It's the same idea as the
//! tunnel itself: the bytes pass *through* this process, so this is the one place they
//! can be edited without touching either end.
//!
//! Two deliberate limits, both visible in the wizard's help text:
//!
//! * Rules run per chunk, on whatever one read returned. A match split across two reads
//!   won't be seen. In practice a request header arrives in one piece, which is what
//!   this is for; a rule meant to catch something mid-stream is not reliable.
//! * A replacement of a different length changes the payload's size, which breaks any
//!   protocol that already announced it — rewriting a body without fixing
//!   `Content-Length` is on the person who wrote the rule.

use std::fs;

use regex::bytes::Regex;
use serde::{Deserialize, Serialize};

use super::{EventKind, Recorder};
use crate::history;

/// The pattern/replacement pair as the user typed it, which is also exactly what gets
/// saved — both in the execution and in the shared history.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Rule {
    pub find: String,
    pub replace: String,
}

/// How many rules the shared history keeps. Old enough entries fall off the end rather
/// than growing a file nobody prunes.
const HISTORY_LIMIT: usize = 200;

/// A rule with its pattern compiled, ready to run against a payload.
struct Compiled {
    rule: Rule,
    regex: Regex,
}

/// The rules of one execution, compiled once when it starts.
#[derive(Default)]
pub struct Rules(Vec<Compiled>);

impl Rules {
    /// Compiles what `encode` produced. A bad pattern is an error naming itself, so the
    /// wizard can put it in front of the person who wrote it.
    pub fn parse(encoded: &str) -> Result<Self, String> {
        let mut compiled = Vec::new();
        for rule in decode(encoded) {
            // Bytes, not `str`: a tunnel carries arbitrary payloads, and a rule that
            // only worked on valid UTF-8 would quietly stop applying on binary traffic.
            let regex = Regex::new(&rule.find)
                .map_err(|e| format!("regex inválido «{}»: {}", rule.find, complaint(&e)))?;
            compiled.push(Compiled { rule, regex });
        }
        Ok(Self(compiled))
    }

    /// Runs every rule, in order, over `data`. `None` means nothing matched and the
    /// caller should forward the original bytes untouched — worth distinguishing, since
    /// it's also what decides whether the monitor mentions a rewrite at all.
    ///
    /// Rules see each other's output: a second rule matches against what the first one
    /// produced, which is what makes a chain of small edits behave the way it reads.
    pub fn apply(&self, data: &[u8]) -> Option<(Vec<u8>, String)> {
        let mut current: Option<Vec<u8>> = None;
        let mut fired: Vec<String> = Vec::new();
        for entry in &self.0 {
            let input = current.as_deref().unwrap_or(data);
            let hits = entry.regex.find_iter(input).count();
            if hits == 0 {
                continue;
            }
            current = Some(
                entry
                    .regex
                    .replace_all(input, entry.rule.replace.as_bytes())
                    .into_owned(),
            );
            fired.push(format!("«{}» ×{hits}", entry.rule.find));
        }
        current.map(|bytes| (bytes, fired.join(", ")))
    }
}

/// The rules of one execution as a single string, because that's what a tool parameter
/// is. JSON rather than a separator, so a pattern is free to contain anything.
pub fn encode(rules: &[Rule]) -> String {
    serde_json::to_string(rules).unwrap_or_else(|_| "[]".to_string())
}

/// The inverse, forgiving: anything unreadable comes back as no rules, so a hand-edited
/// config costs the rewriting rather than the execution.
pub fn decode(encoded: &str) -> Vec<Rule> {
    if encoded.trim().is_empty() {
        return Vec::new();
    }
    serde_json::from_str(encoded).unwrap_or_default()
}

/// `regex`'s errors are several lines: a header, the pattern, a caret pointing at the
/// offending spot, then the actual complaint. Only that last line fits next to the
/// field, and it's the only one that says anything.
fn complaint(err: &regex::Error) -> String {
    let text = err.to_string();
    let last = text
        .lines()
        .rfind(|line| !line.trim().is_empty())
        .unwrap_or("padrão inválido")
        .trim();
    last.strip_prefix("error: ").unwrap_or(last).to_string()
}

fn path() -> std::path::PathBuf {
    history::data_file("rewrites.json")
}

/// Every rule ever written, newest first, shared by every execution. The point is that
/// a rule is annoying to get right once and unbearable to get right twice.
pub fn history() -> Vec<Rule> {
    match fs::read_to_string(path()) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

/// Files `rule` at the front of the history, moving it there if it already existed so
/// the list stays ordered by when it was last useful.
pub fn remember(rule: &Rule) {
    if rule.find.is_empty() {
        return;
    }
    let mut saved = history();
    saved.retain(|existing| existing != rule);
    saved.insert(0, rule.clone());
    saved.truncate(HISTORY_LIMIT);
    if let Ok(content) = serde_json::to_string_pretty(&saved) {
        let _ = fs::write(path(), content);
    }
}

/// Drops one rule from the shared history. Removing it from an execution leaves it
/// here on purpose — this is the only way it actually goes away.
pub fn forget(rule: &Rule) {
    let mut saved = history();
    saved.retain(|existing| existing != rule);
    if let Ok(content) = serde_json::to_string_pretty(&saved) {
        let _ = fs::write(path(), content);
    }
}

/// Applies the execution's rules to one chunk, borrowing `original` when nothing
/// matched so the common case allocates nothing.
///
/// What gets recorded downstream is the *result*, since the log is meant to show what
/// the target actually received; the note is what tells you a rewrite happened at all.
pub fn rewritten<'a>(
    rules: Option<&Rules>,
    original: &'a [u8],
    conn: u64,
    rec: &Recorder,
) -> RewriteResult<'a> {
    let Some((bytes, fired)) = rules.and_then(|rules| rules.apply(original)) else {
        return RewriteResult::Same(original);
    };
    rec.record(conn, EventKind::Note(format!("reescrito por {fired}")));
    RewriteResult::Changed(bytes)
}

/// Either the untouched chunk or a rewritten copy of it. A tiny enum rather than
/// `Cow`, so the borrow and the owned buffer can both be used as `&[u8]` without the
/// caller caring which it got.
pub enum RewriteResult<'a> {
    Same(&'a [u8]),
    Changed(Vec<u8>),
}

impl std::ops::Deref for RewriteResult<'_> {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        match self {
            Self::Same(bytes) => bytes,
            Self::Changed(bytes) => bytes,
        }
    }
}
