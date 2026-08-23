//! Turning numbers into names: the remote IP of a connection into a hostname, and its
//! port into a service name. Both are "nice to have" — every lookup degrades to
//! showing the raw number rather than failing or blocking.

use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::fs;
use std::net::IpAddr;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

const AF_INET: u16 = 2;
const AF_INET6: u16 = 10;
/// `NI_NAMEREQD`: fail outright when the address has no PTR record instead of quietly
/// returning its numeric form — otherwise "no name" is indistinguishable from a name
/// that happens to look exactly like the IP we passed in.
const NI_NAMEREQD: i32 = 8;
/// `NI_MAXHOST`. A PTR record can't exceed 255 bytes, and glibc errors out with
/// `EAI_OVERFLOW` rather than truncating if the buffer is short.
const HOST_BUF: usize = 1025;

// `getnameinfo` (not a hand-rolled DNS query) so the lookup goes through NSS exactly
// like every other tool on the machine: /etc/hosts, mDNS, and the system resolver's
// configured servers all count, which matters on a box with containers and a VPN
// where half the interesting names never come from public DNS.
unsafe extern "C" {
    fn getnameinfo(
        addr: *const c_void,
        addrlen: u32,
        host: *mut u8,
        hostlen: u32,
        serv: *mut u8,
        servlen: u32,
        flags: i32,
    ) -> i32;
}

#[repr(C)]
struct SockaddrIn {
    sin_family: u16,
    sin_port: u16,
    sin_addr: [u8; 4],
    sin_zero: [u8; 8],
}

#[repr(C)]
struct SockaddrIn6 {
    sin6_family: u16,
    sin6_port: u16,
    sin6_flowinfo: u32,
    sin6_addr: [u8; 16],
    sin6_scope_id: u32,
}

/// Blocking PTR lookup for one address — always called on a throwaway thread, never on
/// the UI thread: a resolver that's slow or unreachable can sit here for seconds.
/// A reverse lookup right here, blocking until it answers or the resolver gives up.
///
/// The `Resolver` above exists because the UI's tick loop must never wait on DNS. A
/// tool running on its own threads has no such constraint, and wants the answer in the
/// line it's about to write rather than two ticks later.
pub fn reverse_now(ip: &str) -> Option<String> {
    ptr_lookup(ip.parse().ok()?)
}

fn ptr_lookup(ip: IpAddr) -> Option<String> {
    let mut host = [0u8; HOST_BUF];
    let rc = match ip {
        IpAddr::V4(v4) => {
            let addr = SockaddrIn {
                sin_family: AF_INET,
                sin_port: 0,
                sin_addr: v4.octets(),
                sin_zero: [0; 8],
            };
            unsafe {
                getnameinfo(
                    (&addr as *const SockaddrIn).cast(),
                    size_of::<SockaddrIn>() as u32,
                    host.as_mut_ptr(),
                    HOST_BUF as u32,
                    std::ptr::null_mut(),
                    0,
                    NI_NAMEREQD,
                )
            }
        }
        IpAddr::V6(v6) => {
            let addr = SockaddrIn6 {
                sin6_family: AF_INET6,
                sin6_port: 0,
                sin6_flowinfo: 0,
                sin6_addr: v6.octets(),
                sin6_scope_id: 0,
            };
            unsafe {
                getnameinfo(
                    (&addr as *const SockaddrIn6).cast(),
                    size_of::<SockaddrIn6>() as u32,
                    host.as_mut_ptr(),
                    HOST_BUF as u32,
                    std::ptr::null_mut(),
                    0,
                    NI_NAMEREQD,
                )
            }
        }
    };
    if rc != 0 {
        return None;
    }
    let len = host.iter().position(|&b| b == 0).unwrap_or(host.len());
    String::from_utf8(host[..len].to_vec())
        .ok()
        .filter(|s| !s.is_empty())
}

/// What `Resolver::reverse` knows about an address right now.
pub enum Lookup {
    /// A lookup is in flight on a background thread — ask again next tick.
    Pending,
    /// The address resolves to this hostname.
    Name(String),
    /// Resolved, but there's no PTR record (or the lookup failed) — don't ask again.
    Unnamed,
}

