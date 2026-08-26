use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::time::Instant;

use sysinfo::{Pid, Process};

use super::ports::{listening_port_set, record_traffic, socket_inodes, socket_table};
use super::resolve::user_name;
use super::{Danger, Detail, DetailSection, Rates, SystemState, TableMonitor, TableRow, mark};
use crate::format;
use crate::tools::Handoff;

const HEADERS: [&str; 3] = ["Command", "Time", "Usage"];

/// `System::processes()` includes threads (Linux exposes each thread under its own
/// `/proc/<tid>`), each reporting the whole process' shared RSS — filter those out so
/// the top lists show actual processes, not duplicated thread entries.
fn is_process(p: &&Process) -> bool {
    p.thread_kind().is_none()
}

/// Full command line (falls back to the process name if `cmd()` is unavailable, e.g.
/// a kernel thread or a process we don't have permission to read `/proc/<pid>/cmdline`
/// for). Left untruncated — the table widget clips it to the column width on render.
pub(super) fn command_of(p: &Process) -> String {
    let cmd = p.cmd();
    if cmd.is_empty() {
        p.name().to_string_lossy().into_owned()
    } else {
        cmd.iter()
            .map(|s| s.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Command and age (`human_duration(run_time())`) for `pid`, or `("?", "-")` if it's
/// not a currently-running process (e.g. a port/connection whose owner we couldn't
/// resolve, or that's since exited). Shared by every table that attributes a row to a
/// process it only knows the pid of (Ports, Connections) rather than owning it
/// directly (Top CPU/Memory, which already hold a `&Process` at row-build time).
pub(super) fn describe_owner(state: &SystemState, pid: u32) -> (String, String) {
    match state.sys.process(Pid::from_u32(pid)) {
        Some(p) => (command_of(p), format::human_duration(p.run_time())),
        None => ("?".to_string(), "-".to_string()),
    }
}

/// Flat top-`limit` processes ranked by their own `metric` — no hierarchy. Used for the
/// compact panel: a tree's top row is ranked by its *subtree's* combined usage, so an
/// idle root sitting above a busy child would show a misleadingly blank Usage column
/// with no room to expand and explain it. The uncapped fullscreen view doesn't have
/// that problem — it shows the whole tree at once — so it keeps `build_tree` instead.
fn build_flat_top(
    state: &SystemState,
    limit: usize,
    metric: &dyn Fn(&Process) -> f64,
    format_usage: &dyn Fn(&Process) -> String,
) -> Vec<TableRow> {
    let mut procs: Vec<&Process> = state.sys.processes().values().filter(is_process).collect();
    procs.sort_by(|a, b| metric(b).total_cmp(&metric(a)));
    procs
        .into_iter()
        .take(limit)
        .map(|p| {
            TableRow::leaf(
                vec![
                    command_of(p),
                    format::human_duration(p.run_time()),
                    format_usage(p),
                ],
                p.pid().as_u32(),
            )
        })
        .collect()
}

/// Bottom-up aggregate `metric` per subtree (own value + every descendant's), and the
/// full list of descendant pids below each node — memoized per pid so ranking siblings
/// never re-walks the same subtree twice.
fn aggregate(
    pid: u32,
    procs: &HashMap<u32, &Process>,
    children: &HashMap<u32, Vec<u32>>,
    metric: &dyn Fn(&Process) -> f64,
    agg: &mut HashMap<u32, f64>,
    descendants: &mut HashMap<u32, Vec<u32>>,
) -> f64 {
    let own = procs.get(&pid).map(|p| metric(p)).unwrap_or(0.0);
    let mut total = own;
    let mut desc = Vec::new();
    if let Some(kids) = children.get(&pid) {
        for &kid in kids {
            total += aggregate(kid, procs, children, metric, agg, descendants);
            desc.push(kid);
            if let Some(kid_desc) = descendants.get(&kid) {
                desc.extend(kid_desc.iter().copied());
            }
        }
    }
    agg.insert(pid, total);
    descendants.insert(pid, desc);
    total
}

/// Siblings (and roots) are ranked by their subtree's aggregate metric, descending —
/// so a root that's idle itself but has a heavy descendant still rises to the top. Ties
/// break by pid for a stable order.
fn rank(pids: &mut [u32], agg: &HashMap<u32, f64>) {
    pids.sort_by(|a, b| {
        agg.get(b)
            .unwrap_or(&0.0)
            .partial_cmp(agg.get(a).unwrap_or(&0.0))
            .unwrap_or(Ordering::Equal)
            .then(a.cmp(b))
    });
}

/// Depth-first, pre-order flatten of one subtree into `TableRow`s.
#[allow(clippy::too_many_arguments)]
fn flatten(
    pid: u32,
    depth: usize,
    is_last_sibling: bool,
    guides: Vec<bool>,
    procs: &HashMap<u32, &Process>,
    children: &HashMap<u32, Vec<u32>>,
    agg: &HashMap<u32, f64>,
    descendants: &HashMap<u32, Vec<u32>>,
    format_usage: &dyn Fn(&Process) -> String,
    out: &mut Vec<TableRow>,
) {
    let Some(&p) = procs.get(&pid) else { return };

    let mut kids = children.get(&pid).cloned().unwrap_or_default();
    rank(&mut kids, agg);

    out.push(TableRow {
        cells: vec![
            command_of(p),
            format::human_duration(p.run_time()),
            format_usage(p),
        ],
        pid,
        depth,
        is_last_sibling,
        guides: guides.clone(),
        mark: None,
        child_count: kids.len(),
        descendant_pids: descendants.get(&pid).cloned().unwrap_or_default(),
        key: String::new(),
    });

    let n = kids.len();
    for (i, kid) in kids.into_iter().enumerate() {
        let mut child_guides = guides.clone();
        child_guides.push(is_last_sibling);
        flatten(
            kid,
            depth + 1,
            i + 1 == n,
            child_guides,
            procs,
            children,
            agg,
            descendants,
            format_usage,
            out,
        );
    }
}

/// The full process tree ranked by `metric`, flattened into pre-order rows — every
/// node, unfiltered. Used only for the fullscreen view, where the UI applies its own
/// live expand/collapse on top of this.
fn build_tree(
    state: &SystemState,
    metric: &dyn Fn(&Process) -> f64,
    format_usage: &dyn Fn(&Process) -> String,
) -> Vec<TableRow> {
    let procs: HashMap<u32, &Process> = state
        .sys
        .processes()
        .values()
        .filter(is_process)
        .map(|p| (p.pid().as_u32(), p))
        .collect();

    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut roots: Vec<u32> = Vec::new();
    for (&pid, p) in &procs {
        match p.parent().map(|pp| pp.as_u32()) {
            Some(ppid) if procs.contains_key(&ppid) => {
                children.entry(ppid).or_default().push(pid);
            }
            _ => roots.push(pid),
        }
    }

    let mut agg: HashMap<u32, f64> = HashMap::new();
    let mut descendants: HashMap<u32, Vec<u32>> = HashMap::new();
    for &pid in &roots {
        aggregate(pid, &procs, &children, metric, &mut agg, &mut descendants);
    }
    rank(&mut roots, &agg);

    let mut rows = Vec::new();
    let n = roots.len();
    for (i, pid) in roots.into_iter().enumerate() {
        flatten(
            pid,
            0,
            i + 1 == n,
            Vec::new(),
            &procs,
            &children,
            &agg,
            &descendants,
            format_usage,
            &mut rows,
        );
    }
    rows
}

/// Compact panel: flat top-N by own usage (see `build_flat_top`). Fullscreen: the full
/// tree (see `build_tree`), which the UI then expands/collapses live.
fn sample_processes(
    state: &SystemState,
    limit: Option<usize>,
    metric: &dyn Fn(&Process) -> f64,
    format_usage: &dyn Fn(&Process) -> String,
) -> Vec<TableRow> {
    match limit {
        Some(n) => build_flat_top(state, n, metric, format_usage),
        None => build_tree(state, metric, format_usage),
    }
}

/// Refreshes the Time/Usage cells of already-fetched rows in place, by pid — no
/// re-ranking or re-shaping, so a fullscreened table's row order (and any expanded
/// subtree) stays exactly where the user left it while the numbers keep moving.
fn refresh_rows(
    state: &SystemState,
    rows: &mut [TableRow],
    format_usage: &dyn Fn(&Process) -> String,
) {
    for row in rows.iter_mut() {
        let Some(p) = state.sys.process(Pid::from_u32(row.pid)) else {
            continue;
        };
        if let Some(cell) = row.cells.get_mut(1) {
            *cell = format::human_duration(p.run_time());
        }
        if let Some(cell) = row.cells.get_mut(2) {
            *cell = format_usage(p);
        }
    }
}

// --- detail view ---------------------------------------------------------------------

/// `/proc/<pid>/status`, the answer to everything the per-tick process refresh
/// deliberately doesn't ask sysinfo for.
///
/// Fetching uid, thread count and memory peaks for *every* process on every tick would
/// be paid by the whole table; read here, it's one file for the one process whose
/// detail is open.
pub(super) struct ProcStatus(HashMap<String, String>);

impl ProcStatus {
    pub fn read(pid: u32) -> Self {
        let Ok(content) = fs::read_to_string(format!("/proc/{pid}/status")) else {
            return Self(HashMap::new());
        };
        Self(
            content
                .lines()
                .filter_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    Some((key.to_string(), value.trim().to_string()))
                })
                .collect(),
        )
    }

    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    /// A `VmRSS: 12345 kB`-shaped field, in bytes.
    fn bytes(&self, key: &str) -> Option<u64> {
        let value = self.get(key)?;
        let number: u64 = value.split_whitespace().next()?.parse().ok()?;
        Some(number * 1024)
    }

    /// Real uid — the first of the four the kernel prints (real, effective, saved, fs).
    pub(super) fn uid(&self) -> Option<u32> {
        self.get("Uid")?.split_whitespace().next()?.parse().ok()
    }

    fn described_user(&self) -> String {
        match self.uid() {
            Some(uid) => match user_name(uid) {
                Some(name) => format!("{name} (uid {uid})"),
                None => format!("uid {uid}"),
            },
            None => String::new(),
        }
    }
}

/// The scheduler state letter from `/proc/<pid>/status`, spelled out. `D` keeps its
/// letter alongside the words because that's how it's talked about — an uninterruptible
/// sleep is the one state worth recognising on sight.
fn state_name(state: &str) -> String {
    let letter = state.chars().next().unwrap_or('?');
    match letter {
        'R' => "executando".to_string(),
        'S' => "dormindo".to_string(),
        'D' => "espera ininterrompível (D) — travado em I/O".to_string(),
        'Z' => "zumbi — já morreu, ninguém coletou".to_string(),
        'T' => "parado (SIGSTOP)".to_string(),
        't' => "parado por um depurador".to_string(),
        'I' => "ocioso".to_string(),
        'X' | 'x' => "morto".to_string(),
        _ => state.to_string(),
    }
}

/// How many file descriptors the process holds. `None` where the directory isn't ours
/// to read, which is every other user's process.
fn open_fds(pid: u32) -> Option<usize> {
    Some(fs::read_dir(format!("/proc/{pid}/fd")).ok()?.count())
}

/// Bytes this process has actually moved to and from block devices, from
/// `/proc/<pid>/io` — `read_bytes`/`write_bytes` rather than `rchar`/`wchar`, which
/// count every read/write syscall including the ones the page cache served.
/// Unreadable for other users' processes (the file is owner-only), which is why every
/// caller treats it as optional rather than as zero.
fn disk_io(pid: u32) -> Option<(u64, u64)> {
    let content = fs::read_to_string(format!("/proc/{pid}/io")).ok()?;
    let field = |name: &str| -> Option<u64> {
        content
            .lines()
            .find_map(|line| line.strip_prefix(name)?.trim().parse().ok())
    };
    Some((field("read_bytes:")?, field("write_bytes:")?))
}

/// Turns the process' cumulative I/O counters into a rate, by remembering the previous
/// reading. One slot, not a map: only one detail view is open at a time, and a slot
/// belonging to a different pid is simply the first reading of a new subject.
#[derive(Default)]
pub struct IoSampler {
    last: Option<(u32, u64, u64, Instant)>,
}

impl IoSampler {
    fn rate(&mut self, pid: u32) -> Option<(f64, f64)> {
        let (read, written) = disk_io(pid)?;
        let now = Instant::now();
        let rate = match self.last {
            Some((last_pid, last_read, last_written, at)) if last_pid == pid => {
                let seconds = now.duration_since(at).as_secs_f64();
                if seconds <= 0.0 {
                    (0.0, 0.0)
                } else {
                    (
                        read.saturating_sub(last_read) as f64 / seconds,
                        written.saturating_sub(last_written) as f64 / seconds,
                    )
                }
            }
            // First reading of this process: there's no interval to divide by yet.
            _ => (0.0, 0.0),
        };
        self.last = Some((pid, read, written, now));
        Some(rate)
    }
}

/// Identity, command and lifetime of the process behind a row — the section every table
/// that attributes something to a pid (a port, a connection, a login session) shows
/// about its owner, so all three say the same things in the same order.
///
/// `missing` is what to say when the pid resolves to nothing, which differs by caller:
/// a socket's owner may simply belong to another user, while a process that's gone from
/// the table has exited.
pub(super) fn owner_section(state: &SystemState, pid: u32, missing: &str) -> DetailSection {
    let mut section = DetailSection::new("Processo");
    let Some(p) = state.sys.process(Pid::from_u32(pid)) else {
        section.push("PID", missing);
        return section;
    };
    let status = ProcStatus::read(pid);
    section.push("PID", pid.to_string());
    section.push("Usuário", status.described_user());
    section.push("Ativo há", format::human_duration(p.run_time()));
    // `exe` isn't in the refresh kind the Processes tab asks sysinfo for — reading the
    // one link here is far cheaper than making every process pay for it on every tick.
    if let Ok(exe) = fs::read_link(format!("/proc/{pid}/exe")) {
        section.push("Executável", exe.to_string_lossy());
    }
    if let Some(cwd) = p.cwd() {
        section.push("Diretório", cwd.to_string_lossy());
    }
    section.push("Linha de comando", command_of(p));
    section
}

/// The shape every `Del` confirmation on a process-backed table has: who dies, what
/// goes with them, and the one fact about SIGKILL that people forget in the moment.
///
/// `extra` is what killing means *on this table specifically* — a port stops answering,
/// a connection drops, a person is thrown off the machine — since the pid alone never
/// says that. `None` when the row has no live process behind it, which is how a table
/// whose rows aren't processes (or whose owner we never resolved) opts out of the key
/// entirely.
pub(super) fn kill_danger(
    state: &SystemState,
    row: &TableRow,
    action: &'static str,
    subject: &str,
    extra: Vec<String>,
) -> Option<Danger> {
    let p = state.sys.process(Pid::from_u32(row.pid))?;
    let command = command_of(p);
    let mut lines = extra;
    lines.push(format!("Processo: {command}"));
    lines.push(
        "SIGKILL não pode ser ignorado nem tratado: o processo morre onde estiver, sem \
         gravar nada, sem fechar arquivo nenhum."
            .to_string(),
    );

    let living: Vec<&Process> = row
        .descendant_pids
        .iter()
        .filter_map(|&pid| state.sys.process(Pid::from_u32(pid)))
        .collect();
    if !living.is_empty() {
        let names: Vec<String> = living
            .iter()
            .take(MAX_NAMED_VICTIMS)
            .map(|child| format!("{} ({})", child.name().to_string_lossy(), child.pid()))
            .collect();
        let tail = if living.len() > MAX_NAMED_VICTIMS {
            format!(" e mais {}", living.len() - MAX_NAMED_VICTIMS)
        } else {
            String::new()
        };
        lines.push(format!(
            "Vão junto {} processo(s) abaixo dele: {}{tail}.",
            living.len(),
            names.join(", ")
        ));
    }
    Some(Danger {
        action,
        // The name goes in the title, not only in the lines: "matar este processo" is
        // the same sentence for every row, and the one word that isn't is the one that
        // tells you whether you have the right row.
        title: format!(
            "{subject}: {} (pid {})?",
            p.name().to_string_lossy(),
            row.pid
        ),
        lines,
    })
}

/// Descendants named one by one in the confirmation before the rest becomes a number.
const MAX_NAMED_VICTIMS: usize = 6;

/// Direct children of `pid`, and how many processes hang below it in total.
fn family(state: &SystemState, pid: u32) -> (Vec<&Process>, usize) {
    let parent = Pid::from_u32(pid);
    let mut children: Vec<&Process> = state
        .sys
        .processes()
        .values()
        .filter(|p| is_process(p) && p.parent() == Some(parent))
        .collect();
    // Busiest first: the list below is capped, and an arbitrary dozen out of forty
    // children would be a worse answer than the dozen doing something.
    children.sort_by(|a, b| {
        b.cpu_usage()
            .total_cmp(&a.cpu_usage())
            .then(a.pid().cmp(&b.pid()))
    });

    let mut total = 0;
    let mut frontier: Vec<Pid> = children.iter().map(|p| p.pid()).collect();
    while let Some(current) = frontier.pop() {
        total += 1;
        frontier.extend(
            state
                .sys
                .processes()
                .values()
                .filter(|p| p.parent() == Some(current))
                .map(|p| p.pid()),
        );
    }
    (children, total)
}

/// The sockets this process holds open, matched from its file descriptors back to the
/// kernel's socket tables. It's the same walk the Ports panel does, run in reverse: pid
/// first, sockets second.
fn sockets_section(pid: u32) -> Option<(DetailSection, Vec<Handoff>)> {
    let inodes = socket_inodes(pid);
    if inodes.is_empty() {
        return None;
    }
    let table = socket_table();
    let mine: Vec<&super::ports::SocketRow> = table
        .iter()
        .filter(|row| inodes.contains(&row.inode))
        .collect();
    if mine.is_empty() {
        return None;
    }

    let mut section = DetailSection::new("Rede");
    // Deduped by protocol and port: a service bound on both families is two sockets to
    // the kernel and one port to a reader — and, further down, one offer rather than
    // the same tunnel proposed twice.
    let mut seen = HashSet::new();
    let listening: Vec<&&super::ports::SocketRow> = mine
        .iter()
        .filter(|row| row.is_listening() && seen.insert((row.proto, row.local_port)))
        .collect();
    if !listening.is_empty() {
        section.push(
            "Escutando",
            listening
                .iter()
                .map(|row| format!("{} {}", row.proto, row.local()))
                .collect::<Vec<_>>()
                .join("  ·  "),
        );
    }
    let open: Vec<&&super::ports::SocketRow> = mine
        .iter()
        .filter(|row| !row.is_listening() && row.remote_port != 0)
        .collect();
    section.push("Conexões abertas", open.len().to_string());
    for row in open.iter().take(MAX_CONNECTIONS) {
        section.push(
            &format!("{} {}", row.proto, row.local()),
            format!("→ {}", row.remote()),
        );
    }
    if open.len() > MAX_CONNECTIONS {
        section.push(
            "E mais",
            format!(
                "{} conexão(ões) — veja o painel Connections",
                open.len() - MAX_CONNECTIONS
            ),
        );
    }

    // A port this process is listening on is a tunnel's whole configuration, the same
    // way one of its own rows is on the Ports panel. Built through one shared set of
    // taken ports so no two of them ask to listen on the same number.
    let mut taken = listening_port_set(&table);
    let handoffs = listening
        .iter()
        .flat_map(|row| record_traffic(row.proto, row.local_port, &mut taken))
        .collect();
    Some((section, handoffs))
}

/// Connections listed one by one before the tail is summarised. A browser holds
/// hundreds; the panel next door is the place to read those.
const MAX_CONNECTIONS: usize = 12;
/// Children listed by name before the tail is summarised.
const MAX_CHILDREN: usize = 12;

/// Everything known about one process, for the detail view behind Enter on either of
/// the two top-N tables.
fn build_detail(state: &SystemState, pid: u32, io: &mut IoSampler) -> Option<Detail> {
    let p = state.sys.process(Pid::from_u32(pid))?;
    let status = ProcStatus::read(pid);

    let mut identity = DetailSection::new("Processo");
    identity.push("PID", pid.to_string());
    identity.push("Nome", p.name().to_string_lossy());
    identity.push("Usuário", status.described_user());
    if let Some(state_letter) = status.get("State") {
        identity.push("Estado", state_name(state_letter));
    }
    match p.parent().and_then(|ppid| state.sys.process(ppid)) {
        Some(parent) => identity.push(
            "Pai",
            format!("{} · {}", parent.pid().as_u32(), command_of(parent)),
        ),
        None => identity.push("Pai", "nenhum — processo raiz"),
    }
    identity.push("Ativo há", format::human_duration(p.run_time()));
    identity.push("Threads", status.get("Threads").unwrap_or_default());
    if let Some(fds) = open_fds(pid) {
        identity.push("Descritores abertos", fds.to_string());
    }
    if let Ok(score) = fs::read_to_string(format!("/proc/{pid}/oom_score")) {
        identity.push(
            "Nota do OOM killer",
            format!(
                "{} (quanto maior, mais cedo morre se faltar memória)",
                score.trim()
            ),
        );
    }

    let mut command = DetailSection::new("Comando");
    if let Ok(exe) = fs::read_link(format!("/proc/{pid}/exe")) {
        command.push("Executável", exe.to_string_lossy());
    }
    if let Some(cwd) = p.cwd() {
        command.push("Diretório", cwd.to_string_lossy());
    }
    command.push("Linha de comando", command_of(p));

    let mut resources = DetailSection::new("Recursos");
    resources.push("CPU", format!("{:.1}%", p.cpu_usage()));
    let total_memory = state.sys.total_memory();
    let share = if total_memory > 0 {
        format!(
            "  ({:.1}% da máquina)",
            p.memory() as f64 / total_memory as f64 * 100.0
        )
    } else {
        String::new()
    };
    resources.push(
        "Memória residente",
        format!("{}{share}", format::human_bytes(p.memory() as f64)),
    );
    if let Some(peak) = status.bytes("VmHWM") {
        resources.push("Pico residente", format::human_bytes(peak as f64));
    }
    resources.push(
        "Memória virtual",
        format::human_bytes(p.virtual_memory() as f64),
    );
    if let Some(swap) = status.bytes("VmSwap").filter(|swap| *swap > 0) {
        resources.push("Em swap", format::human_bytes(swap as f64));
    }
    match disk_io(pid) {
        Some((read, written)) => {
            resources.push("Lido do disco", format::human_bytes(read as f64));
            resources.push("Gravado no disco", format::human_bytes(written as f64));
        }
        None => resources.push(
            "Disco",
            "contadores ilegíveis — /proc/<pid>/io só se abre para o dono do processo",
        ),
    }

    let (children, descendants) = family(state, pid);
    let mut tree = DetailSection::new("Árvore");
    tree.push("Filhos diretos", children.len().to_string());
    tree.push("Descendentes", descendants.to_string());
    for child in children.iter().take(MAX_CHILDREN) {
        tree.push(&child.pid().as_u32().to_string(), command_of(child));
    }
    if children.len() > MAX_CHILDREN {
        tree.push(
            "E mais",
            format!("{} filho(s)", children.len() - MAX_CHILDREN),
        );
    }

    let mut sections = vec![identity, command, resources, tree];
    let mut handoffs = Vec::new();
    if let Some((network, offers)) = sockets_section(pid) {
        sections.push(network);
        handoffs = offers;
    }

    Some(Detail {
        title: format!("{} · pid {pid}", p.name().to_string_lossy()),
        gone_note: "encerrado",
        sections,
        rates: io.rate(pid).map(|(read, written)| Rates {
            labels: ("↓ Lendo do disco", "↑ Gravando no disco"),
            values: (read, written),
        }),
        handoffs,
        handoff_title: "Gravar o tráfego deste processo",
    })
}

#[derive(Default)]
pub struct TopCpuMonitor {
    io: IoSampler,
}

impl TableMonitor for TopCpuMonitor {
    fn id(&self) -> &'static str {
        "top-cpu"
    }

    fn title(&self) -> &'static str {
        "Top CPU"
    }

    fn headers(&self) -> &'static [&'static str] {
        &HEADERS
    }

    /// A process is its command line, and a marked process usually means the tree under
    /// it too — a build that matters is the build plus everything it spawned.
    fn mark_kinds(&self) -> &'static [mark::MarkKind] {
        &[mark::MarkKind {
            name: "comando",
            column: 0,
            numeric: false,
            help: "trecho do comando, ou uma expressão regular",
        }]
    }

    fn tree(&self) -> bool {
        true
    }

    fn sample(&mut self, state: &SystemState, limit: Option<usize>) -> Vec<TableRow> {
        sample_processes(state, limit, &|p| p.cpu_usage() as f64, &|p| {
            format!("{:.1}%", p.cpu_usage())
        })
    }

    fn refresh_values(&mut self, state: &SystemState, rows: &mut [TableRow]) {
        refresh_rows(state, rows, &|p| format!("{:.1}%", p.cpu_usage()));
    }

    fn detail(&mut self, state: &SystemState, row: &TableRow) -> Option<Detail> {
        build_detail(state, row.pid, &mut self.io)
    }

    fn has_detail(&self) -> bool {
        true
    }

    fn danger(&self, state: &SystemState, row: &TableRow) -> Option<Danger> {
        kill_danger(
            state,
            row,
            "matar processo",
            "Matar este processo",
            Vec::new(),
        )
    }
}

