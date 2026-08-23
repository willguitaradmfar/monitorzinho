//! Tools: things monitorzinho *runs*, as opposed to the monitors, which only watch.
//!
//! A tool owns background threads and outlives the screen that started it — the user
//! adds an execution from the Ferramentas tab, it keeps working while they go look at
//! something else, and it stops when they remove it or the app exits.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

pub mod cert;
pub mod dns;
pub mod http;
pub mod icmp;
pub mod listen;
pub mod mdns;
pub mod net;
pub mod persist;
pub mod poll;
pub mod rewrite;
pub mod rota;
pub mod scan;
pub mod smtp;
pub mod tail;
pub mod tls;
pub mod tunnel;
pub mod x509;

/// How one parameter is edited in the add-execution wizard.
#[derive(Clone)]
pub enum ParamKind {
    /// Free text — an address, a path, anything typed.
    Text,
    /// One of a fixed set, cycled with ←/→ rather than typed, so it can't be spelled
    /// wrong.
    Choice(&'static [&'static str]),
    /// A list of search/replace rules, edited on its own screen because it's the one
    /// parameter that isn't a single value. The stored value is `rewrite::encode`d.
    Rules,
}

/// One ready-made value a text field offers alongside what can be typed into it.
///
/// The `value` is what the field becomes when this one is picked; the `note` is never
/// stored, only shown — it's the answer to "which of these is my wifi", which a bare
/// CIDR can't give. An empty `value` is a legitimate suggestion: it's how a field whose
/// blank state *means* something ("all of them") can offer that meaning as an option
/// instead of leaving the user to guess it.
#[derive(Clone)]
pub struct Suggestion {
    pub value: String,
    pub note: String,
}

impl Suggestion {
    pub fn new(value: impl Into<String>, note: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            note: note.into(),
        }
    }
}

/// One thing a tool needs to know before it can start.
#[derive(Clone)]
pub struct ParamSpec {
    /// Stable lookup key, passed back to `Tool::start`.
    pub key: &'static str,
    pub label: &'static str,
    /// Shown under the field while it's focused: what to put here, and why.
    pub help: &'static str,
    pub default: &'static str,
    pub kind: ParamKind,
    /// Values the machine already knows are plausible, walked with ←/→. Only a text
    /// field uses these, and having them never stops it being typed into: a suggestion
    /// is a shortcut past looking something up elsewhere, not a fixed set of answers.
    /// Empty for every field that has nothing to suggest.
    pub suggestions: Vec<Suggestion>,
}

impl ParamSpec {
    pub fn text(
        key: &'static str,
        label: &'static str,
        default: &'static str,
        help: &'static str,
    ) -> Self {
        Self {
            key,
            label,
            help,
            default,
            kind: ParamKind::Text,
            suggestions: Vec::new(),
        }
    }

    /// Offers `suggestions` on a text field. Put the value the field means when it's
    /// left alone first, so walking the list from the top starts where the user is.
    pub fn suggesting(mut self, suggestions: Vec<Suggestion>) -> Self {
        self.suggestions = suggestions;
        self
    }

    pub fn rules(key: &'static str, label: &'static str, help: &'static str) -> Self {
        Self {
            key,
            label,
            help,
            default: "",
            kind: ParamKind::Rules,
            suggestions: Vec::new(),
        }
    }

    pub fn choice(
        key: &'static str,
        label: &'static str,
        options: &'static [&'static str],
        help: &'static str,
    ) -> Self {
        Self {
            key,
            label,
            help,
            default: options[0],
            kind: ParamKind::Choice(options),
            suggestions: Vec::new(),
        }
    }
}

