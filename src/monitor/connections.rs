use std::collections::HashMap;
use std::ffi::c_void;
use std::fs;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::time::Instant;

use sysinfo::Pid;

use super::ports::inode_to_pid;
use super::process::{command_of, describe_owner};
use super::resolve::{Lookup, Resolver, Services, user_name};
use super::{Detail, DetailSection, SystemState, TableMonitor, TableRow};
use crate::format;
use crate::tools::Handoff;

const HEADERS: [&str; 6] = ["Proto", "Process", "Connection", "Age", "Traffic", "Rate"];

// --- raw netlink INET_DIAG plumbing -------------------------------------------------
//
// Per-connection byte counters aren't in /proc/net/tcp — the kernel only exposes them
// through the same `SOCK_DIAG_BY_FAMILY` netlink request `ss -i` uses (a `tcp_info`
// attached to each socket's dump entry). Hand-rolled here, with no netlink crate: it's
// four small request/response exchanges a tick, not worth a dependency for.

const AF_INET: u8 = 2;
const AF_INET6: u8 = 10;
const AF_NETLINK: i32 = 16;
const SOCK_RAW: i32 = 3;
const NETLINK_SOCK_DIAG: i32 = 4;
const SOL_SOCKET: i32 = 1;
const SO_RCVTIMEO: i32 = 20;

const NLM_F_REQUEST: u16 = 0x01;
const NLM_F_DUMP: u16 = 0x300; // NLM_F_ROOT | NLM_F_MATCH
const NLMSG_ERROR: u16 = 2;
const NLMSG_DONE: u16 = 3;
const SOCK_DIAG_BY_FAMILY: u16 = 20;

const IPPROTO_TCP: u8 = 6;
const IPPROTO_UDP: u8 = 17;
/// `enum tcp_state` values reused by UDP sockets too (a UDP socket that's been
/// `connect()`-ed reports ESTABLISHED; an unconnected bound one reports CLOSE).
const TCP_ESTABLISHED: u32 = 1;
const TCP_SYN_SENT: u32 = 2;
const TCP_SYN_RECV: u32 = 3;
const TCP_CLOSE_WAIT: u32 = 8;
/// States that read as a genuinely open connection — excludes not just LISTEN
/// (already covered by the Ports panel) but the lingering post-close states
/// (TIME_WAIT, CLOSE, LAST_ACK, CLOSING): a busy machine can have hundreds of those at
/// any moment, almost always with no owning process left to attribute them to, and
/// they'd otherwise drown out the connections actually moving traffic.
const TCP_OPEN_STATES: u32 =
    (1 << TCP_ESTABLISHED) | (1 << TCP_SYN_SENT) | (1 << TCP_SYN_RECV) | (1 << TCP_CLOSE_WAIT);

const INET_DIAG_INFO: u16 = 2;
/// `idiag_ext` bit requesting a `tcp_info` (bytes_acked/received, among others)
/// attached to each response — `1 << (INET_DIAG_INFO - 1)`.
const INET_DIAG_REQ_EXT_INFO: u8 = 1 << (INET_DIAG_INFO - 1);

/// Byte offsets of every `tcp_info` field we read, confirmed against this system's
/// `<linux/tcp.h>` via `offsetof`. The struct has only ever been appended to, never
/// reordered, since Linux 4.2, so these are stable across kernel versions — and every
/// read is bounds-checked, so a kernel whose `tcp_info` stops short simply reports
/// zero for the fields it doesn't carry instead of misreading unrelated bytes.
mod tcpi {
    pub const RETRANSMITS: usize = 2; // u8
    pub const PROBES: usize = 3; // u8
    pub const RTO: usize = 8;
    pub const ATO: usize = 12;
    pub const SND_MSS: usize = 16;
    pub const RCV_MSS: usize = 20;
    pub const LOST: usize = 32;
    pub const RETRANS: usize = 36;
    pub const LAST_DATA_SENT: usize = 44;
    pub const LAST_DATA_RECV: usize = 52;
    pub const PMTU: usize = 60;
    pub const RTT: usize = 68;
    pub const RTTVAR: usize = 72;
    pub const SND_CWND: usize = 80;
    pub const ADVMSS: usize = 84;
    pub const TOTAL_RETRANS: usize = 100;
    pub const PACING_RATE: usize = 104; // u64
    pub const BYTES_ACKED: usize = 120; // u64
    pub const BYTES_RECEIVED: usize = 128; // u64
    pub const SEGS_OUT: usize = 136;
    pub const SEGS_IN: usize = 140;
    pub const NOTSENT_BYTES: usize = 144;
    pub const MIN_RTT: usize = 148;
    pub const DATA_SEGS_IN: usize = 152;
    pub const DATA_SEGS_OUT: usize = 156;
    pub const DELIVERY_RATE: usize = 160; // u64
}