/// Non-blocking reverse DNS: the first `reverse` call for an address kicks off a
/// background lookup and reports `Pending`, and every later call reads the cached
/// answer. Results are cached for the lifetime of the process — a PTR record changing
/// mid-session isn't worth re-querying every open connection over.
pub struct Resolver {
    cache: HashMap<String, Option<String>>,
    /// Addresses with a lookup thread already running, so a detail view redrawn every
    /// two seconds doesn't spawn a new thread for the same address every tick.
    in_flight: HashSet<String>,
    tx: Sender<(String, Option<String>)>,
    rx: Receiver<(String, Option<String>)>,
}

impl Resolver {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Self {
            cache: HashMap::new(),
            in_flight: HashSet::new(),
            tx,
            rx,
        }
    }

    pub fn reverse(&mut self, ip: &str) -> Lookup {
        while let Ok((addr, name)) = self.rx.try_recv() {
            self.in_flight.remove(&addr);
            self.cache.insert(addr, name);
        }
        if let Some(entry) = self.cache.get(ip) {
            return match entry {
                Some(name) => Lookup::Name(name.clone()),
                None => Lookup::Unnamed,
            };
        }
        // An address we can't even parse can't be looked up — cache it as unnamed so
        // we don't retry it forever.
        let Ok(parsed) = ip.parse::<IpAddr>() else {
            self.cache.insert(ip.to_string(), None);
            return Lookup::Unnamed;
        };
        if self.in_flight.insert(ip.to_string()) {
            let tx = self.tx.clone();
            let addr = ip.to_string();
            // Detached: if it's still blocked in the resolver when the user closes the
            // view (or the app), the send just fails against a dropped receiver.
            thread::spawn(move || {
                let name = ptr_lookup(parsed);
                let _ = tx.send((addr, name));
            });
        }
        Lookup::Pending
    }
}

impl Default for Resolver {
    fn default() -> Self {
        Self::new()
    }
}

/// IANA service names by port, split by protocol, parsed from `/etc/services` rather
/// than hardcoded — the file is already on every Linux box, covers far more ports than
/// a table worth maintaining here, and picks up whatever local entries the machine has
/// added. Empty if it's missing, in which case ports just show as bare numbers.
#[derive(Default)]
pub struct Services {
    tcp: HashMap<u16, String>,
    udp: HashMap<u16, String>,
}

impl Services {
    pub fn load() -> Self {
        let mut services = Self::default();
        let Ok(content) = fs::read_to_string("/etc/services") else {
            return services;
        };
        for line in content.lines() {
            let line = line.split('#').next().unwrap_or("");
            let mut fields = line.split_whitespace();
            let (Some(name), Some(port_proto)) = (fields.next(), fields.next()) else {
                continue;
            };
            let Some((port, proto)) = port_proto.split_once('/') else {
                continue;
            };
            let Ok(port) = port.parse::<u16>() else {
                continue;
            };
            let map = match proto {
                "tcp" => &mut services.tcp,
                "udp" => &mut services.udp,
                // sctp/ddp entries exist but nothing here ever asks about them.
                _ => continue,
            };
            // First entry wins: /etc/services lists the canonical name before aliases.
            map.entry(port).or_insert_with(|| name.to_string());
        }
        services
    }

    /// e.g. `(true, 443)` → `https`. `None` for ephemeral and unregistered ports, which
    /// is most local ports on an outgoing connection.
    pub fn name(&self, tcp: bool, port: u16) -> Option<&str> {
        let map = if tcp { &self.tcp } else { &self.udp };
        map.get(&port).map(String::as_str)
    }
}

/// Username for a uid, from `/etc/passwd`. `None` when the uid only exists in a remote
/// directory (LDAP/SSSD) that we'd need NSS to reach — the bare number is shown then.
/// Re-read per call rather than cached: it's a couple of kilobytes, read at most once a
/// tick by the detail view, and a session that outlives a `useradd` shouldn't go stale.
pub fn user_name(uid: u32) -> Option<String> {
    let content = fs::read_to_string("/etc/passwd").ok()?;
    content.lines().find_map(|line| {
        let mut fields = line.split(':');
        let name = fields.next()?;
        let _password = fields.next()?;
        let entry_uid: u32 = fields.next()?.parse().ok()?;
        (entry_uid == uid).then(|| name.to_string())
    })
}
