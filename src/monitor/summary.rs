use std::collections::HashMap;
use std::ffi::c_void;
use std::fs;
use std::net::Ipv4Addr;

use nvml_wrapper::Nvml;
use sysinfo::Disks;

use super::disk::primary_disk;
use super::iface;
use super::{Detail, DetailSection, SystemState, TableMonitor, TableRow};
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
pub(super) fn hex_ipv4(hex: &str) -> Option<Ipv4Addr> {
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
            hex_ipv4(gateway)
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

/// The address traffic leaves by, and the interface it leaves through. Two facts that
/// are useless apart: on a machine with a VPN, a wired port and three bridges, an IP
/// with no card beside it is an answer to half the question.
fn outbound_address(state: &SystemState) -> String {
    let Some(ip) = outbound_ip() else {
        // No route out at all — worth saying plainly, since "?" reads as a failure to
        // look rather than as an answer.
        return "sem rota de saída".to_string();
    };
    match iface::interface_of(&state.networks, &ip.to_string()) {
        Some(interface) => format!("{ip}  ·  {interface}"),
        None => ip.to_string(),
    }
}

fn row(field: &str, value: String) -> TableRow {
    let mut row = TableRow::leaf(vec![field.to_string(), value], 0);
    // The field name, not a pid: these rows are facts about the machine, and the fact
    // is what the detail view goes back and asks about on the next tick.
    row.key = field.to_string();
    row
}

/// A general "who/what is this machine" summary, laid out beside SSH Sessions since
/// both answer the same question at different scopes: this panel is about the host
/// itself, that one's about who's connected to it.
/// The facts about a machine that cannot change while it is running: what it is, what
/// it boots, who made it. Read once at startup instead of on every tick — they involve a
/// dozen files under /sys and /etc, and the answer at 3pm is the answer from boot.
struct Fixed {
    os: String,
    machine: Option<String>,
    board: Option<String>,
    virtualization: Option<String>,
}

impl Fixed {
    fn read() -> Self {
        Self {
            os: os_summary(),
            machine: machine_summary(),
            board: board_summary(),
            virtualization: virtualization(),
        }
    }
}

pub struct SummaryMonitor {
    /// Probed once at startup — `None` on any machine without a working NVIDIA driver,
    /// same fallback as `gpu::GpuMonitor`. A fresh, independent handle rather than
    /// sharing `GpuMonitor`'s, since table monitors and chart monitors are constructed
    /// and owned separately.
    nvml: Option<Nvml>,
    /// Its own disk list, refreshed when a detail asks for it. `SystemState` only
    /// refreshes disks on the Overview tab, so by the time someone is reading this
    /// panel they'd be minutes old — and a mount that appeared since launch wouldn't be
    /// there at all. Interfaces need no such handling: the Processes tab refreshes them
    /// every tick for the Interfaces panel next door.
    disks: Disks,
    fixed: Fixed,
}

impl SummaryMonitor {
    pub fn new() -> Self {
        Self {
            nvml: Nvml::init().ok(),
            disks: Disks::new_with_refreshed_list(),
            fixed: Fixed::read(),
        }
    }

    fn rows(&self, state: &SystemState) -> Vec<TableRow> {
        let mut rows = vec![row("Host", hostname()), row("OS", self.fixed.os.clone())];
        if let Some(machine) = &self.fixed.machine {
            rows.push(row("Machine", machine.clone()));
        }
        if let Some(board) = &self.fixed.board {
            rows.push(row("Board", board.clone()));
        }
        if let Some(virtual_on) = &self.fixed.virtualization {
            rows.push(row("Virtual", virtual_on.clone()));
        }
        rows.extend([
            row("Uptime", format::human_duration(sysinfo::System::uptime())),
            row("User", current_user()),
            row("IP", outbound_address(state)),
            row("Interfaces", iface::addressed_summary(&state.networks)),
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
    fn id(&self) -> &'static str {
        "system"
    }

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

    fn detail(&mut self, state: &SystemState, row: &TableRow) -> Option<Detail> {
        // Mounts are read here rather than on the tick: this is the only view that
        // shows them, and it's open a fraction of the time the tab is.
        self.disks.refresh(true);
        self.detail_for(state, &row.key.clone())
    }

    fn has_detail(&self) -> bool {
        true
    }
}

// --- detail views --------------------------------------------------------------------
//
// Every row of this panel is a summary of something bigger, and Enter is where the rest
// of it lives: the CPU line becomes the topology and the clocks, the Memory line becomes
// the breakdown, the DNS line becomes every server and search domain. All of it read
// straight from /proc and /sys at the moment it's asked for, rather than from the shared
// `SystemState` — that one only refreshes what the *charts* need, and only while the
// Overview tab is focused, so its memory and CPU figures are stale by the time anyone is
// reading this panel.

/// A one-line file, trimmed. Most of /sys is exactly this.
fn read_line(path: &str) -> Option<String> {
    let content = fs::read_to_string(path).ok()?;
    let trimmed = content.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// `/proc/meminfo`, in bytes. The kernel prints kB.
fn meminfo() -> HashMap<String, u64> {
    let Ok(content) = fs::read_to_string("/proc/meminfo") else {
        return HashMap::new();
    };
    content
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            let kb: u64 = value.split_whitespace().next()?.parse().ok()?;
            Some((key.to_string(), kb * 1024))
        })
        .collect()
}

/// One `/proc/cpuinfo` block per logical CPU, as key/value pairs.
fn cpuinfo() -> Vec<HashMap<String, String>> {
    let Ok(content) = fs::read_to_string("/proc/cpuinfo") else {
        return Vec::new();
    };
    let mut blocks = Vec::new();
    let mut current = HashMap::new();
    for line in content.lines() {
        match line.split_once(':') {
            Some((key, value)) => {
                current.insert(key.trim().to_string(), value.trim().to_string());
            }
            // Blank line: end of one processor's block.
            None => {
                if !current.is_empty() {
                    blocks.push(std::mem::take(&mut current));
                }
            }
        }
    }
    if !current.is_empty() {
        blocks.push(current);
    }
    blocks
}

/// The `/etc/passwd` line for `name`, split into its seven fields.
fn passwd_entry(name: &str) -> Option<Vec<String>> {
    let content = fs::read_to_string("/etc/passwd").ok()?;
    content.lines().find_map(|line| {
        let fields: Vec<String> = line.split(':').map(str::to_string).collect();
        (fields.first().map(String::as_str) == Some(name)).then_some(fields)
    })
}

/// Secondary groups `name` belongs to, from `/etc/group`. The primary group isn't
/// listed there against the member — it lives in the passwd entry's gid.
fn groups_of(name: &str) -> Vec<String> {
    let Ok(content) = fs::read_to_string("/etc/group") else {
        return Vec::new();
    };
    content
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(':').collect();
            let group = fields.first()?;
            let members = fields.get(3)?;
            members
                .split(',')
                .any(|member| member == name)
                .then(|| group.to_string())
        })
        .collect()
}

