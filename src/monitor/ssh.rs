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
    // `as_chunks` rather than `chunks_exact`: the record size is a constant, so each
    // chunk comes back as a fixed-size array and the trailing partial record (a
    // truncated utmp) is dropped by the same token.
    data.as_chunks::<RECORD_SIZE>()
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
        marked: false,
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

// --- detail view ---------------------------------------------------------------------

/// The `sshd` process the session hangs below — the one that actually holds the network
/// socket. `is_ssh_session` walks the same ancestry to answer yes/no; this returns the
/// pid, because the socket is where the session's real origin is written down.
fn sshd_ancestor(state: &SystemState, pid: u32) -> Option<u32> {
    let mut current = Some(Pid::from_u32(pid));
    for _ in 0..MAX_ANCESTOR_DEPTH {
        let p = current.and_then(|pid| state.sys.process(pid))?;
        if p.name().to_string_lossy() == "sshd" {
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
            "não localizado — os descritores do sshd são do root e o utmp não registrou um endereço",
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

fn build_detail(state: &SystemState, entry: &UtmpEntry, now: u64) -> Detail {
    let subtree = subtree_pids(state, entry.pid);
    let owner = state.sys.process(Pid::from_u32(entry.pid));
    let active = most_active(state, &subtree);

    let mut session = DetailSection::new("Sessão");
    session.push("Usuário", entry.user.clone());
    session.push(
        "Origem (utmp)",
        if entry.host.is_empty() {
            "não registrada".to_string()
        } else {
            entry.host.clone()
        },
    );
    session.push("Terminal", entry.line.clone());
    session.push(
        "Conectado há",
        format::human_duration(now.saturating_sub(entry.login_time)),
    );
    session.push("PID da sessão", entry.pid.to_string());
    // utmp registers the privilege-separated sshd, not the shell — see `subtree_pids`.
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
        title: format!("{}@{} · {}", entry.user, entry.line, entry.host),
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
        let entry = read_utmp().into_iter().find(|e| e.pid == row.pid)?;
        Some(build_detail(state, &entry, now))
    }

    fn has_detail(&self) -> bool {
        true
    }
}
