use std::ffi::c_void;
use std::fs;
use std::net::Ipv4Addr;

use nvml_wrapper::Nvml;

use super::disk::primary_disk;
use super::{SystemState, TableMonitor, TableRow};
use crate::format;

const HEADERS: [&str; 2] = ["Field", "Value"];

fn hostname() -> String {
    match fs::read_to_string("/proc/sys/kernel/hostname") {
        Ok(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => "?".to_string(),
    }
}

fn username_for_uid(uid: u32) -> Option<String> {
    let content = fs::read_to_string("/etc/passwd").ok()?;
    content.lines().find_map(|line| {
        let mut fields = line.split(':');
        let name = fields.next()?;
        fields.next()?; // password placeholder, always "x"
        let uid_field = fields.next()?;
        (uid_field.parse::<u32>().ok()? == uid).then(|| name.to_string())
    })
}

/// The machine's actual logged-in user rather than this process' own owner — read from
/// the audit subsystem's `loginuid` (set once at login and inherited by every
/// descendant, unlike `$USER`, which just reflects whatever the parent shell happened
/// to export and can be stale or absent). Falls back to `$USER`/`$LOGNAME` when the
/// audit subsystem never recorded one (e.g. no PAM `pam_loginuid`, common in
/// containers).
fn current_user() -> String {
    fs::read_to_string("/proc/self/loginuid")
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .filter(|&uid| uid != u32::MAX)
        .and_then(username_for_uid)
        .or_else(|| std::env::var("USER").ok())
        .or_else(|| std::env::var("LOGNAME").ok())
        .unwrap_or_else(|| "?".to_string())
}

fn dns_servers() -> String {
    let Ok(content) = fs::read_to_string("/etc/resolv.conf") else {
        return "?".to_string();
    };
    let servers: Vec<&str> = content
        .lines()
        .filter_map(|l| l.strip_prefix("nameserver "))
        .map(str::trim)
        .collect();
    if servers.is_empty() {
        "?".to_string()
    } else {
        servers.join(", ")
    }
}

/// Parses one `/proc/net/route`-style hex address field, e.g. `"0101A8C0"` →
/// `192.168.1.1`. The kernel prints the raw 32-bit address (stored host-endian) as a
/// hex number, so recovering the dotted-quad octet order means reading it back out
/// little-endian rather than the more obvious big-endian.
fn parse_hex_ipv4(hex: &str) -> Option<Ipv4Addr> {
    let bytes = u32::from_str_radix(hex, 16).ok()?.to_le_bytes();
    Some(Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3]))
}

/// The default route's gateway: the `/proc/net/route` row for destination `0.0.0.0`
/// whose gateway isn't itself `0.0.0.0` (an on-link default route, i.e. no real
/// gateway).
fn default_gateway() -> Option<Ipv4Addr> {
    let content = fs::read_to_string("/proc/net/route").ok()?;
    content.lines().skip(1).find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let destination = *fields.get(1)?;
        let gateway = *fields.get(2)?;
        if destination == "00000000" && gateway != "00000000" {
            parse_hex_ipv4(gateway)
        } else {
            None
        }
    })
}

#[repr(C)]
struct SockaddrIn {
    sin_family: u16,
    sin_port: u16,
    sin_addr: u32,
    sin_zero: [u8; 8],
}

const AF_INET: i32 = 2;
const SOCK_DGRAM: i32 = 2;

// `connect` takes `*const c_void` (not `*const SockaddrIn`) to match its real POSIX
// signature — `connections.rs` declares the same symbols for its own netlink socket,
// and the two declarations must agree exactly or rustc's `clashing_extern_declarations`
// lint (rightly) complains that one crate is lying about a shared symbol's type.
unsafe extern "C" {
    fn socket(domain: i32, ty: i32, protocol: i32) -> i32;
    fn connect(fd: i32, addr: *const c_void, len: u32) -> i32;
    fn getsockname(fd: i32, addr: *mut c_void, len: *mut u32) -> i32;
    fn close(fd: i32) -> i32;
}

