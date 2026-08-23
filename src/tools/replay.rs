//! Sending a captured request again.
//!
//! A tunnel shows what went past; the question that follows is always the same — *what
//! happens if that runs again?* Reproducing it by hand means rebuilding the request in
//! `curl` header by header, and the interesting bug is usually in the header nobody
//! thought to copy.
//!
//! So the request travels whole: the bytes a relay saw are pre-filled here, escaped onto
//! one line (see `payload`), and sending them is one key. They stay editable on the way
//! — change the path, drop a cookie, fix a header — which is the other half of why this
//! exists: repeating a request unchanged answers "was it me or them", repeating it
//! changed answers "which part".
//!
//! Nothing is sent until asked. This is on-demand for the same reason the port scanner
//! is: a request that transfers money should be repeated when somebody means it, not
//! because the app started.

use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use super::{Direction, EventKind, Execution, ParamSpec, Recorder, Tool, payload, tls};

/// TLS wording, identical to the tunnel's: the same question deserves the same words.
const TLS_MODES: &[&str] = &["não", "sim", "sim, sem validar certificado"];

/// How much of a reply is read before it's called enough. A reply bigger than this is
/// still a successful repeat — the point is the status and the headers, not archiving
/// the body.
const MAX_REPLY: usize = 1 << 20;

/// How long a read may find nothing before the reply is called complete. A server that
/// keeps the connection open (`keep-alive`) never sends EOF, so something has to decide
/// that it has finished talking.
const IDLE: Duration = Duration::from_millis(400);

pub struct ReplayTool;

impl Tool for ReplayTool {
    fn id(&self) -> &'static str {
        "replay"
    }

    fn name(&self) -> &'static str {
        "Repetir requisição"
    }

    fn description(&self) -> &'static str {
        "Manda de novo uma requisição capturada, com a chance de editar antes — e mostra a resposta inteira"
    }

    fn params(&self) -> Vec<ParamSpec> {
        vec![
            ParamSpec::text(
                "destino",
                "Destino",
                "",
                "host:porta para onde mandar. Vindo de um túnel já vem preenchido com o destino dele",
            ),
            ParamSpec::choice(
                "tls",
                "TLS",
                TLS_MODES,
                "Se o destino fala TLS. O conteúdo continua legível aqui: quem cifra é esta ferramenta",
            ),
            ParamSpec::text(
                "sni",
                "Nome no TLS (SNI)",
                "",
                "Só quando o destino é IP mas o certificado tem nome. Vazio usa o host do destino",
            ),
            ParamSpec::text(
                "payload",
                "Requisição",
                "",
                "Os bytes a mandar, em uma linha: \\r \\n \\t e \\xNN. Edite à vontade antes de repetir",
            ),
            ParamSpec::text(
                "timeout",
                "Tempo limite (ms)",
                "10000",
                "Vale para conectar e para esperar a resposta",
            ),
        ]
    }

    fn summarize(&self, params: &HashMap<&'static str, String>) -> String {
        let get = |key| params.get(key).map(String::as_str).unwrap_or("").trim();
        let head = payload::first_line(get("payload"), 48);
        let scheme = if get("tls") == TLS_MODES[0] {
            ""
        } else {
            "TLS "
        };
        let destination = match get("destino") {
            "" => "destino a preencher".to_string(),
            destination => format!("{scheme}{destination}"),
        };
        if head.is_empty() {
            return destination;
        }
        format!("{head} → {destination}")
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
                "pronto para mandar {} bytes para {}. Nada sai daqui até você abrir ou apertar r",
                plan.bytes.len(),
                plan.destination
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
            send(plan, &recorder);
            recorder.ran();
            finished.store(true, Ordering::Relaxed);
        });
    }

    fn columns(&self, execution: &Execution) -> (String, String) {
        execution.outcome()
    }
}

struct Plan {
    destination: String,
    address: SocketAddr,
    host: String,
    tls: Option<TlsPlan>,
    bytes: Vec<u8>,
    timeout: Duration,
    /// How many times it has been sent, for the log — a repeat is only interesting next
    /// to the one before it.
    attempt: u64,
}

struct TlsPlan {
    sni: String,
    verify: bool,
}

impl Plan {
    fn from(params: &HashMap<&'static str, String>) -> Result<Self, String> {
        let get = |key| params.get(key).map(String::as_str).unwrap_or("").trim();
        let destination = get("destino").to_string();
        if destination.is_empty() {
            return Err("informe o destino (host:porta)".to_string());
        }
        if !destination.contains(':') {
            return Err(format!("{destination} não tem porta — use host:porta"));
        }
        let address = destination
            .to_socket_addrs()
            .map_err(|e| format!("não resolvi {destination}: {e}"))?
            .next()
            .ok_or_else(|| format!("{destination} não resolveu para endereço nenhum"))?;
        let bytes = payload::decode(get("payload"));
        if bytes.is_empty() {
            return Err("não há o que repetir: a requisição está vazia".to_string());
        }
        let mode = get("tls");
        let tls = (mode != TLS_MODES[0]).then(|| TlsPlan {
            sni: get("sni").to_string(),
            verify: mode == TLS_MODES[1],
        });
        let host = destination
            .rsplit_once(':')
            .map(|(host, _)| host.to_string())
            .unwrap_or_else(|| destination.clone());
        Ok(Self {
            destination,
            address,
            host,
            tls,
            bytes,
            timeout: Duration::from_millis(
                get("timeout")
                    .parse::<u64>()
                    .unwrap_or(10_000)
                    .clamp(200, 300_000),
            ),
            attempt: 0,
        })
    }
}

