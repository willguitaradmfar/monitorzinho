//! A netcat-shaped relay: listen on a local port, forward everything to another
//! host:port, and record both directions on the way through.
//!
//! The point isn't the forwarding — `socat` does that — it's the recording. Pointing a
//! client at the tunnel instead of straight at the server is the one way to read a
//! connection's actual payload without `CAP_NET_RAW`, because the bytes pass through
//! this process rather than past it. It also sees plaintext that packet capture
//! wouldn't: for the plain HTTP/Postgres/Redis/gRPC traffic that debugging usually
//! involves, this is the whole conversation.
//!
//! A TLS target goes one better. With TLS on, the client still speaks plain TCP to the
//! tunnel and the tunnel does the handshake with the server, so what gets recorded is
//! the decrypted conversation with a server that would otherwise only ever show
//! ciphertext. (Client-side TLS is a different thing and stays out of scope: a client
//! that speaks TLS *to* the tunnel just logs as ciphertext, since we have no
//! certificate it would trust.)

use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;

use super::poll;
use super::replay::Capture;
use super::rewrite::{Rules, rewritten};
use super::tls;
use super::{Direction, EventKind, Execution, Handoff, ParamSpec, Recorder, Tool, offers_from};

/// Relay buffer. Big enough that a bulk transfer isn't chopped into hundreds of log
/// events, small enough that an interactive protocol still logs message by message.
const RELAY_BUF: usize = 16 * 1024;
/// A UDP datagram can't exceed this, so one read always takes a whole one.
const DATAGRAM_BUF: usize = 64 * 1024;
/// How long a blocked socket read waits before re-checking the stop flag. Short enough
/// that removing an execution feels immediate, long enough not to spin a core.
const POLL: Duration = Duration::from_millis(200);
/// Distinct UDP sources tracked at once. UDP has no connections to close, so without a
/// cap a scanner spraying the port would spawn an unbounded number of reply threads.
const MAX_UDP_CLIENTS: usize = 64;

const PROTOCOLS: &[&str] = &["TCP", "UDP"];
/// Saved verbatim into `tools.json`, so these strings are part of the on-disk format:
/// change the wording and a saved execution silently falls back to the first option.
const TLS_MODES: &[&str] = &["não", "sim", "sim, sem validar certificado"];
/// What the listening side is: a relay to one fixed place, or a proxy that takes the
/// destination from each request.
const MODES: &[&str] = &["destino fixo", "proxy HTTP"];

pub struct TunnelTool;

