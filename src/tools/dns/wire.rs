//! Speaking DNS on the wire, by hand.
//!
//! `getaddrinfo` answers exactly one question — "what address is this name?" — and the
//! investigation this tool exists for is every other question: who is authoritative,
//! what the mail is, what the zone says about itself, whether the nameservers even
//! agree with each other. None of that comes out of the system resolver, so the queries
//! are built and parsed here.
//!
//! It's not a full resolver: no recursion of its own, no validation of signatures. It
//! asks a resolver, and then — for the parts where the answer depends on *who* you ask
//! — it goes and asks each authoritative server directly.

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const A: u16 = 1;
pub const NS: u16 = 2;
pub const CNAME: u16 = 5;
pub const SOA: u16 = 6;
pub const PTR: u16 = 12;
pub const MX: u16 = 15;
pub const TXT: u16 = 16;
pub const AAAA: u16 = 28;
pub const SRV: u16 = 33;
pub const DS: u16 = 43;
pub const DNSKEY: u16 = 48;
pub const NSEC: u16 = 47;
pub const NSEC3: u16 = 50;
pub const NSEC3PARAM: u16 = 51;
pub const CAA: u16 = 257;
pub const AXFR: u16 = 252;

/// The record types worth asking for by name when sweeping a zone apex.
pub const APEX_TYPES: &[u16] = &[SOA, NS, A, AAAA, MX, TXT, CAA, DNSKEY];

/// Cap on a TCP response, including a zone transfer. A zone that doesn't fit in this
/// isn't going to be read in a terminal anyway.
const TCP_LIMIT: usize = 4 * 1024 * 1024;
/// Compression pointers can chain. More than this many jumps means a malicious or
/// broken message pointing at itself.
const MAX_JUMPS: usize = 32;

/// Query ids only need to be unpredictable enough that a stray datagram doesn't get
/// mistaken for an answer; the counter is seeded from the clock so two runs don't
/// start from the same place.
fn next_id() -> u16 {
    static COUNTER: AtomicU16 = AtomicU16::new(0);
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u16)
        .unwrap_or(0);
    COUNTER.fetch_add(1, Ordering::Relaxed).wrapping_add(seed)
}

pub fn type_name(rtype: u16) -> String {
    match rtype {
        A => "A".to_string(),
        NS => "NS".to_string(),
        CNAME => "CNAME".to_string(),
        SOA => "SOA".to_string(),
        PTR => "PTR".to_string(),
        MX => "MX".to_string(),
        TXT => "TXT".to_string(),
        AAAA => "AAAA".to_string(),
        SRV => "SRV".to_string(),
        DS => "DS".to_string(),
        DNSKEY => "DNSKEY".to_string(),
        NSEC => "NSEC".to_string(),
        NSEC3 => "NSEC3".to_string(),
        NSEC3PARAM => "NSEC3PARAM".to_string(),
        CAA => "CAA".to_string(),
        AXFR => "AXFR".to_string(),
        other => format!("TYPE{other}"),
    }
}

/// What the server thought of the question. Anything but `NoError` is worth showing
/// verbatim: `NXDOMAIN` and `REFUSED` mean very different things about a zone.
pub fn rcode_name(rcode: u8) -> &'static str {
    match rcode {
        0 => "NOERROR",
        1 => "FORMERR",
        2 => "SERVFAIL",
        3 => "NXDOMAIN",
        4 => "NOTIMP",
        5 => "REFUSED",
        9 => "NOTAUTH",
        _ => "?",
    }
}

#[derive(Clone, PartialEq)]
pub enum Rdata {
    Addr(IpAddr),
    /// NS, CNAME, PTR — anything whose whole payload is one name.
    Name(String),
    Mx {
        preference: u16,
        host: String,
    },
    Soa {
        primary: String,
        mailbox: String,
        serial: u32,
        refresh: u32,
        retry: u32,
        expire: u32,
        minimum: u32,
    },
    Txt(Vec<String>),
    Srv {
        priority: u16,
        weight: u16,
        port: u16,
        target: String,
    },
    Caa {
        flags: u8,
        tag: String,
        value: String,
    },
    /// DS and DNSKEY: the digest itself is noise in a terminal, so only the parameters
    /// that identify the key are kept.
    Key {
        tag_or_alg: String,
    },
    Raw(String),
}