#[repr(C)]
struct SockaddrNl {
    nl_family: u16,
    nl_pad: u16,
    nl_pid: u32,
    nl_groups: u32,
}

#[repr(C)]
struct Timeval {
    tv_sec: i64,
    tv_usec: i64,
}

// `bind`/`connect`/`setsockopt` take `*const c_void` here (not the specific struct
// types below) to match their real POSIX signatures — `summary.rs` declares the same
// symbols for its own IPv4 socket, and the two declarations must agree exactly or
// rustc's `clashing_extern_declarations` lint (rightly) complains that one crate is
// lying about a shared symbol's type.
unsafe extern "C" {
    fn socket(domain: i32, ty: i32, protocol: i32) -> i32;
    fn bind(fd: i32, addr: *const c_void, len: u32) -> i32;
    fn connect(fd: i32, addr: *const c_void, len: u32) -> i32;
    fn send(fd: i32, buf: *const u8, len: usize, flags: i32) -> isize;
    fn recv(fd: i32, buf: *mut u8, len: usize, flags: i32) -> isize;
    fn setsockopt(fd: i32, level: i32, optname: i32, optval: *const c_void, optlen: u32) -> i32;
    fn close(fd: i32) -> i32;
}

/// A netlink socket, bound and "connected" to the kernel (pid 0) so plain `send`/`recv`
/// work instead of `sendto`/`recvfrom`. Closed on drop; a bounded receive timeout means
/// a wedged dump can never hang the UI thread — worst case a tick shows a stale/empty
/// snapshot instead.
struct NlSocket(i32);

impl NlSocket {
    fn open() -> Option<Self> {
        let fd = unsafe { socket(AF_NETLINK, SOCK_RAW, NETLINK_SOCK_DIAG) };
        if fd < 0 {
            return None;
        }
        let sock = Self(fd);
        let timeout = Timeval {
            tv_sec: 1,
            tv_usec: 0,
        };
        unsafe {
            setsockopt(
                fd,
                SOL_SOCKET,
                SO_RCVTIMEO,
                (&timeout as *const Timeval).cast(),
                size_of::<Timeval>() as u32,
            );
        }
        let addr = SockaddrNl {
            nl_family: AF_NETLINK as u16,
            nl_pad: 0,
            nl_pid: 0,
            nl_groups: 0,
        };
        let addr_ptr = (&addr as *const SockaddrNl).cast();
        if unsafe { bind(fd, addr_ptr, size_of::<SockaddrNl>() as u32) } < 0 {
            return None;
        }
        if unsafe { connect(fd, addr_ptr, size_of::<SockaddrNl>() as u32) } < 0 {
            return None;
        }
        Some(sock)
    }

    fn send_all(&self, buf: &[u8]) -> bool {
        let n = unsafe { send(self.0, buf.as_ptr(), buf.len(), 0) };
        n == buf.len() as isize
    }

    fn recv_into<'a>(&self, buf: &'a mut [u8]) -> Option<&'a [u8]> {
        let n = unsafe { recv(self.0, buf.as_mut_ptr(), buf.len(), 0) };
        if n <= 0 {
            None
        } else {
            Some(&buf[..n as usize])
        }
    }
}

impl Drop for NlSocket {
    fn drop(&mut self) {
        unsafe {
            close(self.0);
        }
    }
}

/// Builds a `SOCK_DIAG_BY_FAMILY` dump request for every `protocol` socket in `family`
/// whose state is set in `states` (a `1 << tcp_state` bitmask).
///
/// Note `inet_diag_req_v2`'s field order: `idiag_states` comes *before* `id`, not after
/// — easy to get backwards since the response struct (`inet_diag_msg`) puts its `id`
/// right after the 4-byte header instead.
fn build_request(family: u8, protocol: u8, states: u32) -> [u8; 72] {
    let mut buf = [0u8; 72];
    buf[0..4].copy_from_slice(&72u32.to_ne_bytes()); // nlmsg_len
    buf[4..6].copy_from_slice(&SOCK_DIAG_BY_FAMILY.to_ne_bytes()); // nlmsg_type
    buf[6..8].copy_from_slice(&(NLM_F_REQUEST | NLM_F_DUMP).to_ne_bytes()); // nlmsg_flags
    // nlmsg_seq, nlmsg_pid: 0 — left zeroed.
    buf[16] = family; // inet_diag_req_v2.sdiag_family
    buf[17] = protocol; // .sdiag_protocol
    buf[18] = INET_DIAG_REQ_EXT_INFO; // .idiag_ext
    buf[20..24].copy_from_slice(&states.to_ne_bytes()); // .idiag_states
    // .id (sockid): sport/dport/src/dst/if all zero (match everything) except cookie,
    // which must be INET_DIAG_NOCOOKIE (all-ones) for a dump query.
    buf[64..68].copy_from_slice(&0xFFFF_FFFFu32.to_ne_bytes());
    buf[68..72].copy_from_slice(&0xFFFF_FFFFu32.to_ne_bytes());
    buf
}

