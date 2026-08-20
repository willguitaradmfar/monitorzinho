use std::net::{Ipv4Addr, Ipv6Addr};

use sysinfo::Pid;

use super::ports::inode_to_pid;
use super::process::command_of;
use super::{SystemState, TableMonitor, TableRow};
use crate::format;

const HEADERS: [&str; 4] = ["Proto", "Process", "Connection", "Traffic"];

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

/// Byte offsets of `tcp_info.tcpi_bytes_acked`/`tcpi_bytes_received` (both `u64`),
/// confirmed against this system's `<linux/tcp.h>` via `offsetof`. These fields have
/// only ever been appended to, never reordered, since Linux 4.2, so the offsets are
/// stable across kernel versions — bounds-checked below regardless, so an ancient
/// kernel just reports zero traffic instead of misreading unrelated bytes.
const TCPI_BYTES_ACKED_OFFSET: usize = 120;
const TCPI_BYTES_RECEIVED_OFFSET: usize = 128;

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

unsafe extern "C" {
    fn socket(domain: i32, ty: i32, protocol: i32) -> i32;
    fn bind(fd: i32, addr: *const SockaddrNl, len: u32) -> i32;
    fn connect(fd: i32, addr: *const SockaddrNl, len: u32) -> i32;
    fn send(fd: i32, buf: *const u8, len: usize, flags: i32) -> isize;
    fn recv(fd: i32, buf: *mut u8, len: usize, flags: i32) -> isize;
    fn setsockopt(fd: i32, level: i32, optname: i32, optval: *const Timeval, optlen: u32) -> i32;
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
                &timeout,
                size_of::<Timeval>() as u32,
            );
        }
        let addr = SockaddrNl {
            nl_family: AF_NETLINK as u16,
            nl_pad: 0,
            nl_pid: 0,
            nl_groups: 0,
        };
        if unsafe { bind(fd, &addr, size_of::<SockaddrNl>() as u32) } < 0 {
            return None;
        }
        if unsafe { connect(fd, &addr, size_of::<SockaddrNl>() as u32) } < 0 {
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

struct RawConn {
    protocol: u8,
    local: String,
    remote: String,
    traffic: u64,
    inode: u32,
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
            let inode = u32::from_ne_bytes(msg[68..72].try_into().unwrap());

            let mut traffic = 0u64;
            let mut apos = 72; // rtattrs start right after the fixed inet_diag_msg
            while apos + 4 <= msg.len() {
                let rta_len = u16::from_ne_bytes(msg[apos..apos + 2].try_into().unwrap()) as usize;
                let rta_type = u16::from_ne_bytes(msg[apos + 2..apos + 4].try_into().unwrap());
                if rta_len < 4 || apos + rta_len > msg.len() {
                    break;
                }
                if rta_type == INET_DIAG_INFO {
                    let payload = &msg[apos + 4..apos + rta_len];
                    if payload.len() >= TCPI_BYTES_RECEIVED_OFFSET + 8 {
                        let acked = u64::from_ne_bytes(
                            payload[TCPI_BYTES_ACKED_OFFSET..TCPI_BYTES_ACKED_OFFSET + 8]
                                .try_into()
                                .unwrap(),
                        );
                        let received = u64::from_ne_bytes(
                            payload[TCPI_BYTES_RECEIVED_OFFSET..TCPI_BYTES_RECEIVED_OFFSET + 8]
                                .try_into()
                                .unwrap(),
                        );
                        traffic = acked + received;
                    }
                }
                apos += align4(rta_len);
            }

            out.push(RawConn {
                protocol,
                local: format!("{}:{}", local_ip, local_port),
                remote: format!("{}:{}", remote_ip, remote_port),
                traffic,
                inode,
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

/// Connections in either direction — we're the server (someone connected to a port we
/// have listening) or the client (we connected out) — excluding plain listening/
/// unconnected sockets, which the Ports panel already covers. Ranked by total traffic
/// (bytes acked + bytes received) since the connection opened, heaviest first; UDP
/// carries no such counter in the kernel, so its connections report zero and sort last.
fn sample_connections(state: &SystemState, limit: Option<usize>) -> Vec<TableRow> {
    let mut raw = query(IPPROTO_TCP, TCP_OPEN_STATES);
    raw.extend(query(IPPROTO_UDP, 1u32 << TCP_ESTABLISHED));
    raw.sort_by_key(|c| std::cmp::Reverse(c.traffic));

    let owners = inode_to_pid();
    raw.into_iter()
        .take(limit.unwrap_or(usize::MAX))
        .map(|c| {
            let pid = owners.get(&(c.inode as u64)).copied().unwrap_or(0);
            let process = state
                .sys
                .process(Pid::from_u32(pid))
                .map(command_of)
                .unwrap_or_else(|| "?".to_string());
            let proto = if c.protocol == IPPROTO_TCP {
                "TCP"
            } else {
                "UDP"
            };
            TableRow::leaf(
                vec![
                    proto.to_string(),
                    process,
                    format!("{} → {}", c.local, c.remote),
                    format::human_bytes(c.traffic as f64),
                ],
                pid,
            )
        })
        .collect()
}

pub struct ConnectionsMonitor;

impl TableMonitor for ConnectionsMonitor {
    fn title(&self) -> &'static str {
        "Connections"
    }

    fn headers(&self) -> &'static [&'static str] {
        &HEADERS
    }

    fn sample(&mut self, state: &SystemState, limit: Option<usize>) -> Vec<TableRow> {
        sample_connections(state, limit)
    }
}
