//! Finding out what the devices on a network *are*, by listening to what they say about
//! themselves.
//!
//! The sweep next door proves a host exists — it answered, or refused, or is in the
//! neighbour table. What it can't say is that the silent thing at `.47` is a printer, or
//! that `.12` is the television. That information is on the wire all day: mDNS and SSDP
//! are how printers, TVs, speakers, phones and NAS boxes announce themselves, and every
//! one of them answers a question anybody is allowed to ask.
//!
//! Two protocols, one shape: send one small query to a multicast group, then listen for
//! a few seconds. Neither needs privilege and neither needs binding the well-known port
//! — the questions ask for unicast replies, which also means this never fights with the
//! `avahi-daemon` that already owns 5353 on most desktops.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use super::dns::wire;

/// The multicast address and port mDNS lives on.
const MDNS: (Ipv4Addr, u16) = (Ipv4Addr::new(224, 0, 0, 251), 5353);
/// SSDP's, which is what UPnP devices answer on.
const SSDP: (Ipv4Addr, u16) = (Ipv4Addr::new(239, 255, 255, 250), 1900);

/// The DNS-SD question that means "what services exist here" — every responder answers
/// it with the service types it offers.
const SERVICE_ENUMERATION: &str = "_services._dns-sd._udp.local";
/// A handful of service types worth asking about by name, because their answers carry
/// the device's own name in the instance label.
const SERVICES: &[&str] = &[
    "_http._tcp.local",
    "_ipp._tcp.local",
    "_printer._tcp.local",
    "_googlecast._tcp.local",
    "_airplay._tcp.local",
    "_raop._tcp.local",
    "_smb._tcp.local",
    "_ssh._tcp.local",
    "_workstation._tcp.local",
];

const PTR: u16 = 12;
const SRV: u16 = 33;
const TXT: u16 = 16;
const A: u16 = 1;

/// What one device told us about itself.
#[derive(Default, Clone)]
pub(super) struct Announcement {
    /// The friendliest name it gave: a DNS-SD instance label where there is one, its
    /// host name otherwise.
    pub name: String,
    /// What it says it is — a service type, or the `SERVER` line of an SSDP reply.
    pub kind: String,
    /// Which protocol carried it, for the "how" column.
    pub via: &'static str,
}

/// Asks both protocols and listens for `window`, returning what answered, by address.
pub(super) fn discover(window: Duration) -> HashMap<Ipv4Addr, Announcement> {
    let mut found: HashMap<Ipv4Addr, Announcement> = HashMap::new();
    // Half the window each: they are independent questions to different groups, and
    // splitting keeps the whole discovery inside the time the caller allowed.
    let half = window / 2;
    for (address, announcement) in mdns(half) {
        found.entry(address).or_insert(announcement);
    }
    for (address, announcement) in ssdp(half) {
        found
            .entry(address)
            .and_modify(|known| {
                // mDNS names are friendlier; SSDP's `SERVER` says what it runs. Keep
                // both halves rather than letting one overwrite the other.
                if known.kind.is_empty() {
                    known.kind = announcement.kind.clone();
                }
            })
            .or_insert(announcement);
    }
    found
}

/// One socket, bound to an ephemeral port, with a receive timeout — everything here
/// speaks by asking and then listening.
fn socket(timeout: Duration) -> Option<UdpSocket> {
    let socket = UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    socket
        .set_read_timeout(Some(Duration::from_millis(300)))
        .ok()?;
    // Multicast has to be allowed to leave the machine, and one hop is all these
    // protocols are ever meant to travel.
    socket.set_multicast_ttl_v4(1).ok()?;
    let _ = timeout;
    Some(socket)
}

/// The mDNS half, in two rounds, because that is how DNS-SD works.
///
/// The first round asks what service *types* exist here — the answer to that question is
/// a list of types, not of devices, and treating it as names is how a sweep ends up
/// calling six different machines `_services._dns-sd._udp`. The second round asks each
/// type who offers it, and *those* answers carry the instance label the owner typed:
/// "Impressora do escritório", "Sala de estar".
fn mdns(window: Duration) -> HashMap<Ipv4Addr, Announcement> {
    let mut found = HashMap::new();
    let Some(socket) = socket(window) else {
        return found;
    };
    let half = window / 2;

    // Round one: what is offered here.
    let mut types: Vec<String> = SERVICES.iter().map(|s| s.to_string()).collect();
    let discovered = ask(
        &socket,
        &[SERVICE_ENUMERATION.to_string()],
        half,
        &mut found,
    );
    for name in discovered {
        if is_service_type(&name) && !types.contains(&name) {
            types.push(name);
        }
    }

    // Round two: who offers it.
    ask(&socket, &types, half, &mut found);
    found
}

