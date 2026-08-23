use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::net::{Ipv4Addr, Ipv6Addr};

use sysinfo::Pid;

use super::iface;
use super::process::{describe_owner, kill_danger, owner_section};
use super::resolve::{Services, user_name};
use super::{Danger, Detail, DetailSection, SystemState, TableMonitor, TableRow, mark};
use crate::tools::Handoff;

/// TCP_LISTEN, per include/net/tcp_states.h — shared by /proc/net/tcp{,6}.
const TCP_LISTEN: u8 = 0x0A;
/// TCP_CLOSE, reused by the kernel as the "unconnected" state for a bound UDP socket
/// (UDP has no real LISTEN state) — /proc/net/udp{,6}.
const UDP_UNCONN: u8 = 0x07;
pub(super) const TCP_ESTABLISHED: u8 = 0x01;

const HEADERS: [&str; 4] = ["Proto", "Port", "Process", "Age"];

/// The four kernel socket tables, each tagged with what a row in it means. Read
/// together everywhere here: a service bound on both families is one port to a reader,
/// however many sockets it is to the kernel.
const TABLES: [(&str, &str, &str); 4] = [
    ("TCP", "IPv4", "/proc/net/tcp"),
    ("TCP", "IPv6", "/proc/net/tcp6"),
    ("UDP", "IPv4", "/proc/net/udp"),
    ("UDP", "IPv6", "/proc/net/udp6"),
];

/// One row of a `/proc/net/{tcp,udp}[6]` table.
///
/// These tables are the cheap, always-readable view of the socket world — no netlink,
/// no privileges — which is why three panels read them for three different questions:
/// which ports are listening, which sockets a process holds, and where a login came
/// from. Parsed once into this shape rather than three times into three.
pub(crate) struct SocketRow {
    pub proto: &'static str,
    pub family: &'static str,
    pub local_ip: String,
    pub local_port: u16,
    pub remote_ip: String,
    pub remote_port: u16,
    /// Raw `tcp_states.h` value. UDP reuses the same numbers loosely — see `UDP_UNCONN`.
    pub state: u8,
    pub uid: u32,
    pub inode: u64,
    /// For a listening TCP socket the kernel puts the accept backlog here (how many
    /// established connections are waiting for the process to call `accept`), not a
    /// byte count. For everything else it's bytes queued.
    pub rx_queue: u64,
    pub tx_queue: u64,
}

impl SocketRow {
    pub fn is_listening(&self) -> bool {
        (self.proto == "TCP" && self.state == TCP_LISTEN)
            || (self.proto == "UDP" && self.state == UDP_UNCONN)
    }

    /// `192.168.0.10:443`, bracketing IPv6 so the port stays legible.
    pub fn local(&self) -> String {
        endpoint(&self.local_ip, self.local_port)
    }

    pub fn remote(&self) -> String {
        endpoint(&self.remote_ip, self.remote_port)
    }
}

pub(super) fn endpoint(ip: &str, port: u16) -> String {
    if ip.contains(':') {
        format!("[{ip}]:{port}")
    } else {
        format!("{ip}:{port}")
    }
}

/// Parses one address field, `<hex address>:<hex port>`. The kernel prints each 32-bit
/// word host-endian, so an IPv4 address comes back with its octets reversed and an IPv6
/// address with each of its four words reversed — recovering both means reading the
/// words back little-endian rather than the more obvious big-endian.
fn parse_endpoint(field: &str) -> Option<(String, u16)> {
    let (addr, port) = field.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    let ip = match addr.len() {
        8 => Ipv4Addr::from(u32::from_str_radix(addr, 16).ok()?.to_le_bytes()).to_string(),
        32 => {
            let mut bytes = [0u8; 16];
            for (i, word) in addr.as_bytes().chunks(8).enumerate() {
                let word = u32::from_str_radix(std::str::from_utf8(word).ok()?, 16).ok()?;
                bytes[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
            }
            Ipv6Addr::from(bytes).to_string()
        }
        _ => return None,
    };
    Some((ip, port))
}

/// Parses one socket table by path. The four host tables are the usual callers, and
/// `/proc/<pid>/net/tcp` — the same format, for another network namespace — is the
/// reason this takes a path rather than knowing its own.
pub(super) fn parse_table(proto: &'static str, family: &'static str, path: &str) -> Vec<SocketRow> {
    parse_table_where(proto, family, path, |_| true)
}

/// The same, keeping only the rows whose state `wanted` accepts.
///
/// The filter is applied *before* the addresses are turned into text, which is the whole
/// reason it exists: on a busy machine most of a socket table is listeners and
/// time-waits, and formatting two addresses per row for something about to be discarded
/// is most of the cost of reading the table at all.
pub(super) fn parse_table_where(
    proto: &'static str,
    family: &'static str,
    path: &str,
    wanted: impl Fn(u8) -> bool,
) -> Vec<SocketRow> {
    let Ok(content) = fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let state = u8::from_str_radix(fields.get(3)?, 16).ok()?;
            if !wanted(state) {
                return None;
            }
            let (local_ip, local_port) = parse_endpoint(fields.get(1)?)?;
            let (remote_ip, remote_port) = parse_endpoint(fields.get(2)?)?;
            let (tx_queue, rx_queue) = fields.get(4)?.split_once(':')?;
            Some(SocketRow {
                proto,
                family,
                local_ip,
                local_port,
                remote_ip,
                remote_port,
                state,
                uid: fields.get(7)?.parse().ok()?,
                inode: fields.get(9)?.parse().ok()?,
                rx_queue: u64::from_str_radix(rx_queue, 16).unwrap_or(0),
                tx_queue: u64::from_str_radix(tx_queue, 16).unwrap_or(0),
            })
        })
        .collect()
}

