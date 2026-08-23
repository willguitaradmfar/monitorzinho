//! What a mail server says about itself before anyone tries to send anything through it.
//!
//! The certificate reader already speaks STARTTLS, so "is the certificate fine" is
//! answered next door. What is left is everything else a mail server announces and
//! nobody checks: which extensions it offers, whether it will take a password over a
//! plaintext connection, how big a message it accepts, whether the name it greets you
//! with is its own — and whether it will relay mail for a stranger, which is the one
//! question whose wrong answer ends up on a blocklist.
//!
//! Nothing is ever sent. The relay test stops at `RCPT TO` and resets: that is the point
//! where a server has already decided, and going further would mean actually mailing
//! somebody.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use super::tls;
use super::{EventKind, Execution, ParamSpec, Recorder, Tool};

const TLS_MODES: &[&str] = &["automático", "STARTTLS", "TLS direto", "sem TLS"];
const YES_NO: &[&str] = &["sim", "não"];
/// The address the relay test claims to be from and to. Both are in reserved domains
/// that can never exist, so a server that accepts them is accepting anything.
const RELAY_FROM: &str = "probe@monitorzinho.invalid";
const RELAY_TO: &str = "relay-test@example.com";

pub struct SmtpTool;

impl Tool for SmtpTool {
    fn id(&self) -> &'static str {
        "smtp"
    }

    fn name(&self) -> &'static str {
        "Sonda SMTP"
    }

    fn description(&self) -> &'static str {
        "Conversa com um servidor de e-mail: banner, extensões, exigência de TLS e de senha, tamanho aceito e se ele repassa e-mail de estranho"
    }

    fn params(&self) -> Vec<ParamSpec> {
        vec![
            ParamSpec::text(
                "alvo",
                "Servidor",
                "smtp.gmail.com",
                "Host do servidor de e-mail. Um MX que a investigação DNS achou serve",
            ),
            ParamSpec::text(
                "porta",
                "Porta",
                "587",
                "25 entre servidores, 587 submissão com STARTTLS, 465 TLS direto",
            ),
            ParamSpec::choice(
                "tls",
                "TLS",
                TLS_MODES,
                "«automático» usa TLS direto na 465 e STARTTLS nas outras, quando oferecido",
            ),
            ParamSpec::choice(
                "relay",
                "Testar relay aberto",
                YES_NO,
                "Tenta enviar de um domínio inexistente para outro externo e para no RCPT — nada é enviado. Servidor bem configurado recusa",
            ),
            ParamSpec::text(
                "timeout",
                "Tempo limite (ms)",
                "8000",
                "Vale para conectar e para cada resposta do servidor",
            ),
        ]
    }

    fn summarize(&self, params: &HashMap<&'static str, String>) -> String {
        let get = |key| params.get(key).map(String::as_str).unwrap_or("").trim();
        format!("{}:{}", get("alvo"), get("porta"))
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
                "pronto para conversar com {} ({}). Nada roda até você abrir",
                plan.host, plan.address
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
            probe(plan, &recorder);
            recorder.ran();
            finished.store(true, Ordering::Relaxed);
        });
    }

    fn columns(&self, execution: &Execution) -> (String, String) {
        execution.outcome()
    }
}

struct Plan {
    host: String,
    address: SocketAddr,
    tls: String,
    relay: bool,
    timeout: Duration,
}

impl Plan {
    fn from(params: &HashMap<&'static str, String>) -> Result<Self, String> {
        let get = |key| params.get(key).map(String::as_str).unwrap_or("").trim();
        let host = get("alvo").to_string();
        if host.is_empty() {
            return Err("informe o servidor".to_string());
        }
        let port: u16 = match get("porta") {
            "" => 587,
            text => text
                .parse()
                .map_err(|_| format!("porta: «{text}» não é um número"))?,
        };
        let address = (host.as_str(), port)
            .to_socket_addrs()
            .map_err(|e| format!("não consegui resolver {host}: {e}"))?
            .next()
            .ok_or_else(|| format!("{host} não resolveu para nenhum endereço"))?;
        Ok(Self {
            host,
            address,
            tls: get("tls").to_string(),
            relay: get("relay") == "sim",
            timeout: Duration::from_millis(
                get("timeout")
                    .parse::<u64>()
                    .unwrap_or(8000)
                    .clamp(500, 60_000),
            ),
        })
    }

    /// Whether to wrap the connection in TLS before saying anything, as port 465 wants.
    fn direct_tls(&self) -> bool {
        self.tls == "TLS direto" || (self.tls == "automático" && self.address.port() == 465)
    }

