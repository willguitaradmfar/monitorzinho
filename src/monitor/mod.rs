use sysinfo::{Disks, Networks, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use crate::tools::Handoff;

pub mod connections;
pub mod cpu;
pub mod disk;
pub mod gpu;
pub mod iface;
pub mod memory;
pub mod netns;
pub mod network;
pub mod ports;
pub mod process;
pub mod resolve;
pub mod ssh;
pub mod summary;

/// Shared, refreshed-once-per-tick system state passed to every monitor's `sample`.
pub struct SystemState {
    pub sys: System,
    pub disks: Disks,
    pub networks: Networks,
}

impl SystemState {
    pub fn new() -> Self {
        Self {
            sys: System::new_all(),
            disks: Disks::new_with_refreshed_list(),
            networks: Networks::new_with_refreshed_list(),
        }
    }

    /// Refreshes what the chart panels (Overview tab) need. Split from
    /// `refresh_processes` so each tab only pays for the sysinfo work its own
    /// monitors actually use.
    pub fn refresh_overview(&mut self) {
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        self.disks.refresh(true);
        self.networks.refresh(true);
    }

    /// Refreshes the process list (Processes tab: ports/top CPU/top memory), plus the
    /// interface list that tab's network panels read. This is the most expensive part
    /// of a tick, so it only runs while that tab is focused.
    pub fn refresh_processes(&mut self) {
        // Interfaces and their addresses: cheap next to the process walk below, and
        // every network panel on this tab is wrong without them — an address that
        // changed when a VPN came up would otherwise stay as it was at launch.
        self.networks.refresh(true);
        self.sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .with_memory()
                .with_cpu()
                // A process' argv never changes, so fetch it once and cache it instead
                // of re-reading /proc/<pid>/cmdline for every process on every tick.
                .with_cmd(UpdateKind::OnlyIfNotSet)
                // Unlike argv, cwd can change over a process' lifetime (e.g. a shell
                // after `cd`), so this one's refreshed every tick.
                .with_cwd(UpdateKind::Always),
        );
    }
}

/// One "monitorzinho" with a scalar value tracked over time (sparkline + persisted history).
/// To add a new one: implement this trait in a new file and register it in `all_monitors()`.
pub trait Monitor: Send {
    /// Stable key used for persistence — never change once shipped.
    fn id(&self) -> &'static str;
    fn title(&self) -> &'static str;
    fn sample(&mut self, state: &SystemState) -> f64;
    /// How to render a sampled value, e.g. "32.8%" or "1.3 MB/s".
    fn format(&self, value: f64) -> String;
    /// Natural upper bound for this metric, if one exists (e.g. 100.0 for a percentage).
    /// Drives the near-limit color signal in the UI. `None` means unbounded (no signal).
    fn limit(&self) -> Option<f64> {
        None
    }
    /// Category used to visually group related panels together (e.g. "Disk" keeps
    /// usage%/read/write next to each other instead of scattered across the grid).
    fn group(&self) -> &'static str {
        "General"
    }
    /// Optional human-readable absolute quantity shown next to the formatted value,
    /// e.g. "5.6 GB / 16.0 GB" for a percentage-of-capacity metric. Sampled once per
    /// tick alongside `sample()`. `None` for metrics with no natural capacity (CPU%,
    /// byte rates, ...).
    fn extra(&self, _state: &SystemState) -> Option<String> {
        None
    }
    /// Total capacity behind a percentage-of-capacity metric, in the same unit
    /// `sample()`'s percentage is computed against (e.g. total memory in bytes).
    /// Sampled once per tick. Lets the UI express an arbitrary value (e.g. the
    /// retained-window peak) in absolute units too, not just the live sample.
    /// `None` for metrics with no fixed capacity (CPU%, byte rates, ...).
    fn capacity(&self, _state: &SystemState) -> Option<f64> {
        None
    }
}

pub fn all_monitors() -> Vec<Box<dyn Monitor>> {
    let mut monitors: Vec<Box<dyn Monitor>> = vec![
        Box::new(cpu::CpuMonitor),
        Box::new(memory::MemoryMonitor),
        Box::new(disk::DiskMonitor),
        Box::new(network::NetRxMonitor),
        Box::new(network::NetTxMonitor),
        Box::new(disk::DiskReadMonitor),
        Box::new(disk::DiskWriteMonitor),
    ];
    // Only present on machines with a working NVIDIA driver — absent everywhere else.
    if let Some(gpu) = gpu::GpuMonitor::probe() {
        monitors.push(Box::new(gpu));
    }
    monitors
}