/// Every socket the kernel will show us, across both protocols and both families.
pub(super) fn socket_table() -> Vec<SocketRow> {
    TABLES
        .iter()
        .flat_map(|(proto, family, path)| parse_table(proto, family, path))
        .collect()
}

/// Distinct listening ports per protocol, deduped by port (same service bound on both
/// families shows up once) with the inode of whichever socket was seen first.
fn listening_ports(sockets: &[SocketRow]) -> BTreeMap<(&'static str, u16), u64> {
    let mut ports = BTreeMap::new();
    for row in sockets.iter().filter(|row| row.is_listening()) {
        ports
            .entry((row.proto, row.local_port))
            .or_insert(row.inode);
    }
    ports
}

/// Maps each open socket's inode to the pid holding it open, by scanning every
/// `/proc/<pid>/fd` for `socket:[<inode>]` symlinks. Processes we don't have
/// permission to inspect are silently skipped — their ports just show no owner.
/// Shared with `connections`, which needs the same inode→pid mapping.
pub(super) fn inode_to_pid() -> HashMap<u64, u32> {
    let Ok(proc_dir) = fs::read_dir("/proc") else {
        return HashMap::new();
    };
    let pids: Vec<u32> = proc_dir
        .flatten()
        .filter_map(|entry| entry.file_name().to_string_lossy().parse().ok())
        .collect();

    // Split across cores: this is one `readdir` plus a `readlink` per descriptor for
    // every process on the machine, which on a busy server is tens of thousands of
    // syscalls and the second-largest thing a tick pays for. Every worker builds its own
    // map and they are merged, so nothing is shared while the walking happens.
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(8)
        .min(pids.len().max(1));
    let next = std::sync::atomic::AtomicUsize::new(0);
    let maps: Vec<HashMap<u64, u32>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                let (next, pids) = (&next, &pids);
                scope.spawn(move || {
                    let mut mine = HashMap::new();
                    loop {
                        let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(&pid) = pids.get(index) else { break };
                        for inode in socket_inodes(pid) {
                            mine.entry(inode).or_insert(pid);
                        }
                    }
                    mine
                })
            })
            .collect();
        handles
            .into_iter()
            .filter_map(|handle| handle.join().ok())
            .collect()
    });

    let mut map = HashMap::new();
    for partial in maps {
        for (inode, pid) in partial {
            // First writer wins, as before: a socket shared by a parent and its child
            // belongs to whichever we happened to see first, and both are true.
            map.entry(inode).or_insert(pid);
        }
    }
    map
}

/// Socket inodes held open by one process — the same `/proc/<pid>/fd` walk as above,
/// for the case where the pid is known and the sockets are the question.
pub(super) fn socket_inodes(pid: u32) -> HashSet<u64> {
    let mut inodes = HashSet::new();
    let Ok(fds) = fs::read_dir(format!("/proc/{pid}/fd")) else {
        return inodes;
    };
    for fd in fds.flatten() {
        let Ok(link) = fs::read_link(fd.path()) else {
            continue;
        };
        let Some(inode) = link
            .to_str()
            .and_then(|s| s.strip_prefix("socket:["))
            .and_then(|s| s.strip_suffix(']'))
            .and_then(|s| s.parse().ok())
        else {
            continue;
        };
        inodes.insert(inode);
    }
    inodes
}

