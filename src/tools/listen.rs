//! A port that receives and writes down, and forwards nothing.
//!
//! The tunnel next door needs somewhere to send what it catches; this is for the case
//! where there is nowhere — a webhook you asked a provider to call, an OAuth redirect,
//! a device that POSTs somewhere every minute, a script somebody swears is sending the
//! right thing. What you want then isn't a relay, it's a wall with a microphone: accept
//! the connection, keep every byte, and answer with whatever keeps the sender happy.
//!
//! `nc -l` does the accepting part and then the bytes scroll past. What this adds is the
//! same log as every other execution here — searchable, hex-viewable, scrollable, still
//! there an hour later — plus an answer worth sending: a status line and a body, so the
//! caller sees a 200 and stops retrying, or sees the 500 you asked for and shows you
//! what it does about it.

use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;

use super::replay::Capture;
use super::{Direction, EventKind, Execution, ParamSpec, Recorder, Tool, poll};

/// How long an accept or receive waits before going around to check the stop flag.
const POLL: Duration = Duration::from_millis(200);
/// Read buffer. Same size as the tunnel's, so a body of any size logs the same way.
const BUF: usize = 16 * 1024;
/// How long a connection may stay open with nothing arriving before it's closed. A
/// receiver that keeps every idle connection is a receiver that runs out of threads.
const IDLE: Duration = Duration::from_secs(30);

const PROTOCOLS: &[&str] = &["TCP", "UDP"];
/// An HTTP reply — and so a body to put in it — is a TCP affair. A UDP receiver either
/// echoes the datagram or says nothing.
const TCP_ONLY: &[&str] = &[PROTOCOLS[0]];
const REPLIES: &[&str] = &[
    "HTTP 200", "HTTP 204", "HTTP 400", "HTTP 500", "eco", "nada",
];
/// The replies that carry a body to write. 204 is deliberately not among them — "no
/// content" is the one status that promises there is none — and neither are the two that
/// aren't HTTP at all.
const HAS_BODY: &[&str] = &[REPLIES[0], REPLIES[2], REPLIES[3]];

pub struct ListenTool;