/// Every default route, not just the first: a laptop on a VPN has two, and which one
/// wins is the metric — the number that explains where traffic is actually going.
fn default_routes() -> Vec<(String, Ipv4Addr, u32)> {
    let Ok(content) = fs::read_to_string("/proc/net/route") else {
        return Vec::new();
    };
    let mut routes: Vec<(String, Ipv4Addr, u32)> = content
        .lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if *fields.get(1)? != "00000000" {
                return None;
            }
            let gateway = hex_ipv4(fields.get(2)?)?;
            Some((
                fields.first()?.to_string(),
                gateway,
                fields.get(6)?.parse().unwrap_or(0),
            ))
        })
        .collect();
    routes.sort_by_key(|(_, _, metric)| *metric);
    routes
}

/// The MAC an address has answered ARP with, from the neighbour table. Present for
/// anything this machine has spoken to recently, which the gateway always has.
fn arp_mac(ip: &str) -> Option<String> {
    let content = fs::read_to_string("/proc/net/arp").ok()?;
    content.lines().skip(1).find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let mac = fields.get(3)?;
        (*fields.first()? == ip).then(|| mac.to_string())
    })
}

/// `/etc/resolv.conf` keyword lines — `nameserver`, `search`, `options`, in order.
fn resolv_conf(keyword: &str, path: &str) -> Vec<String> {
    let Ok(content) = fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .map(|line| line.split('#').next().unwrap_or("").trim())
        .filter_map(|line| Some(line.strip_prefix(keyword)?.trim().to_string()))
        .filter(|value| !value.is_empty())
        .collect()
}

