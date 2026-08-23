//! What the internet sees when this machine speaks: the public address, the port it
//! comes out on, and what the NAT in between is doing with them.
//!
//! `curl ifconfig.me` gives the address and nothing else, and it gives it by asking a
//! web server to tell you — which works until the thing you are debugging is why UDP
//! doesn't come back. STUN answers the same question at the level where NAT lives, and
//! two questions more: whether the port stays the same when the destination changes,
//! and whether anything unexpected is rewriting the packets.
//!
//! The protocol is small enough to speak directly (RFC 5389): a twenty-byte header, a
//! magic cookie, and an attribute in the reply holding the address the server saw,
//! obfuscated by XOR against that same cookie.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use super::{EventKind, Execution, ParamSpec, Recorder, Tool};

/// Public STUN servers, asked in order. Two of them, from different operators, because
/// the interesting comparison is what two different destinations see.
const SERVERS: &str = "stun.l.google.com:19302, stun.cloudflare.com:3478";

/// `MAGIC_COOKIE` from RFC 5389 — the constant that separates a STUN message from
/// whatever else might arrive on a UDP port.
const COOKIE: u32 = 0x2112A442;
const BINDING_REQUEST: u16 = 0x0001;
const BINDING_SUCCESS: u16 = 0x0101;
const XOR_MAPPED_ADDRESS: u16 = 0x0020;
const MAPPED_ADDRESS: u16 = 0x0001;

pub struct StunTool;

impl Tool for StunTool {
    fn id(&self) -> &'static str {
        "stun"
    }

    fn name(&self) -> &'static str {
        "Endereço público (STUN)"
    }

    fn description(&self) -> &'static str {
        "Descobre o endereço e a porta com que esta máquina aparece na internet, e o que o NAT no meio faz com eles"
    }

    fn params(&self) -> Vec<ParamSpec> {
        vec![
            ParamSpec::text(
                "servidores",
                "Servidores STUN",
                SERVERS,
                "Separados por vírgula. Dois destinos diferentes é o que revela o comportamento do NAT",
            ),
            ParamSpec::text(
                "timeout",
                "Tempo por consulta (ms)",
                "3000",
                "Quanto esperar cada resposta",
            ),
        ]
    }

    fn summarize(&self, params: &HashMap<&'static str, String>) -> String {
        let servers = params
            .get("servidores")
            .map(String::as_str)
            .unwrap_or(SERVERS);
        format!("{} servidor(es)", servers.split(',').count())
    }

    fn on_demand(&self, _params: &HashMap<&'static str, String>) -> bool {
        true
    }

    fn start(&self, id: u64, params: &HashMap<&'static str, String>) -> Result<Execution, String> {
        let plan = Plan::from(params)?;
        let (execution, recorder) = Execution::new(id, self.name(), self.summarize(params));
        recorder.record(
            0,
            EventKind::Note(format!(
                "pronto para perguntar a {} servidor(es) STUN. Nada roda até você abrir",
                plan.servers.len()
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
            ask(plan, &recorder);
            recorder.ran();
            finished.store(true, Ordering::Relaxed);
        });
    }

    fn columns(&self, execution: &Execution) -> (String, String) {
        execution.outcome()
    }
}

struct Plan {
    servers: Vec<(String, SocketAddr)>,
    timeout: Duration,
}

impl Plan {
    fn from(params: &HashMap<&'static str, String>) -> Result<Self, String> {
        let get = |key| params.get(key).map(String::as_str).unwrap_or("").trim();
        let text = match get("servidores") {
            "" => SERVERS,
            text => text,
        };
        let mut servers = Vec::new();
        for entry in text.split(',').map(str::trim).filter(|e| !e.is_empty()) {
            let address = entry
                .to_socket_addrs()
                .map_err(|e| format!("não resolvi {entry}: {e}"))?
                .find(|address| address.is_ipv4())
                .ok_or_else(|| format!("{entry} não tem endereço IPv4"))?;
            servers.push((entry.to_string(), address));
        }
        if servers.is_empty() {
            return Err("informe ao menos um servidor STUN".to_string());
        }
        Ok(Self {
            servers,
            timeout: Duration::from_millis(
                get("timeout")
                    .parse::<u64>()
                    .unwrap_or(3000)
                    .clamp(200, 30_000),
            ),
        })
    }
}

