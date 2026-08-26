use std::collections::HashMap;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use sysinfo::{Pid, Process};

use super::iface;
use super::ports::{SocketRow, TCP_ESTABLISHED, socket_inodes, socket_table};
use super::process::{command_of, kill_danger};
use super::resolve::user_name;
use super::{Danger, Detail, DetailSection, SystemState, TableMonitor, TableRow, mark};
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

/// A live login session, as either source records it — one per tty session, however it
/// was opened (SSH, console, X, a re-attached tmux, ...).
struct Session {
    pid: u32,
    /// The terminal, when the source names it. utmp always does; logind mostly
    /// doesn't, and `tty_of` reads it back off the processes instead.
    line: String,
    user: String,
    host: String,
    login_time: u64,
    /// Whether the source itself vouched that this is an SSH login. utmp doesn't — a
    /// non-empty `host` alone isn't a reliable signal there, since a local tmux
    /// re-attach or the X display populate it too (as `will(tmux(...).%1)` or `:0`) —
    /// so those entries still go through `is_ssh_session`. logind names the PAM
    /// service that opened the session, which is a direct answer.
    vouched_ssh: bool,
}

fn cstr(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// Every logged-in session in the system's utmp database, or `None` when the machine
/// keeps no utmp at all — which is the one case `sessions` has to tell apart from an
/// utmp that's simply empty because nobody is logged in.
fn read_utmp() -> Option<Vec<Session>> {
    let data = UTMP_PATHS.iter().find_map(|p| fs::read(p).ok())?;
    // `as_chunks` rather than `chunks_exact`: the record size is a constant, so each
    // chunk comes back as a fixed-size array and the trailing partial record (a
    // truncated utmp) is dropped by the same token.
    let entries = data
        .as_chunks::<RECORD_SIZE>()
        .0
        .iter()
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
            Some(Session {
                pid: pid as u32,
                line: cstr(&rec[UT_LINE..UT_LINE + UT_LINE_LEN]),
                user: cstr(&rec[UT_USER..UT_USER + UT_USER_LEN]),
                host: cstr(&rec[UT_HOST..UT_HOST + UT_HOST_LEN]),
                login_time,
                vouched_ssh: false,
            })
        })
        .collect();
    Some(entries)
}

// --- logind sessions -----------------------------------------------------------------
//
// systemd can be built without utmp support (`-UTMP` in `systemctl --version`), and
// from systemd 257 distributions have started shipping it that way — Debian 13 does.
// On those machines `/run/utmp` doesn't exist at all and `who` reads logind instead;
// without this fallback the panel is simply empty there, since the file it wants is
// never coming back.
//
// The session files carry the same four facts utmp did — who, from where, since when,
// and the pid of the session leader — as plain `KEY=value` lines, world-readable. Their
// header says "This is private data. Do not parse.", which is about the format being
// unstable rather than secret: the supported route is libsystemd, and linking a C
// library for four strings is a worse trade than parsing them. So every field is read
// defensively and a session missing any of them is dropped, which costs a row rather
// than a crash if the format does move.

const LOGIND_SESSIONS: &str = "/run/systemd/sessions";

/// The PAM service names that mean "somebody logged in over SSH". OpenSSH asks for
/// `sshd` unless it was built to ask for something else, and `ssh` is the one other
/// spelling packagers pick — both are accepted rather than betting on the first.
const SSH_SERVICES: [&str; 2] = ["sshd", "ssh"];

/// Every SSH login logind currently holds open. Sessions of any other class or service
/// — logind's own `manager-early` bookkeeping, a console login, a display manager —
/// aren't this panel's subject and are dropped here rather than downstream.
fn read_logind() -> Vec<Session> {
    let Ok(dir) = fs::read_dir(LOGIND_SESSIONS) else {
        return Vec::new();
    };
    dir.filter_map(Result::ok)
        // Beside each session file sits a `<id>.ref` FIFO that logind keeps open for as
        // long as the session lives; opening it here would block on the writer.
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|entry| fs::read_to_string(entry.path()).ok())
        .filter_map(|content| parse_logind_session(&content))
        .collect()
}