impl Tool for TunnelTool {
    fn id(&self) -> &'static str {
        "tunnel"
    }

    fn name(&self) -> &'static str {
        "Túnel TCP/UDP"
    }

    fn description(&self) -> &'static str {
        "Ouve numa porta local e repassa tudo para outro host:porta, registrando o que trafega nos dois sentidos"
    }

    fn params(&self) -> Vec<ParamSpec> {
        vec![
            ParamSpec::choice(
                "modo",
                "Modo",
                MODES,
                "«destino fixo» encaminha tudo para um host:porta. «proxy HTTP» tira o destino de cada requisição — aponte http_proxy para cá e veja todos os destinos",
            ),
            ParamSpec::choice(
                "proto",
                "Protocolo",
                PROTOCOLS,
                "TCP relaia conexões; UDP relaia datagramas, um fluxo por origem",
            ),
            ParamSpec::text(
                "listen",
                "Ouvir em",
                "127.0.0.1:8080",
                "Onde apontar o cliente. 127.0.0.1 só aceita desta máquina; use 0.0.0.0 para expor na rede",
            ),
            ParamSpec::text(
                "target",
                "Encaminhar para",
                "127.0.0.1:5432",
                "Destino real, host:porta. Nomes são resolvidos agora, na criação",
            ),
            ParamSpec::choice(
                "tls",
                "TLS no destino",
                TLS_MODES,
                "Só para TCP: o cliente continua em texto puro e o túnel fala TLS com o destino, então o log mostra o conteúdo decifrado",
            ),
            ParamSpec::rules(
                "rewrite",
                "Regex/replace",
                "Regras aplicadas ao que o cliente manda, antes de sair para o destino. Enter abre a lista",
            ),
            ParamSpec::text(
                "sni",
                "Nome no certificado",
                "",
                "Só com TLS: nome enviado no SNI e conferido no certificado. Vazio usa o host do destino — preencha quando o destino for um IP",
            ),
        ]
    }

    fn summarize(&self, params: &HashMap<&'static str, String>) -> String {
        let get = |key| params.get(key).map(String::as_str).unwrap_or("?");
        let tls = match tls_mode(params) {
            TlsMode::Off => "",
            TlsMode::Verified => "TLS ",
            TlsMode::Unverified => "TLS(sem validar) ",
        };
        let rules = match super::rewrite::decode(get("rewrite")).len() {
            0 => String::new(),
            1 => "  ·  1 regra".to_string(),
            n => format!("  ·  {n} regras"),
        };
        if is_proxy(params) {
            return format!("proxy HTTP em {}{rules}", get("listen"));
        }
        format!(
            "{} {} → {tls}{}{rules}",
            get("proto"),
            get("listen"),
            get("target")
        )
    }

    fn columns(&self, execution: &Execution) -> (String, String) {
        let stats = &execution.stats;
        let (total, active) = (
            stats.connections.load(Ordering::Relaxed),
            stats.active.load(Ordering::Relaxed),
        );
        let (noun, adjective) = (
            if total == 1 { "conexão" } else { "conexões" },
            if active == 1 { "ativa" } else { "ativas" },
        );
        let headline = format!("{total} {noun} ({active} {adjective})");
        (
            headline,
            format!(
                "→{} ←{}",
                crate::format::human_bytes(stats.to_target.load(Ordering::Relaxed) as f64),
                crate::format::human_bytes(stats.from_target.load(Ordering::Relaxed) as f64)
            ),
        )
    }

    /// The usual offers, plus the destination for anything that came out of this
    /// tunnel's traffic.
    ///
    /// This is the override the trait describes: a captured request is a finding like
    /// any other, but *where to send it again* isn't in the request — it's in this
    /// tunnel's configuration, which only this tunnel knows. A proxy is the exception
    /// and stays blank: its destination is whatever each request asked for, not one
    /// place, so the field is left for the user rather than filled in wrong.
    fn handoffs(&self, execution: &Execution) -> Vec<Handoff> {
        let mut offers = offers_from(execution);
        let Some(spec) = execution.spec() else {
            return offers;
        };
        if spec
            .params
            .get("modo")
            .is_some_and(|mode| is_proxy_mode(mode))
        {
            return offers;
        }
        let Some(target) = spec.params.get("target").filter(|t| !t.trim().is_empty()) else {
            return offers;
        };
        for offer in offers.iter_mut().filter(|offer| offer.tool == "replay") {
            offer.params.push(("destino", target.clone()));
            if let Some(tls) = spec.params.get("tls") {
                offer.params.push(("tls", tls.clone()));
            }
            if let Some(sni) = spec.params.get("sni").filter(|s| !s.trim().is_empty()) {
                offer.params.push(("sni", sni.clone()));
            }
        }
        offers
    }

    fn start(&self, id: u64, params: &HashMap<&'static str, String>) -> Result<Execution, String> {
        let proto = params.get("proto").map(String::as_str).unwrap_or("TCP");
        let listen = params.get("listen").map(String::as_str).unwrap_or_default();
        let target = params.get("target").map(String::as_str).unwrap_or_default();
        let sni = params.get("sni").map(String::as_str).unwrap_or_default();
        let mode = tls_mode(params);
        // Compiled before anything is listening, so a bad pattern is an error on the
        // form rather than a rule that silently never matches.
        let rules = Arc::new(Rules::parse(
            params
                .get("rewrite")
                .map(String::as_str)
                .unwrap_or_default(),
        )?);

        let listen_addr = resolve(listen, "endereço de escuta")?;
        let proxy = is_proxy(params);
        if proxy {
            if proto == "UDP" {
                return Err("proxy HTTP só faz sentido em TCP".to_string());
            }
            if mode != TlsMode::Off {
                return Err(
                    "proxy HTTP não usa a opção de TLS: cada destino tem o seu, e o CONNECT passa cifrado de ponta a ponta"
                        .to_string(),
                );
            }
        } else {
            // Resolved here purely to fail early on a typo'd host; the relay reconnects
            // by name so a target behind a changing DNS record still works.
            resolve(target, "destino")?;
        }

        if mode != TlsMode::Off && proto == "UDP" {
            return Err("TLS só vale para TCP — para UDP, desligue a opção".to_string());
        }
        // Built now, before anything is listening, so a bad SNI or an unusable trust
        // store is an error in the form rather than a connection that fails later.
        let tls_client = match mode {
            TlsMode::Off => None,
            mode => Some(Arc::new(tls::Client::new(
                target,
                sni,
                mode == TlsMode::Verified,
            )?)),
        };

        let (execution, recorder) = Execution::new(id, self.name(), self.summarize(params));
        let finished = execution.finish_flag();
        let target = target.to_string();

        match proto {
            "UDP" => {
                let socket = UdpSocket::bind(listen_addr)
                    .map_err(|e| format!("não consegui ouvir em {listen_addr}: {e}"))?;
                socket
                    .set_read_timeout(Some(POLL))
                    .map_err(|e| format!("não consegui configurar o socket: {e}"))?;
                thread::spawn(move || {
                    serve_udp(socket, target, rules, &recorder);
                    finished.store(true, Ordering::Relaxed);
                });
            }
            _ => {
                let listener = TcpListener::bind(listen_addr)
                    .map_err(|e| format!("não consegui ouvir em {listen_addr}: {e}"))?;
                // Non-blocking so the accept loop can notice the stop flag; without it
                // a tunnel nobody ever connects to could never be removed.
                listener
                    .set_nonblocking(true)
                    .map_err(|e| format!("não consegui configurar o socket: {e}"))?;
                thread::spawn(move || {
                    if proxy {
                        serve_proxy(listener, rules, &recorder);
                    } else {
                        serve_tcp(listener, target, tls_client, rules, &recorder);
                    }
                    finished.store(true, Ordering::Relaxed);
                });
            }
        }

        Ok(execution)
    }
}

