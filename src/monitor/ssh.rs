use std::collections::HashMap;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use sysinfo::{Pid, Process};

use super::process::command_of;
use super::{SystemState, TableMonitor, TableRow};
use crate::format;

const HEADERS: [&str; 6] = ["User", "Host", "TTY", "Time", "Folder", "Command"];

// --- utmp parsing --------------------------------------------------------------------
//
// `who`/`w` read the same file to answer "who's logged in, from where, since when" — we
// do the same rather than shelling out. Layout confirmed against this system's
// <utmp.h> via `offsetof` (glibc's `struct utmp`, x86_64): fixed 384-byte native-endian
// records, one per login slot.

const UTMP_PATHS: [&str; 2] = ["/var/run/utmp", "/run/utmp"];
const RECORD_SIZE: usize = 384;
/// `USER_PROCESS` from <utmp.h> — a slot currently holding a live login session (as
/// opposed to e.g. `DEAD_PROCESS` for one that's since logged out).
const USER_PROCESS: i16 = 7;

const UT_TYPE: usize = 0;
const UT_PID: usize = 4;
const UT_LINE: usize = 8;
const UT_LINE_LEN: usize = 32;
const UT_USER: usize = 44;
const UT_USER_LEN: usize = 32;
const UT_HOST: usize = 76;
const UT_HOST_LEN: usize = 256;
const UT_TV_SEC: usize = 340;

/// A live login slot from utmp — one per tty session, however it was opened (SSH,
/// console, X, a re-attached tmux, ...). `is_ssh_session` below narrows this down to
/// genuine SSH logins, since a non-empty `host` alone isn't a reliable signal — e.g. a
/// local tmux re-attach or the X display both populate it too (as `will(tmux(...).%1)`
/// or `:0`).
struct UtmpEntry {
    pid: u32,
    line: String,
    user: String,
    host: String,
    login_time: u64,
}

fn cstr(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// Every logged-in session in the system's utmp database. Empty if it can't be read
/// (unsupported platform, permissions) — same best-effort fallback used elsewhere in
/// this module for `/proc`-derived data.
fn read_utmp() -> Vec<UtmpEntry> {
    let Some(data) = UTMP_PATHS.iter().find_map(|p| fs::read(p).ok()) else {
        return Vec::new();
    };
    data.chunks_exact(RECORD_SIZE)
        .filter_map(|rec| {
            let ty = i16::from_ne_bytes(rec[UT_TYPE..UT_TYPE + 2].try_into().unwrap());
            if ty != USER_PROCESS {
                return None;
            }
            let pid = i32::from_ne_bytes(rec[UT_PID..UT_PID + 4].try_into().unwrap());
            if pid <= 0 {
                return None;
            }
            let login_time =
                i32::from_ne_bytes(rec[UT_TV_SEC..UT_TV_SEC + 4].try_into().unwrap()).max(0) as u64;
            Some(UtmpEntry {
                pid: pid as u32,
                line: cstr(&rec[UT_LINE..UT_LINE + UT_LINE_LEN]),
                user: cstr(&rec[UT_USER..UT_USER + UT_USER_LEN]),
                host: cstr(&rec[UT_HOST..UT_HOST + UT_HOST_LEN]),
                login_time,
            })
        })
        .collect()
}

const MAX_ANCESTOR_DEPTH: usize = 8;

/// Whether `pid` (utmp's login-slot pid — the session's shell) descends from an `sshd`
/// process within a handful of generations. This, not a non-empty utmp host, is what
/// actually distinguishes an SSH login from a local tty/X/tmux session.
fn is_ssh_session(state: &SystemState, pid: u32) -> bool {
    let mut current = Some(Pid::from_u32(pid));
    for _ in 0..MAX_ANCESTOR_DEPTH {
        let Some(p) = current.and_then(|pid| state.sys.process(pid)) else {
            return false;
        };
        if p.name().to_string_lossy() == "sshd" {
            return true;
        }
        current = p.parent();
    }
    false
}

/// Every descendant pid of `root`, however many generations deep — not just its direct
/// children. OpenSSH's privilege-separated model registers utmp's pid on a "monitor"
/// process (still shown as `sshd: user@pts/N`, one setuid hop removed from the socket
/// and *forking* — not exec'ing — the login shell) so the real shell is already one
/// level down, and a login shell that itself re-execs into a multiplexer, `su`, etc.
/// pushes the session's real activity down further still. A single-hop child lookup
/// misses all of that.
fn subtree_pids(state: &SystemState, root: u32) -> Vec<u32> {
    let mut result = Vec::new();
    let mut frontier = vec![root];
    while let Some(pid) = frontier.pop() {
        let parent = Pid::from_u32(pid);
        for child in state
            .sys
            .processes()
            .values()
            .filter(|p| p.parent() == Some(parent))
        {
            let child_pid = child.pid().as_u32();
            result.push(child_pid);
            frontier.push(child_pid);
        }
    }
    result
}

/// The process in `subtree` most likely to be "what the session is doing right now":
/// highest CPU usage, ties (most commonly all-idle) broken toward the most recently
/// started, since a command just launched in the foreground outranks a long-idle
/// background one reading the same 0%.
fn most_active<'a>(state: &'a SystemState, subtree: &[u32]) -> Option<&'a Process> {
    subtree
        .iter()
        .filter_map(|&pid| state.sys.process(Pid::from_u32(pid)))
        .max_by(|a, b| {
            a.cpu_usage()
                .total_cmp(&b.cpu_usage())
                .then(a.start_time().cmp(&b.start_time()))
        })
}

