//! Finding what's alive on the local network.
//!
//! Without `CAP_NET_RAW` there's no ARP sweep and no raw ICMP, so this uses what a
//! normal user actually has, in order of how much it costs:
//!
//! * **The neighbour table.** Anything the machine has spoken to recently is already in
//!   `/proc/net/arp`, with its MAC. Free, instant, and proof the host exists.
//! * **ICMP echo through an unprivileged datagram socket.** Linux allows this when
//!   `net.ipv4.ping_group_range` includes the caller's group — which is how `ping`
//!   works without setuid on most desktops. Where the kernel says no, the tool says so
//!   once and carries on.
//! * **TCP connect.** The point isn't finding a service: a refused connection is a
//!   *reply*, and a reply proves the host is there. A host that answers `RST` on every
//!   port is as discovered as one running a web server.
//!
//! Anything that answers also gets looked up in the neighbour table again afterwards,
//! because a host that replied has necessarily answered ARP on the way.

use std::collections::HashMap;
use std::ffi::c_int;
use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::monitor::resolve;

use super::{EventKind, Execution, Handoff, ParamSpec, Recorder, Tool};

/// Ports probed to provoke *any* answer. Chosen to cover the things that tend to be
/// listening on a LAN, but a refusal from a closed one counts just as much.
const DEFAULT_PORTS: &str = "22,80,443,445,3389,8080,5900,53,139,631";
/// Hosts probed at once, at most.
const MAX_WORKERS: usize = 512;
/// Addresses one execution will sweep. A /24 is 254; this allows a /19 and refuses to
/// silently start something that would take an hour.
const MAX_HOSTS: usize = 8192;
/// Hosts probed before the row's progress figure is refreshed.
const PROGRESS_EVERY: usize = 32;
/// Where the system keeps its OUI database, when it has one. Read rather than embedded:
/// a vendor table baked into the binary is wrong the month after it ships.
const OUI_PATHS: &[&str] = &[
    "/usr/share/ieee-data/oui.txt",
    "/var/lib/ieee-data/oui.txt",
    "/usr/share/nmap/nmap-mac-prefixes",
    "/usr/share/wireshark/manuf",
];

const YES_NO: &[&str] = &["sim", "não"];

pub struct NetTool;

impl Tool for NetTool {
    fn id(&self) -> &'static str {
        "net"
    }

    fn name(&self) -> &'static str {
        "Scanner de rede"
    }

    fn description(&self) -> &'static str {
        "Descobre quais IPs estão vivos na rede local, com MAC, fabricante e nome de cada um"
    }

    fn params(&self) -> Vec<ParamSpec> {
        vec![
            ParamSpec::text(
                "rede",
                "Rede",
                "",
                "Vazio varre as redes locais detectadas nas rotas. Aceita CIDR: 192.168.1.0/24",
            ),
            ParamSpec::text(
                "portas",
                "Portas de sondagem",
                DEFAULT_PORTS,
                "Uma conexão recusada já prova que o host existe, então portas fechadas contam igual",
            ),
            ParamSpec::text(
                "concorrencia",
                "Hosts simultâneos",
                "128",
                "Quantos endereços são sondados ao mesmo tempo",
            ),
            ParamSpec::text(
                "timeout",
                "Tempo por host (ms)",
                "400",
                "Quanto esperar por uma resposta. Numa rede sem fio congestionada, aumente",
            ),
            ParamSpec::choice(
                "nomes",
                "Resolver nomes",
                YES_NO,
                "Faz DNS reverso de cada host encontrado — em rede doméstica normalmente vem do roteador",
            ),
        ]
    }

    fn summarize(&self, params: &HashMap<&'static str, String>) -> String {
        let get = |key| params.get(key).map(String::as_str).unwrap_or("").trim();
        match get("rede") {
            "" => "redes locais detectadas".to_string(),
            network => network.to_string(),
        }
    }

    fn on_demand(&self) -> bool {
        true
    }

    fn start(&self, id: u64, params: &HashMap<&'static str, String>) -> Result<Execution, String> {
        let plan = Plan::from(params)?;
        let (execution, recorder) = Execution::new(id, self.name(), self.summarize(params));
        recorder.record(
            0,
            EventKind::Note(format!(
                "pronto para varrer {} — {} endereço(s). Nada roda até você abrir",
                plan.networks
                    .iter()
                    .map(|net| net.to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
                plan.hosts().len()
            )),
        );
        Ok(execution.on_demand())
    }

    fn open(&self, execution: &Execution, params: &HashMap<&'static str, String>) {
        if execution.has_result() || execution.is_working() {
            return;
        }
        self.rerun(execution, params);
    }

    fn rerun(&self, execution: &Execution, params: &HashMap<&'static str, String>) {
        if execution.is_working() {
            return;
        }
        let Ok(plan) = Plan::from(params) else {
            return;
        };
        let recorder = execution.recorder();
        let finished = execution.finish_flag();
        finished.store(false, Ordering::Relaxed);
        thread::spawn(move || {
            sweep(plan, &recorder);
            recorder.ran();
            finished.store(true, Ordering::Relaxed);
        });
    }

    fn columns(&self, execution: &Execution) -> (String, String) {
        execution.outcome()
    }

    /// Every host found, ready to be handed to the port scanner. This is the pair the
    /// tool exists for: find what's out there, then look at one of them properly.
    fn handoffs(&self, execution: &Execution) -> Vec<Handoff> {
        execution
            .findings("ip")
            .into_iter()
            .map(|address| Handoff {
                label: format!("varrer portas de {address}"),
                tool: "scan",
                params: vec![("alvo", address), ("faixa", "comuns".to_string())],
            })
            .collect()
    }
}

