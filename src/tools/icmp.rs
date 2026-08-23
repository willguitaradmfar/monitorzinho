//! ICMP without privileges: reaching a host, and finding out what is between here and
//! there.
//!
//! `SOCK_DGRAM` + `IPPROTO_ICMP` is the mode `ping` uses when it isn't setuid — the
//! kernel owns the identifier and only delivers replies belonging to this socket, so no
//! capability is needed and no other process's traffic is visible. It is gated by
//! `net.ipv4.ping_group_range`, which on a desktop includes everybody and on a hardened
//! server may include nobody; where it excludes us, opening the socket fails and the
//! caller says so instead of pretending.
//!
//! Tracing a route needs the ICMP *errors* provoked by a packet that ran out of hops
//! rather than an echo, and those never arrive on a socket's ordinary receive queue: the
//! kernel puts them on its error queue, collected with `recvmsg(MSG_ERRQUEUE)`, carrying
//! both the error and the address of the router that sent it.
//!
//! Which is why the tracer sends **UDP**, not ICMP. An ICMP echo socket is a privilege
//! away from existing at all — `ping_group_range` is empty on both machines this was
//! developed against, including for root — while a UDP socket with `IP_RECVERR` needs
//! nothing from anybody and collects the same ICMP answers. Routers reply "time
//! exceeded" to the datagram exactly as they would to an echo, and the destination
//! replies "port unreachable", which is how the tracer knows it arrived. It is what
//! `tracepath` does, and it works everywhere.

use std::ffi::{c_int, c_void};
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use super::poll;

const AF_INET: c_int = 2;
const SOCK_DGRAM: c_int = 2;
const IPPROTO_ICMP: c_int = 1;
const SOL_SOCKET: c_int = 1;
const SO_RCVTIMEO: c_int = 20;
/// `SOL_IP`, and the two options that make a traceroute: how far a packet may travel,
/// and asking to be told when it doesn't arrive.
const SOL_IP: c_int = 0;
const IP_TTL: c_int = 2;
const IP_RECVERR: c_int = 11;
/// `MSG_ERRQUEUE` — read from the error queue rather than from the data one.
const MSG_ERRQUEUE: c_int = 0x2000;

const ICMP_ECHO: u8 = 8;
const ICMP_ECHOREPLY: u8 = 0;
pub const ICMP_UNREACHABLE: u8 = 3;
pub const ICMP_TIME_EXCEEDED: u8 = 11;
/// The unreachable code the destination itself sends when nothing is listening on the
/// port a traceroute aimed at — which is exactly what "we arrived" looks like.
const PORT_UNREACHABLE: u8 = 3;

/// `SO_EE_ORIGIN_ICMP` — the error came from an ICMP message, which is the only origin
/// that carries the router's address.
const ORIGIN_ICMP: u8 = 2;