/// Extra pids to take down alongside the session's own registered pid so Del actually
/// disconnects the user instead of just orphaning their shell: every process in its
/// subtree, plus — when it's an only child — its direct parent too. That parent is
/// almost always the per-connection privileged `sshd` monitor that actually owns the
/// network socket (privsep hands the pty off to a forked, setuid'd child rather than
/// exec'ing it away); killing just the session pid leaves that monitor holding the
/// connection open. The only-child check exists so this never targets something
/// shared, like the main listening sshd, which has many.
fn kill_targets(state: &SystemState, session_pid: u32, subtree: &[u32]) -> Vec<u32> {
    let mut targets = subtree.to_vec();
    if let Some(parent) = state
        .sys
        .process(Pid::from_u32(session_pid))
        .and_then(Process::parent)
    {
        let siblings = state
            .sys
            .processes()
            .values()
            .filter(|p| p.parent() == Some(parent))
            .count();
        if siblings <= 1 {
            targets.push(parent.as_u32());
        }
    }
    targets
}

/// Builds a row from an utmp entry: user/host/tty straight from utmp, connected time as
/// an age, and folder/command taken from whichever process in the session's subtree
/// looks like its current activity (see `most_active`) — falling back to the session's
/// own registered pid when it's simply sitting idle at a prompt with no active
/// descendant yet.
fn build_row(state: &SystemState, entry: &UtmpEntry, now: u64) -> TableRow {
    let subtree = subtree_pids(state, entry.pid);
    let owner = state.sys.process(Pid::from_u32(entry.pid));
    let active = most_active(state, &subtree);

    let (command, cwd_source) = match active {
        Some(p) => (command_of(p), Some(p)),
        None => (
            owner.map(command_of).unwrap_or_else(|| "-".to_string()),
            owner,
        ),
    };
    let folder = cwd_source
        .and_then(Process::cwd)
        .map(|c| c.display().to_string())
        .unwrap_or_else(|| "?".to_string());
    let host = if entry.host.is_empty() {
        "-".to_string()
    } else {
        entry.host.clone()
    };

    TableRow {
        cells: vec![
            entry.user.clone(),
            host,
            entry.line.clone(),
            format::human_duration(now.saturating_sub(entry.login_time)),
            folder,
            command,
        ],
        pid: entry.pid,
        depth: 0,
        is_last_sibling: true,
        guides: Vec::new(),
        child_count: 0,
        descendant_pids: kill_targets(state, entry.pid, &subtree),
        key: String::new(),
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn sample_sessions(state: &SystemState, limit: Option<usize>) -> Vec<TableRow> {
    let now = now_secs();
    let mut entries: Vec<UtmpEntry> = read_utmp()
        .into_iter()
        .filter(|e| is_ssh_session(state, e.pid))
        .collect();
    // Most recently connected first — the sessions someone's likely to care about.
    entries.sort_by_key(|e| std::cmp::Reverse(e.login_time));

    entries
        .into_iter()
        .take(limit.unwrap_or(usize::MAX))
        .map(|e| build_row(state, &e, now))
        .collect()
}

pub struct SshSessionsMonitor;

impl TableMonitor for SshSessionsMonitor {
    fn title(&self) -> &'static str {
        "SSH Sessions"
    }

    fn headers(&self) -> &'static [&'static str] {
        &HEADERS
    }

    fn sample(&mut self, state: &SystemState, limit: Option<usize>) -> Vec<TableRow> {
        sample_sessions(state, limit)
    }

    fn refresh_values(&mut self, state: &SystemState, rows: &mut [TableRow]) {
        let now = now_secs();
        let entries: HashMap<u32, UtmpEntry> =
            read_utmp().into_iter().map(|e| (e.pid, e)).collect();
        for row in rows.iter_mut() {
            // A session that's since logged out just keeps showing its last known
            // values — same "stale beats missing" tradeoff as a dead pid elsewhere.
            let Some(entry) = entries.get(&row.pid) else {
                continue;
            };
            *row = build_row(state, entry, now);
        }
    }
}