/// What one round produced, in the terms the row shows.
struct Outcome {
    headline: String,
    summary: String,
}

fn send(mut plan: Plan, rec: &Recorder) {
    // Each repeat is its own "connection" in the log, so the numbering separates one
    // attempt's bytes from the next one's exactly as a tunnel separates two clients.
    plan.attempt = rec.stats().connections.fetch_add(1, Ordering::Relaxed) + 1;
    let conn = plan.attempt;
    // A note rather than the `Opened`/`Closed` pair a relay uses: those are worded for
    // somebody connecting *to* us, and here it is this tool dialling out.
    rec.record(
        conn,
        EventKind::Note(format!(
            "tentativa {} — mandando {} bytes para {}",
            plan.attempt,
            plan.bytes.len(),
            plan.destination
        )),
    );

    match round(&plan, conn, rec) {
        Ok(outcome) => rec.report(outcome.headline, outcome.summary),
        Err(reason) => {
            rec.record(conn, EventKind::Error(reason.clone()));
            rec.report("falhou", reason);
        }
    }
}

fn round(plan: &Plan, conn: u64, rec: &Recorder) -> Result<Outcome, String> {
    let started = Instant::now();
    let mut socket = TcpStream::connect_timeout(&plan.address, plan.timeout)
        .map_err(|e| format!("não conectei em {}: {e}", plan.destination))?;
    // Without this the request sits in the kernel waiting for company, and the number
    // this tool exists to report — how long the answer took — comes back inflated.
    let _ = socket.set_nodelay(true);
    let _ = socket.set_read_timeout(Some(IDLE));
    let _ = socket.set_write_timeout(Some(plan.timeout));
    let connected = started.elapsed();

    let mut reply = Vec::new();
    let (first_byte, handshake) = match &plan.tls {
        None => (exchange(&mut socket, plan, conn, rec, &mut reply)?, None),
        Some(tls_plan) => {
            let client = tls::Client::new(&plan.destination, &tls_plan.sni, tls_plan.verify)?;
            let session = client.session()?;
            let mut stream = tls::OwnedStream::new(session, socket);
            let handshake_started = Instant::now();
            // The handshake happens on the first write; timing it separately is what
            // separates "the server is slow" from "TLS is slow".
            stream
                .flush()
                .map_err(|e| format!("handshake TLS falhou: {e}"))?;
            let handshake = handshake_started.elapsed();
            (
                exchange(&mut stream, plan, conn, rec, &mut reply)?,
                Some(handshake),
            )
        }
    };

    if reply.is_empty() {
        return Err("o destino não respondeu nada antes do tempo limite".to_string());
    }
    let status = status_line(&reply);
    if let Some(status) = &status {
        rec.record(conn, EventKind::Note(format!("resposta: {status}")));
    }
    // The reply's own address is worth passing on: from here the whole address menu is
    // one Ctrl+P away.
    rec.found(
        if plan.host.parse::<std::net::IpAddr>().is_ok() {
            "ip"
        } else {
            "dominio"
        },
        plan.host.clone(),
    );

    // One decimal, not zero: on loopback a round trip is a fraction of a millisecond,
    // and "0 ms" reads as a broken measurement rather than a fast one.
    let mut summary = format!("conectou em {:.1} ms", connected.as_secs_f64() * 1000.0);
    if let Some(handshake) = handshake {
        summary.push_str(&format!(
            " · TLS {:.1} ms",
            handshake.as_secs_f64() * 1000.0
        ));
    }
    summary.push_str(&format!(
        " · primeiro byte {:.1} ms · {} bytes · tentativa {}",
        first_byte.as_secs_f64() * 1000.0,
        reply.len(),
        plan.attempt
    ));
    Ok(Outcome {
        headline: status.unwrap_or_else(|| format!("{} bytes", reply.len())),
        summary,
    })
}

/// Writes the request and reads until the far side stops talking. Returns how long the
/// first byte took, which is the number worth comparing between repeats.
fn exchange<S: Read + Write>(
    stream: &mut S,
    plan: &Plan,
    conn: u64,
    rec: &Recorder,
    reply: &mut Vec<u8>,
) -> Result<Duration, String> {
    let sent = Instant::now();
    stream
        .write_all(&plan.bytes)
        .map_err(|e| format!("não consegui mandar: {e}"))?;
    let _ = stream.flush();
    // Recorded as relayed bytes rather than as a note: the log renders it exactly like a
    // tunnel's traffic, hex toggle and search included.
    rec.record_data(conn, Direction::ToTarget, &plan.bytes);

    let mut first_byte = None;
    let mut buf = vec![0u8; 16 * 1024];
    let deadline = Instant::now() + plan.timeout;
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if first_byte.is_none() {
                    first_byte = Some(sent.elapsed());
                }
                rec.record_data(conn, Direction::FromTarget, &buf[..n]);
                reply.extend_from_slice(&buf[..n]);
                if reply.len() >= MAX_REPLY {
                    rec.record(
                        conn,
                        EventKind::Note(format!(
                            "parei de ler em {MAX_REPLY} bytes — a resposta continua vindo"
                        )),
                    );
                    break;
                }
            }
            // The idle timeout is the normal end of a keep-alive reply, not a failure.
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
                if first_byte.is_some() || Instant::now() >= deadline || rec.stopping() {
                    break;
                }
            }
            Err(e) => return Err(format!("leitura falhou: {e}")),
        }
    }
    Ok(first_byte.unwrap_or_else(|| sent.elapsed()))
}