fn machine_section() -> DetailSection {
    let mut section = DetailSection::new("Firmware (DMI)");
    for (label, field) in [
        ("Fabricante", "sys_vendor"),
        ("Produto", "product_name"),
        ("Versão do produto", "product_version"),
        ("Família", "product_family"),
        ("Fabricante da placa", "board_vendor"),
        ("Placa", "board_name"),
        ("Versão da placa", "board_version"),
        ("BIOS", "bios_vendor"),
        ("Versão da BIOS", "bios_version"),
    ] {
        if let Some(value) = dmi(field) {
            section.push(label, value);
        }
    }
    if let Some(date) = dmi("bios_date") {
        section.push("Data da BIOS", iso_date(&date));
    }
    if let Some(chassis) = chassis() {
        section.push("Gabinete", chassis);
    }
    if section.fields.is_empty() {
        section.push(
            "DMI",
            "nenhum — normal dentro de um contêiner, onde não há firmware a consultar",
        );
    }
    section
}

fn os_detail() -> Vec<DetailSection> {
    let mut distro = DetailSection::new("Distribuição");
    for (label, key) in [
        ("Nome", "NAME"),
        ("Versão", "VERSION"),
        ("ID", "ID"),
        ("Baseada em", "ID_LIKE"),
        ("Como se apresenta", "PRETTY_NAME"),
    ] {
        if let Some(value) = os_release(key) {
            distro.push(label, value);
        }
    }

    let mut kernel = DetailSection::new("Kernel");
    if let Some(version) = sysinfo::System::kernel_version() {
        kernel.push("Versão", version);
    }
    kernel.push("Completa", sysinfo::System::kernel_long_version());
    kernel.push("Arquitetura", sysinfo::System::cpu_arch());
    if let Some(init) = read_line("/proc/1/comm") {
        kernel.push("Init (pid 1)", init);
    }
    if let Some(cmdline) = read_line("/proc/cmdline") {
        kernel.push("Linha de boot", cmdline);
    }
    vec![distro, kernel]
}

fn uptime_section() -> DetailSection {
    let mut section = DetailSection::new("Tempo e carga");
    section.push(
        "Ligado há",
        format::human_duration(sysinfo::System::uptime()),
    );
    let load = sysinfo::System::load_average();
    section.push(
        "Carga média",
        format!(
            "{:.2} · {:.2} · {:.2}   (1, 5 e 15 min)",
            load.one, load.five, load.fifteen
        ),
    );
    // Field 4 of /proc/loadavg, `running/total`: how many of the machine's tasks are
    // actually on a CPU right now, against how many exist.
    if let Some(loadavg) = read_line("/proc/loadavg") {
        let fields: Vec<&str> = loadavg.split_whitespace().collect();
        if let Some(tasks) = fields.get(3) {
            section.push("Tarefas", format!("{tasks}  (executando/total)"));
        }
        if let Some(last_pid) = fields.get(4) {
            section.push("Último PID criado", last_pid.to_string());
        }
    }
    section
}

fn user_section() -> DetailSection {
    let mut section = DetailSection::new("Usuário");
    let name = current_user();
    section.push("Login", name.clone());
    match passwd_entry(&name) {
        Some(fields) => {
            if let Some(uid) = fields.get(2) {
                section.push("UID", uid.clone());
            }
            if let Some(gid) = fields.get(3) {
                let group = gid.parse().ok().and_then(user_group_name);
                section.push(
                    "Grupo primário",
                    match group {
                        Some(group) => format!("{group} (gid {gid})"),
                        None => format!("gid {gid}"),
                    },
                );
            }
            if let Some(gecos) = fields.get(4).filter(|g| !g.is_empty()) {
                section.push("Nome completo", gecos.clone());
            }
            if let Some(home) = fields.get(5) {
                section.push("Home", home.clone());
            }
            if let Some(shell) = fields.get(6) {
                section.push("Shell", shell.clone());
            }
        }
        None => section.push(
            "Cadastro",
            "não está em /etc/passwd — usuário de um diretório remoto (LDAP/SSSD)",
        ),
    }
    let groups = groups_of(&name);
    if !groups.is_empty() {
        section.push("Outros grupos", groups.join(", "));
    }
    // Who monitorzinho itself runs as, which is only interesting when it differs.
    if let Some(process_user) = std::env::var("USER").ok().filter(|u| *u != name) {
        section.push("Processo rodando como", process_user);
    }
    section
}