unsafe extern "C" {
    fn socket(domain: c_int, kind: c_int, protocol: c_int) -> c_int;
    fn close(fd: c_int) -> c_int;
    fn setsockopt(fd: c_int, level: c_int, name: c_int, value: *const c_void, len: u32) -> c_int;
    fn sendto(
        fd: c_int,
        buf: *const u8,
        len: usize,
        flags: c_int,
        addr: *const SockaddrIn,
        addrlen: u32,
    ) -> isize;
    fn recv(fd: c_int, buf: *mut u8, len: usize, flags: c_int) -> isize;
    fn recvmsg(fd: c_int, msg: *mut MsgHdr, flags: c_int) -> isize;
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

#[repr(C)]
struct IoVec {
    base: *mut u8,
    len: usize,
}

#[repr(C)]
struct MsgHdr {
    name: *mut c_void,
    namelen: u32,
    _pad1: u32,
    iov: *mut IoVec,
    iovlen: usize,
    control: *mut c_void,
    controllen: usize,
    flags: c_int,
    _pad2: u32,
}

/// `struct sock_extended_err`, as `<linux/errqueue.h>` declares it: the error the kernel
/// is reporting, followed in the control message by the address of whoever sent it.
#[repr(C)]
#[derive(Clone, Copy)]
struct ExtendedErr {
    errno: u32,
    origin: u8,
    kind: u8,
    code: u8,
    _pad: u8,
    info: u32,
    data: u32,
}

/// What came back from one probe.
pub enum Hop {
    /// The target itself answered — the route ends here.
    Reply { from: Ipv4Addr, rtt: Duration },
    /// A router said the packet ran out of hops. This is a step on the way.
    Exceeded { from: Ipv4Addr, rtt: Duration },
    /// Somebody said it cannot be delivered, and why.
    Unreachable {
        from: Ipv4Addr,
        rtt: Duration,
        code: u8,
    },
    /// Nothing came back before the timeout. Common and not conclusive: plenty of
    /// routers are configured never to answer.
    Silent,
}

/// An unprivileged ICMP socket.
pub struct Pinger {
    fd: c_int,
}

impl Pinger {
    /// `None` when the kernel won't give an unprivileged process an ICMP socket — see
    /// `net.ipv4.ping_group_range`.
    pub fn new() -> Option<Self> {
        let fd = unsafe { socket(AF_INET, SOCK_DGRAM, IPPROTO_ICMP) };
        if fd < 0 {
            return None;
        }
        let pinger = Self { fd };
        // Asking for the errors is what separates this from a ping: without it the
        // kernel drops the "time exceeded" that every hop of a traceroute is made of.
        pinger.set_int(SOL_IP, IP_RECVERR, 1);
        Some(pinger)
    }

    fn set_int(&self, level: c_int, name: c_int, value: c_int) -> bool {
        set_int(self.fd, level, name, value)
    }

    fn set_timeout(&self, timeout: Duration) -> bool {
        let timeval = Timeval {
            tv_sec: timeout.as_secs() as i64,
            tv_usec: timeout.subsec_micros() as i64,
        };
        unsafe {
            setsockopt(
                self.fd,
                SOL_SOCKET,
                SO_RCVTIMEO,
                (&raw const timeval).cast(),
                size_of::<Timeval>() as u32,
            ) == 0
        }
    }

    /// Whether `address` answers an echo request. What the network sweep asks.
    pub fn reaches(&self, address: Ipv4Addr, timeout: Duration) -> bool {
        // A full 255 hops: this is a question about the host, not about the path.
        self.set_int(SOL_IP, IP_TTL, 255);
        matches!(
            self.probe(address, 1, timeout),
            Hop::Reply { .. } | Hop::Unreachable { .. }
        )
    }

    fn probe(&self, address: Ipv4Addr, sequence: u16, timeout: Duration) -> Hop {
        if !self.set_timeout(timeout) {
            return Hop::Silent;
        }
        // Type, code, checksum, identifier, sequence. The kernel rewrites the identifier
        // and fixes the checksum on a datagram ICMP socket, but a correct one costs
        // nothing and keeps the packet valid if that ever changes.
        let mut packet = [0u8; 16];
        packet[0] = ICMP_ECHO;
        packet[6..8].copy_from_slice(&sequence.to_be_bytes());
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
            return Hop::Silent;
        }

        let started = Instant::now();
        let deadline = started + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Hop::Silent;
            }
            // POLLERR is how the error queue announces itself; POLLIN is the echo reply
            // coming back the ordinary way. Waiting on both is what lets one call
            // answer "we arrived" and "we got as far as this router" alike.
            if !poll::readable_or_error(self.fd, left.as_millis() as i32) {
                return Hop::Silent;
            }
            if let Some(hop) = self.take_error(started) {
                return hop;
            }
            let mut buf = [0u8; 256];
            let received = unsafe { recv(self.fd, buf.as_mut_ptr(), buf.len(), 0) };
            if received > 0 && buf[0] == ICMP_ECHOREPLY {
                return Hop::Reply {
                    from: address,
                    rtt: started.elapsed(),
                };
            }
            // Nothing useful this time round — a stray reply, or the error queue was
            // announced before it had anything. Try again until the deadline.
        }
    }

    /// One message off the error queue, if there is one.
    fn take_error(&self, started: Instant) -> Option<Hop> {
        read_error_queue(self.fd, started)
    }
}

/// A path tracer: a plain UDP socket that asks to be told about the ICMP errors its
/// datagrams provoke.
pub struct Tracer {
    fd: c_int,
}

impl Tracer {
    /// Never fails for want of privilege — that is the entire reason it is UDP.
    pub fn new() -> Option<Self> {
        let fd = unsafe { socket(AF_INET, SOCK_DGRAM, 0) };
        if fd < 0 {
            return None;
        }
        let tracer = Self { fd };
        // Without this the kernel swallows every "time exceeded", and a traceroute
        // becomes a column of stars.
        if !set_int(fd, SOL_IP, IP_RECVERR, 1) {
            return None;
        }
        Some(tracer)
    }

    /// Sends one datagram that may travel `ttl` hops, and reports who complained.
    ///
    /// `port` should be one nothing is listening on — the destination's "port
    /// unreachable" is what says we arrived. The classic range starting at 33434 is
    /// used for the same reason every traceroute uses it: it is reserved by convention
    /// and nothing answers there.
    pub fn hop(&self, address: Ipv4Addr, ttl: u8, port: u16, timeout: Duration) -> Hop {
        if !set_int(self.fd, SOL_IP, IP_TTL, ttl as c_int) {
            return Hop::Silent;
        }
        let destination = SockaddrIn {
            sin_family: AF_INET as u16,
            sin_port: port.to_be(),
            sin_addr: u32::from(address).to_be(),
            sin_zero: [0; 8],
        };
        // A short payload, so the datagram is small enough never to be fragmented and
        // large enough to be a datagram.
        let payload = b"monitorzinho";
        let sent = unsafe {
            sendto(
                self.fd,
                payload.as_ptr(),
                payload.len(),
                0,
                &raw const destination,
                size_of::<SockaddrIn>() as u32,
            )
        };
        if sent < 0 {
            return Hop::Silent;
        }

        let started = Instant::now();
        let deadline = started + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Hop::Silent;
            }
            if !poll::readable_or_error(self.fd, left.as_millis() as i32) {
                return Hop::Silent;
            }
            if let Some(hop) = read_error_queue(self.fd, started) {
                return hop;
            }
        }
    }
}