/// A tool the user can launch from the Ferramentas tab.
pub trait Tool: Send + Sync {
    /// Stable key used to match a saved execution back to its tool on the next run —
    /// never change one once shipped, same rule as `Monitor::id`.
    fn id(&self) -> &'static str;
    fn name(&self) -> &'static str;
    /// One line explaining what it does, shown next to the name while picking.
    fn description(&self) -> &'static str;
    fn params(&self) -> Vec<ParamSpec>;
    /// Validates `params` and starts the work, returning the live execution.
    ///
    /// Everything that can predictably fail — a malformed address, a port already in
    /// use, an unresolvable host — must fail *here*, while the user is still standing
    /// in front of the form and can fix it. A tool that returns `Ok` and then dies in a
    /// thread nobody is watching is the failure mode this signature exists to prevent.
    fn start(&self, id: u64, params: &HashMap<&'static str, String>) -> Result<Execution, String>;
    /// One-line description of what these parameters amount to, e.g.
    /// `TCP 127.0.0.1:8080 → 10.0.0.5:5432`. Used both for a running execution's row
    /// and for one that failed to start, so the two read identically in the list.
    fn summarize(&self, params: &HashMap<&'static str, String>) -> String {
        self.params()
            .iter()
            .filter_map(|spec| params.get(spec.key).cloned())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Whether this tool only works when asked. An on-demand execution starts nothing
    /// — not when it's created, not when it's restored on launch — and does its work
    /// when the user opens its monitor. A scan of sixty thousand ports has no business
    /// running because the app happened to start.
    fn on_demand(&self, _params: &HashMap<&'static str, String>) -> bool {
        false
    }

    /// The user opened this execution's monitor. Where an on-demand tool does its work;
    /// a no-op for one that's been running all along.
    fn open(&self, _execution: &Execution, _params: &HashMap<&'static str, String>) {}

    /// Asked for again, explicitly ('r'). Only reached for an on-demand tool — for
    /// anything else 'r' recreates the execution from its configuration instead.
    fn rerun(&self, _execution: &Execution, _params: &HashMap<&'static str, String>) {}

    /// The two result columns of this execution's row: a headline figure and a summary
    /// beside it. Both empty until there is something to say, which for an on-demand
    /// tool means until it has been run at least once.
    fn columns(&self, _execution: &Execution) -> (String, String) {
        (String::new(), String::new())
    }

    /// Executions this one's findings suggest creating. A DNS investigation ends
    /// holding a list of addresses and the port scanner starts by asking for one, so
    /// the answer to the first is the input to the second — retyping it by hand is
    /// busywork the tools can spare the user.
    ///
    /// The default is the whole answer for every tool shipped so far: record findings
    /// with `Recorder::found` and what they're worth doing is decided by their kind in
    /// `offers_for`. Override it only for an offer that depends on something other than
    /// the findings themselves.
    fn handoffs(&self, execution: &Execution) -> Vec<Handoff> {
        offers_from(execution)
    }
}

/// One execution another one is offering to create, already filled in.
pub struct Handoff {
    /// Shown in the picker, e.g. `varrer portas de 93.184.216.34`.
    pub label: String,
    /// `Tool::id` of what to create.
    pub tool: &'static str,
    /// Parameters to pre-fill; anything not named keeps the tool's default.
    pub params: Vec<(&'static str, String)>,
}

/// What can be done with one finding, decided by what the finding *is*.
///
/// This is the whole point of typed findings. An address is an address whether a network
/// sweep, a DNS investigation or a certificate reader turned it up, and what an address
/// is worth doing — scan its ports, read its certificate — is a property of addresses,
/// not of the tool that happened to produce one. Writing the offers here instead of in
/// each tool means a tool earns every hand-off in the app by recording what it found,
/// and a new tool that consumes addresses becomes available to every existing tool at
/// once.
///
/// The kinds, and what they carry:
///
/// * `ip` — a bare address
/// * `dominio` — a domain or host name
/// * `mx` — a mail exchanger's host name
/// * `porta` — `host:porta`, open, plaintext as far as anyone knows
/// * `porta-tls` — `host:porta`, open and answered a TLS handshake
/// * `rede` — a CIDR
pub fn offers_for(kind: &str, value: &str) -> Vec<Handoff> {
    match kind {
        "ip" => vec![
            Handoff {
                label: format!("varrer portas de {value}"),
                tool: "scan",
                params: vec![("alvo", value.to_string()), ("faixa", "comuns".to_string())],
            },
            Handoff {
                label: format!("ler o certificado de {value}:443"),
                tool: "cert",
                params: vec![("alvo", value.to_string()), ("porta", "443".to_string())],
            },
        ],
        "dominio" => vec![
            Handoff {
                label: format!("investigar o DNS de {value}"),
                tool: "dns",
                params: vec![("dominio", value.to_string())],
            },
            Handoff {
                label: format!("ler o certificado de {value}"),
                tool: "cert",
                params: vec![
                    ("alvo", value.to_string()),
                    ("porta", "443".to_string()),
                    ("sni", value.to_string()),
                ],
            },
            Handoff {
                label: format!("varrer portas de {value}"),
                tool: "scan",
                params: vec![("alvo", value.to_string()), ("faixa", "comuns".to_string())],
            },
        ],
        // A mail exchanger is a host name like any other, plus the one thing that is
        // only true of mail exchangers: its certificate lives behind STARTTLS on 25.
        "mx" => vec![
            Handoff {
                label: format!("ler o certificado de {value} (SMTP, porta 25)"),
                tool: "cert",
                params: vec![
                    ("alvo", value.to_string()),
                    ("porta", "25".to_string()),
                    ("starttls", "smtp".to_string()),
                ],
            },
            Handoff {
                label: format!("investigar o DNS de {value}"),
                tool: "dns",
                params: vec![("dominio", value.to_string())],
            },
        ],
        "porta" | "porta-tls" => {
            let Some((host, port)) = value.rsplit_once(':') else {
                return Vec::new();
            };
            let Ok(number) = port.parse::<u16>() else {
                return Vec::new();
            };
            let tls = kind == "porta-tls";
            let mut offers = Vec::new();
            if tls {
                offers.push(Handoff {
                    label: format!("ler o certificado de {value}"),
                    tool: "cert",
                    params: vec![
                        ("alvo", host.to_string()),
                        ("porta", port.to_string()),
                        ("sni", host.to_string()),
                    ],
                });
            }
            offers.push(Handoff {
                label: if tls {
                    format!("túnel decifrando o tráfego de {value}")
                } else {
                    format!("túnel gravando o tráfego de {value}")
                },
                tool: "tunnel",
                params: vec![
                    ("proto", "TCP".to_string()),
                    // One number to the right, since the port itself is taken on the
                    // far side and usually on this one too.
                    ("listen", format!("127.0.0.1:{}", number.saturating_add(1))),
                    ("target", value.to_string()),
                    (
                        "tls",
                        if tls {
                            // The scanner never verified anything either — it's a probe,
                            // not a client that trusts the port.
                            "sim, sem validar certificado".to_string()
                        } else {
                            "não".to_string()
                        },
                    ),
                ],
            });
            offers
        }
        "rede" => vec![Handoff {
            label: format!("varrer a rede {value}"),
            tool: "net",
            params: vec![("rede", value.to_string())],
        }],
        _ => Vec::new(),
    }
}

/// Every offer an execution's findings add up to, deduplicated and in a stable order:
/// the default answer for every tool, and the reason a tool only has to record what it
/// found to be wired into all the others.
pub fn offers_from(execution: &Execution) -> Vec<Handoff> {
    let mut offers: Vec<Handoff> = Vec::new();
    for (kind, value) in execution.all_findings() {
        for offer in offers_for(&kind, &value) {
            // The same address can be found twice over — as an A record and again in
            // the reverse lookup — and the picker should show one row for it.
            if !offers
                .iter()
                .any(|seen| seen.tool == offer.tool && seen.params == offer.params)
            {
                offers.push(offer);
            }
        }
    }
    offers
}

pub fn all_tools() -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(tunnel::TunnelTool),
        Box::new(listen::ListenTool),
        Box::new(tail::TailTool),
        Box::new(http::HttpTool),
        Box::new(scan::ScanTool),
        Box::new(dns::DnsTool),
        Box::new(cert::CertTool),
        Box::new(smtp::SmtpTool),
        Box::new(net::NetTool),
        Box::new(rota::RotaTool),
    ]
}

/// Live counters for an execution, written by its threads and read by the UI every
/// tick. Atomics rather than another mutex: the UI must never be able to block behind
/// a relay thread that's mid-copy.
#[derive(Default)]
pub struct Stats {
    /// Connections (or, for UDP, distinct client addresses) handled since the start.
    pub connections: AtomicU64,
    pub active: AtomicUsize,
    /// Bytes relayed towards the target, and back from it.
    pub to_target: AtomicU64,
    pub from_target: AtomicU64,
}

/// Which way a chunk of relayed data was going.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// From whoever connected to us, onward to the target.
    ToTarget,
    /// From the target, back to the client.
    FromTarget,
}

impl Direction {
    /// Arrow used in the monitor. Reads as "leaving us" / "coming back to us".
    pub fn arrow(self) -> &'static str {
        match self {
            Direction::ToTarget => "→",
            Direction::FromTarget => "←",
        }
    }
}

pub enum EventKind {
    /// Someone connected to the listening side.
    Opened {
        peer: String,
    },
    Closed {
        reason: String,
    },
    /// A chunk of relayed bytes. `preview` is what got kept (see `Event::preview`),
    /// `len` is how big the chunk really was.
    Data {
        dir: Direction,
        len: usize,
        preview: Vec<u8>,
        stage: Stage,
    },
    /// Something the tool itself wants to say — started, stopped, limit hit.
    Note(String),
    Error(String),
}

/// Which version of a chunk an event is showing. Only matters where a rewrite rule
/// changed something: then the log carries both, because a rule you can't see the
/// effect of is a rule you can't tell is working.
#[derive(Clone, PartialEq, Eq)]
pub enum Stage {
    /// What crossed, with no rule involved. The ordinary case.
    Wire,
    /// What arrived, before the rules touched it.
    Original,
    /// What left, after the rules touched it, carrying the indices of the lines that
    /// actually differ. Two near-identical blocks with one changed header between them
    /// is precisely the thing an eye slides over, so the renderer marks the difference
    /// rather than leaving it to be found.
    Rewritten { changed: Vec<usize> },
}

impl Stage {
    /// Suffix on the size line, so the two halves of a rewrite can't be confused.
    pub fn label(&self) -> &'static str {
        match self {
            Stage::Wire => "",
            Stage::Original => "  antes do replace",
            Stage::Rewritten { .. } => "  depois do replace",
        }
    }

