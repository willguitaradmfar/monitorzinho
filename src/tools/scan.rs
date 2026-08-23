//! A port scanner: what a host has open, and as much as can be said about each one
//! without root.
//!
//! It's a TCP connect scan — a real connection to every port, not a half-open SYN probe
//! — because raw sockets need `CAP_NET_RAW` and this runs as a normal user. That's the
//! honest trade: it shows up in the target's logs, and it can't tell you anything about
//! UDP. What it gets in exchange is that every open port is a socket we're already
//! holding, so the scan can go on to *ask* the port what it is rather than guessing
//! from a table: read whatever it greets us with, try a TLS handshake, try an HTTP
//! request. A port number alone is the least interesting thing to learn here.
//!
//! Nothing runs until the user opens the execution. A scan is a burst of work with an
//! answer at the end, not something to keep running in the background, and sixty
//! thousand connections have no business happening because the app was launched.

use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::monitor::resolve::Services;

use super::{EventKind, Execution, ParamSpec, Recorder, Tool};

/// How much of a banner is kept. Enough for a version string and a couple of headers;
/// past that it's a payload, not an identification.
const BANNER_BYTES: usize = 512;
/// Ports scanned by one worker before it checks the stop flag. Small enough that
/// removing an execution mid-scan feels immediate.
const BATCH: usize = 8;
/// Upper bound on worker threads, whatever the user types. Past a few hundred sockets
/// in flight the bottleneck is the target and the kernel, not us.
const MAX_WORKERS: usize = 1024;
/// Ports probed before the row's progress figure is refreshed.
const PROGRESS_EVERY: usize = 64;
/// Characters of service names the row's summary column gets before the rest become a
/// "+N". Sized to what the column actually shows, with room for the count and elapsed.
const SUMMARY_WIDTH: usize = 20;

const PRESETS: &[&str] = &["comuns", "1-1024", "1-10000", "tudo (1-65535)"];
const BANNERS: &[&str] = &["sim", "não"];

/// The ports worth trying when someone doesn't want to wait for 65535 of them. Not
/// nmap's top-1000 list — just the ones a person actually goes looking for on a box
/// they administer.
const COMMON: &[u16] = &[
    21, 22, 23, 25, 53, 67, 68, 69, 79, 80, 88, 110, 111, 123, 135, 137, 138, 139, 143, 161, 162,
    179, 389, 443, 445, 464, 465, 514, 515, 587, 593, 631, 636, 873, 902, 989, 990, 993, 995, 1080,
    1194, 1433, 1521, 1723, 1883, 2049, 2181, 2375, 2376, 2379, 3000, 3128, 3306, 3389, 4369, 4444,
    4567, 5000, 5060, 5432, 5601, 5672, 5900, 5984, 6000, 6379, 6443, 7000, 7001, 8000, 8008, 8080,
    8081, 8086, 8088, 8123, 8443, 8500, 8888, 9000, 9042, 9090, 9092, 9100, 9200, 9300, 11211,
    15672, 27017, 27018, 50000,
];

pub struct ScanTool;

