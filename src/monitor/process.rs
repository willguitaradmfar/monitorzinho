use std::cmp::Ordering;
use std::collections::HashMap;

use sysinfo::{Pid, Process};

use super::{SystemState, TableMonitor, TableRow};
use crate::format;

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

pub struct TopCpuMonitor;

impl TableMonitor for TopCpuMonitor {
    fn title(&self) -> &'static str {
        "Top CPU"
    }

    fn headers(&self) -> &'static [&'static str] {
        &HEADERS
    }

    fn sample(&mut self, state: &SystemState, limit: Option<usize>) -> Vec<TableRow> {
        sample_processes(state, limit, &|p| p.cpu_usage() as f64, &|p| {
            format!("{:.1}%", p.cpu_usage())
        })
    }

    fn refresh_values(&mut self, state: &SystemState, rows: &mut [TableRow]) {
        refresh_rows(state, rows, &|p| format!("{:.1}%", p.cpu_usage()));
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

    fn sample(&mut self, state: &SystemState, limit: Option<usize>) -> Vec<TableRow> {
        sample_processes(state, limit, &|p| p.memory() as f64, &|p| {
            format::human_bytes(p.memory() as f64)
        })
    }

    fn refresh_values(&mut self, state: &SystemState, rows: &mut [TableRow]) {
        refresh_rows(state, rows, &|p| format::human_bytes(p.memory() as f64));
    }
}