/// One session file, or `None` if it isn't an SSH login or doesn't carry what a row
/// needs. `REALTIME` is microseconds since the epoch; `TTY` is usually absent for an
/// SSH session, because sshd registers the session with PAM before it has allocated the
/// pty, so the terminal is worked out later from the processes (`tty_of`).
fn parse_logind_session(content: &str) -> Option<Session> {
    let fields: HashMap<&str, &str> = content
        .lines()
        .filter_map(|line| line.split_once('='))
        .collect();

    if fields.get("CLASS") != Some(&"user") {
        return None;
    }
    if !SSH_SERVICES.contains(fields.get("SERVICE")?) {
        return None;
    }
    Some(Session {
        pid: fields.get("LEADER")?.parse().ok()?,
        line: fields.get("TTY").unwrap_or(&"").to_string(),
        user: fields.get("USER")?.to_string(),
        host: fields.get("REMOTE_HOST").unwrap_or(&"").to_string(),
        login_time: fields.get("REALTIME")?.parse::<u64>().ok()? / 1_000_000,
        vouched_ssh: true,
    })
}

/// Every live login the system will tell us about. utmp stays the source wherever the
/// machine still keeps one — it's what `who` has always read, and it records the
/// terminal directly — with logind as the fallback for machines that no longer have the
/// file at all.
fn sessions() -> Vec<Session> {
    read_utmp().unwrap_or_else(read_logind)
}

const MAX_ANCESTOR_DEPTH: usize = 8;

/// The process names OpenSSH runs a login under. Up to 9.7 that was one binary,
/// `sshd`, for the listener and every connection alike; 9.8 split the per-connection
/// work out into `sshd-session` (and the authentication step again into `sshd-auth`),
/// leaving only the listener called `sshd`. Matching just the old name still finds the
/// listener at the top of the ancestry, so the sessions kept being recognised — but
/// `sshd_ancestor` would then answer with the *listener*, whose sockets are the ones it
/// accepts on, not the one this session is.
const SSHD_NAMES: [&str; 2] = ["sshd", "sshd-session"];

/// Whether `pid` (the session's registered pid) descends from an sshd process within a
/// handful of generations. This, not a non-empty utmp host, is what actually
/// distinguishes an SSH login from a local tty/X/tmux session.
fn is_ssh_session(state: &SystemState, pid: u32) -> bool {
    sshd_ancestor(state, pid).is_some()
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
fn build_row(state: &SystemState, entry: &Session, now: u64) -> TableRow {
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
            tty_of(state, entry, &subtree),
            format::human_duration(now.saturating_sub(entry.login_time)),
            folder,
            command,
        ],
        pid: entry.pid,
        depth: 0,
        is_last_sibling: true,
        guides: Vec::new(),
        mark: None,
        child_count: 0,
        descendant_pids: kill_targets(state, entry.pid, &subtree),
        key: String::new(),
    }
}

/// The session's terminal. utmp records it directly; logind's session file usually
/// doesn't, so it's read back off the session's processes instead — from `tty_nr` in
/// `/proc/<pid>/stat` rather than `/proc/<pid>/fd/0`, because that file is world-
/// readable and another user's shell's descriptors are not.
///
/// The login shell is asked first: every process in the session shares its controlling
/// terminal, but one that opened a pty of its own (a `screen`, a nested `ssh`) would
/// answer with that one instead.
fn tty_of(state: &SystemState, entry: &Session, subtree: &[u32]) -> String {
    if !entry.line.is_empty() {
        return entry.line.clone();
    }
    login_shell(state, subtree)
        .map(|shell| shell.pid().as_u32())
        .and_then(tty_name)
        .or_else(|| subtree.iter().copied().find_map(tty_name))
        .or_else(|| tty_name(entry.pid))
        .unwrap_or_else(|| "-".to_string())
}