impl Tool for ListenTool {
    fn id(&self) -> &'static str {
        "listen"
    }

    fn name(&self) -> &'static str {
        "Receptor de requisições"
    }

    fn description(&self) -> &'static str {
        "Ouve numa porta, grava tudo que chega e responde o que você mandar — para webhook, callback e dispositivo que só sabe enviar"
    }

    fn params(&self) -> Vec<ParamSpec> {
        vec![
            ParamSpec::choice(
                "proto",
                "Protocolo",
                PROTOCOLS,
                "TCP aceita conexões e responde; UDP recebe datagramas e nunca responde",
            ),
            ParamSpec::text(
                "listen",
                "Ouvir em",
                "0.0.0.0:8080",
                "0.0.0.0 aceita de qualquer lugar da rede — é o que um webhook de fora precisa. 127.0.0.1 só desta máquina",
            ),
            ParamSpec::choice(
                "resposta",
                "Responder com",
                REPLIES,
                "O que devolver a quem chamou. «eco» devolve os próprios bytes; «nada» fecha calado. Em UDP só «eco» e «nada» valem — não há requisição HTTP a responder",
            ),
            ParamSpec::text(
                "corpo",
                "Corpo da resposta",
                "",
                "Vai no corpo do HTTP. Vazio manda um corpo mínimo; use para devolver o JSON que o chamador espera",
            )
            .only_when("proto", TCP_ONLY)
            .only_when("resposta", HAS_BODY),
        ]
    }

    fn summarize(&self, params: &HashMap<&'static str, String>) -> String {
        let get = |key| params.get(key).map(String::as_str).unwrap_or("");
        let proto = get("proto");
        // What the row promises has to be what the execution does: an HTTP status is
        // not something a UDP receiver can send, and saying so on the row would be a
        // small lie repeated every time the list is drawn.
        let reply = match (proto, Reply::from(get("resposta"), "")) {
            (_, Reply::Silent) => "não responde".to_string(),
            ("UDP", Reply::Echo) => "ecoa cada datagrama".to_string(),
            ("UDP", _) => "não responde (UDP)".to_string(),
            (_, Reply::Echo) => "ecoa o que chegar".to_string(),
            (_, Reply::Http { status, .. }) => format!("responde {status}"),
        };
        format!("{proto} {} → {reply}", get("listen"))
    }

    fn columns(&self, execution: &Execution) -> (String, String) {
        let stats = &execution.stats;
        let total = stats.connections.load(Ordering::Relaxed);
        let noun = if total == 1 {
            "requisição"
        } else {
            "requisições"
        };
        (
            format!("{total} {noun}"),
            format!(
                "recebeu {} · respondeu {}",
                crate::format::human_bytes(stats.to_target.load(Ordering::Relaxed) as f64),
                crate::format::human_bytes(stats.from_target.load(Ordering::Relaxed) as f64)
            ),
        )
    }

    fn start(&self, id: u64, params: &HashMap<&'static str, String>) -> Result<Execution, String> {
        let get = |key| params.get(key).map(String::as_str).unwrap_or("");
        let listen = get("listen").trim();
        if listen.is_empty() {
            return Err("informe onde ouvir".to_string());
        }
        let address = listen
            .to_socket_addrs()
            .map_err(|e| format!("endereço de escuta inválido ({listen}): {e}"))?
            .next()
            .ok_or_else(|| format!("{listen} não resolveu para nenhum endereço"))?;
        let reply = Reply::from(get("resposta"), get("corpo"));

        let (execution, recorder) = Execution::new(id, self.name(), self.summarize(params));
        let finished = execution.finish_flag();

        match get("proto") {
            "UDP" => {
                let socket = UdpSocket::bind(address)
                    .map_err(|e| format!("não consegui ouvir em {address}: {e}"))?;
                socket
                    .set_read_timeout(Some(POLL))
                    .map_err(|e| format!("não consegui configurar o socket: {e}"))?;
                thread::spawn(move || {
                    receive_udp(socket, address, reply, &recorder);
                    finished.store(true, Ordering::Relaxed);
                });
            }
            _ => {
                let listener = TcpListener::bind(address)
                    .map_err(|e| format!("não consegui ouvir em {address}: {e}"))?;
                // Non-blocking so the accept loop notices the stop flag; without it a
                // receiver nobody ever calls could never be removed.
                listener
                    .set_nonblocking(true)
                    .map_err(|e| format!("não consegui configurar o socket: {e}"))?;
                thread::spawn(move || {
                    accept(listener, address, Arc::new(reply), &recorder);
                    finished.store(true, Ordering::Relaxed);
                });
            }
        }
        Ok(execution)
    }
}

/// What to send back. Everything here is chosen for one reason: a caller that gets an
/// answer it understands stops retrying, and a caller that gets the error you asked for
/// shows you what it does about errors.
enum Reply {
    Http {
        status: &'static str,
        body: String,
    },
    /// The bytes that arrived, straight back. What a "does anything reach it at all"
    /// test wants, and what a line protocol expects.
    Echo,
    /// Close without a word.
    Silent,
}

impl Reply {
    fn from(choice: &str, body: &str) -> Self {
        let body = body.to_string();
        match choice {
            "HTTP 204" => Reply::Http {
                status: "204 No Content",
                body: String::new(),
            },
            "HTTP 400" => Reply::Http {
                status: "400 Bad Request",
                body,
            },
            "HTTP 500" => Reply::Http {
                status: "500 Internal Server Error",
                body,
            },
            "eco" => Reply::Echo,
            "nada" => Reply::Silent,
            _ => Reply::Http {
                status: "200 OK",
                body,
            },
        }
    }