fn align4(n: usize) -> usize {
    (n + 3) & !3
}

/// Address bytes are copied as-is (not reinterpreted as an integer) — for IPv4 the
/// address is just its 4 octets in order in the first 4 bytes; for IPv6 it's the full
/// 16 bytes in order. Ports, unlike addresses, are genuine big-endian integers.
fn format_ip(family: u8, raw: &[u8]) -> String {
    if family == AF_INET {
        Ipv4Addr::new(raw[0], raw[1], raw[2], raw[3]).to_string()
    } else {
        let bytes: [u8; 16] = raw[..16].try_into().unwrap_or([0; 16]);
        Ipv6Addr::from(bytes).to_string()
    }
}

/// The subset of `tcp_info` worth showing — cumulative counters and the current state
/// of the sending machinery. All-zero for UDP, which carries no `tcp_info` at all, and
/// for any field a shorter (older-kernel) payload didn't reach.
#[derive(Default, Clone, Copy)]
struct TcpInfo {
    /// Consecutive retransmits of the segment currently in flight, and keepalive/zero-
    /// window probes sent — both non-zero only while a connection is actively stuck.
    retransmits: u8,
    probes: u8,
    /// Retransmission and delayed-ACK timeouts, microseconds.
    rto: u32,
    ato: u32,
    /// Maximum segment size we send with / the peer sends with / we advertised.
    snd_mss: u32,
    rcv_mss: u32,
    advmss: u32,
    /// Segments the sender currently believes are lost, and retransmits outstanding
    /// right now — as opposed to `total_retrans` over the connection's whole life.
    lost: u32,
    retrans: u32,
    total_retrans: u32,
    /// Milliseconds since we last sent / last received data. The pair reads as "which
    /// side has gone quiet, and for how long".
    last_data_sent: u32,
    last_data_recv: u32,
    /// Path MTU as discovered for this connection.
    pmtu: u32,
    /// Smoothed round-trip time and its variance, microseconds, plus the lowest RTT
    /// ever measured — the floor the path is capable of, useful next to `rtt` as a
    /// "how much of this latency is queueing" reading.
    rtt: u32,
    rttvar: u32,
    min_rtt: u32,
    /// Congestion window, in segments — multiply by `snd_mss` for the bytes we're
    /// allowed to have in flight.
    snd_cwnd: u32,
    /// Bytes queued in the socket that haven't been put on the wire yet.
    notsent_bytes: u32,
    /// What the kernel is pacing us at, and what it measured we actually achieved —
    /// both bytes/s, and both far more honest about a connection's ceiling than a
    /// throughput sample taken over one tick.
    pacing_rate: u64,
    delivery_rate: u64,
    /// Cumulative bytes we've sent-and-had-acked / received on this socket since it
    /// opened.
    bytes_acked: u64,
    bytes_received: u64,
    /// Total segments in each direction, and how many of them carried payload — the
    /// gap between the two is pure ACK/handshake overhead.
    segs_out: u32,
    segs_in: u32,
    data_segs_in: u32,
    data_segs_out: u32,
}

fn u8_at(buf: &[u8], offset: usize) -> u8 {
    buf.get(offset).copied().unwrap_or(0)
}

fn u32_at(buf: &[u8], offset: usize) -> u32 {
    buf.get(offset..offset + 4)
        .and_then(|b| b.try_into().ok())
        .map(u32::from_ne_bytes)
        .unwrap_or(0)
}

fn u64_at(buf: &[u8], offset: usize) -> u64 {
    buf.get(offset..offset + 8)
        .and_then(|b| b.try_into().ok())
        .map(u64::from_ne_bytes)
        .unwrap_or(0)
}

fn parse_tcp_info(payload: &[u8]) -> TcpInfo {
    TcpInfo {
        retransmits: u8_at(payload, tcpi::RETRANSMITS),
        probes: u8_at(payload, tcpi::PROBES),
        rto: u32_at(payload, tcpi::RTO),
        ato: u32_at(payload, tcpi::ATO),
        snd_mss: u32_at(payload, tcpi::SND_MSS),
        rcv_mss: u32_at(payload, tcpi::RCV_MSS),
        advmss: u32_at(payload, tcpi::ADVMSS),
        lost: u32_at(payload, tcpi::LOST),
        retrans: u32_at(payload, tcpi::RETRANS),
        total_retrans: u32_at(payload, tcpi::TOTAL_RETRANS),
        last_data_sent: u32_at(payload, tcpi::LAST_DATA_SENT),
        last_data_recv: u32_at(payload, tcpi::LAST_DATA_RECV),
        pmtu: u32_at(payload, tcpi::PMTU),
        rtt: u32_at(payload, tcpi::RTT),
        rttvar: u32_at(payload, tcpi::RTTVAR),
        min_rtt: u32_at(payload, tcpi::MIN_RTT),
        snd_cwnd: u32_at(payload, tcpi::SND_CWND),
        notsent_bytes: u32_at(payload, tcpi::NOTSENT_BYTES),
        pacing_rate: u64_at(payload, tcpi::PACING_RATE),
        delivery_rate: u64_at(payload, tcpi::DELIVERY_RATE),
        bytes_acked: u64_at(payload, tcpi::BYTES_ACKED),
        bytes_received: u64_at(payload, tcpi::BYTES_RECEIVED),
        segs_out: u32_at(payload, tcpi::SEGS_OUT),
        segs_in: u32_at(payload, tcpi::SEGS_IN),
        data_segs_in: u32_at(payload, tcpi::DATA_SEGS_IN),
        data_segs_out: u32_at(payload, tcpi::DATA_SEGS_OUT),
    }
}