#[derive(Clone)]
pub struct TableRow {
    pub cells: Vec<String>,
    /// PID of the process this row represents — kept alongside the formatted cells so
    /// a frozen, fullscreened snapshot can still target the right process (e.g. kill).
    pub pid: u32,
    /// Tree depth (0 = root / flat row for non-tree tables like Ports).
    pub depth: usize,
    /// Whether this row is the last child among its siblings — picks the `└─` vs `├─`
    /// connector glyph.
    pub is_last_sibling: bool,
    /// One entry per ancestor level (0..depth): `true` if that ancestor was the last
    /// child of its own siblings (draw blank space at that column), `false` if it had
    /// a later sibling (draw a continuing `│`).
    pub guides: Vec<bool>,
    /// Number of direct children. 0 means this row is a leaf.
    pub child_count: usize,
    /// Every pid in this row's subtree (not including its own pid) — used for
    /// cascading kill, valid even while some descendants are collapsed/hidden.
    pub descendant_pids: Vec<u32>,
    /// Opaque identity a monitor can use to re-match this row against a fresh sample
    /// in `TableMonitor::refresh_values`, for tables where `pid` alone isn't unique
    /// enough (e.g. Connections: several sockets can share one owning process). Empty
    /// and unused by monitors that don't need it — they match by `pid` directly.
    pub key: String,
}

impl TableRow {
    /// A flat row with no tree structure (e.g. Ports) — depth 0, no children.
    pub fn leaf(cells: Vec<String>, pid: u32) -> Self {
        Self {
            cells,
            pid,
            depth: 0,
            is_last_sibling: true,
            guides: Vec::new(),
            child_count: 0,
            descendant_pids: Vec::new(),
            key: String::new(),
        }
    }
}

/// One labelled group of `field: value` lines in a detail view — e.g. everything about
/// the owning process, kept visually apart from everything about the wire.
pub struct DetailSection {
    pub title: &'static str,
    pub fields: Vec<(String, String)>,
}

impl DetailSection {
    pub fn new(title: &'static str) -> Self {
        Self {
            title,
            fields: Vec::new(),
        }
    }

    /// Appends a field. Skips empty values outright, so a section only ever shows what
    /// we actually managed to read — a `-` placeholder per unreadable field would
    /// bury the ones that did resolve.
    pub fn push(&mut self, label: &str, value: impl Into<String>) {
        let value = value.into();
        if !value.is_empty() {
            self.fields.push((label.to_string(), value));
        }
    }
}

/// The pair of live figures a detail sparklines above its fields. Both are byte rates
/// — what they're rates *of* is the subject's business, hence the labels: a socket
/// moves bytes across the wire, a process moves them to and from the disk.
pub struct Rates {
    pub labels: (&'static str, &'static str),
    pub values: (f64, f64),
}

/// Everything the fullscreen detail view (Enter on a row) shows about one selected
/// row. Rebuilt from scratch every tick while the view is open, so its values stay
/// live without the view having to track what changed.
pub struct Detail {
    /// Headline identifying the subject, e.g. `TCP 192.168.0.10:54312 → 142.250.0.1:443`.
    pub title: String,
    /// What the title gains once the subject stops existing — "encerrada" for a
    /// connection, "encerrado" for a process, "desconectada" for a session. A single
    /// word rather than a sentence, because it's appended to a title that already
    /// says what the subject is.
    pub gone_note: &'static str,
    pub sections: Vec<DetailSection>,
    /// Current throughput in bytes/s, when the subject has any — the app feeds these
    /// into a pair of `History`s so the view can sparkline them.
    pub rates: Option<Rates>,
    /// Executions this subject suggests creating. A connection to a database is the
    /// exact configuration of a tunnel to that database, and retyping the address into
    /// a form while looking straight at it is work the app can do.
    pub handoffs: Vec<Handoff>,
    /// Heading of the hand-off picker, since what the offers *are* differs by subject:
    /// a connection's two ends and a listening port's traffic are not the same thing.
    pub handoff_title: &'static str,
}