/// An IPv4 network as `base/prefix`, kept in host order so iteration is trivial.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Network {
    base: u32,
    prefix: u8,
}

impl Network {
    fn new(address: u32, prefix: u8) -> Self {
        let mask = mask_of(prefix);
        Self {
            base: address & mask,
            prefix,
        }
    }

    /// Usable addresses: network and broadcast are skipped on anything wider than a
    /// /31, where they don't exist as such.
    fn hosts(&self) -> impl Iterator<Item = Ipv4Addr> + '_ {
        let size = 1u64 << (32 - self.prefix);
        let (first, last) = if self.prefix >= 31 {
            (0, size - 1)
        } else {
            (1, size - 2)
        };
        (first..=last).map(move |offset| Ipv4Addr::from(self.base + offset as u32))
    }

    fn contains(&self, address: Ipv4Addr) -> bool {
        u32::from(address) & mask_of(self.prefix) == self.base
    }
}

impl std::fmt::Display for Network {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", Ipv4Addr::from(self.base), self.prefix)
    }
}

fn mask_of(prefix: u8) -> u32 {
    if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    }
}

struct Plan {
    networks: Vec<Network>,
    ports: Vec<u16>,
    workers: usize,
    timeout: Duration,
    names: bool,
}

impl Plan {
    fn from(params: &HashMap<&'static str, String>) -> Result<Self, String> {
        let get = |key| params.get(key).map(String::as_str).unwrap_or("").trim();

        let networks = match get("rede") {
            "" => {
                let found = local_networks();
                if found.is_empty() {
                    return Err(
                        "nenhuma rede local encontrada nas rotas — informe um CIDR".to_string()
                    );
                }
                found
            }
            text => vec![parse_cidr(text)?],
        };

        let ports = parse_ports(get("portas"))?;
        if ports.is_empty() {
            return Err("informe ao menos uma porta de sondagem".to_string());
        }

        let workers = get("concorrencia")
            .parse::<usize>()
            .map_err(|_| {
                format!(
                    "hosts simultâneos: «{}» não é um número",
                    get("concorrencia")
                )
            })?
            .clamp(1, MAX_WORKERS);
        let timeout = get("timeout")
            .parse::<u64>()
            .map_err(|_| format!("tempo por host: «{}» não é um número", get("timeout")))?
            .clamp(50, 30_000);

        let plan = Self {
            networks,
            ports,
            workers,
            timeout: Duration::from_millis(timeout),
            names: get("nomes") != "não",
        };
        let total = plan.hosts().len();
        if total > MAX_HOSTS {
            return Err(format!(
                "{total} endereços é grande demais para uma varredura (limite {MAX_HOSTS}) — use um CIDR menor"
            ));
        }
        Ok(plan)
    }