/// TCP and UDP listening ports in one list, newest first (owning process' run time
/// ascending — a port whose owner we can't resolve sinks to the bottom, since we have
/// no age to rank it by). Ties keep TCP before UDP.
fn sample_ports(state: &SystemState, limit: Option<usize>) -> Vec<TableRow> {
    let owners = state.inode_to_pid();
    let mut rows: Vec<(&'static str, u16, u32)> = listening_ports(state.sockets())
        .into_iter()
        .map(|((proto, port), inode)| (proto, port, owners.get(&inode).copied().unwrap_or(0)))
        .collect();
    rows.sort_by_key(|&(_, port, pid)| {
        let age = state
            .sys
            .process(Pid::from_u32(pid))
            .map(|p| p.run_time())
            .unwrap_or(u64::MAX);
        (age, port)
    });

    rows.into_iter()
        .take(limit.unwrap_or(usize::MAX))
        .map(|(proto, port, pid)| {
            let (process, age) = describe_owner(state, pid);
            let mut row =
                TableRow::leaf(vec![proto.to_string(), port.to_string(), process, age], pid);
            // Protocol and port, not the pid: the owner is what a port's row is *about*,
            // but it's also the part that can be missing, change, or be shared.
            row.key = port_key(proto, port);
            row
        })
        .collect()
}

fn port_key(proto: &str, port: u16) -> String {
    format!("{proto} {port}")
}

/// Which cards the port can be reached on. A wildcard bind is on all of them, which is
/// the answer people are usually checking for; a bound address names exactly one.
fn bound_interfaces(state: &SystemState, sockets: &[&SocketRow]) -> String {
    if sockets
        .iter()
        .any(|row| row.local_ip == "0.0.0.0" || row.local_ip == "::")
    {
        let all = state.interfaces();
        let names: Vec<&str> = all
            .iter()
            .filter(|interface| interface.is_up() && !interface.addresses.is_empty())
            .map(|interface| interface.name.as_str())
            .collect();
        return format!("todas — {}", names.join(", "));
    }
    let named: Vec<String> = sockets
        .iter()
        .filter_map(|row| {
            let interface = iface::interface_of(&state.networks, &row.local_ip)?;
            Some(format!("{interface} ({})", row.local_ip))
        })
        .collect();
    named.join("  ·  ")
}

/// Who the port is reachable by, read from the addresses it's bound to. The distinction
/// people actually want from this panel: a development server nobody outside can touch
/// versus one the whole network can.
fn reach(sockets: &[&SocketRow]) -> &'static str {
    if sockets
        .iter()
        .any(|row| row.local_ip == "0.0.0.0" || row.local_ip == "::")
    {
        "qualquer endereço desta máquina — alcançável pela rede"
    } else if sockets
        .iter()
        .all(|row| row.local_ip.starts_with("127.") || row.local_ip == "::1")
    {
        "só o loopback — nada de fora desta máquina chega aqui"
    } else {
        "endereços específicos, listados acima"
    }
}

/// Every port currently bound, whatever it's bound to — the set a new listener has to
/// stay out of.
pub(super) fn listening_port_set(sockets: &[SocketRow]) -> HashSet<u16> {
    sockets
        .iter()
        .filter(|row| row.is_listening())
        .map(|row| row.local_port)
        .collect()
}

/// A tunnel that records everything this port receives. It can't listen on the port
/// itself — that one is taken, by definition — so it takes the first free port above
/// it, and the client gets pointed one number to the right.
///
/// `taken` is both what to avoid and where the choice is recorded: several of these are
/// built side by side for one process, and two offers proposing the same local port
/// would mean the second one failing to bind the moment both are accepted.
pub(super) fn record_traffic(proto: &str, port: u16, taken: &mut HashSet<u16>) -> Vec<Handoff> {
    let Some(listen) = (port.saturating_add(1)..=port.saturating_add(64))
        .find(|candidate| !taken.contains(candidate))
    else {
        return Vec::new();
    };
    taken.insert(listen);
    vec![Handoff {
        label: format!("túnel {proto} em {listen} gravando o que iria para {port}"),
        tool: "tunnel",
        params: vec![
            ("proto", proto.to_string()),
            ("listen", format!("127.0.0.1:{listen}")),
            ("target", format!("127.0.0.1:{port}")),
        ],
    }]
}

pub struct PortsMonitor {
    services: Services,
}

impl PortsMonitor {
    pub fn new() -> Self {
        Self {
            services: Services::load(),
        }
    }