/// What a destructive key would do, said out loud before it does it.
///
/// `Del` on a process table sends SIGKILL to a whole subtree, and on a session table it
/// throws a person off the machine. Both are one keypress away from a row someone is
/// merely reading, and neither can be undone — so the key stops here first, and this is
/// what it stops to say.
pub struct Danger {
    /// Verb for the footer hint, e.g. `matar processo`, `desconectar sessão`. What the
    /// key does differs by table, and a footer that says "matar" over a table of
    /// interfaces is worse than one that says nothing.
    pub action: &'static str,
    /// Heading of the confirmation box, naming the exact subject.
    pub title: String,
    /// What will happen, one consequence per line — the whole reason for stopping.
    pub lines: Vec<String>,
}

/// One "monitorzinho" that shows a ranked snapshot list instead of a time series
/// (e.g. top processes). No history/persistence — it's a live snapshot.
pub trait TableMonitor: Send {
    fn title(&self) -> &'static str;
    /// Column headers; each `TableRow::cells` must have the same length as this.
    fn headers(&self) -> &'static [&'static str];
    /// Ranked rows, capped to `limit` entries — or every ranked entry when `None`
    /// (used for the fullscreen view, where there's room, and reason, to see it all).
    fn sample(&mut self, state: &SystemState, limit: Option<usize>) -> Vec<TableRow>;
    /// Refreshes already-fetched `rows` in place (matched by `pid`, or by `key` for
    /// monitors where `pid` isn't a unique row identity) instead of re-sampling —
    /// called every tick for a fullscreened table instead of `sample`, so live values
    /// (age, CPU/memory usage, throughput, ...) keep moving without re-ranking or
    /// reshaping the rows out from under whatever the user is reading, searching, or
    /// has expanded. `&mut self` because some monitors track state between calls to
    /// compute a rate (e.g. Connections' download/upload throughput). The default
    /// no-op fits monitors with nothing that changes tick to tick.
    fn refresh_values(&mut self, _state: &SystemState, _rows: &mut [TableRow]) {}
    /// Everything known about one selected `row`, for the fullscreen detail view
    /// opened with Enter — called once on entry and again every tick while it's open.
    /// `None` means either that this table has no detail view (the default, so Enter
    /// simply does nothing on it) or that the row's subject has since disappeared, in
    /// which case the view keeps showing its last known values, flagged as stale.
    fn detail(&mut self, _state: &SystemState, _row: &TableRow) -> Option<Detail> {
        None
    }
    /// Whether this table has a detail view at all. Only drives the fullscreen footer
    /// hint — `detail()` is what actually decides — so a table that can't say anything
    /// about its rows doesn't advertise an Enter that would do nothing.
    fn has_detail(&self) -> bool {
        false
    }

    /// Something the table needs to say about *itself* rather than about a row —
    /// shown in the corner of the panel, in both the compact and the fullscreen view.
    ///
    /// It exists for one honest purpose: a table that can only see part of what it is
    /// about has to say so. A connections panel that cannot open the root-owned
    /// containers on the machine is not wrong, but it is incomplete, and a reader who
    /// isn't told that will read it as the whole picture.
    fn note(&self) -> Option<String> {
        None
    }

    /// What `Del` would do to `row`, for the confirmation it has to get through and for
    /// the footer that offers it. `None` — the default — means the key does nothing
    /// here, and then nothing advertises it: a table of interfaces or of facts about the
    /// machine has no process to kill, and a footer promising one is a lie the reader
    /// only finds out about by pressing it.
    fn danger(&self, _state: &SystemState, _row: &TableRow) -> Option<Danger> {
        None
    }
}

pub fn all_table_monitors() -> Vec<Box<dyn TableMonitor>> {
    vec![
        Box::new(ports::PortsMonitor::new()),
        Box::new(connections::ConnectionsMonitor::new()),
        Box::new(process::TopCpuMonitor::default()),
        Box::new(process::TopMemMonitor::default()),
        Box::new(ssh::SshSessionsMonitor),
        Box::new(iface::InterfacesMonitor::new()),
        Box::new(summary::SummaryMonitor::new()),
    ]
}