    fn wants_starttls(&self) -> bool {
        match self.tls.as_str() {
            "STARTTLS" => true,
            "automático" => !self.direct_tls(),
            _ => false,
        }
    }
}

fn note(rec: &Recorder, text: impl Into<String>) {
    rec.record(0, EventKind::Note(text.into()));
}

fn section(rec: &Recorder, title: &str) {
    note(rec, format!("── {title} ──"));
    rec.report("conversando…", title.to_string());
}

fn field(rec: &Recorder, label: &str, value: impl AsRef<str>) {
    let value = value.as_ref();
    if !value.is_empty() {
        note(rec, format!("  {label:<24}{value}"));
    }
}

/// One SMTP conversation, over whichever transport the plan asked for.
///
/// The two transports differ only in what the bytes travel through, so the dialogue is
/// written once against `Read + Write` and handed either a socket or a TLS stream.
fn probe(plan: Plan, rec: &Recorder) {
    let started = Instant::now();
    rec.found("dominio", plan.host.clone());
    rec.found("ip", plan.address.ip().to_string());
    note(
        rec,
        format!("conversando com {} ({})", plan.host, plan.address),
    );

    let socket = match TcpStream::connect_timeout(&plan.address, plan.timeout) {
        Ok(socket) => socket,
        Err(e) => {
            rec.record(
                0,
                EventKind::Error(format!("não conectou em {}: {e}", plan.address)),
            );
            rec.report("não conectou", format!("{e}"));
            return;
        }
    };
    let _ = socket.set_read_timeout(Some(plan.timeout));
    let _ = socket.set_write_timeout(Some(plan.timeout));
    let _ = socket.set_nodelay(true);

    let mut findings: Vec<String> = Vec::new();
    let outcome = if plan.direct_tls() {
        match upgrade(socket, &plan, rec, "direto na conexão") {
            Ok((mut session, mut _keep)) => {
                let capabilities = greet_and_ehlo(&mut session, &plan, rec, true, &mut findings);
                finish(&mut session, &plan, rec, &mut findings, capabilities, true)
            }
            Err(message) => {
                rec.record(0, EventKind::Error(message.clone()));
                rec.report("TLS falhou", message);
                return;
            }
        }
    } else {
        let mut session = Session::new(socket, plan.timeout);
        let capabilities = greet_and_ehlo(&mut session, &plan, rec, false, &mut findings);
        let offered = capabilities.iter().any(|c| c.starts_with("STARTTLS"));
        field(
            rec,
            "STARTTLS",
            if offered {
                "oferecido"
            } else {
                "NÃO oferecido — esta conexão só existe em texto puro"
            },
        );
        if !offered {
            findings.push(
                "não oferece STARTTLS: tudo que passar por aqui, inclusive senha, vai em claro"
                    .to_string(),
            );
        }
        if capabilities.iter().any(|c| c.starts_with("AUTH")) {
            findings.push(
                "aceita AUTH antes de qualquer TLS — uma senha enviada aqui viaja legível"
                    .to_string(),
            );
        }

        if offered && plan.wants_starttls() {
            match session.command("STARTTLS") {
                Ok(reply) if reply.code == 220 => {
                    // Everything after this has to be encrypted — the server is waiting
                    // for a handshake, and a plaintext command here gets the connection
                    // reset, which is what it did before this was written properly.
                    match upgrade(session.into_inner(), &plan, rec, "por STARTTLS") {
                        Ok((mut session, mut _keep)) => {
                            // A second EHLO, because the extension list changes once the
                            // connection is private — AUTH usually only appears here.
                            let capabilities =
                                greet_and_ehlo(&mut session, &plan, rec, true, &mut findings);
                            finish(&mut session, &plan, rec, &mut findings, capabilities, true)
                        }
                        Err(message) => {
                            findings.push(message.clone());
                            format!("STARTTLS falhou: {message}")
                        }
                    }
                }
                Ok(reply) => {
                    findings.push(format!(
                        "STARTTLS anunciado mas recusado com {} — anúncio que não se cumpre",
                        reply.code
                    ));
                    finish(&mut session, &plan, rec, &mut findings, capabilities, false)
                }
                Err(e) => {
                    findings.push(format!("STARTTLS falhou: {e}"));
                    "STARTTLS falhou".to_string()
                }
            }
        } else {
            finish(&mut session, &plan, rec, &mut findings, capabilities, false)
        }
    };

    section(rec, "Avaliação");
    if findings.is_empty() {
        note(rec, "  nada a apontar");
    }
    for finding in &findings {
        rec.record(0, EventKind::Error(format!("  {finding}")));
    }
    note(
        rec,
        format!(
            "conversa concluída em {:.1}s",
            started.elapsed().as_secs_f64()
        ),
    );
    rec.report(
        outcome,
        match findings.len() {
            0 => plan.host.clone(),
            n => format!("{n} alerta(s) · {}", plan.host),
        },
    );
}