    fn hosts(&self) -> Vec<Ipv4Addr> {
        let mut hosts: Vec<Ipv4Addr> = self
            .networks
            .iter()
            .flat_map(|network| network.hosts())
            .collect();
        hosts.sort();
        hosts.dedup();
        hosts
    }
}

/// `192.168.1.0/24`, or a bare address meaning a single host.
fn parse_cidr(text: &str) -> Result<Network, String> {
    let (address, prefix) = match text.split_once('/') {
        Some((address, prefix)) => (
            address,
            prefix
                .trim()
                .parse::<u8>()
                .map_err(|_| format!("prefixo inválido em «{text}»"))?,
        ),
        None => (text, 32),
    };
    if prefix > 32 {
        return Err(format!("prefixo inválido em «{text}»"));
    }
    let address: Ipv4Addr = address
        .trim()
        .parse()
        .map_err(|_| format!("endereço inválido em «{text}»"))?;
    Ok(Network::new(u32::from(address), prefix))
}

fn parse_ports(spec: &str) -> Result<Vec<u16>, String> {
    let mut ports = Vec::new();
    for piece in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        ports.push(
            piece
                .parse::<u16>()
                .map_err(|_| format!("porta inválida: «{piece}»"))?,
        );
    }
    ports.sort_unstable();
    ports.dedup();
    Ok(ports)
}

/// The networks this machine is directly attached to, from the kernel's routing table.
///
/// `/proc/net/route` rather than a guess at "192.168.x": a laptop on a VPN with three
/// container bridges is attached to five networks, and the interesting host is as
/// likely to be on one of those as on the wifi.
fn local_networks() -> Vec<Network> {
    let Ok(content) = fs::read_to_string("/proc/net/route") else {
        return Vec::new();
    };
    let mut networks = Vec::new();
    for line in content.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 8 || fields[0] == "lo" {
            continue;
        }
        let (Ok(destination), Ok(mask)) = (
            u32::from_str_radix(fields[1], 16),
            u32::from_str_radix(fields[7], 16),
        ) else {
            continue;
        };
        // Little-endian in the file; the default route has no mask and isn't a network.
        let (destination, mask) = (destination.swap_bytes(), mask.swap_bytes());
        if mask == 0 {
            continue;
        }
        let prefix = mask.count_ones() as u8;
        // A /8 bridge network — docker's default — is 16 million addresses nobody meant
        // to sweep. Anything that wide is skipped unless asked for by name.
        if prefix < 22 {
            continue;
        }
        let network = Network::new(destination, prefix);
        if !networks.contains(&network) {
            networks.push(network);
        }
    }
    networks
}

/// The kernel's neighbour table: address, MAC, and which interface saw it.
fn neighbours() -> HashMap<Ipv4Addr, String> {
    let mut table = HashMap::new();
    let Ok(content) = fs::read_to_string("/proc/net/arp") else {
        return table;
    };
    for line in content.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 4 {
            continue;
        }
        let (Ok(address), mac) = (fields[0].parse::<Ipv4Addr>(), fields[3]) else {
            continue;
        };
        // All-zero means the entry is incomplete: asked for, never answered.
        if mac == "00:00:00:00:00:00" {
            continue;
        }
        table.insert(address, mac.to_ascii_lowercase());
    }
    table
}

/// Vendor names for the MAC prefixes actually seen, read from whatever OUI database the
/// system happens to ship. One pass over the file, looking only for what's needed.
fn vendors(macs: &[String]) -> HashMap<String, String> {
    let mut wanted: HashMap<String, String> = HashMap::new();
    for mac in macs {
        let prefix: String = mac.split(':').take(3).collect::<Vec<_>>().join("");
        wanted.insert(prefix.to_ascii_uppercase(), String::new());
    }
    if wanted.is_empty() {
        return HashMap::new();
    }

    let Some(content) = OUI_PATHS
        .iter()
        .find_map(|path| fs::read_to_string(path).ok())
    else {
        return HashMap::new();
    };
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Both formats in the wild start with the prefix: `1C-1D-D3 (hex) Vendor` and
        // `1c:1d:d3\tVendor`. Normalising the separators covers both.
        let mut parts = line.split_whitespace();
        let Some(raw) = parts.next() else {
            continue;
        };
        let prefix: String = raw
            .chars()
            .filter(char::is_ascii_hexdigit)
            .collect::<String>()
            .to_ascii_uppercase();
        if prefix.len() != 6 {
            continue;
        }
        let Some(slot) = wanted.get_mut(&prefix) else {
            continue;
        };
        if !slot.is_empty() {
            continue;
        }
        let rest: String = parts.collect::<Vec<_>>().join(" ");
        let name = rest.trim_start_matches("(hex)").trim();
        if !name.is_empty() {
            *slot = name.to_string();
        }
    }
    wanted.retain(|_, name| !name.is_empty());
    wanted
}