/// Group name for a gid, from `/etc/group` — the group-side twin of `user_name`.
fn user_group_name(gid: u32) -> Option<String> {
    let content = fs::read_to_string("/etc/group").ok()?;
    content.lines().find_map(|line| {
        let fields: Vec<&str> = line.split(':').collect();
        let group = fields.first()?;
        (fields.get(2)?.parse::<u32>().ok()? == gid).then(|| group.to_string())
    })
}

fn dns_section() -> DetailSection {
    let mut section = DetailSection::new("Resolução de nomes");
    let servers = resolv_conf("nameserver", "/etc/resolv.conf");
    if servers.is_empty() {
        section.push("Servidores", "nenhum em /etc/resolv.conf");
    }
    for (i, server) in servers.iter().enumerate() {
        section.push(&format!("Servidor {}", i + 1), server.clone());
    }
    for search in resolv_conf("search", "/etc/resolv.conf") {
        section.push("Domínios de busca", search);
    }
    for option in resolv_conf("options", "/etc/resolv.conf") {
        section.push("Opções", option);
    }
    // A stub resolver listening on loopback answers every query itself; the servers it
    // actually forwards to are the ones worth knowing, and they're in its own file.
    if servers.iter().any(|s| s.starts_with("127.")) {
        let upstream = resolv_conf("nameserver", "/run/systemd/resolve/resolv.conf");
        section.push(
            "Encaminha para",
            if upstream.is_empty() {
                "stub local (127.x) — o destino real não está publicado em /run/systemd/resolve"
                    .to_string()
            } else {
                upstream.join(", ")
            },
        );
    }
    section
}

fn cpu_section(state: &SystemState) -> DetailSection {
    let mut section = DetailSection::new("Processador");
    let blocks = cpuinfo();
    let first = blocks.first();
    if let Some(model) = first.and_then(|b| b.get("model name")) {
        section.push("Modelo", model.clone());
    }
    if let Some(vendor) = first.and_then(|b| b.get("vendor_id")) {
        section.push("Fabricante", vendor.clone());
    }
    let logical = state.sys.cpus().len().max(blocks.len());
    match sysinfo::System::physical_core_count() {
        Some(physical) => {
            section.push("Núcleos", format!("{physical} físicos · {logical} lógicos"))
        }
        None => section.push("Núcleos", format!("{logical} lógicos")),
    }
    // Read per-core rather than from the first block alone: on a machine with efficiency
    // cores, or one whose governor has half the cores parked, a single number is a lie.
    let clocks: Vec<f64> = blocks
        .iter()
        .filter_map(|b| b.get("cpu MHz")?.parse().ok())
        .collect();
    if let (Some(min), Some(max)) = (
        clocks.iter().cloned().reduce(f64::min),
        clocks.iter().cloned().reduce(f64::max),
    ) {
        let average = clocks.iter().sum::<f64>() / clocks.len() as f64;
        section.push(
            "Frequência agora",
            format!("{average:.0} MHz em média · {min:.0} a {max:.0} MHz entre os núcleos"),
        );
    }
    if let (Some(min), Some(max)) = (
        read_line("/sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_min_freq"),
        read_line("/sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_max_freq"),
    ) {
        let mhz = |khz: String| khz.parse::<f64>().unwrap_or(0.0) / 1000.0;
        section.push(
            "Faixa do hardware",
            format!("{:.0} a {:.0} MHz", mhz(min), mhz(max)),
        );
    }
    if let Some(governor) = read_line("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor") {
        section.push("Governador", governor);
    }
    for level in 1..=3 {
        let caches: Vec<String> = (0..8)
            .filter_map(|index| {
                let base = format!("/sys/devices/system/cpu/cpu0/cache/index{index}");
                (read_line(&format!("{base}/level"))? == level.to_string())
                    .then(|| {
                        Some(format!(
                            "{} ({})",
                            read_line(&format!("{base}/size"))?,
                            read_line(&format!("{base}/type"))?
                        ))
                    })
                    .flatten()
            })
            .collect();
        if !caches.is_empty() {
            section.push(&format!("Cache L{level}"), caches.join(" · "));
        }
    }
    // A handful of flags worth recognising, rather than the two hundred the kernel
    // prints: what accelerates what, and whether this is a guest.
    if let Some(flags) = first.and_then(|b| b.get("flags")) {
        let present: Vec<&str> = [
            "avx512f",
            "avx2",
            "avx",
            "aes",
            "sha_ni",
            "vmx",
            "svm",
            "hypervisor",
        ]
        .into_iter()
        .filter(|flag| flags.split_whitespace().any(|f| f == *flag))
        .collect();
        if !present.is_empty() {
            section.push("Recursos", present.join(", "));
        }
    }
    section
}