/// Hands a connected socket to TLS and gives back a session speaking through it.
///
/// The `rustls` session has to outlive the stream that borrows it, so it comes back
/// alongside — the caller keeps it alive without ever touching it.
fn upgrade(
    socket: TcpStream,
    plan: &Plan,
    rec: &Recorder,
    how: &str,
) -> Result<(Session<tls::OwnedStream>, ()), String> {
    let client = super::tls::Client::new(
        &format!("{}:{}", plan.host, plan.address.port()),
        &plan.host,
        true,
    )?;
    let mut session = client.session()?;
    let mut socket = socket;
    session
        .complete_io(&mut socket)
        .map_err(|e| format!("handshake TLS falhou: {e}"))?;
    field(
        rec,
        "TLS",
        format!(
            "{how} — {} · {}",
            session
                .protocol_version()
                .map(|v| format!("{v:?}"))
                .unwrap_or_default(),
            session
                .negotiated_cipher_suite()
                .map(|s| format!("{:?}", s.suite()))
                .unwrap_or_default()
        ),
    );
    Ok((
        Session::new(tls::OwnedStream::new(session, socket), plan.timeout),
        (),
    ))
}

/// Reads the greeting (when there is one to read) and asks for the extension list.
fn greet_and_ehlo<S: Read + Write>(
    session: &mut Session<S>,
    plan: &Plan,
    rec: &Recorder,
    after_tls: bool,
    findings: &mut Vec<String>,
) -> Vec<String> {
    if !after_tls {
        section(rec, "Apresentação");
        let greeting = match session.read_reply() {
            Ok(reply) => reply,
            Err(e) => {
                rec.record(0, EventKind::Error(format!("sem saudação: {e}")));
                findings.push(format!("sem saudação: {e}"));
                return Vec::new();
            }
        };
        field(rec, "Saudação", greeting.text.trim());
        if greeting.code != 220 {
            findings.push(format!(
                "saudação com código {} — o servidor não está aceitando conexões",
                greeting.code
            ));
            return Vec::new();
        }
        // The name a server greets with is supposed to be its own, and a mismatch is
        // the first sign of a relay nobody finished configuring.
        let announced = greeting.text.split_whitespace().nth(1).unwrap_or_default();
        if !announced.is_empty() && !announced.eq_ignore_ascii_case(&plan.host) {
            field(rec, "Nome anunciado", announced);
        }
    }

    section(
        rec,
        if after_tls {
            "Extensões (EHLO, já com TLS)"
        } else {
            "Extensões (EHLO)"
        },
    );
    let ehlo = match session.command("EHLO monitorzinho.local") {
        Ok(reply) => reply,
        Err(e) => {
            rec.record(0, EventKind::Error(format!("EHLO falhou: {e}")));
            findings.push(format!("EHLO falhou: {e}"));
            return Vec::new();
        }
    };
    let capabilities = capabilities_of(&ehlo.text);
    report_capabilities(rec, &capabilities);
    capabilities
}

/// The relay test, the size line and QUIT — everything that happens once the connection
/// is as private as it is going to get.
fn finish<S: Read + Write>(
    session: &mut Session<S>,
    plan: &Plan,
    rec: &Recorder,
    findings: &mut Vec<String>,
    capabilities: Vec<String>,
    secured: bool,
) -> String {
    if secured && !capabilities.iter().any(|c| c.starts_with("AUTH")) {
        field(
            rec,
            "AUTH",
            "não oferecido nem com TLS — este não é um servidor de submissão",
        );
    } else if secured {
        field(
            rec,
            "AUTH",
            capabilities
                .iter()
                .find(|c| c.starts_with("AUTH"))
                .cloned()
                .unwrap_or_default(),
        );
    }
    if plan.relay {
        section(rec, "Relay");
        relay_test(session, rec, findings);
    }
    let _ = session.command("QUIT");

    if let Some(size) = capabilities
        .iter()
        .find_map(|c| c.strip_prefix("SIZE "))
        .and_then(|size| size.trim().parse::<u64>().ok())
    {
        field(
            rec,
            "Maior mensagem",
            crate::format::human_bytes(size as f64),
        );
    }
    match (secured, findings.len()) {
        (true, 0) => "ok, com TLS".to_string(),
        (false, 0) => "ok, sem TLS".to_string(),
        (_, n) => format!("{n} alerta(s)"),
    }
}