struct RawConn {
    protocol: u8,
    family: u8,
    /// `enum tcp_state` of the socket, straight from `inet_diag_msg.idiag_state`.
    state: u8,
    /// Which kernel timer (if any) is armed on this socket, and how long until it
    /// fires — see `timer_name`. Zero means no timer, i.e. nothing pending.
    timer: u8,
    expires_ms: u32,
    local_ip: String,
    local_port: u16,
    remote_ip: String,
    remote_port: u16,
    /// Bytes sitting in the receive/send socket buffers right now — data the
    /// application hasn't read yet, and data we haven't managed to send yet. Unlike
    /// `TcpInfo`, these come from `inet_diag_msg` itself, so they're just as valid for
    /// a UDP socket.
    rqueue: u32,
    wqueue: u32,
    uid: u32,
    inode: u32,
    info: TcpInfo,
}

impl RawConn {
    fn local(&self) -> String {
        endpoint(&self.local_ip, self.local_port)
    }

    fn remote(&self) -> String {
        endpoint(&self.remote_ip, self.remote_port)
    }

    /// Lifetime bytes in both directions. Always 0 for UDP.
    fn total_bytes(&self) -> u64 {
        self.info.bytes_acked + self.info.bytes_received
    }
}

/// `ip:port`, bracketing an IPv6 address so its own colons stay visually separate from
/// the one before the port.
/// The tunnels this connection describes.
///
/// A connection already names both ends and the protocol, which is everything the
/// tunnel tool asks for — so the offer is to relay to one of those ends, listening on
/// the same port locally. Point the client at localhost instead of at the far side and
/// the same conversation goes through a process that writes it down.
///
/// Both ends are offered because which one is the server depends on which way the
/// connection was opened, and reading that off the port numbers would be guessing. They
/// are labelled as what they are, and identical ends collapse to one.
fn tunnels(c: &RawConn, proto: &str) -> Vec<Handoff> {
    let mut offers: Vec<Handoff> = Vec::new();
    for (side, ip, port) in [
        ("o outro lado", &c.remote_ip, c.remote_port),
        ("este lado", &c.local_ip, c.local_port),
    ] {
        if port == 0 || ip.is_empty() {
            continue;
        }
        let target = endpoint(ip, port);
        if offers.iter().any(|offer| {
            offer
                .params
                .iter()
                .any(|(key, value)| *key == "target" && value == &target)
        }) {
            continue;
        }
        offers.push(Handoff {
            label: format!("túnel {proto} para {target} ({side})"),
            tool: "tunnel",
            params: vec![
                ("proto", proto.to_string()),
                // The same port locally, so a client's configuration usually needs only
                // its host changed to 127.0.0.1.
                ("listen", format!("127.0.0.1:{port}")),
                ("target", target),
            ],
        });
    }
    offers
}

fn endpoint(ip: &str, port: u16) -> String {
    if ip.contains(':') {
        format!("[{ip}]:{port}")
    } else {
        format!("{ip}:{port}")
    }
}

/// `enum tcp_state` names, indexed by `idiag_state`. UDP sockets reuse the same enum,
/// so a connected UDP socket reads as ESTABLISHED here too.
fn state_name(state: u8) -> &'static str {
    match state {
        1 => "ESTABLISHED",
        2 => "SYN_SENT",
        3 => "SYN_RECV",
        4 => "FIN_WAIT1",
        5 => "FIN_WAIT2",
        6 => "TIME_WAIT",
        7 => "CLOSE",
        8 => "CLOSE_WAIT",
        9 => "LAST_ACK",
        10 => "LISTEN",
        11 => "CLOSING",
        _ => "?",
    }
}

/// `idiag_timer` values, per `inet_diag.h` — which clock is currently running against
/// the socket. Anything but "none" on a connection that looks idle is the interesting
/// case: a retransmit timer means we're waiting on an ACK that isn't coming.
fn timer_name(timer: u8) -> Option<&'static str> {
    match timer {
        1 => Some("retransmissão"),
        2 => Some("keepalive"),
        3 => Some("timewait"),
        4 => Some("janela zero"),
        _ => None,
    }
}

