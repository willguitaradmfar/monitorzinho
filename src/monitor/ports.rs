use std::collections::{BTreeMap, HashMap};
use std::fs;

use super::process::describe_owner;
use super::{SystemState, TableMonitor, TableRow};

/// TCP_LISTEN, per include/net/tcp_states.h — shared by /proc/net/tcp{,6}.
const TCP_LISTEN: &str = "0A";
/// TCP_CLOSE, reused by the kernel as the "unconnected" state for a bound UDP socket
/// (UDP has no real LISTEN state) — /proc/net/udp{,6}.
const UDP_UNCONN: &str = "07";

const HEADERS: [&str; 4] = ["Proto", "Port", "Process", "Age"];

struct PortEntry {
    port: u16,
    inode: u64,
}

/// Ports (with their socket inode) found in a `/proc/net/{tcp,udp}[6]` table whose
/// state field matches `want_state`.
fn parse_ports(path: &str, want_state: &str) -> Vec<PortEntry> {
    let Ok(content) = fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let local = fields.get(1)?;
            let state = fields.get(3)?;
            let inode = fields.get(9)?;
            if !state.eq_ignore_ascii_case(want_state) {
                return None;
            }
            let port_hex = local.rsplit(':').next()?;
            Some(PortEntry {
                port: u16::from_str_radix(port_hex, 16).ok()?,
                inode: inode.parse().ok()?,
            })
        })
        .collect()
}

/// Distinct local ports across the IPv4 and IPv6 tables, deduped by port (same
/// service bound on both families shows up once), sorted ascending.
fn collect_ports(paths: &[&str], want_state: &str) -> BTreeMap<u16, u64> {
    let mut ports = BTreeMap::new();
    for path in paths {
        for entry in parse_ports(path, want_state) {
            ports.entry(entry.port).or_insert(entry.inode);
        }
    }
    ports
}

/// Maps each open socket's inode to the pid holding it open, by scanning every
/// `/proc/<pid>/fd` for `socket:[<inode>]` symlinks. Processes we don't have
/// permission to inspect are silently skipped — their ports just show no owner.
/// Shared with `connections`, which needs the same inode→pid mapping.
pub(super) fn inode_to_pid() -> HashMap<u64, u32> {
    let mut map = HashMap::new();
    let Ok(proc_dir) = fs::read_dir("/proc") else {
        return map;
    };
    for entry in proc_dir.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(fds) = fs::read_dir(entry.path().join("fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            let Ok(link) = fs::read_link(fd.path()) else {
                continue;
            };
            let Some(inode_str) = link
                .to_str()
                .and_then(|s| s.strip_prefix("socket:["))
                .and_then(|s| s.strip_suffix(']'))
            else {
                continue;
            };
            if let Ok(inode) = inode_str.parse() {
                map.entry(inode).or_insert(pid);
            }
        }
    }
    map
}

/// TCP and UDP listening ports in one list, sorted by port (ties keep TCP before UDP).
fn sample_ports(state: &SystemState, limit: Option<usize>) -> Vec<TableRow> {
    let owners = inode_to_pid();
    let mut rows: Vec<(&'static str, u16, u32)> = Vec::new();
    for (proto, paths, want_state) in [
        ("TCP", ["/proc/net/tcp", "/proc/net/tcp6"], TCP_LISTEN),
        ("UDP", ["/proc/net/udp", "/proc/net/udp6"], UDP_UNCONN),
    ] {
        for (port, inode) in collect_ports(&paths, want_state) {
            let pid = owners.get(&inode).copied().unwrap_or(0);
            rows.push((proto, port, pid));
        }
    }
    rows.sort_by_key(|&(_, port, _)| port);

    rows.into_iter()
        .take(limit.unwrap_or(usize::MAX))
        .map(|(proto, port, pid)| {
            let (process, age) = describe_owner(state, pid);
            TableRow::leaf(vec![proto.to_string(), port.to_string(), process, age], pid)
        })
        .collect()
}

pub struct PortsMonitor;

impl TableMonitor for PortsMonitor {
    fn title(&self) -> &'static str {
        "Ports"
    }

    fn headers(&self) -> &'static [&'static str] {
        &HEADERS
    }

    fn sample(&mut self, state: &SystemState, limit: Option<usize>) -> Vec<TableRow> {
        sample_ports(state, limit)
    }

    fn refresh_values(&mut self, state: &SystemState, rows: &mut [TableRow]) {
        for row in rows.iter_mut() {
            let (process, age) = describe_owner(state, row.pid);
            if let Some(cell) = row.cells.get_mut(2) {
                *cell = process;
            }
            if let Some(cell) = row.cells.get_mut(3) {
                *cell = age;
            }
        }
    }
}