/// Field index of `tty_nr` in `/proc/<pid>/stat`, counted from the field after the
/// command — the 7th overall, after `state`, `ppid`, `pgrp` and `session`.
const TTY_NR_FIELD: usize = 4;

/// The terminal `pid` is attached to, or `None` if it has none.
fn tty_name(pid: u32) -> Option<String> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The second field is the command in parentheses, and a command may contain both
    // spaces and parentheses — so the fields are counted from the last `)`, not from
    // the start of the line.
    let rest = &stat[stat.rfind(')')? + 1..];
    tty_from_dev(rest.split_whitespace().nth(TTY_NR_FIELD)?.parse().ok()?)
}

/// A terminal's device number spelled the way everything else spells it — `pts/4`,
/// `tty1`. `tty_nr` arrives in the encoding the kernel writes device numbers with: the
/// minor's low byte at the bottom, the major above it, and whatever is left of the
/// minor above that. Zero is what a process with no controlling terminal has.
fn tty_from_dev(tty_nr: u32) -> Option<String> {
    if tty_nr == 0 {
        return None;
    }
    let major = (tty_nr >> 8) & 0xfff;
    let minor = (tty_nr & 0xff) | ((tty_nr >> 12) & 0xfff00);
    Some(match major {
        // UNIX98 pty slaves — eight majors of 256 terminals each, numbered end to end.
        136..=143 => format!("pts/{}", (major - 136) * 256 + minor),
        // Virtual consoles, and the serial lines that share their major above 63.
        4 if minor < 64 => format!("tty{minor}"),
        4 => format!("ttyS{}", minor - 64),
        _ => format!("{major}:{minor}"),
    })
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn sample_sessions(state: &SystemState, limit: Option<usize>) -> Vec<TableRow> {
    let now = now_secs();
    let mut entries: Vec<Session> = sessions()
        .into_iter()
        // A session whose leader is gone is a session that ended, whatever the source
        // still says. utmp's own filter catches this on the way past `is_ssh_session`;
        // logind needs it said out loud, because it holds a session open for as long as
        // anything started under it is still running — a tmux server that outlived the
        // login it was started from keeps its scope, and its session file, alive for
        // days.
        .filter(|e| state.sys.process(Pid::from_u32(e.pid)).is_some())
        .filter(|e| e.vouched_ssh || is_ssh_session(state, e.pid))
        .collect();
    // Most recently connected first — the sessions someone's likely to care about.
    entries.sort_by_key(|e| std::cmp::Reverse(e.login_time));

    entries
        .into_iter()
        .take(limit.unwrap_or(usize::MAX))
        .map(|e| build_row(state, &e, now))
        .collect()
}

// --- detail view ---------------------------------------------------------------------

/// The sshd process the session hangs below — the one that actually holds the network
/// socket, which is where the session's real origin is written down. `is_ssh_session`
/// is the same walk asked as a yes/no question.
fn sshd_ancestor(state: &SystemState, pid: u32) -> Option<u32> {
    let mut current = Some(Pid::from_u32(pid));
    for _ in 0..MAX_ANCESTOR_DEPTH {
        let p = current.and_then(|pid| state.sys.process(pid))?;
        if SSHD_NAMES.contains(&p.name().to_string_lossy().as_ref()) {
            return Some(p.pid().as_u32());
        }
        current = p.parent();
    }
    None
}

/// Where the login actually came from, in as much detail as the kernel will give us.
///
/// The socket belongs to sshd, whose `/proc/<pid>/fd` is closed to everyone but root —
/// so the inode route only works when monitorzinho is running as root. Failing that,
/// the socket *tables* are world-readable even when the descriptors aren't: an
/// established connection from the address utmp recorded is the same connection, found
/// from the other end.
fn connection_section(state: &SystemState, session_pid: u32, host: &str) -> DetailSection {
    let mut section = DetailSection::new("Conexão");
    let Some(sshd) = sshd_ancestor(state, session_pid) else {
        section.push("sshd", "não encontrado na ancestralidade desta sessão");
        return section;
    };
    section.push("Processo sshd", sshd.to_string());

    let table = socket_table();
    let inodes = socket_inodes(sshd);
    if let Some(row) = table
        .iter()
        .find(|row| inodes.contains(&row.inode) && row.remote_port != 0)
    {
        push_socket(&mut section, "", row, state);
        return section;
    }

    let candidates: Vec<&SocketRow> = table
        .iter()
        .filter(|row| row.proto == "TCP" && row.state == TCP_ESTABLISHED && row.remote_ip == host)
        .collect();
    match candidates.as_slice() {
        // One connection from that address: it can only be this session's.
        [row] => push_socket(&mut section, "", row, state),
        // Several sessions from the same address are indistinguishable from here — utmp
        // doesn't record the port — so they're offered as what they are rather than one
        // of them being picked and presented as fact.
        [..] if !candidates.is_empty() => {
            section.push(
                "Conexões de " .to_owned().as_str(),
                format!("{host} — {} abertas, indistinguíveis daqui", candidates.len()),
            );
            for row in candidates.iter().take(MAX_CANDIDATES) {
                push_socket(&mut section, "candidata: ", row, state);
            }
        }
        _ => section.push(
            "Socket",
            "não localizado — os descritores do sshd são do root e a sessão não registrou um endereço",
        ),
    }
    section
}

/// Candidate connections shown when the address alone can't pick one out.
const MAX_CANDIDATES: usize = 4;

/// The endpoint pair and what's queued on it, as read from the socket table.
fn push_socket(section: &mut DetailSection, prefix: &str, row: &SocketRow, state: &SystemState) {
    section.push(&format!("{prefix}De"), row.remote());
    section.push(
        &format!("{prefix}Para"),
        format!("{} [{}]", row.local(), row.family),
    );
    if let Some(interface) = iface::interface_of(&state.networks, &row.local_ip) {
        section.push(&format!("{prefix}Interface"), interface);
    }
    section.push(
        &format!("{prefix}Filas"),
        format!(
            "{} a receber · {} a enviar",
            format::human_bytes(row.rx_queue as f64),
            format::human_bytes(row.tx_queue as f64)
        ),
    );
    section.push(
        &format!("{prefix}Dono do socket"),
        match user_name(row.uid) {
            Some(name) => format!("{name} (uid {})", row.uid),
            None => format!("uid {}", row.uid),
        },
    );
}

/// The session's login shell. A login shell is exec'd with a dash in front of its name
/// — the convention that tells it to read the login profile — which is what tells it
/// apart from every other command the session has since started.
fn login_shell<'a>(state: &'a SystemState, subtree: &[u32]) -> Option<&'a Process> {
    subtree
        .iter()
        .filter_map(|&pid| state.sys.process(Pid::from_u32(pid)))
        .find(|p| command_of(p).starts_with('-'))
}