fn ask(plan: Plan, rec: &Recorder) {
    let started = Instant::now();
    // One socket for every question: the whole point is what the NAT does with *this*
    // mapping, and a fresh socket per server would measure a different mapping each time.
    let Ok(socket) = UdpSocket::bind(("0.0.0.0", 0)) else {
        rec.record(
            0,
            EventKind::Error("não consegui abrir um socket UDP".to_string()),
        );
        rec.report("sem socket", "o sistema recusou um socket UDP");
        return;
    };
    let _ = socket.set_read_timeout(Some(plan.timeout));
    let local = socket
        .local_addr()
        .map(|address| address.to_string())
        .unwrap_or_default();
    rec.record(
        0,
        EventKind::Note(format!("perguntando de {local} — a porta local é esta")),
    );

    let mut seen: Vec<(String, SocketAddr)> = Vec::new();
    for (name, address) in &plan.servers {
        match query(&socket, *address, plan.timeout) {
            Ok((mapped, elapsed)) => {
                rec.record(
                    0,
                    EventKind::Note(format!(
                        "{name:<28} vê {mapped}   ({:.0} ms)",
                        elapsed.as_secs_f64() * 1000.0
                    )),
                );
                seen.push((name.clone(), mapped));
            }
            Err(e) => rec.record(0, EventKind::Error(format!("{name:<28} {e}"))),
        }
    }

    let Some((_, first)) = seen.first().cloned() else {
        rec.record(
            0,
            EventKind::Error(
                "nenhum servidor respondeu — UDP de saída pode estar bloqueado".to_string(),
            ),
        );
        rec.report("sem resposta", "nenhum servidor STUN respondeu");
        return;
    };

    rec.record(0, EventKind::Note("── Conclusão ──".to_string()));
    rec.record(
        0,
        EventKind::Note(format!("  Endereço público      {}", first.ip())),
    );
    rec.found("ip", first.ip().to_string());

    let same_address = seen.iter().all(|(_, address)| address.ip() == first.ip());
    let same_port = seen
        .iter()
        .all(|(_, address)| address.port() == first.port());
    let local_port = socket.local_addr().map(|a| a.port()).unwrap_or(0);

    if seen.len() > 1 {
        // The classic distinction, and the one that decides whether a peer-to-peer
        // connection has any chance: a NAT that keeps one port per socket can be
        // punched through, and one that picks a new port per destination cannot.
        rec.record(
            0,
            EventKind::Note(format!(
                "  Comportamento do NAT  {}",
                match (same_address, same_port) {
                    (true, true) =>
                        "mesma porta para destinos diferentes — cone, atravessável (P2P funciona)",
                    (true, false) =>
                        "porta diferente por destino — NAT simétrico, P2P direto não funciona",
                    (false, _) => "endereços públicos diferentes — saída por mais de um link",
                }
            )),
        );
    }
    rec.record(
        0,
        EventKind::Note(format!(
            "  Porta               {} local → {} pública{}",
            local_port,
            first.port(),
            if local_port == first.port() {
                " (preservada)"
            } else {
                " (traduzida)"
            }
        )),
    );
    rec.record(
        0,
        EventKind::Note(format!(
            "consulta concluída em {:.1}s",
            started.elapsed().as_secs_f64()
        )),
    );
    rec.report(
        first.ip().to_string(),
        match (seen.len(), same_port) {
            (1, _) => "1 servidor".to_string(),
            (_, true) => format!(
                "{} servidores · porta preservada entre destinos",
                seen.len()
            ),
            (_, false) => format!("{} servidores · NAT simétrico", seen.len()),
        },
    );
}

/// One binding request, and the address the far side says it came from.
fn query(
    socket: &UdpSocket,
    server: SocketAddr,
    timeout: Duration,
) -> Result<(SocketAddr, Duration), String> {
    // Transaction id: twelve bytes that tie a reply to its request. Derived from the
    // clock rather than from a random source, which this crate doesn't have — uniqueness
    // within one execution is all that is needed.
    let stamp = Instant::now().elapsed().as_nanos() as u64
        ^ std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
    let mut transaction = [0u8; 12];
    transaction[..8].copy_from_slice(&stamp.to_be_bytes());
    transaction[8..].copy_from_slice(&(server.port() as u32).to_be_bytes());

    let mut request = Vec::with_capacity(20);
    request.extend_from_slice(&BINDING_REQUEST.to_be_bytes());
    request.extend_from_slice(&0u16.to_be_bytes()); // no attributes
    request.extend_from_slice(&COOKIE.to_be_bytes());
    request.extend_from_slice(&transaction);

    let started = Instant::now();
    socket
        .send_to(&request, server)
        .map_err(|e| format!("não consegui enviar: {e}"))?;

    let deadline = started + timeout;
    let mut buffer = [0u8; 1024];
    loop {
        if Instant::now() > deadline {
            return Err("sem resposta".to_string());
        }
        let (read, from) = socket
            .recv_from(&mut buffer)
            .map_err(|_| "sem resposta".to_string())?;
        if from.ip() != server.ip() || read < 20 {
            continue;
        }
        let message = &buffer[..read];
        if u16::from_be_bytes([message[0], message[1]]) != BINDING_SUCCESS
            || message[8..20] != transaction
        {
            continue;
        }
        return match mapped_address(message) {
            Some(address) => Ok((address, started.elapsed())),
            None => Err("resposta sem o endereço mapeado".to_string()),
        };
    }
}

/// The address out of a binding response, from whichever attribute carries it.
///
/// `XOR-MAPPED-ADDRESS` is the one that matters: the address is XORed with the magic
/// cookie precisely so that a NAT rewriting addresses inside packets — some do — can't
/// silently "fix" it on the way back.
fn mapped_address(message: &[u8]) -> Option<SocketAddr> {
    let mut offset = 20;
    while offset + 4 <= message.len() {
        let kind = u16::from_be_bytes([message[offset], message[offset + 1]]);
        let length = u16::from_be_bytes([message[offset + 2], message[offset + 3]]) as usize;
        let value = message.get(offset + 4..offset + 4 + length)?;
        match kind {
            XOR_MAPPED_ADDRESS if value.len() >= 8 && value[1] == 0x01 => {
                let port = u16::from_be_bytes([value[2], value[3]]) ^ (COOKIE >> 16) as u16;
                let address = u32::from_be_bytes([value[4], value[5], value[6], value[7]]) ^ COOKIE;
                return Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::from(address)), port));
            }
            MAPPED_ADDRESS if value.len() >= 8 && value[1] == 0x01 => {
                let port = u16::from_be_bytes([value[2], value[3]]);
                let address = Ipv4Addr::new(value[4], value[5], value[6], value[7]);
                return Some(SocketAddr::new(IpAddr::V4(address), port));
            }
            _ => {}
        }
        // Attributes are padded to four bytes.
        offset += 4 + length.div_ceil(4) * 4;
    }
    None
}
