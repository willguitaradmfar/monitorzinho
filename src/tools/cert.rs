//! Everything a TLS certificate says about itself, from a host name or a bare address.
//!
//! `openssl s_client` will show you the same bytes, and then you read the DER dump
//! yourself. What this does is the reading: the whole chain the server sent, each
//! certificate's names, dates, key, usages and revocation endpoints laid out in order,
//! the two questions a browser actually asks (does the name match, does the chain lead
//! to a root this machine trusts) answered separately, and a closing list of what is
//! wrong with it — expiry, self-signature, a weak key, a name it doesn't cover.
//!
//! It connects twice on purpose. The first handshake accepts anything, because a
//! certificate that fails verification is precisely the one worth reading; the second
//! verifies properly, and its error message is the verdict. Nothing runs until the
//! execution is opened, like the scanner and the DNS sweep.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use super::x509::Cert;
use super::{EventKind, Execution, Handoff, ParamSpec, Recorder, Tool, x509};

/// Certificates issued for longer than this stopped being accepted by browsers in 2020.
/// One that still has it was issued by something that isn't watching.
const MAX_LIFETIME_DAYS: i64 = 398;
/// Below this, renewal is no longer "someday".
const RENEW_SOON_DAYS: i64 = 30;
/// An RSA key smaller than this hasn't been acceptable for a decade.
const WEAK_RSA_BITS: usize = 2048;

const STARTTLS: &[&str] = &["não", "smtp", "imap", "pop3"];

pub struct CertTool;