    /// Whether this line of the payload is one a rule changed.
    pub fn changed(&self, line: usize) -> bool {
        match self {
            Stage::Rewritten { changed } => changed.contains(&line),
            _ => false,
        }
    }
}

/// Lines of `rewritten` that don't appear in `original` — what the rules produced.
///
/// Compared by content rather than position: a rule that changes a header's value
/// leaves every other line where it was, and a rule that adds or removes one shifts
/// everything after it without changing any of it.
fn changed_lines(original: &[u8], rewritten: &[u8]) -> Vec<usize> {
    let before: HashSet<&[u8]> = original.split(|byte| *byte == b'\n').collect();
    rewritten
        .split(|byte| *byte == b'\n')
        .enumerate()
        .filter(|(_, line)| !before.contains(line))
        .map(|(index, _)| index)
        .collect()
}

pub struct Event {
    /// Monotonic, assigned on append and never reused. The monitor shows events newest
    /// first, so new ones appear at the *top* and push everything down; comparing
    /// against the newest sequence seen last frame is how it works out exactly how far
    /// to shift the viewport to keep the reader looking at the same line.
    pub seq: u64,
    /// Time since the execution started, not wall clock. What a relay log is read for
    /// is the gap between a request and its answer, and a relative clock needs no
    /// timezone plumbing to be exact.
    pub at: Duration,
    /// Which relayed connection this belongs to; 0 for events about the tool itself.
    pub conn: u64,
    pub kind: EventKind,
}