impl Tool for ScanTool {
    fn id(&self) -> &'static str {
        "scan"
    }

    fn name(&self) -> &'static str {
        "Scanner de portas"
    }

    fn description(&self) -> &'static str {
        "Varre as portas TCP de um host e identifica o que responde em cada uma que estiver aberta"
    }

    fn params(&self) -> Vec<ParamSpec> {
        vec![
            ParamSpec::text(
                "alvo",
                "Alvo",
                "127.0.0.1",
                "Host ou IP a varrer. O nome é resolvido uma vez, na criação",
            ),
            ParamSpec::choice(
                "faixa",
                "Faixa",
                PRESETS,
                "Quais portas tentar. 'comuns' são ~90 portas conhecidas; a varredura inteira leva bem mais tempo",
            ),
            ParamSpec::text(
                "portas",
                "Portas (opcional)",
                "",
                "Sobrepõe a faixa quando preenchido. Aceita lista e intervalos: 22,80,443,8000-8100",
            ),
            ParamSpec::text(
                "concorrencia",
                "Conexões simultâneas",
                "256",
                "Quantas portas são tentadas ao mesmo tempo. Mais é mais rápido e mais barulhento",
            ),
            ParamSpec::text(
                "timeout",
                "Tempo por porta (ms)",
                "600",
                "Quanto esperar por uma porta antes de considerá-la filtrada. Numa rede distante, aumente",
            ),
            ParamSpec::choice(
                "banner",
                "Identificar serviço",
                BANNERS,
                "Em cada porta aberta: lê o que o serviço anuncia, tenta TLS e tenta HTTP para descobrir o que é",
            ),
        ]
    }

    fn summarize(&self, params: &HashMap<&'static str, String>) -> String {
        let get = |key| params.get(key).map(String::as_str).unwrap_or("").trim();
        let target = if get("alvo").is_empty() {
            "?"
        } else {
            get("alvo")
        };
        let range = if get("portas").is_empty() {
            get("faixa").to_string()
        } else {
            get("portas").to_string()
        };
        format!("{target}  ·  {range}")
    }

    fn on_demand(&self) -> bool {
        true
    }

    /// Validates everything and starts nothing. The scan waits for the user to open it.
    fn start(&self, id: u64, params: &HashMap<&'static str, String>) -> Result<Execution, String> {
        let plan = Plan::from(params)?;
        let (execution, recorder) = Execution::new(id, self.name(), self.summarize(params));
        recorder.record(
            0,
            EventKind::Note(format!(
                "pronto para varrer {} ({}) — {} portas. Nada roda até você abrir",
                plan.host,
                plan.address,
                plan.ports.len()
            )),
        );
        Ok(execution.on_demand())
    }

    /// Opening the monitor is what triggers the first scan. Opening it again just shows
    /// what's already there — re-reading a result shouldn't cost the target another
    /// sixty thousand connections. 'r' is how you ask for a fresh one.
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
            scan(plan, &recorder);
            recorder.ran();
            finished.store(true, Ordering::Relaxed);
        });
    }

    fn columns(&self, execution: &Execution) -> (String, String) {
        execution.outcome()
    }
}

/// Everything the scan needs, settled while the user can still fix it.
struct Plan {
    host: String,
    address: IpAddr,
    ports: Vec<u16>,
    workers: usize,
    timeout: Duration,
    banner: bool,
}

impl Plan {
    fn from(params: &HashMap<&'static str, String>) -> Result<Self, String> {
        let get = |key| params.get(key).map(String::as_str).unwrap_or("").trim();

        let host = get("alvo");
        if host.is_empty() {
            return Err("informe o alvo".to_string());
        }
        // Resolved once, here: doing it per port would be sixty thousand DNS lookups,
        // and a name that changed mid-scan would make the results a fiction.
        let address = (host, 0u16)
            .to_socket_addrs()
            .map_err(|e| format!("alvo inválido ({host}): {e}"))?
            .next()
            .map(|addr| addr.ip())
            .ok_or_else(|| format!("alvo ({host}) não resolveu para nenhum endereço"))?;

        let ports = if get("portas").is_empty() {
            preset_ports(get("faixa"))
        } else {
            parse_ports(get("portas"))?
        };
        if ports.is_empty() {
            return Err("nenhuma porta para varrer".to_string());
        }

        let workers = number(get("concorrencia"), "conexões simultâneas")?.clamp(1, MAX_WORKERS);
        let timeout = number(get("timeout"), "tempo por porta")?.clamp(50, 30_000);

        Ok(Self {
            host: host.to_string(),
            address,
            ports,
            workers,
            timeout: Duration::from_millis(timeout as u64),
            banner: get("banner") != "não",
        })
    }
}

fn number(text: &str, what: &str) -> Result<usize, String> {
    text.parse::<usize>()
        .map_err(|_| format!("{what}: «{text}» não é um número"))
}

fn preset_ports(preset: &str) -> Vec<u16> {
    match preset {
        "1-1024" => (1..=1024).collect(),
        "1-10000" => (1..=10_000).collect(),
        p if p.starts_with("tudo") => (1..=65535).collect(),
        _ => COMMON.to_vec(),
    }
}