#[derive(Default)]
pub struct TopMemMonitor {
    io: IoSampler,
}

impl TableMonitor for TopMemMonitor {
    fn id(&self) -> &'static str {
        "top-mem"
    }

    fn title(&self) -> &'static str {
        "Top Memory"
    }

    fn headers(&self) -> &'static [&'static str] {
        &HEADERS
    }

    /// A process is its command line, and a marked process usually means the tree under
    /// it too — a build that matters is the build plus everything it spawned.
    fn mark_kinds(&self) -> &'static [mark::MarkKind] {
        &[mark::MarkKind {
            name: "comando",
            column: 0,
            numeric: false,
            help: "trecho do comando, ou uma expressão regular",
        }]
    }

    fn tree(&self) -> bool {
        true
    }

    fn sample(&mut self, state: &SystemState, limit: Option<usize>) -> Vec<TableRow> {
        sample_processes(state, limit, &|p| p.memory() as f64, &|p| {
            format::human_bytes(p.memory() as f64)
        })
    }

    fn refresh_values(&mut self, state: &SystemState, rows: &mut [TableRow]) {
        refresh_rows(state, rows, &|p| format::human_bytes(p.memory() as f64));
    }

    fn detail(&mut self, state: &SystemState, row: &TableRow) -> Option<Detail> {
        build_detail(state, row.pid, &mut self.io)
    }

    fn has_detail(&self) -> bool {
        true
    }

    fn danger(&self, state: &SystemState, row: &TableRow) -> Option<Danger> {
        kill_danger(
            state,
            row,
            "matar processo",
            "Matar este processo",
            Vec::new(),
        )
    }
}