/// The first line of an HTTP reply, if it looks like one.
fn status_line(reply: &[u8]) -> Option<String> {
    let head = reply.get(..reply.len().min(200))?;
    let text = String::from_utf8_lossy(head);
    let line = text.lines().next()?.trim();
    line.starts_with("HTTP/").then(|| line.to_string())
}

/// The capture side: rebuilding whole requests out of a relay's stream of chunks.
///
/// A request is not a chunk. It arrives split across reads, several may share one
/// connection back to back, and a body only ends where `Content-Length` says it does —
/// so offering "repeat this" means framing the stream properly rather than grabbing
/// whatever the last `read` happened to return. Half a request repeated is not the
/// request, and would answer a question nobody asked.
pub struct Capture {
    buf: Vec<u8>,
    kept: usize,
    warned: bool,
}

/// Biggest request kept whole. Past this the offer is dropped rather than truncated —
/// an upload is not something anyone wants pre-filled in a text field anyway.
const MAX_CAPTURE: usize = 64 * 1024;

/// How many distinct requests one execution offers to repeat. A relay can see thousands;
/// a menu with thousands of entries is not a menu.
const MAX_KEPT: usize = 20;

/// Methods a chunk may start with to be a request worth keeping.
const METHODS: [&str; 9] = [
    "GET ", "POST ", "PUT ", "DELETE ", "PATCH ", "HEAD ", "OPTIONS ", "TRACE ", "CONNECT ",
];

impl Capture {
    pub fn new() -> Self {
        Self {
            buf: Vec::new(),
            kept: 0,
            warned: false,
        }
    }

    /// Feeds one relayed chunk. Records every complete request it can make out of what
    /// it has seen, and forgets anything it cannot frame.
    pub fn feed(&mut self, chunk: &[u8], rec: &Recorder) {
        if self.kept >= MAX_KEPT {
            if !self.warned {
                self.warned = true;
                rec.record(
                    0,
                    EventKind::Note(format!(
                        "guardei {MAX_KEPT} requisições para repetir — as próximas seguem sendo relaiadas e registradas, só não entram no menu do Ctrl+P"
                    )),
                );
            }
            return;
        }
        if self.buf.is_empty() && !starts_request(chunk) {
            return;
        }
        self.buf.extend_from_slice(chunk);

        while let Some(head_end) = find(&self.buf, b"\r\n\r\n").map(|i| i + 4) {
            let head = String::from_utf8_lossy(&self.buf[..head_end]).to_ascii_lowercase();
            // A chunked body ends where a terminator says it does, and repeating a head
            // without its body would leave the far side waiting for one. Not offered.
            if head.contains("transfer-encoding:") && head.contains("chunked") {
                self.buf.clear();
                return;
            }
            let total = head_end + content_length(&head).unwrap_or(0);
            if total > MAX_CAPTURE {
                self.buf.clear();
                return;
            }
            if self.buf.len() < total {
                return;
            }
            rec.found("requisicao", payload::encode(&self.buf[..total]));
            self.kept += 1;
            self.buf.drain(..total);
            if self.kept >= MAX_KEPT {
                return;
            }
        }
        // No complete head yet. Either more is coming, or this was never a request.
        if self.buf.len() > MAX_CAPTURE {
            self.buf.clear();
        }
    }
}

fn starts_request(chunk: &[u8]) -> bool {
    let start = String::from_utf8_lossy(&chunk[..chunk.len().min(8)]).to_string();
    METHODS.iter().any(|method| start.starts_with(method))
}

fn content_length(lowercased_head: &str) -> Option<usize> {
    lowercased_head
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|value| value.trim().parse::<usize>().ok())
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_length_decides_where_a_request_ends() {
        let head = "post /x http/1.1\r\ncontent-length: 12\r\n\r\n";
        assert_eq!(content_length(head), Some(12));
        assert_eq!(content_length("get /x http/1.1\r\n\r\n"), None);
    }

    #[test]
    fn only_a_request_starts_a_capture() {
        assert!(starts_request(b"GET /x HTTP/1.1\r\n"));
        assert!(starts_request(b"OPTIONS /x HTTP/1.1\r\n"));
        assert!(!starts_request(b"HTTP/1.1 200 OK\r\n"));
        assert!(!starts_request(b"\x16\x03\x01"));
    }
}