/// Parses every complete netlink message in one `recv()`'s worth of `data` (a dump
/// reply is usually spread across several `recv()` calls, each containing several
/// messages back to back). Returns `true` once the dump is done (or errored out) — the
/// caller stops polling.
fn parse_dump(data: &[u8], protocol: u8, out: &mut Vec<RawConn>) -> bool {
    let mut pos = 0;
    while pos + 16 <= data.len() {
        let nlmsg_len = u32::from_ne_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        if nlmsg_len < 16 || pos + nlmsg_len > data.len() {
            break;
        }
        let nlmsg_type = u16::from_ne_bytes(data[pos + 4..pos + 6].try_into().unwrap());
        if nlmsg_type == NLMSG_DONE || nlmsg_type == NLMSG_ERROR {
            return true;
        }
        if nlmsg_type == SOCK_DIAG_BY_FAMILY && nlmsg_len >= 16 + 72 {
            let msg = &data[pos + 16..pos + nlmsg_len];
            let family = msg[0];
            let sockid = &msg[4..52];
            let local_port = u16::from_be_bytes(sockid[0..2].try_into().unwrap());
            let remote_port = u16::from_be_bytes(sockid[2..4].try_into().unwrap());
            let local_ip = format_ip(family, &sockid[4..20]);
            let remote_ip = format_ip(family, &sockid[20..36]);
            // The tail of the fixed inet_diag_msg, after the 48-byte sockid:
            // idiag_expires, idiag_rqueue, idiag_wqueue, idiag_uid, idiag_inode.
            let expires_ms = u32::from_ne_bytes(msg[52..56].try_into().unwrap());
            let rqueue = u32::from_ne_bytes(msg[56..60].try_into().unwrap());
            let wqueue = u32::from_ne_bytes(msg[60..64].try_into().unwrap());
            let uid = u32::from_ne_bytes(msg[64..68].try_into().unwrap());
            let inode = u32::from_ne_bytes(msg[68..72].try_into().unwrap());

            let mut info = TcpInfo::default();
            let mut apos = 72; // rtattrs start right after the fixed inet_diag_msg
            while apos + 4 <= msg.len() {
                let rta_len = u16::from_ne_bytes(msg[apos..apos + 2].try_into().unwrap()) as usize;
                let rta_type = u16::from_ne_bytes(msg[apos + 2..apos + 4].try_into().unwrap());
                if rta_len < 4 || apos + rta_len > msg.len() {
                    break;
                }
                if rta_type == INET_DIAG_INFO {
                    info = parse_tcp_info(&msg[apos + 4..apos + rta_len]);
                }
                apos += align4(rta_len);
            }

            out.push(RawConn {
                protocol,
                family,
                state: msg[1], // inet_diag_msg.idiag_state
                timer: msg[2], // .idiag_timer
                expires_ms,
                local_ip,
                local_port,
                remote_ip,
                remote_port,
                rqueue,
                wqueue,
                uid,
                inode,
                info,
            });
        }
        pos += align4(nlmsg_len);
    }
    false
}

/// Every `protocol` connection (IPv4 and IPv6) whose state is in `states`. Empty on any
/// failure (no `NETLINK_SOCK_DIAG` support, permission denied, ...) — same best-effort
/// fallback as `ports::inode_to_pid`.
fn query(protocol: u8, states: u32) -> Vec<RawConn> {
    let mut out = Vec::new();
    let Some(sock) = NlSocket::open() else {
        return out;
    };
    let mut buf = [0u8; 8192];
    for family in [AF_INET, AF_INET6] {
        if !sock.send_all(&build_request(family, protocol, states)) {
            continue;
        }
        while let Some(data) = sock.recv_into(&mut buf) {
            if parse_dump(data, protocol, &mut out) {
                break;
            }
        }
    }
    out
}

/// Identifies one connection across successive dumps (its 4-tuple doesn't change for
/// its lifetime), so tick-to-tick byte deltas — and a fullscreened row — can be matched
/// back up to the right entry.
fn conn_key(protocol: u8, local: &str, remote: &str) -> String {
    format!("{protocol}|{local}|{remote}")
}

fn format_rate(down_per_sec: f64, up_per_sec: f64) -> String {
    format!(
        "↓{} ↑{}",
        format::human_bytes_per_sec(down_per_sec),
        format::human_bytes_per_sec(up_per_sec)
    )
}

