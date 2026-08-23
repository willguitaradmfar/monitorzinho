//! Speaking TLS to the far side of a tunnel while the near side stays plaintext.
//!
//! This is the useful direction for debugging: the client connects to the tunnel over
//! plain TCP, the tunnel does the handshake with the real server, and everything the
//! recorder sees is the *decrypted* conversation. Point a plain `curl`, `psql` or
//! `redis-cli` at the local port and you get to read what would otherwise be
//! ciphertext, without touching the server or installing a CA anywhere.
//!
//! The relay here can't reuse the two-thread pump from `tunnel`: a rustls session is a
//! single state machine driving both directions, so it can't be split across a reader
//! thread and a writer thread. Instead one thread runs both directions over `poll(2)`,
//! which also means an idle connection costs nothing instead of waking up to spin.

use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::os::fd::AsRawFd;
use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{
    ClientConfig, ClientConnection, DigitallySignedStruct, RootCertStore, SignatureScheme,
};

use super::poll::{self, Fd};
use super::rewrite::{Rules, rewritten};
use super::{Direction, EventKind, Recorder};

/// Plaintext moved per round. Same size as the plain relay's buffer, so a bulk transfer
/// produces the same shape of log either way.
const RELAY_BUF: usize = 16 * 1024;

/// Everything needed to start TLS sessions toward one target: the config (roots,
/// versions, whether to verify at all) plus the name the certificate is checked
/// against. Built once when the execution starts, shared by every connection.
pub struct Client {
    config: Arc<ClientConfig>,
    name: ServerName<'static>,
}

impl Client {
    /// `sni` overrides the name taken from `target`, which is what you need when the
    /// target is an IP but the certificate names a host. `verify: false` accepts any
    /// certificate — a debugging escape hatch for self-signed and internal CAs, and the
    /// reason the wizard spells it out instead of hiding it behind a checkbox.
    pub fn new(target: &str, sni: &str, verify: bool) -> Result<Self, String> {
        let host = if sni.trim().is_empty() {
            host_of(target)
        } else {
            sni.trim()
        };
        let name = ServerName::try_from(host.to_string())
            .map_err(|_| format!("nome inválido para o certificado: {host}"))?;

        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let builder = ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .map_err(|e| format!("não consegui configurar TLS: {e}"))?;
        let config = if verify {
            builder
                .with_root_certificates(roots())
                .with_no_client_auth()
        } else {
            builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(AcceptAnything(provider)))
                .with_no_client_auth()
        };

        Ok(Self {
            config: Arc::new(config),
            name,
        })
    }

    /// A fresh session for one connection. Fails only on a bad configuration, never on
    /// anything network-related — the handshake itself happens inside `relay`.
    pub fn session(&self) -> Result<ClientConnection, String> {
        ClientConnection::new(self.config.clone(), self.name.clone())
            .map_err(|e| format!("não consegui iniciar a sessão TLS: {e}"))
    }
}

/// The system trust store plus the bundled Mozilla roots. Both, rather than one or the
/// other: the system store is what makes an internal CA work on a machine that already
/// trusts it, and the bundled set keeps public sites verifiable on a box where the
/// store is missing or unreadable.
fn roots() -> RootCertStore {
    let mut store = RootCertStore::empty();
    let native = rustls_native_certs::load_native_certs();
    store.add_parsable_certificates(native.certs);
    store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    store
}

/// The host part of `host:port`, brackets stripped if it's an IPv6 literal. Falls back
/// to the whole string, which then fails validation with the address in the message.
fn host_of(target: &str) -> &str {
    let target = target.trim();
    if let Some(rest) = target.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(target);
    }
    target
        .rsplit_once(':')
        .map(|(host, _)| host)
        .unwrap_or(target)
}

/// Accepts every certificate. Signature checking still runs, so the handshake is a real
/// handshake — what's skipped is deciding whether the peer is who it claims to be.
#[derive(Debug)]
struct AcceptAnything(Arc<CryptoProvider>);