/// What to do with the connection to the target, once the wizard's wording is out of
/// the way.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TlsMode {
    Off,
    Verified,
    /// TLS with certificate checking turned off, for self-signed and internal CAs.
    Unverified,
}

/// Whether this execution is a proxy rather than a relay to a fixed target.
fn is_proxy(params: &HashMap<&'static str, String>) -> bool {
    params.get("modo").is_some_and(|mode| is_proxy_mode(mode))
}

fn is_proxy_mode(mode: &str) -> bool {
    mode == MODES[1]
}

fn tls_mode(params: &HashMap<&'static str, String>) -> TlsMode {
    match params.get("tls").map(String::as_str) {
        Some(mode) if mode == TLS_MODES[1] => TlsMode::Verified,
        Some(mode) if mode == TLS_MODES[2] => TlsMode::Unverified,
        _ => TlsMode::Off,
    }
}

/// Parses `host:port`, resolving a hostname if that's what it is. `what` names the
/// field in the error, since the wizard shows it verbatim next to the form.
fn resolve(addr: &str, what: &str) -> Result<SocketAddr, String> {
    if addr.trim().is_empty() {
        return Err(format!("informe o {what}"));
    }
    addr.to_socket_addrs()
        .map_err(|e| format!("{what} inválido ({addr}): {e}"))?
        .next()
        .ok_or_else(|| format!("{what} ({addr}) não resolveu para nenhum endereço"))
}

/// A read that returned empty-handed because the timeout expired, rather than because
/// anything went wrong — the loop should go around and check the stop flag.
fn is_timeout(err: &std::io::Error) -> bool {
    matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
}

fn serve_tcp(
    listener: TcpListener,
    target: String,
    tls_client: Option<Arc<tls::Client>>,
    rules: Arc<Rules>,
    rec: &Recorder,
) {
    let how = if tls_client.is_some() { " via TLS" } else { "" };
    rec.record(
        0,
        EventKind::Note(format!("túnel TCP no ar, encaminhando para {target}{how}")),
    );
    let mut next_conn: u64 = 0;
    while !rec.stopping() {
        match listener.accept() {
            Ok((client, peer)) => {
                next_conn += 1;
                let conn = next_conn;
                rec.stats().connections.fetch_add(1, Ordering::Relaxed);
                rec.stats().active.fetch_add(1, Ordering::Relaxed);
                let (target, rec) = (target.clone(), rec.clone());
                let (tls_client, rules) = (tls_client.clone(), rules.clone());
                thread::spawn(move || {
                    relay_tcp(
                        client,
                        peer,
                        &target,
                        tls_client.as_deref(),
                        &rules,
                        conn,
                        &rec,
                    );
                    rec.stats().active.fetch_sub(1, Ordering::Relaxed);
                });
            }
            // Nothing waiting. Sleeping here instead would charge every new connection
            // up to a full `POLL` before it was even accepted.
            Err(e) if is_timeout(&e) => {
                poll::readable(listener.as_raw_fd(), poll::TIMEOUT_MS);
            }
            Err(e) => {
                rec.record(0, EventKind::Error(format!("accept falhou: {e}")));
                break;
            }
        }
    }
    rec.record(0, EventKind::Note("túnel encerrado".to_string()));
}