/// `22,80,443,8000-8100` — the shape everyone already types into nmap. Sorted and
/// de-duplicated, so an overlapping range doesn't get scanned twice.
fn parse_ports(spec: &str) -> Result<Vec<u16>, String> {
    let mut ports = Vec::new();
    for piece in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        match piece.split_once('-') {
            Some((from, to)) => {
                let from = port(from)?;
                let to = port(to)?;
                if from > to {
                    return Err(format!("intervalo invertido: «{piece}»"));
                }
                ports.extend(from..=to);
            }
            None => ports.push(port(piece)?),
        }
    }
    ports.sort_unstable();
    ports.dedup();
    Ok(ports)
}

fn port(text: &str) -> Result<u16, String> {
    match text.trim().parse::<u16>() {
        Ok(0) | Err(_) => Err(format!("porta inválida: «{}»", text.trim())),
        Ok(value) => Ok(value),
    }
}

/// What one port turned out to be.
struct Found {
    port: u16,
    /// How long the connection took to establish — the closest thing to a ping this
    /// scan gets, and it says whether the host is next door or across an ocean.
    connect: Duration,
    detail: String,
}

fn scan(plan: Plan, rec: &Recorder) {
    let started = Instant::now();
    let total = plan.ports.len();
    rec.record(
        0,
        EventKind::Note(format!(
            "varrendo {} ({}) — {total} portas, {} simultâneas, {} ms cada",
            plan.host,
            plan.address,
            plan.workers,
            plan.timeout.as_millis()
        )),
    );
    rec.report(format!("0/{total}"), "varrendo…");

    let services = Arc::new(Services::load());
    let plan = Arc::new(plan);
    let cursor = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(AtomicUsize::new(0));
    let refused = Arc::new(AtomicUsize::new(0));

    let workers: Vec<_> = (0..plan.workers.min(total))
        .map(|_| {
            let (plan, services) = (Arc::clone(&plan), Arc::clone(&services));
            let (cursor, done, refused) =
                (Arc::clone(&cursor), Arc::clone(&done), Arc::clone(&refused));
            let rec = rec.clone();
            thread::spawn(move || {
                let mut found = Vec::new();
                loop {
                    if rec.stopping() {
                        break;
                    }
                    let from = cursor.fetch_add(BATCH, Ordering::Relaxed);
                    if from >= plan.ports.len() {
                        break;
                    }
                    let to = (from + BATCH).min(plan.ports.len());
                    for &port in &plan.ports[from..to] {
                        match probe(&plan, &services, port) {
                            Probe::Open(open) => found.push(open),
                            Probe::Refused => {
                                refused.fetch_add(1, Ordering::Relaxed);
                            }
                            Probe::Filtered => {}
                        }
                        let seen = done.fetch_add(1, Ordering::Relaxed) + 1;
                        if seen.is_multiple_of(PROGRESS_EVERY) {
                            rec.report(format!("{seen}/{}", plan.ports.len()), "varrendo…");
                        }
                    }
                }
                found
            })
        })
        .collect();

    let mut open: Vec<Found> = workers
        .into_iter()
        .filter_map(|worker| worker.join().ok())
        .flatten()
        .collect();
    // Workers finish out of order and each holds its own findings until it does, so the
    // log would otherwise read as a shuffled list.
    open.sort_by_key(|found| found.port);

    let stopped = rec.stopping();
    for found in &open {
        rec.record(
            0,
            EventKind::Note(format!(
                "{:>5}/tcp aberta  ·  {:>6.1} ms  ·  {}",
                found.port,
                found.connect.as_secs_f64() * 1000.0,
                found.detail
            )),
        );
        // Published for the other tools. An open port is a tunnel's whole
        // configuration, and one that answered a TLS handshake is a certificate
        // waiting to be read — both carry the host, since a hand-off is built from
        // the finding alone.
        rec.found(
            // One kind or the other, never both: a TLS port's offers include the plain
            // ones, and recording it twice would put the same tunnel in the list twice.
            if found.detail.contains("TLS (") {
                "porta-tls"
            } else {
                "porta"
            },
            format!("{}:{}", plan.host, found.port),
        );
    }

    let seen = done.load(Ordering::Relaxed);
    let refused = refused.load(Ordering::Relaxed);
    let filtered = seen.saturating_sub(open.len() + refused);
    let elapsed = started.elapsed();
    rec.record(
        0,
        EventKind::Note(format!(
            "{}: {} aberta(s), {refused} fechada(s), {filtered} sem resposta, de {seen} porta(s) em {:.1}s",
            if stopped { "varredura interrompida" } else { "varredura concluída" },
            open.len(),
            elapsed.as_secs_f64()
        )),
    );
    rec.report(
        format!("{} de {seen} abertas", open.len()),
        headline_summary(&open, &services, elapsed),
    );
}