impl ServerCertVerifier for AcceptAnything {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// Copies one connection between a plaintext client and a TLS server, recording the
/// decrypted bytes in both directions.
///
/// Backpressure is what keeps memory bounded: the client is only read while nothing is
/// still queued for the server, and plaintext is only pulled out of the session while
/// nothing is still queued for the client. A slow side therefore stalls its own reader
/// instead of growing a buffer.
pub fn relay(
    mut client: TcpStream,
    mut upstream: TcpStream,
    mut tls: ClientConnection,
    rules: &Rules,
    conn: u64,
    rec: &Recorder,
) {
    if client.set_nonblocking(true).is_err() || upstream.set_nonblocking(true).is_err() {
        rec.record(
            conn,
            EventKind::Error("não consegui configurar os sockets".to_string()),
        );
        return;
    }
    // Without this, Nagle holds each small write until the previous one is acknowledged
    // and the handshake's short flights collide with the peer's delayed ACK — worth
    // ~150 ms per connection, measured. A relay has no reason to coalesce anyway: it
    // never generates traffic of its own, it only passes on what someone already sent.
    // A relay never frames anything itself, it only passes on what someone else already
    // decided to send, so waiting to coalesce writes can only add delay. It costs
    // nothing on loopback, where this was measured, but a request/response protocol
    // over a real link is exactly what this tool gets pointed at.
    let _ = client.set_nodelay(true);
    let _ = upstream.set_nodelay(true);

    let mut buf = vec![0u8; RELAY_BUF];
    // Plaintext read out of the session and not yet accepted by the client socket.
    let mut pending: Vec<u8> = Vec::new();
    let mut sent = 0usize;
    let mut client_open = true;
    let mut upstream_open = true;
    let mut announced = false;
    // Whether the previous round moved bytes. If it did there may be more waiting in a
    // buffer rather than on a socket, so the next `poll` only collects readiness
    // instead of sleeping — that difference is the whole round-trip latency.
    let mut busy = true;

    while !rec.stopping() {
        if !announced && !tls.is_handshaking() {
            announced = true;
            rec.record(conn, EventKind::Note(handshake_summary(&tls)));
        }

        // Everything that needs no readiness runs first: plaintext already decrypted
        // inside the session, bytes already queued for a socket. Polling before draining
        // these would mean sleeping on a socket while the answer sits in memory.
        let mut progress = false;

        // Session → client, one chunk at a time so `pending` never grows past a read.
        // This is also where a closed server turns into EOF, which is why it has to run
        // before the loop can decide to wait.
        if pending.is_empty() {
            sent = 0;
            match tls.reader().read(&mut buf) {
                Ok(0) => upstream_open = false,
                Ok(n) => {
                    rec.record_data(conn, Direction::FromTarget, &buf[..n]);
                    pending.extend_from_slice(&buf[..n]);
                    progress = true;
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                Err(e) if e.kind() == ErrorKind::UnexpectedEof => upstream_open = false,
                Err(e) => {
                    rec.record(conn, EventKind::Error(format!("leitura falhou: {e}")));
                    upstream_open = false;
                }
            }
        }

        if sent < pending.len() {
            match client.write(&pending[sent..]) {
                Ok(0) => client_open = false,
                Ok(n) => {
                    sent += n;
                    progress = true;
                    if sent == pending.len() {
                        pending.clear();
                        sent = 0;
                    }
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                Err(e) => {
                    rec.record(conn, EventKind::Error(format!("escrita falhou: {e}")));
                    client_open = false;
                }
            }
        }

        // Session → server socket. Ends on `WouldBlock`, after which the poll below
        // waits for `POLLOUT` instead of retrying in a spin.
        while tls.wants_write() {
            match tls.write_tls(&mut upstream) {
                Ok(0) => break,
                Ok(_) => progress = true,
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) => {
                    rec.record(conn, EventKind::Error(format!("envio TLS falhou: {e}")));
                    upstream_open = false;
                    break;
                }
            }
        }

        // Nothing left to deliver, and nowhere left to get more.
        if !upstream_open && pending.is_empty() {
            break;
        }
        if !client_open && !upstream_open {
            break;
        }

        let client_wants = client_events(client_open, &tls, sent < pending.len());
        let upstream_wants = upstream_events(upstream_open, &tls);
        if client_wants == 0 && upstream_wants == 0 {
            break;
        }
        let mut fds = [
            Fd::new(client.as_raw_fd(), client_wants),
            Fd::new(upstream.as_raw_fd(), upstream_wants),
        ];

        // A round that moved bytes doesn't sleep: whatever comes next may already be in
        // a buffer rather than on the wire, and the poll is only here to collect
        // readiness. An idle round waits properly instead of spinning.
        let timeout = if busy || progress {
            0
        } else {
            poll::TIMEOUT_MS
        };
        busy = progress;
        if let Err(e) = poll::wait(&mut fds, timeout) {
            rec.record(conn, EventKind::Error(format!("poll falhou: {e}")));
            break;
        }

        // Client → session. On EOF the close_notify goes out before the loop stops
        // reading, so the server sees a clean shutdown rather than a dropped socket.
        if fds[0].watching(poll::IN) && fds[0].ready(poll::READABLE) {
            match client.read(&mut buf) {
                Ok(0) => {
                    client_open = false;
                    tls.send_close_notify();
                }
                Ok(n) => {
                    // Rewriting happens on the plaintext, before rustls encrypts it —
                    // the whole reason a rule can touch a TLS target at all.
                    let payload = rewritten(Some(rules), &buf[..n], Direction::ToTarget, conn, rec);
                    let _ = tls.writer().write_all(&payload);
                    busy = true;
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                Err(e) => {
                    rec.record(conn, EventKind::Error(format!("leitura falhou: {e}")));
                    client_open = false;
                    tls.send_close_notify();
                }
            }
        }

        // Server socket → session.
        if fds[1].watching(poll::IN) && fds[1].ready(poll::READABLE) {
            match tls.read_tls(&mut upstream) {
                Ok(0) => upstream_open = false,
                Ok(_) => busy = true,
                Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                Err(e) => {
                    rec.record(conn, EventKind::Error(format!("leitura TLS falhou: {e}")));
                    upstream_open = false;
                }
            }
            if let Err(e) = tls.process_new_packets() {
                // A rejected certificate lands here. rustls has an alert queued for the
                // server explaining why, so flush it before giving up.
                rec.record(conn, EventKind::Error(format!("TLS falhou: {e}")));
                let _ = tls.write_tls(&mut upstream);
                break;
            }
        }
    }

    let _ = client.shutdown(Shutdown::Both);
    let _ = upstream.shutdown(Shutdown::Both);
}

/// What the client socket is waiting for. Reading pauses while ciphertext is still
/// queued for the server, which is the backpressure toward the client.
fn client_events(open: bool, tls: &ClientConnection, has_pending: bool) -> i16 {
    let mut events = 0;
    if open && !tls.wants_write() {
        events |= poll::IN;
    }
    if has_pending {
        events |= poll::OUT;
    }
    events
}

fn upstream_events(open: bool, tls: &ClientConnection) -> i16 {
    let mut events = 0;
    if open && tls.wants_read() {
        events |= poll::IN;
    }
    if tls.wants_write() {
        events |= poll::OUT;
    }
    events
}

/// What the handshake actually settled on, recorded once per connection — the first
/// thing worth knowing when a TLS target misbehaves.
fn handshake_summary(tls: &ClientConnection) -> String {
    let version = tls
        .protocol_version()
        .map(|v| format!("{v:?}"))
        .unwrap_or_else(|| "versão desconhecida".to_string());
    match tls.negotiated_cipher_suite() {
        Some(suite) => format!("TLS estabelecido: {version}, {:?}", suite.suite()),
        None => format!("TLS estabelecido: {version}"),
    }
}

/// A TLS connection that owns both halves, so it can be handed to a `Session` the same
/// way a plain socket is. `rustls::Stream` borrows its session, which makes it unable to
/// live inside anything that outlives the call — this owns instead of borrowing.
pub struct OwnedStream {
    session: rustls::ClientConnection,
    socket: TcpStream,
}

impl OwnedStream {
    pub fn new(session: rustls::ClientConnection, socket: TcpStream) -> Self {
        Self { session, socket }
    }
}

impl Read for OwnedStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        rustls::Stream::new(&mut self.session, &mut self.socket).read(buf)
    }
}

impl Write for OwnedStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        rustls::Stream::new(&mut self.session, &mut self.socket).write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        rustls::Stream::new(&mut self.session, &mut self.socket).flush()
    }
}