impl Rdata {
    pub fn text(&self) -> String {
        match self {
            Rdata::Addr(ip) => ip.to_string(),
            Rdata::Name(name) => name.clone(),
            Rdata::Mx { preference, host } => format!("{preference} {host}"),
            Rdata::Soa {
                primary,
                mailbox,
                serial,
                refresh,
                retry,
                expire,
                minimum,
            } => format!(
                "{primary} {mailbox} serial={serial} refresh={refresh} retry={retry} expire={expire} min={minimum}"
            ),
            Rdata::Txt(parts) => parts.join(""),
            Rdata::Srv {
                priority,
                weight,
                port,
                target,
            } => format!("{priority} {weight} {port} {target}"),
            Rdata::Caa { flags, tag, value } => format!("{flags} {tag} \"{value}\""),
            Rdata::Key { tag_or_alg } => tag_or_alg.clone(),
            Rdata::Raw(hex) => hex.clone(),
        }
    }
}

#[derive(Clone)]
pub struct Record {
    pub name: String,
    pub rtype: u16,
    pub ttl: u32,
    pub data: Rdata,
}

impl Record {
    /// One line, in the shape `dig` prints — familiar, and it lines up in a log.
    pub fn line(&self) -> String {
        format!(
            "{:<28} {:>7}  {:<7} {}",
            self.name,
            self.ttl,
            type_name(self.rtype),
            self.data.text()
        )
    }
}

pub struct Response {
    pub rcode: u8,
    /// Whether the answering server claims authority for the zone.
    pub authoritative: bool,
    pub answers: Vec<Record>,
    pub authority: Vec<Record>,
    pub additional: Vec<Record>,
}

impl Response {
    /// Answers of one type, following a CNAME chain being the caller's problem — for
    /// this tool a CNAME *is* the answer worth showing.
    pub fn of_type(&self, rtype: u16) -> Vec<&Record> {
        self.answers
            .iter()
            .filter(|record| record.rtype == rtype)
            .collect()
    }
}

