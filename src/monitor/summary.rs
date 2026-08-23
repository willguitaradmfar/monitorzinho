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

/// A DMI field the firmware published about this machine. Most are world-readable;
/// the serial numbers aren't, and those aren't wanted here anyway.
fn dmi(field: &str) -> Option<String> {
    let value = fs::read_to_string(format!("/sys/class/dmi/id/{field}")).ok()?;
    let value = value.trim();
    // Boards ship with the field literally set to this when the vendor didn't bother.
    let empty = value.is_empty()
        || value.eq_ignore_ascii_case("to be filled by o.e.m.")
        || value.eq_ignore_ascii_case("default string")
        || value.eq_ignore_ascii_case("system product name")
        || value.eq_ignore_ascii_case("unknown");
    (!empty).then(|| value.to_string())
}

/// SMBIOS chassis types. A fixed enum from the specification, not a list that goes
/// stale — the numbers have meant the same thing for twenty years.
fn chassis() -> Option<&'static str> {
    Some(match dmi("chassis_type")?.as_str() {
        "3" | "4" | "6" | "7" | "15" | "24" => "desktop",
        "5" | "17" | "23" | "25" => "servidor",
        "8" | "9" | "10" | "14" => "notebook",
        "11" | "30" | "31" | "32" => "portátil",
        "13" => "all-in-one",
        _ => return None,
    })
}

/// One value from `/etc/os-release`, unquoted.
fn os_release(key: &str) -> Option<String> {
    let content = fs::read_to_string("/etc/os-release")
        .or_else(|_| fs::read_to_string("/usr/lib/os-release"))
        .ok()?;
    content.lines().find_map(|line| {
        let (name, value) = line.split_once('=')?;
        (name.trim() == key).then(|| value.trim().trim_matches('"').to_string())
    })
}

/// The distribution as it names itself. `NAME` plus `VERSION` rather than `PRETTY_NAME`
/// because the version field is where the codename lives — "Linux Mint 22.1 (Xia)"
/// against "Linux Mint 22.1". Falls back through what's actually there.
fn distribution() -> Option<String> {
    match (os_release("NAME"), os_release("VERSION")) {
        (Some(name), Some(version)) => Some(format!("{name} {version}")),
        (Some(name), None) => Some(name),
        _ => os_release("PRETTY_NAME"),
    }
    .or_else(sysinfo::System::name)
}

/// Distribution, kernel and architecture on one line — the three things anyone means by
/// "what is this machine running", and no use to each other apart.
fn os_summary() -> String {
    let mut parts = vec![distribution().unwrap_or_else(|| "Linux".to_string())];
    if let Some(kernel) = sysinfo::System::kernel_version() {
        parts.push(format!("kernel {kernel}"));
    }
    parts.push(sysinfo::System::cpu_arch());
    parts.join("  ·  ")
}

/// Make, model, form factor and firmware, as the firmware itself reports them. Empty
/// when there's no DMI at all, which is normal inside a container.
fn machine_summary() -> Option<String> {
    let vendor = dmi("sys_vendor");
    let product = dmi("product_name");
    let mut summary = match (vendor, product) {
        (Some(vendor), Some(product)) => format!("{vendor} {product}"),
        (Some(one), None) | (None, Some(one)) => one,
        (None, None) => return None,
    };
    if let Some(chassis) = chassis() {
        summary.push_str(&format!("  ·  {chassis}"));
    }
    if let Some(bios) = dmi("bios_version") {
        summary.push_str(&format!("  ·  BIOS {bios}"));
        if let Some(date) = dmi("bios_date") {
            summary.push_str(&format!(" ({})", iso_date(&date)));
        }
    }
    Some(summary)
}

/// SMBIOS dates are `MM/DD/YYYY` by specification, which reads as the wrong day to
/// most of the world. Reordering it is a defined transformation, not a guess.
fn iso_date(date: &str) -> String {
    let parts: Vec<&str> = date.split('/').collect();
    match parts.as_slice() {
        [month, day, year] if year.len() == 4 => format!("{year}-{month:0>2}-{day:0>2}"),
        _ => date.to_string(),
    }
}

/// The board, when it says something the machine name didn't already.
fn board_summary() -> Option<String> {
    let name = dmi("board_name")?;
    let vendor = dmi("board_vendor");
    let product = dmi("product_name").unwrap_or_default();
    if name == product {
        return None;
    }
    Some(match vendor {
        Some(vendor) if !product.starts_with(&vendor) => format!("{vendor} {name}"),
        _ => name,
    })
}

/// Whether this is running on top of something else, and what. Worth a line only when
/// the answer is yes — on bare metal the absence is the answer.
fn virtualization() -> Option<String> {
    if fs::metadata("/.dockerenv").is_ok() {
        return Some("contêiner Docker".to_string());
    }
    if let Ok(cgroup) = fs::read_to_string("/proc/1/cgroup") {
        for (marker, name) in [
            ("docker", "contêiner Docker"),
            ("lxc", "contêiner LXC"),
            ("kubepods", "pod Kubernetes"),
        ] {
            if cgroup.contains(marker) {
                return Some(name.to_string());
            }
        }
    }
    // The hypervisor writes its own name into the DMI the guest sees.
    let hint = format!(
        "{} {}",
        dmi("sys_vendor").unwrap_or_default(),
        dmi("product_name").unwrap_or_default()
    );
    for (marker, name) in [
        ("KVM", "KVM"),
        ("QEMU", "QEMU"),
        ("VirtualBox", "VirtualBox"),
        ("VMware", "VMware"),
        ("Hyper-V", "Hyper-V"),
        ("Virtual Machine", "Hyper-V"),
        ("Xen", "Xen"),
        ("Parallels", "Parallels"),
        ("Amazon EC2", "Amazon EC2"),
        ("Google", "Google Compute Engine"),
    ] {
        if hint.contains(marker) {
            return Some(name.to_string());
        }
    }
    None
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
        let mut rows = vec![row("Host", hostname()), row("OS", os_summary())];
        if let Some(machine) = machine_summary() {
            rows.push(row("Machine", machine));
        }
        if let Some(board) = board_summary() {
            rows.push(row("Board", board));
        }
        if let Some(virtual_on) = virtualization() {
            rows.push(row("Virtual", virtual_on));
        }
        rows.extend([
            row("Uptime", format::human_duration(sysinfo::System::uptime())),
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
        ]);
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

    /// Deliberately ignores `limit`. It exists so a ranked table shows its top N in the
    /// compact grid, and this table isn't ranked — it's a fixed set of facts where the
    /// eleventh is no less true than the first. The panel shows what fits and
    /// fullscreening it shows the rest.
    fn sample(&mut self, state: &SystemState, _limit: Option<usize>) -> Vec<TableRow> {
        self.rows(state)
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