/// Connections in either direction — we're the server (someone connected to a port we
/// have listening) or the client (we connected out) — excluding plain listening/
/// unconnected sockets, which the Ports panel already covers. UDP carries no byte
/// counter in the kernel, so its connections always report zero traffic/rate.
pub struct ConnectionsMonitor {
    /// Last-seen (bytes_acked, bytes_received, when) per connection, keyed by
    /// `conn_key` — diffed against the next sample to turn cumulative counters into a
    /// throughput. Rebuilt every sample so a closed connection's entry just drops out
    /// instead of accumulating forever.
    history: HashMap<String, (u64, u64, Instant)>,
    /// Reverse DNS for the detail view only — the table itself never resolves, since
    /// a hundred rows would mean a hundred lookups a tick for names nobody's reading.
    resolver: Resolver,
    /// `/etc/services`, read once at startup: it doesn't change while we're running,
    /// and re-reading it per detail redraw would be pointless I/O.
    services: Services,
}

impl ConnectionsMonitor {
    pub fn new() -> Self {
        Self {
            history: HashMap::new(),
            resolver: Resolver::new(),
            services: Services::load(),
        }
    }

    /// Fetches a fresh netlink dump and turns it into `(RawConn, download/s, upload/s)`
    /// triples keyed by `conn_key`, updating `self.history` for the next call in the
    /// same motion. Shared by `sample` (fresh rows, possibly capped) and
    /// `refresh_values` (update existing rows in place, matched by their stored `key`).
    fn refresh_snapshot(&mut self) -> HashMap<String, (RawConn, f64, f64)> {
        let mut raw = query(IPPROTO_TCP, TCP_OPEN_STATES);
        raw.extend(query(IPPROTO_UDP, 1u32 << TCP_ESTABLISHED));

        let now = Instant::now();
        let mut next_history = HashMap::with_capacity(raw.len());
        let mut snapshot = HashMap::with_capacity(raw.len());
        for c in raw {
            let key = conn_key(c.protocol, &c.local(), &c.remote());
            let (down, up) = match self.history.get(&key) {
                Some(&(prev_acked, prev_received, prev_time)) => {
                    let dt = now.duration_since(prev_time).as_secs_f64().max(0.001);
                    (
                        c.info.bytes_received.saturating_sub(prev_received) as f64 / dt,
                        c.info.bytes_acked.saturating_sub(prev_acked) as f64 / dt,
                    )
                }
                // First time we've seen it — no prior sample to diff against yet.
                None => (0.0, 0.0),
            };
            next_history.insert(
                key.clone(),
                (c.info.bytes_acked, c.info.bytes_received, now),
            );
            snapshot.insert(key, (c, down, up));
        }
        self.history = next_history;
        snapshot
    }