/// A bounded log of what an execution has seen. Old events fall off the front — a busy
/// tunnel would otherwise grow without limit — and the count of what was dropped is
/// kept so the monitor can say so instead of quietly showing a partial story.
pub struct EventLog {
    events: VecDeque<Event>,
    dropped: u64,
    capacity: usize,
    next_seq: u64,
}

impl EventLog {
    fn new(capacity: usize) -> Self {
        Self {
            events: VecDeque::new(),
            dropped: 0,
            capacity,
            next_seq: 0,
        }
    }

    fn push(&mut self, at: Duration, conn: u64, kind: EventKind) {
        if self.events.len() == self.capacity {
            self.events.pop_front();
            self.dropped += 1;
        }
        self.next_seq += 1;
        self.events.push_back(Event {
            seq: self.next_seq,
            at,
            conn,
            kind,
        });
    }

    pub fn iter(&self) -> std::collections::vec_deque::Iter<'_, Event> {
        self.events.iter()
    }

    /// Empties the scrollback on request. The sequence counter keeps running, so a
    /// viewport anchored to an event it can no longer find knows it's gone rather than
    /// finding a different event wearing the same number.
    pub fn clear(&mut self) {
        self.events.clear();
        // What fell off the end is no longer a fact about this log: the buffer is empty
        // because someone asked, not because it overflowed.
        self.dropped = 0;
    }

    /// A note from outside the tool's own threads. Clearing the log is the app's doing,
    /// and a screen that simply goes blank reads as broken rather than as cleared.
    pub fn note(&mut self, at: Duration, text: String) {
        self.push(at, 0, EventKind::Note(text));
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }
}