/// What the session is running, listed rather than reduced to the one "most active"
/// process the table column shows. A login sitting at a prompt has one entry here; one
/// running a build inside tmux has the whole tree.
fn processes_section(state: &SystemState, subtree: &[u32]) -> DetailSection {
    let mut section = DetailSection::new("Processos da sessão");
    let mut listed: Vec<&Process> = subtree
        .iter()
        .filter_map(|&pid| state.sys.process(Pid::from_u32(pid)))
        .filter(|p| p.thread_kind().is_none())
        .collect();
    // Busiest first, same ordering `most_active` picks the headline command by.
    listed.sort_by(|a, b| {
        b.cpu_usage()
            .total_cmp(&a.cpu_usage())
            .then(b.start_time().cmp(&a.start_time()))
    });
    section.push("Total", listed.len().to_string());
    for p in listed.iter().take(MAX_SESSION_PROCESSES) {
        section.push(
            &p.pid().as_u32().to_string(),
            format!("{:.1}%  {}", p.cpu_usage(), command_of(p)),
        );
    }
    if listed.len() > MAX_SESSION_PROCESSES {
        section.push(
            "E mais",
            format!("{} processo(s)", listed.len() - MAX_SESSION_PROCESSES),
        );
    }
    section
}

/// Processes listed before the tail is summarised. A session inside a multiplexer can
/// hold dozens; the first screenful is what identifies it.
const MAX_SESSION_PROCESSES: usize = 15;