/// Sends one question per name, then reads every answer for `window`, folding what it
/// learns into `found`. Returns the record data seen, which round one uses as its list
/// of service types.
fn ask(
    socket: &UdpSocket,
    questions: &[String],
    window: Duration,
    found: &mut HashMap<Ipv4Addr, Announcement>,
) -> Vec<String> {
    let destination = SocketAddr::from(MDNS);
    for (id, question) in questions.iter().enumerate() {
        // The unicast-response bit: replies come straight back to our ephemeral port,
        // so nothing has to bind 5353 and fight the daemon that already has it.
        let Ok(mut packet) = wire::build_query(id as u16 + 1, question, PTR, false, false) else {
            continue;
        };
        set_unicast_response(&mut packet);
        let _ = socket.send_to(&packet, destination);
    }

    let mut seen = Vec::new();
    let deadline = Instant::now() + window;
    let mut buffer = [0u8; 4096];
    while Instant::now() < deadline {
        let Ok((read, from)) = socket.recv_from(&mut buffer) else {
            continue;
        };
        let SocketAddr::V4(from) = from else { continue };
        let Ok(response) = wire::parse_message(&buffer[..read]) else {
            continue;
        };

        let mut name = String::new();
        let mut kind = String::new();
        let mut hostname = String::new();
        for record in response.answers.iter().chain(response.additional.iter()) {
            match record.rtype {
                PTR => {
                    let data = record.data.text();
                    seen.push(data.trim_end_matches('.').to_string());
                    // An instance label is a name somebody chose; a service type is not.
                    if let Some(instance) = instance_label(&data) {
                        name = instance;
                        kind = service_label(&data);
                    }
                }
                SRV | A | TXT => {
                    if hostname.is_empty() && !record.name.starts_with('_') {
                        hostname = trim_local(&record.name);
                    }
                    if kind.is_empty() {
                        kind = service_label(&record.name);
                    }
                    if let Some(instance) = instance_label(&record.name)
                        && name.is_empty()
                    {
                        name = instance;
                    }
                }
                _ => {}
            }
        }
        if name.is_empty() {
            // No instance label anywhere in the packet: the responder's own host name
            // is the next best thing, and nothing at all beats a made-up name.
            name = hostname;
        }
        if name.is_empty() && kind.is_empty() {
            continue;
        }
        let entry = found.entry(*from.ip()).or_insert(Announcement {
            name: name.clone(),
            kind: kind.clone(),
            via: "mDNS",
        });
        // A later packet may carry the friendly name the first one lacked.
        if entry.name.is_empty() {
            entry.name = name;
        }
        if entry.kind.is_empty() {
            entry.kind = kind;
        }
    }
    seen
}

/// Whether a name is a service type — `_ipp._tcp.local` — rather than an instance of
/// one. The first label starting with an underscore is what says so.
fn is_service_type(name: &str) -> bool {
    let name = name.trim_end_matches('.');
    name.starts_with('_') && name.ends_with(".local") && name.matches('.').count() == 2
}

/// Sets the unicast-response bit on the question's class field, which is the last four
/// bytes of a single-question packet: type (2) then class (2), with the bit at the top
/// of the class.
fn set_unicast_response(packet: &mut [u8]) {
    let len = packet.len();
    if len >= 2 {
        packet[len - 2] |= 0x80;
    }
}

/// `Impressora HP._ipp._tcp.local` → `Impressora HP`.
fn instance_label(name: &str) -> Option<String> {
    let name = name.trim_end_matches('.');
    let (instance, rest) = name.split_once("._")?;
    // A service type answering the enumeration question has no instance in front of it.
    (!instance.is_empty() && rest.contains("._")).then(|| unescape(instance))
}

/// `_googlecast._tcp.local` → `googlecast`.
fn service_label(name: &str) -> String {
    name.trim_end_matches('.')
        .split('.')
        .find(|part| part.starts_with('_') && *part != "_tcp" && *part != "_udp")
        .map(|part| part.trim_start_matches('_').to_string())
        .unwrap_or_default()
}

fn trim_local(name: &str) -> String {
    name.trim_end_matches('.')
        .trim_end_matches(".local")
        .to_string()
}

/// DNS-SD escapes spaces and dots in instance labels; a name is for reading.
fn unescape(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    let mut chars = label.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some(escaped) if escaped.is_ascii_digit() => {
                // A three-digit decimal escape, e.g. `\032` for a space.
                let mut code = escaped.to_digit(10).unwrap_or(0);
                for _ in 0..2 {
                    if let Some(digit) = chars.next().and_then(|c| c.to_digit(10)) {
                        code = code * 10 + digit;
                    }
                }
                out.push(char::from_u32(code).unwrap_or('?'));
            }
            Some(escaped) => out.push(escaped),
            None => break,
        }
    }
    out
}

/// The SSDP half: one `M-SEARCH`, then read the replies, which are HTTP-shaped.
fn ssdp(window: Duration) -> HashMap<Ipv4Addr, Announcement> {
    let mut found = HashMap::new();
    let Some(socket) = socket(window) else {
        return found;
    };
    let search = format!(
        "M-SEARCH * HTTP/1.1\r\n\
         HOST: {}:{}\r\n\
         MAN: \"ssdp:discover\"\r\n\
         MX: 1\r\n\
         ST: ssdp:all\r\n\
         USER-AGENT: monitorzinho\r\n\r\n",
        SSDP.0, SSDP.1
    );
    let _ = socket.send_to(search.as_bytes(), SocketAddr::from(SSDP));

    let deadline = Instant::now() + window;
    let mut buffer = [0u8; 4096];
    while Instant::now() < deadline {
        let Ok((read, from)) = socket.recv_from(&mut buffer) else {
            continue;
        };
        let SocketAddr::V4(from) = from else { continue };
        let text = String::from_utf8_lossy(&buffer[..read]);
        let header = |name: &str| -> String {
            text.lines()
                .find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.trim()
                        .eq_ignore_ascii_case(name)
                        .then(|| value.trim().to_string())
                })
                .unwrap_or_default()
        };
        // `SERVER` is the device saying what it runs, which is usually the most
        // recognisable thing about it; `ST` says what kind of thing answered.
        let server = header("server");
        let kind = header("st");
        found.entry(*from.ip()).or_insert(Announcement {
            name: friendly(&server),
            kind: kind.rsplit(':').next().unwrap_or_default().to_string(),
            via: "SSDP",
        });
    }
    found
}

/// The recognisable part of an SSDP `SERVER` line, which is otherwise three
/// slash-separated version strings.
fn friendly(server: &str) -> String {
    server
        .split_whitespace()
        .find(|part| !part.contains('/') || part.split('/').next().is_some_and(|n| n.len() > 4))
        .unwrap_or(server)
        .to_string()
}