/// Handles one accepted connection: dial the target, then copy in both directions until
/// either side hangs up.
/// The proxy: one listener, and the destination comes from each request.
///
/// A relay to a fixed target answers "what is this client saying to that server". A
/// proxy answers a bigger question — *everything* a client is saying, to everyone —
/// which is what you want when the client is a program you didn't write and the list of
/// hosts it talks to is the thing you're after.
///
/// Two shapes arrive here. A plain request carries an absolute URL (`GET
/// http://host/path`), and that one is readable: it is logged, rewritten if there are
/// rules, and forwarded with the request line put back into the form an origin server
/// expects. A `CONNECT host:port` is a tunnel request, and what flows after it is TLS
/// that we have no certificate to impersonate — so it is relayed byte for byte and the
/// log says which host it was for and how much crossed, which is the honest half of
/// what can be known without becoming a CA.
fn serve_proxy(listener: TcpListener, rules: Arc<Rules>, rec: &Recorder) {
    rec.record(
        0,
        EventKind::Note(
            "proxy HTTP no ar — aponte http_proxy/https_proxy do cliente para este endereço"
                .to_string(),
        ),
    );
    let mut next_conn: u64 = 0;
    while !rec.stopping() {
        match listener.accept() {
            Ok((client, peer)) => {
                next_conn += 1;
                let conn = next_conn;
                rec.stats().connections.fetch_add(1, Ordering::Relaxed);
                rec.stats().active.fetch_add(1, Ordering::Relaxed);
                let (rec, rules) = (rec.clone(), rules.clone());
                thread::spawn(move || {
                    proxy_connection(client, peer, &rules, conn, &rec);
                    rec.stats().active.fetch_sub(1, Ordering::Relaxed);
                });
            }
            Err(e) if is_timeout(&e) => {
                poll::readable(listener.as_raw_fd(), poll::TIMEOUT_MS);
            }
            Err(e) => {
                rec.record(0, EventKind::Error(format!("accept falhou: {e}")));
                break;
            }
        }
    }
    rec.record(0, EventKind::Note("proxy encerrado".to_string()));
}

/// Reads the first request off a proxied connection and hands it to whichever of the
/// two paths it belongs to.
fn proxy_connection(client: TcpStream, peer: SocketAddr, rules: &Rules, conn: u64, rec: &Recorder) {
    rec.record(
        conn,
        EventKind::Opened {
            peer: peer.to_string(),
        },
    );
    let _ = client.set_nodelay(true);
    let head = match read_head(&client) {
        Ok(head) if !head.is_empty() => head,
        Ok(_) => {
            rec.record(
                conn,
                EventKind::Closed {
                    reason: "conectou e não pediu nada".to_string(),
                },
            );
            return;
        }
        Err(e) => {
            rec.record(conn, EventKind::Error(format!("erro ao ler o pedido: {e}")));
            return;
        }
    };

    let request_line = String::from_utf8_lossy(&head)
        .lines()
        .next()
        .unwrap_or_default()
        .to_string();
    let mut parts = request_line.split_whitespace();
    let (method, target) = (
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
    );

    if method.eq_ignore_ascii_case("CONNECT") {
        proxy_connect(client, target, conn, rec);
        return;
    }
    proxy_plain(client, &head, &request_line, rules, conn, rec);
}

/// `CONNECT host:port` — open the pipe, say 200, and get out of the way.
fn proxy_connect(client: TcpStream, target: &str, conn: u64, rec: &Recorder) {
    let target = if target.contains(':') {
        target.to_string()
    } else {
        format!("{target}:443")
    };
    rec.record(
        conn,
        EventKind::Note(format!(
            "CONNECT {target} — daqui em diante é TLS de ponta a ponta, só o volume é visível"
        )),
    );
    let upstream = match TcpStream::connect(&target) {
        Ok(stream) => stream,
        Err(e) => {
            let _ = (&client).write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n");
            rec.record(
                conn,
                EventKind::Error(format!("não conectou em {target}: {e}")),
            );
            let _ = client.shutdown(Shutdown::Both);
            return;
        }
    };
    if (&client)
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .is_err()
    {
        return;
    }
    let _ = upstream.set_nodelay(true);
    pump_both(
        client,
        upstream,
        &Rules::default(),
        conn,
        rec,
        true,
        Capture::new(),
    );
}

