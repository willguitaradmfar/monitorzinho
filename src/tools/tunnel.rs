//! A netcat-shaped relay: listen on a local port, forward everything to another
//! host:port, and record both directions on the way through.
//!
//! The point isn't the forwarding — `socat` does that — it's the recording. Pointing a
//! client at the tunnel instead of straight at the server is the one way to read a
//! connection's actual payload without `CAP_NET_RAW`, because the bytes pass through
//! this process rather than past it. It also sees plaintext that packet capture
//! wouldn't: if the client speaks TLS to the tunnel this shows ciphertext like anything
//! else, but for the plain HTTP/Postgres/Redis/gRPC traffic that debugging usually
//! involves, this is the whole conversation.

use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;

use super::{Direction, EventKind, Execution, ParamSpec, Recorder, Tool};

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
        ]
    }

    fn summarize(&self, params: &HashMap<&'static str, String>) -> String {
        let get = |key| params.get(key).map(String::as_str).unwrap_or("?");
        format!("{} {} → {}", get("proto"), get("listen"), get("target"))
    }

    fn start(&self, id: u64, params: &HashMap<&'static str, String>) -> Result<Execution, String> {
        let proto = params.get("proto").map(String::as_str).unwrap_or("TCP");
        let listen = params.get("listen").map(String::as_str).unwrap_or_default();
        let target = params.get("target").map(String::as_str).unwrap_or_default();

        let listen_addr = resolve(listen, "endereço de escuta")?;
        // Resolved here purely to fail early on a typo'd host; the relay reconnects by
        // name so a target behind a changing DNS record still works.
        resolve(target, "destino")?;

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
                    serve_udp(socket, target, &recorder);
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
                    serve_tcp(listener, target, &recorder);
                    finished.store(true, Ordering::Relaxed);
                });
            }
        }

        Ok(execution)
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

fn serve_tcp(listener: TcpListener, target: String, rec: &Recorder) {
    rec.record(
        0,
        EventKind::Note(format!("túnel TCP no ar, encaminhando para {target}")),
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
                thread::spawn(move || {
                    relay_tcp(client, peer, &target, conn, &rec);
                    rec.stats().active.fetch_sub(1, Ordering::Relaxed);
                });
            }
            Err(e) if is_timeout(&e) => thread::sleep(POLL),
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
fn relay_tcp(client: TcpStream, peer: SocketAddr, target: &str, conn: u64, rec: &Recorder) {
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
            pump(upstream_r, client, Direction::FromTarget, conn, &rec);
        })
    };
    pump(client_r, upstream, Direction::ToTarget, conn, rec);
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
fn pump(mut from: TcpStream, mut to: TcpStream, dir: Direction, conn: u64, rec: &Recorder) {
    let _ = from.set_read_timeout(Some(POLL));
    let mut buf = vec![0u8; RELAY_BUF];
    while !rec.stopping() {
        match from.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                rec.record_data(conn, dir, &buf[..n]);
                if let Err(e) = to.write_all(&buf[..n]) {
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
fn serve_udp(socket: UdpSocket, target: String, rec: &Recorder) {
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
        rec.record_data(*conn, Direction::ToTarget, &buf[..n]);
        if let Err(e) = upstream.send(&buf[..n]) {
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