/// Asks the server to accept mail from a domain that cannot exist, for a recipient it
/// has no business accepting. Stops at `RCPT TO` and resets — the decision has already
/// been made by then, and going on would mean actually sending somebody an email.
fn relay_test<S: Read + Write>(
    session: &mut Session<S>,
    rec: &Recorder,
    findings: &mut Vec<String>,
) {
    field(rec, "Testando", format!("{RELAY_FROM} → {RELAY_TO}"));
    match session.command(&format!("MAIL FROM:<{RELAY_FROM}>")) {
        Ok(reply) if reply.code == 250 => {
            field(rec, "MAIL FROM", format!("{} — aceito", reply.code));
        }
        Ok(reply) => {
            field(
                rec,
                "MAIL FROM",
                format!("{} — recusado: {}", reply.code, reply.text.trim()),
            );
            let _ = session.command("RSET");
            return;
        }
        Err(e) => {
            findings.push(format!("teste de relay falhou: {e}"));
            return;
        }
    }
    match session.command(&format!("RCPT TO:<{RELAY_TO}>")) {
        Ok(reply) if (200..300).contains(&reply.code) => {
            field(rec, "RCPT TO", format!("{} — ACEITO", reply.code));
            findings.push(
                "RELAY ABERTO: aceitou destinatário externo vindo de domínio inexistente — este servidor será usado para spam e acaba em blocklist"
                    .to_string(),
            );
        }
        Ok(reply) => field(
            rec,
            "RCPT TO",
            format!(
                "{} — recusado, que é o esperado: {}",
                reply.code,
                reply.text.trim()
            ),
        ),
        Err(e) => findings.push(format!("teste de relay falhou no RCPT: {e}")),
    }
    let _ = session.command("RSET");
}

/// The extension lines of an EHLO reply, without the code and the separator.
fn capabilities_of(text: &str) -> Vec<String> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let rest = line.get(4..)?.trim();
            (!rest.is_empty()).then(|| rest.to_string())
        })
        .collect()
}

fn report_capabilities(rec: &Recorder, capabilities: &[String]) {
    if capabilities.is_empty() {
        field(rec, "Extensões", "nenhuma anunciada");
        return;
    }
    for capability in capabilities {
        note(rec, format!("      {capability}"));
    }
}

/// One reply: its code and everything the server said, multi-line replies included.
struct Reply {
    code: u16,
    text: String,
}

/// A line-oriented SMTP conversation over anything that reads and writes.
struct Session<S> {
    stream: BufReader<S>,
    timeout: Duration,
}

impl<S: Read + Write> Session<S> {
    fn new(stream: S, timeout: Duration) -> Self {
        Self {
            stream: BufReader::new(stream),
            timeout,
        }
    }

    /// The socket back, for the STARTTLS upgrade. Anything still buffered is dropped,
    /// which is correct here: the server has just said 220 and is waiting for a
    /// handshake, so there is nothing after it to lose.
    fn into_inner(self) -> S {
        self.stream.into_inner()
    }

    fn command(&mut self, line: &str) -> Result<Reply, String> {
        self.stream
            .get_mut()
            .write_all(format!("{line}\r\n").as_bytes())
            .map_err(|e| format!("erro ao enviar «{line}»: {e}"))?;
        self.stream
            .get_mut()
            .flush()
            .map_err(|e| format!("erro ao enviar «{line}»: {e}"))?;
        self.read_reply()
    }

    /// Reads a reply, following the continuation rule: `250-` means more lines follow,
    /// `250 ` means that was the last.
    fn read_reply(&mut self) -> Result<Reply, String> {
        let deadline = Instant::now() + self.timeout;
        let mut text = String::new();
        let mut code = 0u16;
        loop {
            if Instant::now() > deadline {
                return Err("o servidor não terminou a resposta".to_string());
            }
            let mut line = String::new();
            let read = self
                .stream
                .read_line(&mut line)
                .map_err(|e| format!("erro ao ler: {e}"))?;
            if read == 0 {
                return Err("o servidor fechou a conexão".to_string());
            }
            code = line
                .get(..3)
                .and_then(|code| code.parse().ok())
                .unwrap_or(code);
            let more = line.as_bytes().get(3) == Some(&b'-');
            text.push_str(&line);
            if !more {
                return Ok(Reply { code, text });
            }
        }
    }
}