/// Builds the question section. Names are stored as length-prefixed labels; a label
/// over 63 bytes or a name over 255 can't be encoded at all.
fn encode_name(name: &str, out: &mut Vec<u8>) -> Result<(), String> {
    for label in name.trim_end_matches('.').split('.') {
        if label.is_empty() {
            continue;
        }
        if label.len() > 63 {
            return Err(format!("rótulo longo demais em «{name}»"));
        }
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    if out.len() > 255 + 16 {
        return Err(format!("nome longo demais: «{name}»"));
    }
    Ok(())
}

/// UDP payload advertised via EDNS0. Big enough that a signed answer usually arrives in
/// one datagram instead of forcing a TCP retry, small enough to survive most paths.
const EDNS_PAYLOAD: u16 = 1232;

fn build_query(
    id: u16,
    name: &str,
    rtype: u16,
    recursive: bool,
    dnssec: bool,
) -> Result<Vec<u8>, String> {
    let mut packet = Vec::with_capacity(64);
    packet.extend_from_slice(&id.to_be_bytes());
    // QR=0, OPCODE=0, RD as asked.
    packet.extend_from_slice(&(if recursive { 0x0100u16 } else { 0 }).to_be_bytes());
    packet.extend_from_slice(&1u16.to_be_bytes()); // one question
    packet.extend_from_slice(&0u16.to_be_bytes()); // no answers
    packet.extend_from_slice(&0u16.to_be_bytes()); // no authority
    packet.extend_from_slice(&(if dnssec { 1u16 } else { 0 }).to_be_bytes()); // OPT, maybe
    encode_name(name, &mut packet)?;
    packet.extend_from_slice(&rtype.to_be_bytes());
    packet.extend_from_slice(&1u16.to_be_bytes()); // class IN

    if dnssec {
        // An OPT pseudo-record on the root name, carrying the DO bit. Without it a
        // resolver strips NSEC and RRSIG from the answer and the walk sees nothing.
        packet.push(0); // root
        packet.extend_from_slice(&41u16.to_be_bytes()); // OPT
        packet.extend_from_slice(&EDNS_PAYLOAD.to_be_bytes()); // class = payload size
        packet.extend_from_slice(&0x0000_8000u32.to_be_bytes()); // extended rcode + DO
        packet.extend_from_slice(&0u16.to_be_bytes()); // no options
    }
    Ok(packet)
}

/// Reads a name at `pos`, following compression pointers. Returns the name and the
/// position just past it *in the current record* — a pointer doesn't advance the
/// caller past the pointer itself.
fn read_name(message: &[u8], mut pos: usize) -> Result<(String, usize), String> {
    let mut name = String::new();
    let mut after = None;
    let mut jumps = 0;

    loop {
        let length = *message.get(pos).ok_or("mensagem truncada")? as usize;
        if length == 0 {
            pos += 1;
            break;
        }
        if length & 0xC0 == 0xC0 {
            let low = *message.get(pos + 1).ok_or("ponteiro truncado")? as usize;
            let target = ((length & 0x3F) << 8) | low;
            jumps += 1;
            if jumps > MAX_JUMPS {
                return Err("ponteiros de compressão em laço".to_string());
            }
            after.get_or_insert(pos + 2);
            pos = target;
            continue;
        }
        let end = pos + 1 + length;
        let label = message.get(pos + 1..end).ok_or("rótulo truncado")?;
        if !name.is_empty() {
            name.push('.');
        }
        name.push_str(&String::from_utf8_lossy(label));
        pos = end;
    }

    if name.is_empty() {
        name.push('.');
    }
    Ok((name, after.unwrap_or(pos)))
}

fn u16_at(message: &[u8], pos: usize) -> Result<u16, String> {
    let bytes = message.get(pos..pos + 2).ok_or("mensagem truncada")?;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
}

fn u32_at(message: &[u8], pos: usize) -> Result<u32, String> {
    let bytes = message.get(pos..pos + 4).ok_or("mensagem truncada")?;
    Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_record(message: &[u8], pos: usize) -> Result<(Record, usize), String> {
    let (name, mut pos) = read_name(message, pos)?;
    let rtype = u16_at(message, pos)?;
    let ttl = u32_at(message, pos + 4)?;
    let length = u16_at(message, pos + 8)? as usize;
    pos += 10;
    let rdata = message.get(pos..pos + length).ok_or("rdata truncado")?;
    let data = decode_rdata(message, pos, rdata, rtype);
    Ok((
        Record {
            name,
            rtype,
            ttl,
            data,
        },
        pos + length,
    ))
}

fn decode_rdata(message: &[u8], pos: usize, rdata: &[u8], rtype: u16) -> Rdata {
    let fallback = || Rdata::Raw(hex(rdata));
    match rtype {
        A if rdata.len() == 4 => Rdata::Addr(IpAddr::V4(Ipv4Addr::new(
            rdata[0], rdata[1], rdata[2], rdata[3],
        ))),
        AAAA if rdata.len() == 16 => {
            let mut octets = [0u8; 16];
            octets.copy_from_slice(rdata);
            Rdata::Addr(IpAddr::V6(Ipv6Addr::from(octets)))
        }
        NS | CNAME | PTR => match read_name(message, pos) {
            Ok((name, _)) => Rdata::Name(name),
            Err(_) => fallback(),
        },
        MX => match (u16_at(message, pos), read_name(message, pos + 2)) {
            (Ok(preference), Ok((host, _))) => Rdata::Mx { preference, host },
            _ => fallback(),
        },
        SOA => decode_soa(message, pos).unwrap_or_else(fallback),
        NSEC => match read_name(message, pos) {
            // The rest of the rdata is a type bitmap; the next name is the whole point.
            Ok((next, _)) => Rdata::Name(next),
            Err(_) => fallback(),
        },
        TXT => Rdata::Txt(decode_strings(rdata)),
        SRV => decode_srv(message, pos).unwrap_or_else(fallback),
        CAA => decode_caa(rdata).unwrap_or_else(fallback),
        DS if rdata.len() >= 4 => Rdata::Key {
            tag_or_alg: format!(
                "keytag={} alg={} digest={} ({} bytes)",
                u16::from_be_bytes([rdata[0], rdata[1]]),
                rdata[2],
                rdata[3],
                rdata.len() - 4
            ),
        },
        DNSKEY if rdata.len() >= 4 => Rdata::Key {
            tag_or_alg: format!(
                "flags={} proto={} alg={} chave de {} bytes",
                u16::from_be_bytes([rdata[0], rdata[1]]),
                rdata[2],
                rdata[3],
                rdata.len() - 4
            ),
        },
        _ => fallback(),
    }
}

fn decode_soa(message: &[u8], pos: usize) -> Option<Rdata> {
    let (primary, pos) = read_name(message, pos).ok()?;
    let (mailbox, pos) = read_name(message, pos).ok()?;
    Some(Rdata::Soa {
        primary,
        mailbox,
        serial: u32_at(message, pos).ok()?,
        refresh: u32_at(message, pos + 4).ok()?,
        retry: u32_at(message, pos + 8).ok()?,
        expire: u32_at(message, pos + 12).ok()?,
        minimum: u32_at(message, pos + 16).ok()?,
    })
}

fn decode_srv(message: &[u8], pos: usize) -> Option<Rdata> {
    Some(Rdata::Srv {
        priority: u16_at(message, pos).ok()?,
        weight: u16_at(message, pos + 2).ok()?,
        port: u16_at(message, pos + 4).ok()?,
        target: read_name(message, pos + 6).ok()?.0,
    })
}

fn decode_caa(rdata: &[u8]) -> Option<Rdata> {
    let flags = *rdata.first()?;
    let tag_len = *rdata.get(1)? as usize;
    let tag = rdata.get(2..2 + tag_len)?;
    let value = rdata.get(2 + tag_len..)?;
    Some(Rdata::Caa {
        flags,
        tag: String::from_utf8_lossy(tag).to_string(),
        value: String::from_utf8_lossy(value).to_string(),
    })
}

/// TXT rdata is a sequence of length-prefixed strings, which a long SPF record is split
/// into. Joining them back is what the record actually means.
fn decode_strings(rdata: &[u8]) -> Vec<String> {
    let mut parts = Vec::new();
    let mut pos = 0;
    while pos < rdata.len() {
        let length = rdata[pos] as usize;
        let end = (pos + 1 + length).min(rdata.len());
        parts.push(String::from_utf8_lossy(&rdata[pos + 1..end]).to_string());
        pos = end;
    }
    parts
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes.iter().take(64) {
        let _ = write!(out, "{byte:02x}");
    }
    if bytes.len() > 64 {
        let _ = write!(out, "… ({} bytes)", bytes.len());
    }
    out
}

fn parse_message(message: &[u8]) -> Result<Response, String> {
    if message.len() < 12 {
        return Err("resposta curta demais para ser DNS".to_string());
    }
    let flags = u16_at(message, 2)?;
    let counts = [
        u16_at(message, 4)?,
        u16_at(message, 6)?,
        u16_at(message, 8)?,
        u16_at(message, 10)?,
    ];

    let mut pos = 12;
    for _ in 0..counts[0] {
        let (_, next) = read_name(message, pos)?;
        pos = next + 4;
    }

    let mut sections: Vec<Vec<Record>> = Vec::new();
    for count in &counts[1..] {
        let mut records = Vec::new();
        for _ in 0..*count {
            let (record, next) = read_record(message, pos)?;
            records.push(record);
            pos = next;
        }
        sections.push(records);
    }

    let mut sections = sections.into_iter();
    Ok(Response {
        rcode: (flags & 0x000F) as u8,
        authoritative: flags & 0x0400 != 0,
        answers: sections.next().unwrap_or_default(),
        authority: sections.next().unwrap_or_default(),
        additional: sections.next().unwrap_or_default(),
    })
}

/// One question to one server. UDP first; a truncated answer is re-asked over TCP,
/// which is what makes a long TXT or a DNSKEY set come back whole.
pub fn query(
    server: SocketAddr,
    name: &str,
    rtype: u16,
    timeout: Duration,
) -> Result<Response, String> {
    query_with(server, name, rtype, timeout, false)
}

/// The same question, asking the resolver to keep the DNSSEC records in the answer.
pub fn query_signed(
    server: SocketAddr,
    name: &str,
    rtype: u16,
    timeout: Duration,
) -> Result<Response, String> {
    query_with(server, name, rtype, timeout, true)
}

fn query_with(
    server: SocketAddr,
    name: &str,
    rtype: u16,
    timeout: Duration,
    dnssec: bool,
) -> Result<Response, String> {
    let id = next_id();
    let packet = build_query(id, name, rtype, true, dnssec)?;

    let bind = if server.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let socket = UdpSocket::bind(bind).map_err(|e| format!("socket: {e}"))?;
    socket
        .set_read_timeout(Some(timeout))
        .map_err(|e| format!("socket: {e}"))?;
    socket
        .send_to(&packet, server)
        .map_err(|e| format!("envio para {server}: {e}"))?;

    let mut buf = [0u8; EDNS_PAYLOAD as usize];
    loop {
        let (n, from) = socket
            .recv_from(&mut buf)
            .map_err(|e| format!("sem resposta de {server}: {e}"))?;
        // A datagram from somewhere else, or answering a different question, is not an
        // answer to this one.
        if from.ip() != server.ip() || n < 12 || u16_at(&buf, 0)? != id {
            continue;
        }
        let truncated = u16_at(&buf, 2)? & 0x0200 != 0;
        if truncated {
            return query_tcp(server, name, rtype, timeout);
        }
        return parse_message(&buf[..n]);
    }
}

/// The same question over TCP, where the message is prefixed with its length. Also the
/// only way to ask for a zone transfer.
pub fn query_tcp(
    server: SocketAddr,
    name: &str,
    rtype: u16,
    timeout: Duration,
) -> Result<Response, String> {
    let payloads = exchange_tcp(server, name, rtype, timeout, false)?;
    let first = payloads.first().ok_or("resposta vazia")?;
    parse_message(first)
}

/// How a zone transfer went. Three outcomes, not two: a server that answered "no" is
/// correctly configured, while one that never answered says nothing about its policy —
/// reporting both as "refused" would credit a firewall for a decision it didn't make.
pub enum Transfer {
    /// The server handed over the zone. This is the finding.
    Zone(Vec<Record>),
    /// The server answered, declining.
    Refused(&'static str),
    /// No usable answer: no TCP, no response, or nothing that parsed.
    NoAnswer(String),
}

pub fn zone_transfer(server: SocketAddr, zone: &str, timeout: Duration) -> Transfer {
    let payloads = match exchange_tcp(server, zone, AXFR, timeout, true) {
        Ok(payloads) => payloads,
        Err(e) => return Transfer::NoAnswer(e),
    };
    let mut records = Vec::new();
    for payload in &payloads {
        let response = match parse_message(payload) {
            Ok(response) => response,
            Err(e) => return Transfer::NoAnswer(e),
        };
        if response.rcode != 0 {
            return Transfer::Refused(rcode_name(response.rcode));
        }
        records.extend(response.answers);
    }
    if records.is_empty() {
        return Transfer::NoAnswer("respondeu sem registros".to_string());
    }
    Transfer::Zone(records)
}

fn exchange_tcp(
    server: SocketAddr,
    name: &str,
    rtype: u16,
    timeout: Duration,
    drain: bool,
) -> Result<Vec<Vec<u8>>, String> {
    let packet = build_query(next_id(), name, rtype, true, false)?;
    let mut stream =
        TcpStream::connect_timeout(&server, timeout).map_err(|e| format!("{server}: {e}"))?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|e| format!("socket: {e}"))?;
    stream
        .write_all(&(packet.len() as u16).to_be_bytes())
        .and_then(|_| stream.write_all(&packet))
        .map_err(|e| format!("envio para {server}: {e}"))?;

    let mut raw = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                raw.extend_from_slice(&buf[..n]);
                if raw.len() > TCP_LIMIT {
                    break;
                }
                if !drain && complete(&raw) {
                    break;
                }
            }
            Err(_) => break,
        }
    }

    let mut payloads = Vec::new();
    let mut pos = 0;
    while pos + 2 <= raw.len() {
        let length = u16::from_be_bytes([raw[pos], raw[pos + 1]]) as usize;
        let end = pos + 2 + length;
        if end > raw.len() {
            break;
        }
        payloads.push(raw[pos + 2..end].to_vec());
        pos = end;
    }
    if payloads.is_empty() {
        return Err(format!("{server} não respondeu por TCP"));
    }
    Ok(payloads)
}

