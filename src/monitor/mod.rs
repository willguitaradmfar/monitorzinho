use sysinfo::{Disks, Networks, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

pub mod connections;
pub mod cpu;
pub mod disk;
pub mod gpu;
pub mod memory;
pub mod network;
pub mod ports;
pub mod process;

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

    /// Refreshes the process list (Processes tab: ports/top CPU/top memory). This is
    /// the most expensive part of a tick, so it only runs while that tab is focused.
    pub fn refresh_processes(&mut self) {
        self.sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .with_memory()
                .with_cpu()
                // A process' argv never changes, so fetch it once and cache it instead
                // of re-reading /proc/<pid>/cmdline for every process on every tick.
                .with_cmd(UpdateKind::OnlyIfNotSet),
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
}

pub fn all_table_monitors() -> Vec<Box<dyn TableMonitor>> {
    vec![
        Box::new(ports::PortsMonitor),
        Box::new(connections::ConnectionsMonitor::new()),
        Box::new(process::TopCpuMonitor),
        Box::new(process::TopMemMonitor),
    ]
}