impl Drop for Tracer {
    fn drop(&mut self) {
        unsafe { close(self.fd) };
    }
}

unsafe impl Send for Tracer {}
unsafe impl Sync for Tracer {}

fn set_int(fd: c_int, level: c_int, name: c_int, value: c_int) -> bool {
    unsafe {
        setsockopt(
            fd,
            level,
            name,
            (&raw const value).cast(),
            size_of::<c_int>() as u32,
        ) == 0
    }
}

/// One message off a socket's error queue: what went wrong, and which router said so.
///
/// The address of the sender comes after the `sock_extended_err` in the same control
/// message — `SO_EE_OFFENDER` — as a `sockaddr_in`, which is the only place a traceroute
/// can learn who answered.
fn read_error_queue(fd: c_int, started: Instant) -> Option<Hop> {
    let mut payload = [0u8; 256];
    let mut control = [0u8; 512];
    let mut name = SockaddrIn {
        sin_family: 0,
        sin_port: 0,
        sin_addr: 0,
        sin_zero: [0; 8],
    };
    let mut iov = IoVec {
        base: payload.as_mut_ptr(),
        len: payload.len(),
    };
    let mut header = MsgHdr {
        name: (&raw mut name).cast(),
        namelen: size_of::<SockaddrIn>() as u32,
        _pad1: 0,
        iov: &raw mut iov,
        iovlen: 1,
        control: control.as_mut_ptr().cast(),
        controllen: control.len(),
        flags: 0,
        _pad2: 0,
    };
    let received = unsafe { recvmsg(fd, &raw mut header, MSG_ERRQUEUE) };
    if received < 0 {
        return None;
    }

    // Walk the control messages looking for the extended error. Each is a `cmsghdr` —
    // length, level, type — followed by its data, padded to eight.
    let used = header.controllen.min(control.len());
    let mut offset = 0usize;
    while offset + 16 <= used {
        let len = usize::from_ne_bytes(control[offset..offset + 8].try_into().ok()?);
        let level = i32::from_ne_bytes(control[offset + 8..offset + 12].try_into().ok()?);
        if len < 16 || offset + len > used {
            break;
        }
        let data = &control[offset + 16..offset + len];
        if level == SOL_IP && data.len() >= size_of::<ExtendedErr>() {
            let origin = data[4];
            let kind = data[5];
            let code = data[6];
            if origin != ORIGIN_ICMP {
                break;
            }
            // `sockaddr_in`'s address sits four bytes into it, after family and port.
            let from = data
                .get(size_of::<ExtendedErr>() + 4..size_of::<ExtendedErr>() + 8)
                .and_then(|bytes| Some(Ipv4Addr::from(u32::from_be_bytes(bytes.try_into().ok()?))))
                .unwrap_or(Ipv4Addr::UNSPECIFIED);
            let rtt = started.elapsed();
            return Some(match (kind, code) {
                (ICMP_TIME_EXCEEDED, _) => Hop::Exceeded { from, rtt },
                // Port unreachable from the destination is how a UDP trace arrives:
                // nothing was listening, which only the destination itself can say.
                (ICMP_UNREACHABLE, PORT_UNREACHABLE) => Hop::Reply { from, rtt },
                (ICMP_UNREACHABLE, code) => Hop::Unreachable { from, rtt, code },
                (ICMP_ECHOREPLY, _) => Hop::Reply { from, rtt },
                _ => Hop::Silent,
            });
        }
        offset += len.div_ceil(8) * 8;
    }
    None
}

impl Drop for Pinger {
    fn drop(&mut self) {
        unsafe { close(self.fd) };
    }
}

// The socket is only ever used through `&self`, and every call is a self-contained
// send/receive on a datagram socket the kernel demultiplexes per-socket.
unsafe impl Send for Pinger {}
unsafe impl Sync for Pinger {}

/// Why a destination is unreachable, in the words of RFC 792.
pub fn unreachable_reason(code: u8) -> &'static str {
    match code {
        0 => "rede inalcançável",
        1 => "host inalcançável",
        2 => "protocolo inalcançável",
        3 => "porta fechada",
        4 => "precisa fragmentar mas o pacote proíbe",
        9 | 10 => "proibido administrativamente",
        13 => "bloqueado por filtro",
        _ => "inalcançável",
    }
}

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