fn build_detail(state: &SystemState, entry: &Session, now: u64) -> Detail {
    let subtree = subtree_pids(state, entry.pid);
    let tty = tty_of(state, entry, &subtree);
    let owner = state.sys.process(Pid::from_u32(entry.pid));
    let active = most_active(state, &subtree);

    let mut session = DetailSection::new("Sessão");
    session.push("Usuário", entry.user.clone());
    session.push(
        "Origem registrada",
        if entry.host.is_empty() {
            "não registrada".to_string()
        } else {
            entry.host.clone()
        },
    );
    session.push("Terminal", tty.clone());
    session.push(
        "Conectado há",
        format::human_duration(now.saturating_sub(entry.login_time)),
    );
    session.push("PID da sessão", entry.pid.to_string());
    // What's registered is the privilege-separated sshd, not the shell — see
    // `subtree_pids`.
    session.push(
        "Processo registrado",
        owner.map(command_of).unwrap_or_else(|| "?".to_string()),
    );
    if let Some(shell) = login_shell(state, &subtree) {
        session.push(
            "Shell",
            format!("{} · {}", shell.pid().as_u32(), command_of(shell)),
        );
    }
    session.push(
        "Diretório",
        active
            .or(owner)
            .and_then(Process::cwd)
            .map(|cwd| cwd.display().to_string())
            .unwrap_or_else(|| "?".to_string()),
    );
    session.push(
        "Comando ativo",
        match active {
            Some(p) => command_of(p),
            None => "nada além do shell — sessão parada no prompt".to_string(),
        },
    );

    Detail {
        title: format!("{}@{} · {}", entry.user, tty, entry.host),
        gone_note: "desconectada",
        sections: vec![
            session,
            connection_section(state, entry.pid, &entry.host),
            processes_section(state, &subtree),
        ],
        rates: None,
        handoffs: Vec::new(),
        handoff_title: "",
    }
}

pub struct SshSessionsMonitor;