impl Tool for CertTool {
    fn id(&self) -> &'static str {
        "cert"
    }

    fn name(&self) -> &'static str {
        "Inspetor de certificado"
    }

    fn description(&self) -> &'static str {
        "Lê o certificado TLS de um host ou IP por inteiro: nomes, validade, chave, usos, cadeia e o que há de errado com ele"
    }

    fn params(&self) -> Vec<ParamSpec> {
        vec![
            ParamSpec::text(
                "alvo",
                "Alvo",
                "example.com",
                "Host ou IP. Sem protocolo — é uma conexão TLS direta, não uma URL",
            ),
            ParamSpec::text(
                "porta",
                "Porta",
                "443",
                "Onde o TLS atende. 443 web, 993 IMAP, 465 SMTP sobre TLS, 5432 Postgres com TLS",
            ),
            ParamSpec::text(
                "sni",
                "Nome no handshake (SNI)",
                "",
                "Vazio usa o alvo. Preencha quando o alvo for um IP e o servidor hospedar vários nomes",
            ),
            ParamSpec::choice(
                "starttls",
                "STARTTLS",
                STARTTLS,
                "Para portas que começam em texto puro e sobem para TLS a pedido: 25/587 SMTP, 143 IMAP, 110 POP3",
            ),
            ParamSpec::text(
                "timeout",
                "Tempo limite (ms)",
                "5000",
                "Quanto esperar pela conexão e pelo handshake antes de desistir",
            ),
        ]
    }

    fn summarize(&self, params: &HashMap<&'static str, String>) -> String {
        let get = |key| params.get(key).map(String::as_str).unwrap_or("").trim();
        let target = match get("alvo") {
            "" => "?",
            host => host,
        };
        let port = match get("porta") {
            "" => "443",
            port => port,
        };
        let starttls = match get("starttls") {
            "" | "não" => String::new(),
            mode => format!("  ·  STARTTLS {mode}"),
        };
        format!("{target}:{port}{starttls}")
    }

    fn on_demand(&self) -> bool {
        true
    }

    fn start(&self, id: u64, params: &HashMap<&'static str, String>) -> Result<Execution, String> {
        let plan = Plan::from(params)?;
        let (execution, recorder) = Execution::new(id, self.name(), self.summarize(params));
        recorder.record(
            0,
            EventKind::Note(format!(
                "pronto para ler o certificado de {} ({}). Nada roda até você abrir",
                plan.target, plan.address
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
        finished.store(false, std::sync::atomic::Ordering::Relaxed);
        std::thread::spawn(move || {
            inspect(plan, &recorder);
            recorder.ran();
            finished.store(true, std::sync::atomic::Ordering::Relaxed);
        });
    }

    fn columns(&self, execution: &Execution) -> (String, String) {
        execution.outcome()
    }

    fn handoffs(&self, execution: &Execution) -> Vec<Handoff> {
        execution
            .findings("ip")
            .into_iter()
            .map(|address| Handoff {
                label: format!("varrer portas de {address}"),
                tool: "scan",
                params: vec![("alvo", address), ("faixa", "comuns".to_string())],
            })
            .collect()
    }
}

struct Plan {
    target: String,
    sni: String,
    address: SocketAddr,
    starttls: String,
    timeout: Duration,
}

impl Plan {
    fn from(params: &HashMap<&'static str, String>) -> Result<Self, String> {
        let get = |key| params.get(key).map(String::as_str).unwrap_or("").trim();
        let target = get("alvo").to_string();
        if target.is_empty() {
            return Err("informe um host ou IP".to_string());
        }
        let port: u16 = match get("porta") {
            "" => 443,
            text => text
                .parse()
                .map_err(|_| format!("porta: «{text}» não é um número de porta"))?,
        };
        // Resolved once, here, so a name that doesn't exist fails in front of the person
        // who typed it rather than in a thread nobody is watching.
        let address = (target.as_str(), port)
            .to_socket_addrs()
            .map_err(|e| format!("não consegui resolver {target}: {e}"))?
            .next()
            .ok_or_else(|| format!("{target} não resolveu para nenhum endereço"))?;
        let timeout = get("timeout")
            .parse::<u64>()
            .map_err(|_| format!("tempo limite: «{}» não é um número", get("timeout")))?
            .clamp(200, 60_000);
        Ok(Self {
            sni: match get("sni") {
                "" => target.clone(),
                sni => sni.to_string(),
            },
            target,
            address,
            starttls: get("starttls").to_string(),
            timeout: Duration::from_millis(timeout),
        })
    }
}

fn note(rec: &Recorder, text: impl Into<String>) {
    rec.record(0, EventKind::Note(text.into()));
}

/// Announces a section, so the report reads as one rather than as a dump.
fn section(rec: &Recorder, title: &str) {
    note(rec, format!("── {title} ──"));
    rec.report("lendo…", title.to_string());
}

/// A labelled line, with the labels lined up — the report is read by eye, in a column.
fn field(rec: &Recorder, label: &str, value: impl AsRef<str>) {
    let value = value.as_ref();
    if value.is_empty() {
        return;
    }
    note(rec, format!("  {label:<22}{value}"));
}

fn inspect(plan: Plan, rec: &Recorder) {
    let started = Instant::now();
    note(
        rec,
        format!(
            "lendo o certificado de {} em {} (SNI {})",
            plan.target, plan.address, plan.sni
        ),
    );
    rec.found("ip", plan.address.ip().to_string());

    // Accepting anything first: a certificate that fails verification is exactly the one
    // worth reading, and refusing to read it would answer the question with silence.
    let chain = match fetch(&plan, false) {
        Ok(chain) => chain,
        Err(error) => {
            rec.record(0, EventKind::Error(format!("handshake falhou: {error}")));
            rec.report("sem certificado", error);
            return;
        }
    };

    section(rec, "Conexão");
    field(rec, "Endereço", plan.address.to_string());
    field(rec, "Versão do TLS", &chain.version);
    field(rec, "Cifra negociada", &chain.suite);
    field(
        rec,
        "Handshake",
        format!("{} ms", chain.elapsed.as_millis()),
    );
    field(
        rec,
        "Certificados",
        format!("{} enviados pelo servidor", chain.certificates.len()),
    );

    let Some(leaf) = chain.certificates.first() else {
        rec.record(
            0,
            EventKind::Error("o servidor não enviou certificado nenhum".to_string()),
        );
        rec.report("sem certificado", "servidor não enviou nenhum");
        return;
    };

    section(rec, "Certificado do servidor");
    report_certificate(rec, leaf, true);

    if chain.certificates.len() > 1 {
        section(rec, "Cadeia enviada pelo servidor");
        for (depth, cert) in chain.certificates.iter().enumerate().skip(1) {
            note(rec, format!("  [{depth}] {}", cert.subject_dn()));
            field(rec, "  emitido por", cert.issuer_dn());
            field(rec, "  válido até", validity_line(cert));
            field(rec, "  chave", cert.public_key.clone().unwrap_or_default());
            field(
                rec,
                "  assinatura",
                cert.signature_algorithm.clone().unwrap_or_default(),
            );
        }
    }

    section(rec, "Verificação");
    let covers = leaf.covers(&plan.sni);
    field(
        rec,
        "Nome confere",
        if covers {
            format!("sim — {} está no certificado", plan.sni)
        } else {
            format!(
                "NÃO — {} não aparece entre os nomes do certificado",
                plan.sni
            )
        },
    );
    let trusted = match fetch(&plan, true) {
        Ok(_) => {
            field(
                rec,
                "Cadeia confiável",
                "sim — validada pelo trust store desta máquina",
            );
            true
        }
        Err(error) => {
            field(rec, "Cadeia confiável", format!("NÃO — {error}"));
            false
        }
    };

    section(rec, "Avaliação");
    let problems = problems(leaf, &chain, covers, trusted);
    if problems.is_empty() {
        note(rec, "  nada a apontar");
    }
    for problem in &problems {
        rec.record(0, EventKind::Error(format!("  {problem}")));
    }

    let headline = match leaf.days_left() {
        Some(days) if days < 0 => format!("VENCIDO há {} dias", -days),
        Some(days) => format!("vence em {days} dias"),
        None => "validade ilegível".to_string(),
    };
    let summary = match problems.len() {
        0 => leaf.subject.clone().unwrap_or_else(|| leaf.subject_dn()),
        n => format!("{n} alerta(s) · {}", leaf.subject_dn()),
    };
    note(
        rec,
        format!(
            "leitura concluída em {:.1}s",
            started.elapsed().as_secs_f64()
        ),
    );
    rec.report(headline, summary);
}

/// Everything about one certificate, in the order someone reads it: what it is, when it
/// is, what it covers, what it's made of, and where to check on it.
fn report_certificate(rec: &Recorder, cert: &Cert, full: bool) {
    field(rec, "Sujeito", cert.subject_dn());
    field(rec, "Emissor", cert.issuer_dn());
    if cert.self_signed() {
        field(
            rec,
            "Autoassinado",
            "sim — o sujeito e o emissor são o mesmo",
        );
    }
    field(rec, "Número de série", &cert.serial);
    field(rec, "Versão", format!("v{}", cert.version));
    if let Some(from) = cert.not_before {
        field(rec, "Válido desde", x509::utc(from));
    }
    field(rec, "Válido até", validity_line(cert));
    if !cert.dns_names.is_empty() {
        field(
            rec,
            "Nomes (SAN)",
            format!("{} no total", cert.dns_names.len()),
        );
        for name in &cert.dns_names {
            note(rec, format!("      {name}"));
        }
    }
    for ip in &cert.ip_addresses {
        field(rec, "Endereço (SAN)", ip);
    }
    if !full {
        return;
    }
    field(
        rec,
        "Chave pública",
        cert.public_key.clone().unwrap_or_default(),
    );
    field(
        rec,
        "Assinado com",
        cert.signature_algorithm.clone().unwrap_or_default(),
    );
    if !cert.key_usage.is_empty() {
        field(rec, "Uso da chave", cert.key_usage.join(", "));
    }
    if !cert.extended_key_usage.is_empty() {
        field(rec, "Serve para", cert.extended_key_usage.join(", "));
    }
    field(
        rec,
        "Pode assinar outros",
        if cert.is_ca {
            match cert.path_len {
                Some(depth) => format!("sim, até {depth} nível(is) abaixo"),
                None => "sim, é uma CA".to_string(),
            }
        } else {
            "não — é um certificado de ponta".to_string()
        },
    );
    field(
        rec,
        "ID da chave (SKI)",
        cert.subject_key_id.clone().unwrap_or_default(),
    );
    field(
        rec,
        "ID do emissor (AKI)",
        cert.authority_key_id.clone().unwrap_or_default(),
    );
    for url in &cert.ocsp {
        field(rec, "OCSP", url);
    }
    for url in &cert.ca_issuers {
        field(rec, "Emissor em", url);
    }
    for url in &cert.crl {
        field(rec, "Lista de revogação", url);
    }
    field(
        rec,
        "Transparência (SCT)",
        if cert.has_sct {
            "sim, traz provas embutidas"
        } else {
            "não traz provas embutidas"
        },
    );
    field(rec, "Impressão SHA-256", &cert.fingerprint);
}

/// The expiry line: the date, and what it means from here.
fn validity_line(cert: &Cert) -> String {
    let Some(until) = cert.not_after else {
        return String::new();
    };
    match cert.days_left() {
        Some(days) if days < 0 => format!("{}  ·  VENCIDO há {} dias", x509::utc(until), -days),
        Some(0) => format!("{}  ·  vence hoje", x509::utc(until)),
        Some(days) => format!("{}  ·  faltam {days} dias", x509::utc(until)),
        None => x509::utc(until),
    }
}

/// What's wrong with it, in the order that matters. Everything here is something that
/// either breaks a client today or will break one on a date that can be named.
fn problems(cert: &Cert, chain: &Chain, covers: bool, trusted: bool) -> Vec<String> {
    let mut found = Vec::new();
    match cert.days_left() {
        Some(days) if days < 0 => found.push(format!(
            "VENCIDO há {} dias — todo cliente que verifica recusa a conexão",
            -days
        )),
        Some(days) if days <= RENEW_SOON_DAYS => found.push(format!(
            "vence em {days} dias — renove antes que vire incidente"
        )),
        _ => {}
    }
    if let Some(from) = cert.not_before
        && from > now()
    {
        found.push(format!(
            "ainda não é válido — começa em {}",
            x509::utc(from)
        ));
    }
    if !covers {
        found.push("o nome pedido não está entre os nomes do certificado".to_string());
    }
    if !trusted {
        found.push("a cadeia não fecha num certificado raiz confiado por esta máquina".to_string());
    }
    if cert.self_signed() {
        found.push("autoassinado — só é confiável para quem já o conhece".to_string());
    }
    if let Some(key) = &cert.public_key
        && let Some(bits) = key
            .strip_prefix("RSA ")
            .and_then(|rest| rest.split(' ').next())
            .and_then(|bits| bits.parse::<usize>().ok())
        && bits < WEAK_RSA_BITS
    {
        found.push(format!("chave RSA de {bits} bits — fraca demais para hoje"));
    }
    if cert
        .signature_algorithm
        .as_deref()
        .is_some_and(|algorithm| algorithm.to_ascii_lowercase().contains("sha1"))
    {
        found.push("assinado com SHA-1 — nenhum cliente atual aceita".to_string());
    }
    if let (Some(from), Some(until)) = (cert.not_before, cert.not_after) {
        let days = (until.saturating_sub(from) / 86_400) as i64;
        if days > MAX_LIFETIME_DAYS {
            found.push(format!(
                "emitido por {days} dias — acima do limite de {MAX_LIFETIME_DAYS} que os navegadores aceitam"
            ));
        }
    }
    if chain.certificates.len() == 1 && !cert.self_signed() {
        found.push(
            "o servidor mandou só o certificado de ponta — clientes sem o intermediário em cache falham".to_string(),
        );
    }
    found
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// What one handshake produced.
struct Chain {
    version: String,
    suite: String,
    elapsed: Duration,
    certificates: Vec<Cert>,
}

/// Connects, optionally speaks the STARTTLS dance, and runs the handshake to the point
/// where the server's certificates are in hand.
fn fetch(plan: &Plan, verify: bool) -> Result<Chain, String> {
    let started = Instant::now();
    let client = super::tls::Client::new(
        &format!("{}:{}", plan.sni, plan.address.port()),
        &plan.sni,
        verify,
    )?;
    let mut session = client.session()?;
    let mut stream = TcpStream::connect_timeout(&plan.address, plan.timeout)
        .map_err(|e| format!("não consegui conectar em {}: {e}", plan.address))?;
    let _ = stream.set_read_timeout(Some(plan.timeout));
    let _ = stream.set_write_timeout(Some(plan.timeout));

    if plan.starttls != "não" && !plan.starttls.is_empty() {
        starttls(&mut stream, &plan.starttls)?;
    }

    while session.is_handshaking() {
        if session.wants_write() {
            session
                .write_tls(&mut stream)
                .map_err(|e| format!("erro ao enviar: {e}"))?;
            continue;
        }
        if session.wants_read() {
            match session.read_tls(&mut stream) {
                Ok(0) => return Err("o servidor fechou a conexão durante o handshake".to_string()),
                Ok(_) => {}
                Err(e) => return Err(format!("erro ao ler: {e}")),
            }
            // The verifier's complaint arrives here, and it is the answer to "is this
            // chain trusted" — so it's passed through rather than flattened.
            session.process_new_packets().map_err(|e| format!("{e}"))?;
        }
    }

    let certificates = session
        .peer_certificates()
        .unwrap_or(&[])
        .iter()
        .filter_map(|der| x509::parse(der))
        .collect();
    let chain = Chain {
        version: session
            .protocol_version()
            .map(|v| format!("{v:?}"))
            .unwrap_or_default(),
        suite: session
            .negotiated_cipher_suite()
            .map(|s| format!("{:?}", s.suite()))
            .unwrap_or_default(),
        elapsed: started.elapsed(),
        certificates,
    };
    let _ = stream.shutdown(Shutdown::Both);
    Ok(chain)
}

/// The plaintext preamble a mail protocol needs before it will speak TLS. Each is two
/// lines and a status code, which is why they're here rather than in a mail client.
fn starttls(stream: &mut TcpStream, protocol: &str) -> Result<(), String> {
    let mut buffer = [0u8; 1024];
    let mut read_line = |stream: &mut TcpStream| -> Result<String, String> {
        let read = stream
            .read(&mut buffer)
            .map_err(|e| format!("STARTTLS: nada veio do servidor ({e})"))?;
        Ok(String::from_utf8_lossy(&buffer[..read]).to_string())
    };

    let greeting = read_line(stream)?;
    let commands: &[&str] = match protocol {
        "smtp" => &["EHLO monitorzinho\r\n", "STARTTLS\r\n"],
        "imap" => &["a001 STARTTLS\r\n"],
        "pop3" => &["STLS\r\n"],
        _ => return Ok(()),
    };
    if greeting.is_empty() {
        return Err("STARTTLS: o servidor não se apresentou".to_string());
    }
    for command in commands {
        stream
            .write_all(command.as_bytes())
            .map_err(|e| format!("STARTTLS: não consegui enviar {}: {e}", command.trim()))?;
        let reply = read_line(stream)?;
        // 5xx from SMTP, NO/BAD from IMAP, -ERR from POP3: all mean "not here".
        if reply.starts_with('5') || reply.contains("NO ") || reply.starts_with("-ERR") {
            return Err(format!(
                "STARTTLS recusado pelo servidor: {}",
                reply.lines().next().unwrap_or("").trim()
            ));
        }
    }
    Ok(())
}