/// A plain proxied request: readable, so it is read.
fn proxy_plain(
    client: TcpStream,
    head: &[u8],
    request_line: &str,
    rules: &Rules,
    conn: u64,
    rec: &Recorder,
) {
    let Some((host, port, path)) = absolute_target(request_line) else {
        let _ = (&client).write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n");
        rec.record(
            conn,
            EventKind::Error(format!("pedido que não é de proxy: {request_line}")),
        );
        let _ = client.shutdown(Shutdown::Both);
        return;
    };
    rec.record(
        conn,
        EventKind::Note(format!("{request_line}  →  {host}:{port}")),
    );

    let upstream = match TcpStream::connect((host.as_str(), port)) {
        Ok(stream) => stream,
        Err(e) => {
            let _ = (&client).write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n");
            rec.record(
                conn,
                EventKind::Error(format!("não conectou em {host}:{port}: {e}")),
            );
            let _ = client.shutdown(Shutdown::Both);
            return;
        }
    };
    let _ = upstream.set_nodelay(true);

    // The request line is rewritten from the absolute form a proxy receives to the
    // origin form a server expects — the one part of a proxied request that has to
    // change, and the reason this can't just be a byte pump from the first byte.
    let rest = head.split_at(request_line.len()).1;
    let mut forwarded = format!("{} {} HTTP/1.1", first_word(request_line), path).into_bytes();
    forwarded.extend_from_slice(rest);
    let forwarded = match rules.apply(&forwarded) {
        Some((rewritten, which)) => {
            rec.record_rewrite(conn, Direction::ToTarget, &forwarded, &rewritten, &which);
            rewritten
        }
        None => {
            rec.record_data(conn, Direction::ToTarget, &forwarded);
            forwarded
        }
    };
    if (&upstream).write_all(&forwarded).is_err() {
        let _ = client.shutdown(Shutdown::Both);
        return;
    }
    // The head already left, above, so the capture is primed with it: the body — and
    // any further request on the same kept-alive connection — arrives through the pump.
    let mut capture = Capture::new();
    capture.feed(&forwarded, rec);
    pump_both(client, upstream, rules, conn, rec, false, capture);
}

/// `GET http://host:port/path HTTP/1.1` → the three parts a connection needs.
fn absolute_target(request_line: &str) -> Option<(String, u16, String)> {
    let url = request_line.split_whitespace().nth(1)?;
    let rest = url.strip_prefix("http://")?;
    let (authority, path) = match rest.find('/') {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host.to_string(), port.parse().unwrap_or(80)),
        None => (authority.to_string(), 80),
    };
    (!host.is_empty()).then_some((host, port, path.to_string()))
}

fn first_word(line: &str) -> &str {
    line.split_whitespace().next().unwrap_or("GET")
}