    /// The bytes to send back for a request that carried `received`.
    fn bytes(&self, received: &[u8]) -> Option<Vec<u8>> {
        match self {
            Reply::Silent => None,
            Reply::Echo => Some(received.to_vec()),
            Reply::Http { status, body } => {
                let body = if body.is_empty() && !status.starts_with("204") {
                    "recebido pelo monitorzinho\n"
                } else {
                    body
                };
                Some(
                    format!(
                        "HTTP/1.1 {status}\r\n\
                         Server: monitorzinho\r\n\
                         Content-Type: {}\r\n\
                         Content-Length: {}\r\n\
                         Connection: close\r\n\r\n{body}",
                        content_type(body),
                        body.len()
                    )
                    .into_bytes(),
                )
            }
        }
    }
}

/// JSON if it looks like JSON. A caller that asked for a webhook usually expects one,
/// and getting `text/plain` back is the sort of thing that costs an afternoon.
fn content_type(body: &str) -> &'static str {
    let trimmed = body.trim_start();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        "application/json"
    } else {
        "text/plain; charset=utf-8"
    }
}

fn accept(listener: TcpListener, address: SocketAddr, reply: Arc<Reply>, rec: &Recorder) {
    rec.record(
        0,
        EventKind::Note(format!(
            "recebendo em {address} — tudo que chegar fica gravado aqui"
        )),
    );
    let mut next_connection = 0u64;
    while !rec.stopping() {
        match listener.accept() {
            Ok((stream, peer)) => {
                next_connection += 1;
                let conn = next_connection;
                rec.stats.connections.fetch_add(1, Ordering::Relaxed);
                rec.stats.active.fetch_add(1, Ordering::Relaxed);
                rec.record(
                    conn,
                    EventKind::Opened {
                        peer: peer.to_string(),
                    },
                );
                let rec = rec.clone();
                let reply = Arc::clone(&reply);
                thread::spawn(move || {
                    let reason = serve(stream, conn, &reply, &rec);
                    rec.stats.active.fetch_sub(1, Ordering::Relaxed);
                    rec.record(conn, EventKind::Closed { reason });
                });
            }
            // Nothing waiting: the accept loop exists to notice the stop flag too.
            Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                thread::sleep(POLL);
            }
            Err(e) => {
                rec.record(0, EventKind::Error(format!("erro ao aceitar: {e}")));
                break;
            }
        }
    }
    rec.record(0, EventKind::Note("receptor encerrado".to_string()));
}

/// One connection: read what it has to say, write it down, answer, close.
fn serve(mut stream: TcpStream, conn: u64, reply: &Reply, rec: &Recorder) -> String {
    let _ = stream.set_read_timeout(Some(POLL));
    let mut received: Vec<u8> = Vec::new();
    // What arrives at a receiver is worth sending somewhere else: the webhook that came
    // in here is the one you want to point at your real service. The destination is the
    // one thing this cannot know, so that field is the only one left blank.
    let mut capture = Capture::new();
    let mut idle = Duration::ZERO;

    loop {
        if rec.stopping() {
            return "execução removida".to_string();
        }
        let mut buffer = [0u8; BUF];
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                idle = Duration::ZERO;
                rec.record_data(conn, Direction::ToTarget, &buffer[..read]);
                capture.feed(&buffer[..read], rec);
                received.extend_from_slice(&buffer[..read]);
                // An HTTP request ends at the blank line unless it carries a body, and
                // a caller that keeps the connection open waiting for an answer would
                // otherwise sit here until the idle timeout. Answering at the end of
                // the headers is what every HTTP server does.
                if is_complete(&received) {
                    break;
                }
            }
            Err(ref e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
                idle += POLL;
                if idle >= IDLE {
                    return "sem nada há 30s".to_string();
                }
                // Something already arrived and the sender has gone quiet: that's the
                // whole request, for anything that isn't HTTP.
                if !received.is_empty() {
                    break;
                }
            }
            Err(e) => return format!("erro ao ler: {e}"),
        }
    }

    if received.is_empty() {
        return "conectou e não disse nada".to_string();
    }
    if let Some(answer) = reply.bytes(&received) {
        // Written before the log line, so what the log shows is what actually left.
        match stream.write_all(&answer) {
            Ok(()) => {
                rec.record_data(conn, Direction::FromTarget, &answer);
                let _ = stream.flush();
            }
            Err(e) => return format!("erro ao responder: {e}"),
        }
    }
    // A polite close, so a caller waiting on the body sees the end rather than a reset.
    let _ = stream.shutdown(std::net::Shutdown::Both);
    format!(
        "{} recebidos",
        crate::format::human_bytes(received.len() as f64)
    )
}