fn memory_section() -> DetailSection {
    let info = meminfo();
    let mut section = DetailSection::new("Memória");
    let bytes = |key: &str| info.get(key).copied();
    let show = |section: &mut DetailSection, label: &str, key: &str| {
        if let Some(value) = bytes(key) {
            section.push(label, format::human_bytes(value as f64));
        }
    };
    show(&mut section, "Total", "MemTotal");
    if let (Some(total), Some(available)) = (bytes("MemTotal"), bytes("MemAvailable")) {
        let used = total.saturating_sub(available);
        section.push(
            "Em uso",
            format!(
                "{}  ({:.1}%)",
                format::human_bytes(used as f64),
                used as f64 / total.max(1) as f64 * 100.0
            ),
        );
        // Not the same as free: what the cache is holding can be handed back on demand,
        // which is why a Linux machine with almost no "free" memory is usually fine.
        section.push("Disponível", format::human_bytes(available as f64));
    }
    show(&mut section, "Livre de fato", "MemFree");
    show(&mut section, "Cache de páginas", "Cached");
    show(&mut section, "Buffers", "Buffers");
    show(&mut section, "Sujas (a gravar)", "Dirty");
    show(&mut section, "Compartilhada (tmpfs)", "Shmem");
    show(&mut section, "Swap total", "SwapTotal");
    if let (Some(total), Some(free)) = (bytes("SwapTotal"), bytes("SwapFree")) {
        section.push(
            "Swap em uso",
            format::human_bytes(total.saturating_sub(free) as f64),
        );
    }
    section
}

fn disk_section(disks: &Disks) -> DetailSection {
    let mut section = DetailSection::new("Sistemas de arquivos");
    for disk in disks.list() {
        let total = disk.total_space();
        let free = disk.available_space();
        let used = total.saturating_sub(free);
        let percent = if total > 0 {
            used as f64 / total as f64 * 100.0
        } else {
            0.0
        };
        section.push(
            &disk.mount_point().to_string_lossy(),
            format!(
                "{} de {} ({:.0}%) · livre {} · {}{}",
                format::human_bytes(used as f64),
                format::human_bytes(total as f64),
                percent,
                format::human_bytes(free as f64),
                disk.file_system().to_string_lossy(),
                if disk.is_removable() {
                    " · removível"
                } else {
                    ""
                }
            ),
        );
    }
    if section.fields.is_empty() {
        section.push("Montagens", "nenhuma legível");
    }
    section
}

/// Every interface with what it is and where it is, for the rows that are about the
/// machine's place on the network. The Interfaces panel next door goes deeper on one;
/// this is the whole list at a glance.
fn network_section(interfaces: &[iface::Interface]) -> DetailSection {
    let mut section = DetailSection::new("Interfaces");
    for interface in interfaces {
        let mut value = format!("{}  ·  {}", interface.address_summary(), interface.kind);
        if let Some(mac) = &interface.mac {
            value.push_str(&format!("  ·  {mac}"));
        }
        section.push(&interface.name, value);
    }
    if section.fields.is_empty() {
        section.push("Interfaces", "nenhuma encontrada em /sys/class/net");
    }
    section
}