/// Reads up to the end of the request head, which is where a proxy has to stop and
/// decide. Anything after it belongs to the body and is pumped like everything else.
fn read_head(client: &TcpStream) -> std::io::Result<Vec<u8>> {
    let mut head = Vec::with_capacity(1024);
    let mut byte = [0u8; 1];
    let mut client = client;
    while head.len() < 16 * 1024 {
        match client.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => {
                head.push(byte[0]);
                if head.ends_with(b"\r\n\r\n") || head.ends_with(b"\n\n") {
                    break;
                }
            }
            Err(ref e) if is_timeout(e) => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(head)
}

fn relay_tcp(
    client: TcpStream,
    peer: SocketAddr,
    target: &str,
    tls_client: Option<&tls::Client>,
    rules: &Rules,
    conn: u64,
    rec: &Recorder,
) {
    rec.record(
        conn,
        EventKind::Opened {
            peer: peer.to_string(),
        },
    );
    let upstream = match TcpStream::connect(target) {
        Ok(stream) => stream,
        Err(e) => {
            rec.record(
                conn,
                EventKind::Error(format!("não conectou em {target}: {e}")),
            );
            let _ = client.shutdown(Shutdown::Both);
            return;
        }
    };

    // A TLS session is one state machine for both directions, so it can't be split
    // between two pumps the way a pair of plain sockets can; `tls::relay` runs both
    // directions in this thread instead.
    if let Some(tls_client) = tls_client {
        match tls_client.session() {
            Ok(session) => tls::relay(client, upstream, session, rules, conn, rec),
            Err(e) => {
                rec.record(conn, EventKind::Error(e));
                let _ = client.shutdown(Shutdown::Both);
            }
        }
        rec.record(
            conn,
            EventKind::Closed {
                reason: "conexão encerrada".to_string(),
            },
        );
        return;
    }

    // Same reasoning as the TLS path: a relay only forwards what someone else framed,
    // so coalescing small writes can only add latency.
    let _ = client.set_nodelay(true);
    let _ = upstream.set_nodelay(true);

    pump_both(client, upstream, rules, conn, rec, false, Capture::new());
}

/// Runs both directions of a plain relay until either side is done. Shared by the fixed
/// relay and by both halves of the proxy, since "copy these two sockets into each other
/// and write down what crosses" is the same job however the pair was chosen.
fn pump_both(
    client: TcpStream,
    upstream: TcpStream,
    rules: &Rules,
    conn: u64,
    rec: &Recorder,
    opaque: bool,
    mut capture: Capture,
) {
    // Each direction needs its own handle on both sockets: one to read from, one to
    // write to, and `shutdown` on either end unblocks whichever side is still reading.
    let (Ok(client_r), Ok(upstream_r)) = (client.try_clone(), upstream.try_clone()) else {
        rec.record(
            conn,
            EventKind::Error("não consegui duplicar os sockets".to_string()),
        );
        return;
    };

    let back = {
        let rec = rec.clone();
        thread::spawn(move || {
            pump(
                upstream_r,
                client,
                Leg {
                    dir: Direction::FromTarget,
                    rules: None,
                    conn,
                    rec: &rec,
                    opaque,
                    capture: None,
                },
            );
        })
    };
    pump(
        client_r,
        upstream,
        Leg {
            dir: Direction::ToTarget,
            rules: Some(rules),
            conn,
            rec,
            opaque,
            capture: Some(&mut capture),
        },
    );
    let _ = back.join();

    rec.record(
        conn,
        EventKind::Closed {
            reason: "conexão encerrada".to_string(),
        },
    );
}

/// Copies one direction of a TCP connection, recording every chunk. Ends on EOF, on
/// error, or when the execution is stopped; either way it shuts both sockets down so
/// the opposite pump ends too instead of blocking forever on a half-dead connection.
/// One direction of one connection: what it carries, and everyone it has to tell.
struct Leg<'a> {
    dir: Direction,
    /// Only the client→target direction gets rules; they exist to fix up what the
    /// client sends.
    rules: Option<&'a Rules>,
    conn: u64,
    rec: &'a Recorder,
    /// Somebody else's TLS going past: nothing to rewrite, nothing worth keeping.
    opaque: bool,
    /// Only the client→target direction captures: what is worth repeating is what was
    /// asked, not what was answered.
    capture: Option<&'a mut Capture>,
}

fn pump(mut from: TcpStream, mut to: TcpStream, leg: Leg) {
    let Leg {
        dir,
        rules,
        conn,
        rec,
        opaque,
        mut capture,
    } = leg;
    let _ = from.set_read_timeout(Some(POLL));
    let mut buf = vec![0u8; RELAY_BUF];
    while !rec.stopping() {
        match from.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                // An opaque tunnel carries somebody else's TLS: there is nothing to
                // rewrite and nothing worth keeping, so the bytes go straight across and
                // only the counters move. The row still shows the volume; the log stays
                // readable instead of filling with ciphertext.
                let written = if opaque {
                    rec.count_only(dir, n);
                    to.write_all(&buf[..n])
                } else {
                    let payload = rewritten(rules, &buf[..n], dir, conn, rec);
                    // Fed what actually left, rules already applied: repeating the
                    // request the target received is the only repeat that means
                    // anything.
                    if let Some(capture) = capture.as_deref_mut() {
                        capture.feed(&payload, rec);
                    }
                    to.write_all(&payload)
                };
                if let Err(e) = written {
                    rec.record(conn, EventKind::Error(format!("escrita falhou: {e}")));
                    break;
                }
            }
            Err(e) if is_timeout(&e) => continue,
            Err(e) => {
                rec.record(conn, EventKind::Error(format!("leitura falhou: {e}")));
                break;
            }
        }
    }
    let _ = from.shutdown(Shutdown::Both);
    let _ = to.shutdown(Shutdown::Both);
}