/// A poisoned log mutex means a relay thread panicked while appending. The log itself
/// is still perfectly readable, and losing the whole monitor over it would be a worse
/// outcome than showing what's there, so recover rather than propagate.
pub fn lock_log(log: &Mutex<EventLog>) -> MutexGuard<'_, EventLog> {
    log.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// How many events one execution keeps. Roughly a screenful per 30 events, so this is
/// a deep-enough scrollback to find the request you were looking for.
const LOG_CAPACITY: usize = 4000;
/// How much of each relayed chunk is kept for the monitor. A tunnel can move megabytes
/// a second; the log is there to read protocol traffic, not to store payloads.
pub const PREVIEW_BYTES: usize = 2048;

/// The write end of an execution: what its background threads hold. Bundles the log,
/// the counters and the stop flag so a relay thread carries one clone instead of four.
#[derive(Clone)]
pub struct Recorder {
    log: Arc<Mutex<EventLog>>,
    stats: Arc<Stats>,
    shutdown: Arc<AtomicBool>,
    started: Instant,
    outcome: Arc<Mutex<(String, String)>>,
    runs: Arc<AtomicU64>,
    findings: Arc<Mutex<Vec<(String, String)>>>,
}

/// Bumped whenever any tool writes something. The UI blocks waiting for input between
/// samples, and a relay thread appending to a log has no way to knock on that door —
/// without this the screen would only catch up on the next tick, which is a visible
/// delay on a live log.
static ACTIVITY: AtomicU64 = AtomicU64::new(0);

/// How much the tools have written. Compared against the last value drawn: different
/// means there's something new on screen to show.
pub fn activity() -> u64 {
    ACTIVITY.load(Ordering::Relaxed)
}