    /// Assembles the fullscreen detail for one connection: who it is, who owns it,
    /// what it has moved, and how the path underneath is behaving. Everything here
    /// comes from the netlink dump we already take every tick plus `/proc` — no
    /// packet capture, so it can say who is talking and how well, but not what about.
    fn build_detail(
        &mut self,
        state: &SystemState,
        c: &RawConn,
        down: f64,
        up: f64,
        pid: u32,
    ) -> Detail {
        let is_tcp = c.protocol == IPPROTO_TCP;
        let proto = if is_tcp { "TCP" } else { "UDP" };
        // Resolved first: it's the one field needing `&mut self`, and everything below
        // borrows `self` immutably to look up service names.
        let host = match self.resolver.reverse(&c.remote_ip) {
            Lookup::Name(name) => name,
            Lookup::Pending => "resolvendo…".to_string(),
            Lookup::Unnamed => String::new(),
        };

        let mut conn = DetailSection::new("Conexão");
        conn.push("Estado", state_name(c.state));
        conn.push(
            "Protocolo",
            format!(
                "{proto} · {}",
                if c.family == AF_INET { "IPv4" } else { "IPv6" }
            ),
        );
        conn.push("Local", self.with_service(c, c.local_port, c.local()));
        conn.push("Remoto", self.with_service(c, c.remote_port, c.remote()));
        conn.push("Host remoto", host);
        if let Some(timer) = timer_name(c.timer) {
            conn.push(
                "Timer armado",
                format!("{timer} · dispara em {}", millis_since(c.expires_ms)),
            );
        }
        conn.push(
            "Dono do socket",
            match user_name(c.uid) {
                Some(name) => format!("{name} (uid {})", c.uid),
                None => format!("uid {}", c.uid),
            },
        );

        let mut owner = DetailSection::new("Processo");
        match state.sys.process(Pid::from_u32(pid)) {
            Some(p) => {
                owner.push("PID", pid.to_string());
                owner.push("Ativo há", format::human_duration(p.run_time()));
                // `exe` isn't in the refresh kind the Processes tab asks sysinfo for —
                // reading the one link here is far cheaper than making every process
                // pay for it on every tick.
                if let Ok(exe) = fs::read_link(format!("/proc/{pid}/exe")) {
                    owner.push("Executável", exe.to_string_lossy());
                }
                if let Some(cwd) = p.cwd() {
                    owner.push("Diretório", cwd.to_string_lossy());
                }
                owner.push("Linha de comando", command_of(p));
            }
            // Same best-effort limit as the Ports panel: resolving a socket to a pid
            // means reading every /proc/<pid>/fd, and other users' are off limits.
            None => owner.push(
                "PID",
                "não identificado — socket de outro usuário, ou processo já encerrado",
            ),
        }

        let mut traffic = DetailSection::new("Tráfego");
        if is_tcp {
            traffic.push("Taxa atual", format_rate(down, up));
            traffic.push(
                "Recebido",
                format::human_bytes(c.info.bytes_received as f64),
            );
            traffic.push("Enviado", format::human_bytes(c.info.bytes_acked as f64));
            traffic.push(
                "Segmentos",
                format!(
                    "{} recebidos · {} enviados",
                    c.info.segs_in, c.info.segs_out
                ),
            );
            traffic.push(
                "Destes, com dados",
                format!(
                    "{} recebidos · {} enviados",
                    c.info.data_segs_in, c.info.data_segs_out
                ),
            );
            traffic.push("Último recebimento", millis_since(c.info.last_data_recv));
            traffic.push("Último envio", millis_since(c.info.last_data_sent));
        } else {
            traffic.push(
                "Contadores",
                "o kernel não conta bytes por socket UDP — só as filas abaixo",
            );
        }
        // From inet_diag_msg rather than tcp_info, so these are just as real for UDP:
        // what the application hasn't read yet, and what we haven't put on the wire.
        traffic.push(
            "Fila de recepção",
            format!(
                "{} esperando o processo ler",
                format::human_bytes(c.rqueue as f64)
            ),
        );
        traffic.push(
            "Fila de envio",
            format!("{} esperando a rede", format::human_bytes(c.wqueue as f64)),
        );
        if is_tcp && c.info.notsent_bytes > 0 {
            traffic.push(
                "Sem sair do socket",
                format::human_bytes(c.info.notsent_bytes as f64),
            );
        }

        let mut sections = vec![conn, owner, traffic];
        if is_tcp {
            sections.push(path_section(&c.info));
        }

        Detail {
            title: format!("{proto} {} → {}", c.local(), c.remote()),
            sections,
            // A flat zero line would be worse than no sparkline at all, and UDP never
            // reports bytes.
            rates: is_tcp.then_some((down, up)),
            handoffs: tunnels(c, proto),
        }
    }

    /// `1.2.3.4:443 (https)` when the port is a registered service, plain otherwise —
    /// which is the normal case for the ephemeral local port of an outgoing connection.
    fn with_service(&self, c: &RawConn, port: u16, endpoint: String) -> String {
        match self.services.name(c.protocol == IPPROTO_TCP, port) {
            Some(svc) => format!("{endpoint} ({svc})"),
            None => endpoint,
        }
    }
}

impl Default for ConnectionsMonitor {
    fn default() -> Self {
        Self::new()
    }
}

/// A microsecond duration in whichever unit reads best — `tcp_info` reports RTT/RTO/ATO
/// in microseconds, and they span tens of µs on loopback to whole seconds on a stalled
/// path. Empty for zero, so an inapplicable timer is dropped from its section rather
/// than shown as a meaningless "0 µs".
fn micros(us: u32) -> String {
    if us == 0 {
        String::new()
    } else if us < 1_000 {
        format!("{us} µs")
    } else if us < 1_000_000 {
        format!("{:.1} ms", us as f64 / 1_000.0)
    } else {
        format!("{:.2} s", us as f64 / 1_000_000.0)
    }
}

/// A millisecond "time since" counter — `tcp_info`'s idle clocks, which run from
/// milliseconds on a busy connection to hours on one nobody's touched.
fn millis_since(ms: u32) -> String {
    if ms < 1_000 {
        format!("{ms} ms")
    } else {
        format::human_duration(ms as u64 / 1_000)
    }
}

/// Builds a `TableRow` for one connection, resolving its owning process (name + age)
/// by pid and formatting its traffic total and current download/upload rate.
fn build_row(
    state: &SystemState,
    owners: &HashMap<u64, u32>,
    c: &RawConn,
    down: f64,
    up: f64,
) -> TableRow {
    let pid = owners.get(&(c.inode as u64)).copied().unwrap_or(0);
    let (process, age) = describe_owner(state, pid);
    let proto = if c.protocol == IPPROTO_TCP {
        "TCP"
    } else {
        "UDP"
    };
    let mut row = TableRow::leaf(
        vec![
            proto.to_string(),
            process,
            format!("{} → {}", c.local(), c.remote()),
            age,
            format::human_bytes(c.total_bytes() as f64),
            format_rate(down, up),
        ],
        pid,
    );
    row.key = conn_key(c.protocol, &c.local(), &c.remote());
    row
}