impl TableMonitor for SshSessionsMonitor {
    fn id(&self) -> &'static str {
        "ssh"
    }

    fn title(&self) -> &'static str {
        "SSH Sessions"
    }

    fn headers(&self) -> &'static [&'static str] {
        &HEADERS
    }

    /// A session is a person, first of all — and the origin matters when the same
    /// person is logged in from three places.
    fn mark_kinds(&self) -> &'static [mark::MarkKind] {
        &[
            mark::MarkKind {
                name: "usuário",
                column: 0,
                numeric: false,
                help: "o login, exato ou como expressão regular",
            },
            mark::MarkKind {
                name: "origem",
                column: 1,
                numeric: false,
                help: "de onde a sessão veio",
            },
            mark::MarkKind {
                name: "comando",
                column: 5,
                numeric: false,
                help: "o que a sessão está rodando",
            },
        ]
    }

    fn sample(&mut self, state: &SystemState, limit: Option<usize>) -> Vec<TableRow> {
        sample_sessions(state, limit)
    }

    fn refresh_values(&mut self, state: &SystemState, rows: &mut [TableRow]) {
        let now = now_secs();
        let entries: HashMap<u32, Session> = sessions().into_iter().map(|e| (e.pid, e)).collect();
        for row in rows.iter_mut() {
            // A session that's since logged out just keeps showing its last known
            // values — same "stale beats missing" tradeoff as a dead pid elsewhere.
            let Some(entry) = entries.get(&row.pid) else {
                continue;
            };
            *row = build_row(state, entry, now);
        }
    }

    /// Del here doesn't kill a process someone was looking at — it throws a person off
    /// the machine. The confirmation says whose session it is and what they lose.
    fn danger(&self, state: &SystemState, row: &TableRow) -> Option<Danger> {
        let user = row.cells.first().cloned().unwrap_or_default();
        let tty = row.cells.get(2).cloned().unwrap_or_default();
        let from = row.cells.get(1).cloned().unwrap_or_default();
        kill_danger(
            state,
            row,
            "desconectar sessão",
            &format!("Desconectar a sessão de {user} em {tty}"),
            vec![
                format!("A conexão vinda de {from} cai na hora, sem aviso do outro lado."),
                "Morre o shell, tudo que a sessão estiver rodando e o sshd que segura a \
                 conexão — trabalho não salvo se perde."
                    .to_string(),
            ],
        )
    }

    fn detail(&mut self, state: &SystemState, row: &TableRow) -> Option<Detail> {
        let now = now_secs();
        let entry = sessions().into_iter().find(|e| e.pid == row.pid)?;
        Some(build_detail(state, &entry, now))
    }

    fn has_detail(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A session file as Debian 13 writes one for an SSH login: no `TTY`, because sshd
    /// registers the session before the pty exists.
    const SSH_SESSION: &str = "\
# This is private data. Do not parse.
UID=0
USER=root
ACTIVE=1
STATE=active
REMOTE=1
TYPE=tty
CLASS=user
SCOPE=session-9.scope
REMOTE_HOST=187.116.88.134
SERVICE=sshd
LEADER=2805
REALTIME=1787699034767096
MONOTONIC=903506521
";

    #[test]
    fn an_ssh_login_reads_back_off_a_logind_session_file() {
        let session = parse_logind_session(SSH_SESSION).expect("uma sessão SSH");
        assert_eq!(session.user, "root");
        assert_eq!(session.host, "187.116.88.134");
        assert_eq!(session.pid, 2805);
        // REALTIME is microseconds; the row wants seconds.
        assert_eq!(session.login_time, 1_787_699_034);
        assert!(session.line.is_empty(), "o terminal vem dos processos");
        assert!(session.vouched_ssh, "o próprio logind disse que é sshd");
    }

    #[test]
    fn logind_bookkeeping_is_not_a_login() {
        // logind's own per-user manager: same UID, same file layout, nobody logged in.
        let manager = SSH_SESSION
            .replace("CLASS=user", "CLASS=manager-early")
            .replace("SERVICE=sshd", "SERVICE=systemd-user");
        assert!(parse_logind_session(&manager).is_none());
        // A console login is a session, just not one this panel is about.
        let console = SSH_SESSION.replace("SERVICE=sshd", "SERVICE=login");
        assert!(parse_logind_session(&console).is_none());
    }

    #[test]
    fn a_session_missing_what_a_row_needs_is_dropped() {
        for field in ["LEADER=2805", "USER=root", "REALTIME=1787699034767096"] {
            let without = SSH_SESSION.replace(field, "");
            assert!(
                parse_logind_session(&without).is_none(),
                "sem {field} não dá para montar a linha"
            );
        }
    }

    #[test]
    fn a_terminal_is_named_the_way_everything_else_names_it() {
        assert_eq!(tty_from_dev(34820).as_deref(), Some("pts/4"));
        // The second pty major picks up where the first leaves off.
        assert_eq!(tty_from_dev((137 << 8) | 1).as_deref(), Some("pts/257"));
        assert_eq!(tty_from_dev((4 << 8) | 1).as_deref(), Some("tty1"));
        assert_eq!(tty_from_dev((4 << 8) | 64).as_deref(), Some("ttyS0"));
        assert_eq!(tty_from_dev(0), None, "processo sem terminal de controle");
    }
}