impl Recorder {
    pub fn record(&self, conn: u64, kind: EventKind) {
        lock_log(&self.log).push(self.started.elapsed(), conn, kind);
        ACTIVITY.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a chunk of relayed data, keeping at most `PREVIEW_BYTES` of it and
    /// adding its size to the right counter.
    pub fn record_data(&self, conn: u64, dir: Direction, chunk: &[u8]) {
        let counter = match dir {
            Direction::ToTarget => &self.stats.to_target,
            Direction::FromTarget => &self.stats.from_target,
        };
        counter.fetch_add(chunk.len() as u64, Ordering::Relaxed);
        self.chunk(conn, dir, chunk, Stage::Wire);
    }

    /// Counts a chunk without keeping any of it. For traffic there is no point in
    /// showing: what crosses a `CONNECT` tunnel is TLS this process has no key for, and
    /// a log full of ciphertext hides the lines that mean something under thousands of
    /// bytes that never will.
    pub fn count_only(&self, dir: Direction, len: usize) {
        let counter = match dir {
            Direction::ToTarget => &self.stats.to_target,
            Direction::FromTarget => &self.stats.from_target,
        };
        counter.fetch_add(len as u64, Ordering::Relaxed);
        ACTIVITY.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a chunk a rule changed: what arrived, which rules fired, and what left.
    ///
    /// Three events rather than one, in that order — the monitor shows newest first, so
    /// they read top-down as the result, the reason, and the input. Only the bytes that
    /// actually left count towards the traffic figures; the original is there to be
    /// compared against, not to be counted twice.
    pub fn record_rewrite(
        &self,
        conn: u64,
        dir: Direction,
        original: &[u8],
        rewritten: &[u8],
        fired: &str,
    ) {
        self.chunk(conn, dir, original, Stage::Original);
        self.record(conn, EventKind::Note(format!("reescrito por {fired}")));
        let counter = match dir {
            Direction::ToTarget => &self.stats.to_target,
            Direction::FromTarget => &self.stats.from_target,
        };
        counter.fetch_add(rewritten.len() as u64, Ordering::Relaxed);
        self.chunk(
            conn,
            dir,
            rewritten,
            Stage::Rewritten {
                changed: changed_lines(original, rewritten),
            },
        );
    }

    fn chunk(&self, conn: u64, dir: Direction, bytes: &[u8], stage: Stage) {
        self.record(
            conn,
            EventKind::Data {
                dir,
                len: bytes.len(),
                preview: bytes[..bytes.len().min(PREVIEW_BYTES)].to_vec(),
                stage,
            },
        );
    }

    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    /// Publishes the two result columns of this execution's row. Called as work
    /// progresses, not only at the end, so a long scan shows how far along it is
    /// without anyone having to open it.
    pub fn report(&self, headline: impl Into<String>, summary: impl Into<String>) {
        let mut slot = self
            .outcome
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *slot = (headline.into(), summary.into());
        drop(slot);
        ACTIVITY.fetch_add(1, Ordering::Relaxed);
    }

    /// Marks one piece of on-demand work as finished. What separates "never run" from
    /// "run, and this is the answer" in the list.
    pub fn ran(&self) {
        self.runs.fetch_add(1, Ordering::Relaxed);
        ACTIVITY.fetch_add(1, Ordering::Relaxed);
    }

    /// Records something structured the tool found — an address, a hostname — for
    /// another tool to be offered. The log is for reading; this is for acting on.
    pub fn found(&self, kind: &str, value: impl Into<String>) {
        let value = value.into();
        // A domain whose MX is the root label — RFC 7505's way of saying "this domain
        // takes no mail" — trims to nothing, and an offer to read the certificate of
        // nowhere helps no one.
        if value.trim().is_empty() {
            return;
        }
        let mut findings = self
            .findings
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let entry = (kind.to_string(), value);
        if !findings.contains(&entry) {
            findings.push(entry);
        }
    }

    /// Whether the execution has been removed and every thread should wind down.
    pub fn stopping(&self) -> bool {
        self.shutdown.load(Ordering::Relaxed)
    }
}

/// One running instance of a tool.
pub struct Execution {
    pub id: u64,
    pub tool: &'static str,
    /// One-line description of what this instance is doing, e.g.
    /// `TCP 127.0.0.1:8080 → 10.0.0.5:5432`.
    pub summary: String,
    pub started: Instant,
    pub log: Arc<Mutex<EventLog>>,
    pub stats: Arc<Stats>,
    shutdown: Arc<AtomicBool>,
    /// Set by a tool's main loop when it exits for any reason — so the list can tell
    /// "still working" from "died on its own", instead of showing a dead row as live.
    finished: Arc<AtomicBool>,
    /// Whether this execution's tool only works when asked. Decides how the flags above
    /// read: for an on-demand tool "not finished" means "working right now" rather than
    /// "alive", and being finished is the resting state rather than the end.
    on_demand: bool,
    /// Never started at all — a bad address, a busy port. Distinguished from stopped
    /// because an on-demand execution that has simply never been asked to do anything
    /// looks exactly the same otherwise.
    failed: bool,
    /// How many pieces of on-demand work have completed.
    runs: Arc<AtomicU64>,
    /// The two result columns, written by the tool as it goes.
    outcome: Arc<Mutex<(String, String)>>,
    /// Structured findings, keyed by kind — what another tool could be pointed at.
    findings: Arc<Mutex<Vec<(String, String)>>>,
    /// What would recreate this execution on the next run. `None` for one nobody asked
    /// to persist; the tool itself never sets it, since being saved isn't its concern.
    spec: Option<persist::ExecutionSpec>,
}

impl Execution {
    /// Creates an execution and the `Recorder` its threads will hold. The tool calls
    /// this once it has everything that could fail (bound sockets, resolved addresses)
    /// already in hand.
    pub fn new(id: u64, tool: &'static str, summary: String) -> (Self, Recorder) {
        let log = Arc::new(Mutex::new(EventLog::new(LOG_CAPACITY)));
        let stats = Arc::new(Stats::default());
        let shutdown = Arc::new(AtomicBool::new(false));
        let runs = Arc::new(AtomicU64::new(0));
        let outcome = Arc::new(Mutex::new((String::new(), String::new())));
        let findings = Arc::new(Mutex::new(Vec::new()));
        let started = Instant::now();
        let recorder = Recorder {
            log: Arc::clone(&log),
            stats: Arc::clone(&stats),
            shutdown: Arc::clone(&shutdown),
            started,
            outcome: Arc::clone(&outcome),
            runs: Arc::clone(&runs),
            findings: Arc::clone(&findings),
        };
        let execution = Self {
            id,
            tool,
            summary,
            started,
            log,
            stats,
            shutdown,
            finished: Arc::new(AtomicBool::new(false)),
            on_demand: false,
            failed: false,
            runs,
            outcome,
            findings,
            spec: None,
        };
        (execution, recorder)
    }

    /// Marks this as an execution that idles until asked. Its threads aren't running,
    /// so it starts out finished rather than alive.
    pub fn on_demand(mut self) -> Self {
        self.on_demand = true;
        self.finished.store(true, Ordering::Relaxed);
        self
    }

    /// A second handle for a tool that starts work after the execution already exists.
    pub fn recorder(&self) -> Recorder {
        Recorder {
            log: Arc::clone(&self.log),
            stats: Arc::clone(&self.stats),
            shutdown: Arc::clone(&self.shutdown),
            started: self.started,
            outcome: Arc::clone(&self.outcome),
            runs: Arc::clone(&self.runs),
            findings: Arc::clone(&self.findings),
        }
    }

    /// Whether any work has ever completed here. What the list uses to decide there's a
    /// result worth showing at all.
    pub fn has_result(&self) -> bool {
        self.runs.load(Ordering::Relaxed) > 0
    }

    /// True while an on-demand tool is actually working.
    pub fn is_working(&self) -> bool {
        !self.finished.load(Ordering::Relaxed) && !self.shutdown.load(Ordering::Relaxed)
    }

    /// Where this row stands, in the one vocabulary the list can render.
    pub fn state(&self) -> State {
        if self.failed || self.shutdown.load(Ordering::Relaxed) {
            return State::Stopped;
        }
        if !self.on_demand {
            return if self.finished.load(Ordering::Relaxed) {
                State::Stopped
            } else {
                State::Running
            };
        }
        match (self.is_working(), self.has_result()) {
            (true, _) => State::Running,
            (false, true) => State::Done,
            (false, false) => State::Ready,
        }
    }

    /// Everything this execution found, kind and value, in the order it was found.
    /// What each kind is *worth doing* is decided in one place — see `offers_for`.
    pub fn all_findings(&self) -> Vec<(String, String)> {
        self.findings
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// The tool's own two result columns, blank until it has run.
    pub fn outcome(&self) -> (String, String) {
        self.outcome
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// An execution that never got off the ground, carrying the reason in its log.
    ///
    /// Kept in the list rather than dropped, because the alternative is a restored
    /// configuration silently disappearing when its port happens to be busy — the user
    /// would have no way to know it had ever been there.
    pub fn failed(id: u64, tool: &'static str, summary: String, error: String) -> Self {
        let (mut execution, recorder) = Self::new(id, tool, summary);
        recorder.record(0, EventKind::Error(error));
        execution.finished.store(true, Ordering::Relaxed);
        execution.failed = true;
        execution
    }

    /// Records what would recreate this execution across restarts.
    pub fn with_spec(mut self, spec: persist::ExecutionSpec) -> Self {
        self.spec = Some(spec);
        self
    }

    pub fn spec(&self) -> Option<&persist::ExecutionSpec> {
        self.spec.as_ref()
    }

    /// A second flag the tool's main loop sets on its way out. Handed over separately
    /// from the `Recorder` because only that one loop should ever set it.
    pub fn finish_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.finished)
    }

    /// Asks every thread this execution owns to wind down. They notice within one
    /// socket poll interval; nothing here blocks waiting for them.
    pub fn stop(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }

    pub fn is_running(&self) -> bool {
        !self.finished.load(Ordering::Relaxed) && !self.shutdown.load(Ordering::Relaxed)
    }
}

/// What an execution's row says in its last column.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// On-demand, never asked to do anything yet.
    Ready,
    Running,
    /// On-demand, work finished, results are there to read.
    Done,
    Stopped,
}