/// How the path underneath the connection is behaving: latency, loss, and how much the
/// sender is currently allowed (and able) to push. TCP only — none of it exists for a
/// UDP socket.
fn path_section(info: &TcpInfo) -> DetailSection {
    let mut path = DetailSection::new("Caminho");
    if info.rtt > 0 {
        path.push(
            "RTT",
            format!("{} ± {}", micros(info.rtt), micros(info.rttvar)),
        );
    }
    // Well above `rtt` means the difference is queueing somewhere on the path, not
    // distance — the single most useful comparison in this section.
    path.push("RTT mínimo", micros(info.min_rtt));
    path.push(
        "Retransmissões",
        format!(
            "{} no total · {} pendentes · {} tidos como perdidos",
            info.total_retrans, info.retrans, info.lost
        ),
    );
    if info.retransmits > 0 || info.probes > 0 {
        path.push(
            "Insistindo agora",
            format!(
                "{} retransmissões seguidas · {} sondagens",
                info.retransmits, info.probes
            ),
        );
    }
    if info.snd_cwnd > 0 {
        path.push(
            "Janela de congestão",
            format!(
                "{} segmentos (~{} em voo)",
                info.snd_cwnd,
                format::human_bytes((info.snd_cwnd as u64 * info.snd_mss as u64) as f64)
            ),
        );
    }
    path.push(
        "MSS",
        format!(
            "{} envio · {} recepção · {} anunciado",
            info.snd_mss, info.rcv_mss, info.advmss
        ),
    );
    if info.pmtu > 0 {
        path.push("PMTU", format!("{} bytes", info.pmtu));
    }
    path.push(
        "Ritmo permitido",
        format::human_bytes_per_sec(info.pacing_rate as f64),
    );
    // What the connection actually achieved when it last had something to send —
    // unlike the tick-to-tick rate above, an idle connection keeps reporting the last
    // real measurement instead of falling to zero.
    path.push(
        "Entrega medida",
        format::human_bytes_per_sec(info.delivery_rate as f64),
    );
    path.push("Timeout de retransmissão", micros(info.rto));
    path.push("ACK atrasado", micros(info.ato));
    path
}

impl TableMonitor for ConnectionsMonitor {
    fn title(&self) -> &'static str {
        "Connections"
    }

    fn headers(&self) -> &'static [&'static str] {
        &HEADERS
    }

    fn sample(&mut self, state: &SystemState, limit: Option<usize>) -> Vec<TableRow> {
        let snapshot = self.refresh_snapshot();
        let mut entries: Vec<(RawConn, f64, f64)> = snapshot.into_values().collect();
        // Ranked by current combined throughput, not lifetime total — an SSH session
        // that moved 50 MB an hour ago and is doing nothing right now shouldn't outrank
        // a transfer actively saturating the link at 140 KB/s. A connection's very
        // first tick has no prior sample to diff, so it reports (and sorts as) 0/0
        // until the next one.
        entries.sort_by(|(_, ad, au), (_, bd, bu)| (bd + bu).total_cmp(&(ad + au)));

        let owners = inode_to_pid();
        entries
            .into_iter()
            .take(limit.unwrap_or(usize::MAX))
            .map(|(c, down, up)| build_row(state, &owners, &c, down, up))
            .collect()
    }

    fn refresh_values(&mut self, state: &SystemState, rows: &mut [TableRow]) {
        let snapshot = self.refresh_snapshot();
        let owners = inode_to_pid();
        for row in rows.iter_mut() {
            // A connection that's since closed just keeps showing its last known
            // values — same "stale beats missing" tradeoff as a dead pid elsewhere.
            let Some((c, down, up)) = snapshot.get(&row.key) else {
                continue;
            };
            row.pid = owners.get(&(c.inode as u64)).copied().unwrap_or(0);
            let (process, age) = describe_owner(state, row.pid);
            if let Some(cell) = row.cells.get_mut(1) {
                *cell = process;
            }
            if let Some(cell) = row.cells.get_mut(3) {
                *cell = age;
            }
            if let Some(cell) = row.cells.get_mut(4) {
                *cell = format::human_bytes(c.total_bytes() as f64);
            }
            if let Some(cell) = row.cells.get_mut(5) {
                *cell = format_rate(*down, *up);
            }
        }
    }

    fn has_detail(&self) -> bool {
        true
    }

    fn detail(&mut self, state: &SystemState, row: &TableRow) -> Option<Detail> {
        let snapshot = self.refresh_snapshot();
        // Gone from the dump means the connection closed — the caller keeps the last
        // detail it got and flags it, rather than the view blanking out.
        let (c, down, up) = snapshot.get(&row.key)?;
        let (down, up) = (*down, *up);
        let pid = inode_to_pid().get(&(c.inode as u64)).copied().unwrap_or(0);
        Some(self.build_detail(state, c, down, up, pid))
    }
}