/// Whether at least one length-prefixed message has arrived whole.
fn complete(raw: &[u8]) -> bool {
    raw.len() >= 2 && raw.len() >= 2 + u16::from_be_bytes([raw[0], raw[1]]) as usize
}

/// The resolvers the machine itself uses, so a scan with no server named behaves like
/// everything else on the box — VPN and container resolvers included.
pub fn system_resolvers() -> Vec<SocketAddr> {
    let mut servers = Vec::new();
    if let Ok(content) = std::fs::read_to_string("/etc/resolv.conf") {
        for line in content.lines() {
            let line = line.trim();
            if let Some(address) = line.strip_prefix("nameserver ")
                && let Ok(ip) = address.trim().parse::<IpAddr>()
            {
                servers.push(SocketAddr::new(ip, 53));
            }
        }
    }
    servers
}

/// `1.1.1.1`, `8.8.8.8:53`, or a hostname — whatever the user typed in the field.
pub fn resolver_from(text: &str) -> Result<SocketAddr, String> {
    let text = text.trim();
    if let Ok(ip) = text.parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, 53));
    }
    if let Ok(addr) = text.parse::<SocketAddr>() {
        return Ok(addr);
    }
    (text, 53u16)
        .to_socket_addrs()
        .map_err(|e| format!("resolvedor inválido ({text}): {e}"))?
        .next()
        .ok_or_else(|| format!("resolvedor ({text}) não resolveu"))
}

/// WHOIS is one line in and free text out, on port 43. `flags` is the prefix some
/// registries need — `domain ` for Verisign, nothing for most.
pub fn whois(server: &str, question: &str, timeout: Duration) -> Result<String, String> {
    let addr = (server, 43u16)
        .to_socket_addrs()
        .map_err(|e| format!("{server}: {e}"))?
        .next()
        .ok_or_else(|| format!("{server} não resolveu"))?;
    let mut stream =
        TcpStream::connect_timeout(&addr, timeout).map_err(|e| format!("{server}: {e}"))?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|e| format!("socket: {e}"))?;
    stream
        .write_all(format!("{question}\r\n").as_bytes())
        .map_err(|e| format!("envio para {server}: {e}"))?;

    let mut text = String::new();
    let mut buf = [0u8; 4096];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                text.push_str(&String::from_utf8_lossy(&buf[..n]));
                if text.len() > 256 * 1024 {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    if text.trim().is_empty() {
        return Err(format!("{server} respondeu vazio"));
    }
    Ok(text)
}