/// The row's second column: what's actually listening, by name, because "12 abertas"
/// on its own doesn't tell you whether you're looking at a database or a web server.
fn headline_summary(open: &[Found], services: &Services, elapsed: Duration) -> String {
    if open.is_empty() {
        return format!("nada aberto · {:.1}s", elapsed.as_secs_f64());
    }
    let mut names: Vec<String> = Vec::new();
    for found in open {
        let name = match services.name(true, found.port) {
            Some(name) => name.to_string(),
            None => found.port.to_string(),
        };
        if !names.contains(&name) {
            names.push(name);
        }
    }
    // Filled to fit rather than to a fixed count: a column that ends mid-word says less
    // than a shorter list with an honest "+3" after it.
    let mut shown = String::new();
    let mut listed = 0;
    for name in &names {
        let extra = if shown.is_empty() { 0 } else { 2 };
        if shown.chars().count() + extra + name.chars().count() > SUMMARY_WIDTH {
            break;
        }
        if !shown.is_empty() {
            shown.push_str(", ");
        }
        shown.push_str(name);
        listed += 1;
    }
    let rest = names.len() - listed;
    if rest > 0 {
        format!("{shown} +{rest} · {:.1}s", elapsed.as_secs_f64())
    } else {
        format!("{shown} · {:.1}s", elapsed.as_secs_f64())
    }
}

enum Probe {
    Open(Found),
    /// The host said no. Which is information: something is there, that port just isn't.
    Refused,
    /// Nothing came back at all — a firewall dropping packets looks exactly like this.
    Filtered,
}

fn probe(plan: &Plan, services: &Services, port: u16) -> Probe {
    let addr = SocketAddr::new(plan.address, port);
    let started = Instant::now();
    let stream = match TcpStream::connect_timeout(&addr, plan.timeout) {
        Ok(stream) => stream,
        Err(e) if e.kind() == ErrorKind::ConnectionRefused => return Probe::Refused,
        Err(_) => return Probe::Filtered,
    };
    let connect = started.elapsed();

    // The service table is a guess from the port number; the probe below is an answer.
    // When both are there they're worth saying together, but "desconhecido" in front of
    // a real identification is just noise.
    let guess = services.name(true, port);
    let detail = if plan.banner {
        identify(plan, stream, addr, guess)
    } else {
        guess.unwrap_or("desconhecido").to_string()
    };
    Probe::Open(Found {
        port,
        connect,
        detail,
    })
}

/// Asks an open port what it is, in the order that costs least.
///
/// Plenty of services introduce themselves the moment you connect — SSH, SMTP, FTP,
/// Redis on error — so listening comes first and usually ends it. Silence means the
/// port is waiting to be spoken to, and the two things worth saying are a TLS
/// ClientHello and an HTTP request.
fn identify(plan: &Plan, stream: TcpStream, addr: SocketAddr, guess: Option<&str>) -> String {
    let found = greeting(&stream, plan.timeout).or_else(|| {
        let _ = stream.shutdown(Shutdown::Both);
        tls_probe(plan, addr).or_else(|| http_probe(plan, addr))
    });
    match (guess, found) {
        (Some(guess), Some(found)) => format!("{guess}  ·  {found}"),
        (Some(guess), None) => format!("{guess}  ·  não respondeu a TLS nem HTTP"),
        (None, Some(found)) => found,
        (None, None) => "aberta, sem identificação".to_string(),
    }
}