/// Whether what's arrived is a whole HTTP request: headers complete, and as much body as
/// `Content-Length` promised. Anything that isn't HTTP never looks complete, and falls
/// through to the quiet-sender rule instead.
fn is_complete(received: &[u8]) -> bool {
    let Some(end) = find(received, b"\r\n\r\n").map(|at| at + 4) else {
        return false;
    };
    let headers = String::from_utf8_lossy(&received[..end]).to_ascii_lowercase();
    if !headers.starts_with("get ")
        && !headers.starts_with("post ")
        && !headers.starts_with("put ")
        && !headers.starts_with("patch ")
        && !headers.starts_with("delete ")
        && !headers.starts_with("head ")
        && !headers.starts_with("options ")
    {
        return false;
    }
    let expected: usize = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length:")?.trim().parse().ok())
        .unwrap_or(0);
    received.len() >= end + expected
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// UDP has nobody to answer and no connection to close: every datagram is its own event,
/// logged with where it came from.
fn receive_udp(socket: UdpSocket, address: SocketAddr, reply: Reply, rec: &Recorder) {
    // Echo is the one answer that means anything over UDP: there is no request to
    // reply to, but sending the bytes back proves the round trip in both directions,
    // which is exactly what someone testing a NAT or a firewall is after. An HTTP
    // status would be a reply to a request that was never made, so it is treated as
    // silence — and the row says so rather than claiming otherwise.
    let echo = matches!(reply, Reply::Echo);
    rec.record(
        0,
        EventKind::Note(format!(
            "recebendo datagramas em {address} — {}",
            if echo {
                "cada um volta ecoado para quem mandou"
            } else {
                "sem resposta"
            }
        )),
    );
    let mut senders: HashMap<String, u64> = HashMap::new();
    let mut buffer = vec![0u8; BUF];
    while !rec.stopping() {
        // Waiting on the descriptor rather than on the read timeout alone keeps an idle
        // receiver from waking up several times a second to find nothing.
        if !poll::readable(
            std::os::fd::AsRawFd::as_raw_fd(&socket),
            POLL.as_millis() as i32,
        ) {
            continue;
        }
        match socket.recv_from(&mut buffer) {
            Ok((read, peer)) => {
                let next = senders.len() as u64 + 1;
                let conn = *senders.entry(peer.to_string()).or_insert_with(|| {
                    rec.stats.connections.fetch_add(1, Ordering::Relaxed);
                    next
                });
                if senders.len() as u64 == conn {
                    rec.record(
                        conn,
                        EventKind::Opened {
                            peer: peer.to_string(),
                        },
                    );
                }
                rec.record_data(conn, Direction::ToTarget, &buffer[..read]);
                if echo {
                    match socket.send_to(&buffer[..read], peer) {
                        Ok(sent) => rec.record_data(conn, Direction::FromTarget, &buffer[..sent]),
                        Err(e) => rec.record(
                            conn,
                            EventKind::Error(format!("não consegui ecoar para {peer}: {e}")),
                        ),
                    }
                }
            }
            Err(ref e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {}
            Err(e) => {
                rec.record(0, EventKind::Error(format!("erro ao receber: {e}")));
                break;
            }
        }
    }
    rec.record(0, EventKind::Note("receptor encerrado".to_string()));
}
