use sysinfo::Process;

use super::{SystemState, TableMonitor, TableRow};
use crate::format;

const TOP_N: usize = 10;
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
fn command_of(p: &Process) -> String {
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

pub struct TopCpuMonitor;

impl TableMonitor for TopCpuMonitor {
    fn title(&self) -> &'static str {
        "Top CPU"
    }

    fn headers(&self) -> &'static [&'static str] {
        &HEADERS
    }

    fn sample(&mut self, state: &SystemState) -> Vec<TableRow> {
        let mut procs: Vec<_> = state.sys.processes().values().filter(is_process).collect();
        procs.sort_by(|a, b| b.cpu_usage().total_cmp(&a.cpu_usage()));

        procs
            .into_iter()
            .take(TOP_N)
            .map(|p| TableRow {
                cells: vec![
                    command_of(p),
                    format::human_duration(p.run_time()),
                    format!("{:.1}%", p.cpu_usage()),
                ],
            })
            .collect()
    }
}

pub struct TopMemMonitor;

impl TableMonitor for TopMemMonitor {
    fn title(&self) -> &'static str {
        "Top Memory"
    }

    fn headers(&self) -> &'static [&'static str] {
        &HEADERS
    }

    fn sample(&mut self, state: &SystemState) -> Vec<TableRow> {
        let mut procs: Vec<_> = state.sys.processes().values().filter(is_process).collect();
        procs.sort_by_key(|p| std::cmp::Reverse(p.memory()));

        procs
            .into_iter()
            .take(TOP_N)
            .map(|p| TableRow {
                cells: vec![
                    command_of(p),
                    format::human_duration(p.run_time()),
                    format::human_bytes(p.memory() as f64),
                ],
            })
            .collect()
    }
}