/// A host that answered, and what it answered with.
struct Host {
    address: Ipv4Addr,
    /// How it made itself known — ICMP, an open port, a refusal, the ARP table.
    how: String,
    ports: Vec<u16>,
    latency: Option<Duration>,
}

fn sweep(plan: Plan, rec: &Recorder) {
    let started = Instant::now();
    let hosts = plan.hosts();
    let total = hosts.len();
    let networks = plan
        .networks
        .iter()
        .map(|net| net.to_string())
        .collect::<Vec<_>>()
        .join(", ");

    rec.record(
        0,
        EventKind::Note(format!(
            "varrendo {networks} — {total} endereço(s), {} simultâneos, {} ms cada",
            plan.workers,
            plan.timeout.as_millis()
        )),
    );

    // Whatever the machine already knows, before a single packet is sent.
    let known = neighbours();
    let seeded: Vec<Ipv4Addr> = known
        .keys()
        .copied()
        .filter(|address| plan.networks.iter().any(|net| net.contains(*address)))
        .collect();
    if !seeded.is_empty() {
        rec.record(
            0,
            EventKind::Note(format!(
                "   {} host(s) já na tabela de vizinhos antes de sondar",
                seeded.len()
            )),
        );
    }

    let pinger = Pinger::new();
    if pinger.is_none() {
        rec.record(
            0,
            EventKind::Note(
                "   ICMP sem privilégio indisponível neste kernel — a descoberta vai por TCP"
                    .to_string(),
            ),
        );
    }
    let pinger = Arc::new(pinger);

    rec.report(format!("0/{total}"), "varrendo…");
    let plan = Arc::new(plan);
    let hosts = Arc::new(hosts);
    let cursor = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(AtomicUsize::new(0));

    let workers: Vec<_> = (0..plan.workers.min(total.max(1)))
        .map(|_| {
            let (plan, hosts, cursor, done, pinger, rec) = (
                Arc::clone(&plan),
                Arc::clone(&hosts),
                Arc::clone(&cursor),
                Arc::clone(&done),
                Arc::clone(&pinger),
                rec.clone(),
            );
            thread::spawn(move || {
                let mut found = Vec::new();
                loop {
                    if rec.stopping() {
                        break;
                    }
                    let index = cursor.fetch_add(1, Ordering::Relaxed);
                    let Some(&address) = hosts.get(index) else {
                        break;
                    };
                    if let Some(host) = probe(&plan, pinger.as_ref().as_ref(), address) {
                        found.push(host);
                    }
                    let seen = done.fetch_add(1, Ordering::Relaxed) + 1;
                    if seen.is_multiple_of(PROGRESS_EVERY) {
                        rec.report(format!("{seen}/{total}"), "varrendo…");
                    }
                }
                found
            })
        })
        .collect();

    let mut found: Vec<Host> = workers
        .into_iter()
        .filter_map(|worker| worker.join().ok())
        .flatten()
        .collect();

    // Anything already in the neighbour table is alive whether or not it answered a
    // probe — a host with every port firewalled still had to answer ARP.
    for address in seeded {
        if !found.iter().any(|host| host.address == address) {
            found.push(Host {
                address,
                how: "tabela de vizinhos".to_string(),
                ports: Vec::new(),
                latency: None,
            });
        }
    }
    found.sort_by_key(|host| u32::from(host.address));

    report(&plan, rec, &found, started.elapsed(), total);
}