/// UDP has no connections, so a "flow" here is just everything arriving from one source
/// address. Each source gets its own upstream socket (so the target sees them apart)
/// plus a thread carrying replies back.
fn serve_udp(socket: UdpSocket, target: String, rules: Arc<Rules>, rec: &Recorder) {
    rec.record(
        0,
        EventKind::Note(format!("túnel UDP no ar, encaminhando para {target}")),
    );
    let mut clients: HashMap<SocketAddr, (u64, UdpSocket)> = HashMap::new();
    let mut next_conn: u64 = 0;
    let mut buf = vec![0u8; DATAGRAM_BUF];

    while !rec.stopping() {
        let (n, peer) = match socket.recv_from(&mut buf) {
            Ok(received) => received,
            Err(e) if is_timeout(&e) => continue,
            Err(e) => {
                rec.record(0, EventKind::Error(format!("recv falhou: {e}")));
                break;
            }
        };

        if !clients.contains_key(&peer) {
            if clients.len() >= MAX_UDP_CLIENTS {
                rec.record(
                    0,
                    EventKind::Note(format!(
                        "limite de {MAX_UDP_CLIENTS} origens atingido — datagrama de {peer} descartado"
                    )),
                );
                continue;
            }
            match open_udp_flow(&socket, &target, peer, next_conn + 1, rec) {
                Some(upstream) => {
                    next_conn += 1;
                    rec.stats().connections.fetch_add(1, Ordering::Relaxed);
                    rec.stats().active.fetch_add(1, Ordering::Relaxed);
                    rec.record(
                        next_conn,
                        EventKind::Opened {
                            peer: peer.to_string(),
                        },
                    );
                    clients.insert(peer, (next_conn, upstream));
                }
                None => continue,
            }
        }

        let (conn, upstream) = &clients[&peer];
        let payload = rewritten(Some(&rules), &buf[..n], Direction::ToTarget, *conn, rec);
        if let Err(e) = upstream.send(&payload) {
            rec.record(*conn, EventKind::Error(format!("envio falhou: {e}")));
        }
    }
    rec.record(0, EventKind::Note("túnel encerrado".to_string()));
}

/// Sets up the return path for a newly seen UDP source: an ephemeral socket connected
/// to the target, and a thread that pushes whatever comes back to that source.
/// `None` if either socket operation fails, which is recorded before returning.
fn open_udp_flow(
    listener: &UdpSocket,
    target: &str,
    peer: SocketAddr,
    conn: u64,
    rec: &Recorder,
) -> Option<UdpSocket> {
    let bind_addr = if peer.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let setup = || -> std::io::Result<(UdpSocket, UdpSocket)> {
        let upstream = UdpSocket::bind(bind_addr)?;
        upstream.connect(target)?;
        upstream.set_read_timeout(Some(POLL))?;
        let reply = upstream.try_clone()?;
        Ok((upstream, reply))
    };
    let (upstream, reply) = match setup() {
        Ok(pair) => pair,
        Err(e) => {
            rec.record(
                conn,
                EventKind::Error(format!("não abriu fluxo para {target}: {e}")),
            );
            return None;
        }
    };
    let Ok(back) = listener.try_clone() else {
        rec.record(
            conn,
            EventKind::Error("não consegui duplicar o socket de escuta".to_string()),
        );
        return None;
    };

    let rec = rec.clone();
    thread::spawn(move || {
        let mut buf = vec![0u8; DATAGRAM_BUF];
        while !rec.stopping() {
            match reply.recv(&mut buf) {
                Ok(n) => {
                    rec.record_data(conn, Direction::FromTarget, &buf[..n]);
                    if let Err(e) = back.send_to(&buf[..n], peer) {
                        rec.record(conn, EventKind::Error(format!("resposta falhou: {e}")));
                        break;
                    }
                }
                Err(e) if is_timeout(&e) => continue,
                Err(e) => {
                    rec.record(conn, EventKind::Error(format!("recv falhou: {e}")));
                    break;
                }
            }
        }
        rec.stats().active.fetch_sub(1, Ordering::Relaxed);
    });
    Some(upstream)
}