fn gateway_section() -> DetailSection {
    let mut section = DetailSection::new("Saída para a rede");
    let routes = default_routes();
    if routes.is_empty() {
        section.push(
            "Rota padrão",
            "nenhuma — esta máquina não tem saída roteada",
        );
    }
    for (interface, gateway, metric) in &routes {
        section.push(
            &format!("Via {interface}"),
            format!("{gateway}  ·  métrica {metric}"),
        );
        if let Some(mac) = arp_mac(&gateway.to_string()) {
            section.push("MAC do gateway", mac);
        }
    }
    if let Some(ip) = outbound_ip() {
        section.push("Endereço de origem", ip.to_string());
    }
    section
}

fn gpu_section(nvml: &Nvml) -> DetailSection {
    let mut section = DetailSection::new("GPU");
    if let Ok(version) = nvml.sys_driver_version() {
        section.push("Driver", version);
    }
    let Ok(device) = nvml.device_by_index(0) else {
        section.push("Dispositivo", "não pôde ser aberto");
        return section;
    };
    if let Ok(name) = device.name() {
        section.push("Modelo", name);
    }
    if let Ok(memory) = device.memory_info() {
        section.push(
            "Memória",
            format!(
                "{} de {} em uso",
                format::human_bytes(memory.used as f64),
                format::human_bytes(memory.total as f64)
            ),
        );
    }
    if let Ok(utilization) = device.utilization_rates() {
        section.push(
            "Utilização",
            format!(
                "{}% núcleo · {}% memória",
                utilization.gpu, utilization.memory
            ),
        );
    }
    if let Ok(temperature) =
        device.temperature(nvml_wrapper::enum_wrappers::device::TemperatureSensor::Gpu)
    {
        section.push("Temperatura", format!("{temperature} °C"));
    }
    if let Ok(power) = device.power_usage() {
        section.push("Consumo", format!("{:.1} W", power as f64 / 1000.0));
    }
    section
}

impl SummaryMonitor {
    /// The rest of one summarised fact. `None` for a field with nothing more behind it
    /// than the row already shows, which is how a row opts out of Enter.
    fn detail_for(&mut self, state: &SystemState, field: &str) -> Option<Detail> {
        let sections = match field {
            "Host" => {
                let mut section = DetailSection::new("Identidade");
                section.push("Nome", hostname());
                if let Some(domain) = resolv_conf("search", "/etc/resolv.conf").first() {
                    section.push("Domínio de busca", domain.clone());
                }
                if let Some(id) = read_line("/etc/machine-id") {
                    section.push("machine-id", id);
                }
                if let Some(boot) = read_line("/proc/sys/kernel/random/boot_id") {
                    section.push("boot-id (muda a cada boot)", boot);
                }
                section.push(
                    "Ligado há",
                    format::human_duration(sysinfo::System::uptime()),
                );
                vec![section]
            }
            "OS" => os_detail(),
            "Machine" | "Board" => vec![machine_section()],
            "Virtual" => {
                let mut section = DetailSection::new("Virtualização");
                section.push(
                    "Detectado",
                    virtualization().unwrap_or_else(|| "nada — parece metal puro".to_string()),
                );
                if let Ok(cgroup) = fs::read_to_string("/proc/1/cgroup") {
                    section.push("cgroup do pid 1", cgroup.trim().to_string());
                }
                if fs::metadata("/.dockerenv").is_ok() {
                    section.push("Marcador", "/.dockerenv existe");
                }
                if let Some(hint) = dmi("sys_vendor") {
                    section.push("Fabricante segundo o DMI", hint);
                }
                vec![section]
            }
            "Uptime" => vec![uptime_section()],
            "User" => vec![user_section()],
            "IP" | "Interfaces" => vec![network_section(state.interfaces()), gateway_section()],
            "Gateway" => vec![gateway_section(), network_section(state.interfaces())],
            "DNS" => vec![dns_section()],
            "CPU" => vec![cpu_section(state), uptime_section()],
            "Memory" => vec![memory_section()],
            "Disk" => vec![disk_section(&self.disks)],
            "GPU" => vec![gpu_section(self.nvml.as_ref()?)],
            _ => return None,
        };
        Some(Detail {
            title: field.to_string(),
            gone_note: "indisponível",
            sections,
            rates: None,
            handoffs: Vec::new(),
            handoff_title: "",
        })
    }
}