fn report(plan: &Plan, rec: &Recorder, found: &[Host], elapsed: Duration, total: usize) {
    // Re-read after probing: a host that replied has answered ARP by definition, so the
    // table knows more now than it did at the start.
    let table = neighbours();
    let macs: Vec<String> = found
        .iter()
        .filter_map(|host| table.get(&host.address).cloned())
        .collect();
    let vendors = vendors(&macs);
    let services = resolve::Services::load();

    for host in found {
        let mac = table.get(&host.address);
        let vendor = mac
            .map(|mac| {
                mac.split(':')
                    .take(3)
                    .collect::<Vec<_>>()
                    .join("")
                    .to_ascii_uppercase()
            })
            .and_then(|prefix| vendors.get(&prefix).cloned());
        let name = if plan.names {
            resolve::reverse_now(&host.address.to_string())
        } else {
            None
        };
        let ports = if host.ports.is_empty() {
            String::new()
        } else {
            format!(
                "  ·  {}",
                host.ports
                    .iter()
                    .map(|port| match services.name(true, *port) {
                        Some(service) => format!("{port}/{service}"),
                        None => port.to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        // How it was found is part of the finding: a host known only from the neighbour
        // table is one that answered ARP and dropped everything else, which is a
        // different thing from one that refused a connection.
        rec.record(
            0,
            EventKind::Note(format!(
                "{:<16}{:>9}  {:<16}{:<18}{:<24}{}{ports}",
                host.address,
                host.latency
                    .map(|rtt| format!("{:.1} ms", rtt.as_secs_f64() * 1000.0))
                    .unwrap_or_default(),
                host.how,
                mac.cloned().unwrap_or_default(),
                vendor.clone().unwrap_or_default(),
                name.clone().unwrap_or_default(),
            )),
        );
        rec.found("ip", host.address.to_string());
    }

    let with_ports = found.iter().filter(|host| !host.ports.is_empty()).count();
    rec.record(
        0,
        EventKind::Note(format!(
            "{}: {} host(s) vivos de {total} endereço(s), {with_ports} com porta aberta, em {:.1}s",
            if rec.stopping() {
                "varredura interrompida"
            } else {
                "varredura concluída"
            },
            found.len(),
            elapsed.as_secs_f64()
        )),
    );
    rec.report(
        format!("{} de {total} vivos", found.len()),
        format!(
            "{with_ports} com porta aberta · {:.1}s",
            elapsed.as_secs_f64()
        ),
    );
}

/// Tries every method on one address, cheapest first, and stops at the first proof.
fn probe(plan: &Plan, pinger: Option<&Pinger>, address: Ipv4Addr) -> Option<Host> {
    let started = Instant::now();
    if let Some(pinger) = pinger
        && pinger.reaches(address, plan.timeout)
    {
        return Some(Host {
            address,
            how: "ICMP".to_string(),
            ports: open_ports(plan, address),
            latency: Some(started.elapsed()),
        });
    }

    let mut open = Vec::new();
    let mut refused = false;
    let mut latency = None;
    for &port in &plan.ports {
        let target = SocketAddr::new(IpAddr::V4(address), port);
        let attempt = Instant::now();
        match TcpStream::connect_timeout(&target, plan.timeout) {
            Ok(_) => {
                latency.get_or_insert(attempt.elapsed());
                open.push(port);
            }
            // A refusal is the host talking. That's the discovery.
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                latency.get_or_insert(attempt.elapsed());
                refused = true;
            }
            Err(_) => {}
        }
    }
    if open.is_empty() && !refused {
        return None;
    }
    Some(Host {
        address,
        how: if open.is_empty() {
            "TCP recusado".to_string()
        } else {
            "TCP aberto".to_string()
        },
        ports: open,
        latency,
    })
}

/// The TCP half on its own, for a host already proven alive by ICMP.
fn open_ports(plan: &Plan, address: Ipv4Addr) -> Vec<u16> {
    plan.ports
        .iter()
        .copied()
        .filter(|&port| {
            TcpStream::connect_timeout(&SocketAddr::new(IpAddr::V4(address), port), plan.timeout)
                .is_ok()
        })
        .collect()
}

// --- unprivileged ICMP ---------------------------------------------------------------

const AF_INET: c_int = 2;
const SOCK_DGRAM: c_int = 2;
const IPPROTO_ICMP: c_int = 1;
const SOL_SOCKET: c_int = 1;
const SO_RCVTIMEO: c_int = 20;
const ICMP_ECHO: u8 = 8;
const ICMP_ECHOREPLY: u8 = 0;

unsafe extern "C" {
    fn socket(domain: c_int, kind: c_int, protocol: c_int) -> c_int;
    fn close(fd: c_int) -> c_int;
    fn setsockopt(
        fd: c_int,
        level: c_int,
        name: c_int,
        value: *const std::ffi::c_void,
        len: u32,
    ) -> c_int;
    fn sendto(
        fd: c_int,
        buf: *const u8,
        len: usize,
        flags: c_int,
        addr: *const SockaddrIn,
        addrlen: u32,
    ) -> isize;
    fn recv(fd: c_int, buf: *mut u8, len: usize, flags: c_int) -> isize;
}

#[repr(C)]
struct SockaddrIn {
    sin_family: u16,
    sin_port: u16,
    sin_addr: u32,
    sin_zero: [u8; 8],
}

#[repr(C)]
struct Timeval {
    tv_sec: i64,
    tv_usec: i64,
}

/// An ICMP echo socket, if the kernel will give one to an unprivileged process.
///
/// `SOCK_DGRAM` + `IPPROTO_ICMP` is the mode `ping` uses when it isn't setuid: the
/// kernel owns the identifier and only delivers replies to this socket, so no privilege
/// is needed and no other process's replies are visible. Gated by
/// `net.ipv4.ping_group_range`; where that excludes us, `socket()` fails and the sweep
/// falls back to TCP.
pub struct Pinger {
    fd: c_int,
}

impl Pinger {
    fn new() -> Option<Self> {
        let fd = unsafe { socket(AF_INET, SOCK_DGRAM, IPPROTO_ICMP) };
        (fd >= 0).then_some(Self { fd })
    }

    fn reaches(&self, address: Ipv4Addr, timeout: Duration) -> bool {
        let timeval = Timeval {
            tv_sec: timeout.as_secs() as i64,
            tv_usec: timeout.subsec_micros() as i64,
        };
        let set = unsafe {
            setsockopt(
                self.fd,
                SOL_SOCKET,
                SO_RCVTIMEO,
                (&raw const timeval).cast(),
                size_of::<Timeval>() as u32,
            )
        };
        if set != 0 {
            return false;
        }

        // Type, code, checksum, identifier, sequence. The kernel rewrites the
        // identifier and fixes the checksum for datagram ICMP sockets, but a correct
        // one costs nothing and keeps the packet valid if that ever changes.
        let mut packet = [0u8; 16];
        packet[0] = ICMP_ECHO;
        packet[6..8].copy_from_slice(&1u16.to_be_bytes());
        let sum = checksum(&packet);
        packet[2..4].copy_from_slice(&sum.to_be_bytes());

        let destination = SockaddrIn {
            sin_family: AF_INET as u16,
            sin_port: 0,
            sin_addr: u32::from(address).to_be(),
            sin_zero: [0; 8],
        };
        let sent = unsafe {
            sendto(
                self.fd,
                packet.as_ptr(),
                packet.len(),
                0,
                &raw const destination,
                size_of::<SockaddrIn>() as u32,
            )
        };
        if sent < 0 {
            return false;
        }

        let mut buf = [0u8; 128];
        let received = unsafe { recv(self.fd, buf.as_mut_ptr(), buf.len(), 0) };
        received > 0 && buf[0] == ICMP_ECHOREPLY
    }
}

impl Drop for Pinger {
    fn drop(&mut self) {
        unsafe { close(self.fd) };
    }
}

// The socket is only ever used through `&self` and every call is a self-contained
// send/receive on a datagram socket the kernel demultiplexes per-socket.
unsafe impl Send for Pinger {}
unsafe impl Sync for Pinger {}

/// The internet checksum: one's complement of the one's complement sum of 16-bit words.
fn checksum(data: &[u8]) -> u16 {
    let mut sum = 0u32;
    let (chunks, tail) = data.as_chunks::<2>();
    for chunk in chunks {
        sum += u16::from_be_bytes(*chunk) as u32;
    }
    if let [last] = tail {
        sum += (*last as u32) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}