    fn build_detail(&self, state: &SystemState, proto: &str, port: u16) -> Option<Detail> {
        let sockets = state.sockets();
        let bound: Vec<&SocketRow> = sockets
            .iter()
            .filter(|row| row.proto == proto && row.local_port == port && row.is_listening())
            .collect();
        // Nothing listening on it any more: the service stopped between two ticks.
        let first = *bound.first()?;
        let is_tcp = proto == "TCP";

        let mut socket = DetailSection::new("Porta");
        socket.push("Protocolo", proto);
        socket.push(
            "Porta",
            match self.services.name(is_tcp, port) {
                Some(service) => format!("{port} ({service})"),
                None => port.to_string(),
            },
        );
        socket.push(
            "Escutando em",
            bound
                .iter()
                .map(|row| format!("{} [{}]", row.local(), row.family))
                .collect::<Vec<_>>()
                .join("  ·  "),
        );
        socket.push("Alcance", reach(&bound));
        socket.push("Interfaces", bound_interfaces(state, &bound));
        socket.push(
            "Dono do socket",
            match user_name(first.uid) {
                Some(name) => format!("{name} (uid {})", first.uid),
                None => format!("uid {}", first.uid),
            },
        );
        if is_tcp {
            let waiting: u64 = bound.iter().map(|row| row.rx_queue).sum();
            socket.push(
                "Fila de accept",
                format!("{waiting} conexão(ões) esperando o processo aceitar"),
            );
        }
        socket.push(
            "Inode",
            bound
                .iter()
                .map(|row| row.inode.to_string())
                .collect::<Vec<_>>()
                .join(" · "),
        );

        let pid = state.inode_to_pid().get(&first.inode).copied().unwrap_or(0);
        let owner = owner_section(
            state,
            pid,
            "não identificado — socket de outro usuário, ou processo já encerrado",
        );

        let mut sections = vec![socket, owner];
        if is_tcp {
            sections.push(clients(sockets, port));
        }

        Some(Detail {
            title: format!("{proto} porta {port}"),
            gone_note: "não está mais em escuta",
            sections,
            rates: None,
            handoffs: record_traffic(proto, port, &mut listening_port_set(sockets)),
            handoff_title: "Gravar o tráfego desta porta",
        })
    }
}

/// Who is connected to this port right now, grouped by peer address — a listening port
/// with forty connections from one host and one from another is a different situation
/// from forty separate clients, and the row above can't tell them apart.
fn clients(sockets: &[SocketRow], port: u16) -> DetailSection {
    let mut section = DetailSection::new("Conexões");
    let established: Vec<&SocketRow> = sockets
        .iter()
        .filter(|row| row.proto == "TCP" && row.local_port == port && row.state == TCP_ESTABLISHED)
        .collect();
    section.push("Estabelecidas", established.len().to_string());

    let mut peers: BTreeMap<&str, usize> = BTreeMap::new();
    for row in &established {
        *peers.entry(row.remote_ip.as_str()).or_default() += 1;
    }
    let mut ranked: Vec<(&str, usize)> = peers.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    for (ip, count) in ranked.iter().take(MAX_CLIENTS) {
        section.push(ip, format!("{count} conexão(ões)"));
    }
    if ranked.len() > MAX_CLIENTS {
        section.push(
            "E mais",
            format!("{} outro(s) endereço(s)", ranked.len() - MAX_CLIENTS),
        );
    }
    section
}

/// Peers listed individually before the tail is summarised. Enough to recognise who's
/// talking to a service, short of turning the section into the Connections panel.
const MAX_CLIENTS: usize = 10;

impl Default for PortsMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl TableMonitor for PortsMonitor {
    fn id(&self) -> &'static str {
        "ports"
    }

    fn title(&self) -> &'static str {
        "Ports"
    }

    fn headers(&self) -> &'static [&'static str] {
        &HEADERS
    }

    /// A port is a number and a process is a command: the two things anybody would
    /// point at in this table.
    fn mark_kinds(&self) -> &'static [mark::MarkKind] {
        &[
            mark::MarkKind {
                name: "porta",
                column: 1,
                numeric: true,
                help: "o número da porta, exato",
            },
            mark::MarkKind {
                name: "processo",
                column: 2,
                numeric: false,
                help: "trecho do comando, ou uma expressão regular",
            },
        ]
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

    fn detail(&mut self, state: &SystemState, row: &TableRow) -> Option<Detail> {
        let (proto, port) = row.key.split_once(' ')?;
        self.build_detail(state, proto, port.parse().ok()?)
    }

    /// Del here kills the *process*, not the port — a distinction worth making before
    /// the fact rather than after, since the row is about the port.
    fn danger(&self, state: &SystemState, row: &TableRow) -> Option<Danger> {
        let port = row.key.clone();
        kill_danger(
            state,
            row,
            "matar quem escuta",
            "Matar o processo que escuta esta porta",
            vec![format!(
                "A porta {port} deixa de responder na hora, e qualquer cliente ligado a ela cai junto."
            )],
        )
    }

    fn has_detail(&self) -> bool {
        true
    }
}