/// Whatever the service says first, if it says anything.
fn greeting(stream: &TcpStream, timeout: Duration) -> Option<String> {
    let _ = stream.set_read_timeout(Some(timeout));
    let mut stream = stream;
    let mut buf = [0u8; BANNER_BYTES];
    match stream.read(&mut buf) {
        Ok(0) | Err(_) => None,
        Ok(n) => Some(printable(&buf[..n])),
    }
}

/// A TLS handshake, without verifying anything — the point isn't to trust the port,
/// it's to find out that it speaks TLS and on what terms.
fn tls_probe(plan: &Plan, addr: SocketAddr) -> Option<String> {
    let client =
        super::tls::Client::new(&format!("{}:{}", plan.host, addr.port()), "", false).ok()?;
    let mut session = client.session().ok()?;
    let mut stream = TcpStream::connect_timeout(&addr, plan.timeout).ok()?;
    let _ = stream.set_read_timeout(Some(plan.timeout));
    let _ = stream.set_write_timeout(Some(plan.timeout));

    while session.is_handshaking() {
        if session.wants_write() && session.write_tls(&mut stream).is_err() {
            return None;
        }
        if !session.is_handshaking() {
            break;
        }
        if session.wants_read() {
            match session.read_tls(&mut stream) {
                Ok(0) | Err(_) => return None,
                Ok(_) => {}
            }
            session.process_new_packets().ok()?;
        }
    }
    let version = session.protocol_version().map(|v| format!("{v:?}"))?;
    let suite = session
        .negotiated_cipher_suite()
        .map(|s| format!(", {:?}", s.suite()))
        .unwrap_or_default();
    // The handshake already fetched the chain — the port had to send it to prove
    // anything. Reading four fields out of the leaf is what turns "speaks TLS" into
    // "speaks TLS as api.exemplo.com, signed by Let's Encrypt, for another 62 days".
    let certificate = session
        .peer_certificates()
        .and_then(|chain| chain.first())
        .and_then(|leaf| super::x509::parse(leaf))
        .map(|cert| cert.summary())
        .filter(|summary| !summary.is_empty())
        .map(|summary| format!("  ·  {summary}"))
        .unwrap_or_default();
    let _ = stream.shutdown(Shutdown::Both);
    Some(format!("TLS ({version}{suite}){certificate}"))
}

/// A minimal HTTP request, for a port that stayed silent and doesn't do TLS. The status
/// line and `Server:` header are the identification.
fn http_probe(plan: &Plan, addr: SocketAddr) -> Option<String> {
    let mut stream = TcpStream::connect_timeout(&addr, plan.timeout).ok()?;
    let _ = stream.set_read_timeout(Some(plan.timeout));
    let _ = stream.set_write_timeout(Some(plan.timeout));
    let request = format!(
        "HEAD / HTTP/1.1\r\nHost: {}\r\nUser-Agent: monitorzinho\r\nConnection: close\r\n\r\n",
        plan.host
    );
    stream.write_all(request.as_bytes()).ok()?;

    let mut buf = [0u8; BANNER_BYTES];
    let n = stream.read(&mut buf).ok().filter(|n| *n > 0)?;
    let _ = stream.shutdown(Shutdown::Both);

    let text = String::from_utf8_lossy(&buf[..n]);
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let status = lines.next()?.to_string();
    match lines.find(|line| line.to_ascii_lowercase().starts_with("server:")) {
        Some(server) => Some(format!("{status}  ·  {server}")),
        None => Some(status),
    }
}

/// A banner is arbitrary bytes from a stranger. Control characters become `·` and the
/// whole thing is trimmed to one line, so a hostile response can't scribble over the
/// terminal.
fn printable(bytes: &[u8]) -> String {
    let text: String = String::from_utf8_lossy(bytes)
        .chars()
        .map(|c| match c {
            '\r' | '\n' | '\t' => ' ',
            c if c.is_control() => '·',
            c => c,
        })
        .collect();
    let trimmed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if trimmed.chars().count() > 120 {
        format!("{}…", trimmed.chars().take(120).collect::<String>())
    } else {
        trimmed
    }
}