/// The local address the kernel would route outbound traffic from — found the same way
/// `ip route get`/a Python `socket.connect` trick does: `connect()` a UDP socket to some
/// public address (sends no packets — UDP `connect()` just resolves and caches a route)
/// then read back the address the kernel picked via `getsockname`. Best-effort: `None`
/// on a machine with no route out (e.g. fully offline).
fn outbound_ip() -> Option<Ipv4Addr> {
    let fd = unsafe { socket(AF_INET, SOCK_DGRAM, 0) };
    if fd < 0 {
        return None;
    }
    let dest = SockaddrIn {
        sin_family: AF_INET as u16,
        sin_port: 53u16.to_be(),
        sin_addr: u32::from(Ipv4Addr::new(8, 8, 8, 8)).to_be(),
        sin_zero: [0; 8],
    };
    let dest_ptr = (&dest as *const SockaddrIn).cast();
    let ip = if unsafe { connect(fd, dest_ptr, size_of::<SockaddrIn>() as u32) } == 0 {
        let mut local = SockaddrIn {
            sin_family: 0,
            sin_port: 0,
            sin_addr: 0,
            sin_zero: [0; 8],
        };
        let mut len = size_of::<SockaddrIn>() as u32;
        (unsafe { getsockname(fd, (&mut local as *mut SockaddrIn).cast(), &mut len) } == 0)
            .then(|| Ipv4Addr::from(u32::from_be(local.sin_addr)))
    } else {
        None
    };
    unsafe { close(fd) };
    ip
}

fn cpu_summary(state: &SystemState) -> String {
    let cpus = state.sys.cpus();
    match cpus.first() {
        Some(cpu) => format!("{} cores · {}", cpus.len(), cpu.brand()),
        None => "?".to_string(),
    }
}

fn disk_summary(state: &SystemState) -> String {
    primary_disk(state)
        .map(|d| format::human_bytes(d.total_space() as f64))
        .unwrap_or_else(|| "?".to_string())
}

fn gpu_summary(nvml: &Nvml) -> Option<String> {
    let device = nvml.device_by_index(0).ok()?;
    let name = device.name().unwrap_or_else(|_| "GPU".to_string());
    let total = device.memory_info().ok()?.total;
    Some(format!("{} · {}", name, format::human_bytes(total as f64)))
}

fn row(field: &str, value: String) -> TableRow {
    TableRow::leaf(vec![field.to_string(), value], 0)
}

/// A general "who/what is this machine" summary, laid out beside SSH Sessions since
/// both answer the same question at different scopes: this panel is about the host
/// itself, that one's about who's connected to it.
pub struct SummaryMonitor {
    /// Probed once at startup — `None` on any machine without a working NVIDIA driver,
    /// same fallback as `gpu::GpuMonitor`. A fresh, independent handle rather than
    /// sharing `GpuMonitor`'s, since table monitors and chart monitors are constructed
    /// and owned separately.
    nvml: Option<Nvml>,
}

impl SummaryMonitor {
    pub fn new() -> Self {
        Self {
            nvml: Nvml::init().ok(),
        }
    }

    fn rows(&self, state: &SystemState) -> Vec<TableRow> {
        let mut rows = vec![
            row("Host", hostname()),
            row("User", current_user()),
            row(
                "IP",
                outbound_ip()
                    .map(|ip| ip.to_string())
                    .unwrap_or_else(|| "?".to_string()),
            ),
            row(
                "Gateway",
                default_gateway()
                    .map(|ip| ip.to_string())
                    .unwrap_or_else(|| "?".to_string()),
            ),
            row("DNS", dns_servers()),
            row("CPU", cpu_summary(state)),
            row(
                "Memory",
                format::human_bytes(state.sys.total_memory() as f64),
            ),
            row("Disk", disk_summary(state)),
        ];
        if let Some(gpu) = self.nvml.as_ref().and_then(gpu_summary) {
            rows.push(row("GPU", gpu));
        }
        rows
    }
}

impl Default for SummaryMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl TableMonitor for SummaryMonitor {
    fn title(&self) -> &'static str {
        "System Info"
    }

    fn headers(&self) -> &'static [&'static str] {
        &HEADERS
    }

    fn sample(&mut self, state: &SystemState, limit: Option<usize>) -> Vec<TableRow> {
        let mut rows = self.rows(state);
        if let Some(n) = limit {
            rows.truncate(n);
        }
        rows
    }

    fn refresh_values(&mut self, state: &SystemState, rows: &mut [TableRow]) {
        let fresh = self.rows(state);
        for row in rows.iter_mut() {
            if let Some(f) = fresh.iter().find(|f| f.cells[0] == row.cells[0]) {
                row.cells[1] = f.cells[1].clone();
            }
        }
    }
}
