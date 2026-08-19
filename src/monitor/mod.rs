use sysinfo::{Disks, Networks, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

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

    pub fn refresh(&mut self) {
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
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
        self.disks.refresh(true);
        self.networks.refresh(true);
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
    /// When true, the UI shows this metric as a compact numeric line above its group's
    /// charts instead of its own sparkline panel — for metrics that change too slowly
    /// for a chart to be useful (e.g. disk occupancy).
    fn numeric_only(&self) -> bool {
        false
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
}

pub fn all_table_monitors() -> Vec<Box<dyn TableMonitor>> {
    vec![
        Box::new(ports::PortsMonitor),
        Box::new(process::TopCpuMonitor),
        Box::new(process::TopMemMonitor),
    ]
}
