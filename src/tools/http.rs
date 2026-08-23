//! Asking a URL, on a schedule, and saying where the time went.
//!
//! "Is it up" is the easy half. The half that decides what someone does next is *which
//! part* was slow: a name that took 900 ms to resolve, a handshake that took two
//! seconds, or a server that accepted the connection immediately and then sat on the
//! request. Those are three different problems and the total hides all of them, so this
//! measures the four phases separately — the same ones `curl -w` prints, for the same
//! reason.
//!
//! It keeps running, unlike the scanner and the DNS sweep: an endpoint that answered
//! once is not an endpoint that is up, and the useful shape of this tool is a column of
//! answers over time with the failures standing out in it.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use super::{EventKind, Execution, ParamSpec, Recorder, Tool};

const METHODS: &[&str] = &["GET", "HEAD"];
const YES_NO: &[&str] = &["sim", "não"];
/// Redirects followed before giving up. Anything past this is a loop.
const MAX_REDIRECTS: u8 = 5;
/// Response bytes read before we stop counting. The point is the timing and the status,
/// not keeping the page.
const MAX_BODY: usize = 256 * 1024;

pub struct HttpTool;

impl Tool for HttpTool {
    fn id(&self) -> &'static str {
        "http"
    }

    fn name(&self) -> &'static str {
        "Sonda HTTP"
    }

    fn description(&self) -> &'static str {
        "Chama uma URL de tempos em tempos e diz onde o tempo foi: DNS, conexão, TLS e primeiro byte"
    }

    fn params(&self) -> Vec<ParamSpec> {
        vec![
            ParamSpec::text(
                "url",
                "URL",
                "https://example.com/",
                "Com http:// ou https://. Caminho e query entram como estão",
            ),
            ParamSpec::choice(
                "metodo",
                "Método",
                METHODS,
                "HEAD pede só os cabeçalhos — mais leve, e é o que um health check costuma usar",
            ),
            ParamSpec::text(
                "intervalo",
                "Intervalo (s)",
                "30",
                "De quanto em quanto tempo repetir. A primeira chamada acontece assim que a execução começa",
            ),
            ParamSpec::text(
                "timeout",
                "Tempo limite (ms)",
                "10000",
                "Vale para cada fase: resolver, conectar, handshake e resposta",
            ),
            ParamSpec::choice(
                "redirect",
                "Seguir redirecionamento",
                YES_NO,
                "«sim» segue 301/302/307/308 até 5 vezes e informa onde parou",
            ),
            ParamSpec::text(
                "esperado",
                "Status esperado",
                "2xx",
                "«2xx», «200», «204»… Um status diferente do esperado é registrado como falha",
            ),
        ]
    }

    fn summarize(&self, params: &HashMap<&'static str, String>) -> String {
        let get = |key| params.get(key).map(String::as_str).unwrap_or("").trim();
        format!(
            "{} {}  ·  a cada {}s",
            get("metodo"),
            get("url"),
            get("intervalo")
        )
    }

    fn columns(&self, execution: &Execution) -> (String, String) {
        execution.outcome()
    }

    fn start(&self, id: u64, params: &HashMap<&'static str, String>) -> Result<Execution, String> {
        let plan = Plan::from(params)?;
        let (execution, recorder) = Execution::new(id, self.name(), self.summarize(params));
        let finished = execution.finish_flag();
        thread::spawn(move || {
            watch(plan, &recorder);
            finished.store(true, Ordering::Relaxed);
        });
        Ok(execution)
    }
}

#[derive(Clone)]
struct Target {
    https: bool,
    host: String,
    port: u16,
    path: String,
}

impl Target {
    /// Splits a URL into the four things a request is made of. Deliberately small: no
    /// userinfo, no fragment — neither belongs in a probe, and pretending to parse them
    /// would only hide a typo.
    fn parse(url: &str) -> Result<Self, String> {
        let url = url.trim();
        let (https, rest) = match url {
            _ if url.starts_with("https://") => (true, &url[8..]),
            _ if url.starts_with("http://") => (false, &url[7..]),
            _ => return Err("a URL precisa começar com http:// ou https://".to_string()),
        };
        let (authority, path) = match rest.find('/') {
            Some(at) => (&rest[..at], &rest[at..]),
            None => (rest, "/"),
        };
        if authority.is_empty() {
            return Err("a URL não tem host".to_string());
        }
        // A bracketed IPv6 literal keeps its brackets for the Host header and loses them
        // for the resolver, which is the one place the difference matters.
        let (host, port) = match authority.strip_prefix('[') {
            Some(rest) => {
                let (inside, after) = rest
                    .split_once(']')
                    .ok_or_else(|| "endereço IPv6 sem o ] de fechamento".to_string())?;
                let port = after.strip_prefix(':').and_then(|p| p.parse().ok());
                (inside.to_string(), port)
            }
            None => match authority.rsplit_once(':') {
                Some((host, port)) => (host.to_string(), port.parse().ok()),
                None => (authority.to_string(), None),
            },
        };
        Ok(Self {
            port: port.unwrap_or(if https { 443 } else { 80 }),
            https,
            host,
            path: path.to_string(),
        })
    }

    fn url(&self) -> String {
        let scheme = if self.https { "https" } else { "http" };
        let default = if self.https { 443 } else { 80 };
        if self.port == default {
            format!("{scheme}://{}{}", self.host, self.path)
        } else {
            format!("{scheme}://{}:{}{}", self.host, self.port, self.path)
        }
    }
}

struct Plan {
    target: Target,
    method: String,
    interval: Duration,
    timeout: Duration,
    redirect: bool,
    expected: String,
}

impl Plan {
    fn from(params: &HashMap<&'static str, String>) -> Result<Self, String> {
        let get = |key| params.get(key).map(String::as_str).unwrap_or("").trim();
        Ok(Self {
            target: Target::parse(get("url"))?,
            method: match get("metodo") {
                "" => "GET".to_string(),
                method => method.to_string(),
            },
            interval: Duration::from_secs(
                get("intervalo").parse::<u64>().unwrap_or(30).clamp(1, 3600),
            ),
            timeout: Duration::from_millis(
                get("timeout")
                    .parse::<u64>()
                    .unwrap_or(10_000)
                    .clamp(200, 120_000),
            ),
            redirect: get("redirect") != "não",
            expected: match get("esperado") {
                "" => "2xx".to_string(),
                expected => expected.to_string(),
            },
        })
    }
}

/// What one request cost, phase by phase.
#[derive(Default)]
struct Timing {
    resolve: Duration,
    connect: Duration,
    tls: Duration,
    first_byte: Duration,
    total: Duration,
}

struct Answer {
    status: u16,
    reason: String,
    server: Option<String>,
    location: Option<String>,
    bytes: usize,
    timing: Timing,
    address: SocketAddr,
}

fn watch(plan: Plan, rec: &Recorder) {
    rec.record(
        0,
        EventKind::Note(format!(
            "sondando {} a cada {}s — status esperado {}",
            plan.target.url(),
            plan.interval.as_secs(),
            plan.expected
        )),
    );
    let (mut ok, mut failed) = (0u64, 0u64);
    let mut request = 0u64;
    // One TLS client per host, kept for the life of the execution.
    let mut clients: HashMap<String, Arc<super::tls::Client>> = HashMap::new();

    loop {
        if rec.stopping() {
            break;
        }
        request += 1;
        match attempt(&plan, rec, request, &mut clients) {
            Ok(answer) => {
                let good = matches(&plan.expected, answer.status);
                if good {
                    ok += 1;
                } else {
                    failed += 1;
                }
                report(rec, request, &answer, good, &plan);
                rec.stats.connections.fetch_add(1, Ordering::Relaxed);
                rec.stats
                    .from_target
                    .fetch_add(answer.bytes as u64, Ordering::Relaxed);
                let rate = ok as f64 / (ok + failed) as f64 * 100.0;
                rec.report(
                    format!(
                        "{} em {:.0} ms",
                        answer.status,
                        answer.timing.total.as_secs_f64() * 1000.0
                    ),
                    format!("{ok} de {} ok ({rate:.0}%)", ok + failed),
                );
            }
            Err(error) => {
                failed += 1;
                rec.record(request, EventKind::Error(format!("#{request}  {error}")));
                let rate = ok as f64 / (ok + failed) as f64 * 100.0;
                rec.report(
                    "falhou".to_string(),
                    format!("{ok} de {} ok ({rate:.0}%)", ok + failed),
                );
            }
        }

        // Slept in slices so removing the execution is felt at once rather than at the
        // end of an interval that may be an hour long.
        let wake = Instant::now() + plan.interval;
        while Instant::now() < wake {
            if rec.stopping() {
                return;
            }
            thread::sleep(Duration::from_millis(200).min(wake - Instant::now()));
        }
    }
}

/// One request, following redirects if asked to.
fn attempt(
    plan: &Plan,
    rec: &Recorder,
    request: u64,
    clients: &mut HashMap<String, Arc<super::tls::Client>>,
) -> Result<Answer, String> {
    let mut target = plan.target.clone();
    for redirect in 0..=MAX_REDIRECTS {
        let answer = fetch(&target, plan, clients)?;
        rec.found("dominio", target.host.clone());
        rec.found("ip", answer.address.ip().to_string());

        let is_redirect = matches!(answer.status, 301 | 302 | 303 | 307 | 308);
        if !is_redirect || !plan.redirect {
            return Ok(answer);
        }
        let Some(location) = answer.location.clone() else {
            return Ok(answer);
        };
        if redirect == MAX_REDIRECTS {
            return Err(format!(
                "mais de {MAX_REDIRECTS} redirecionamentos — parece laço"
            ));
        }
        rec.record(
            request,
            EventKind::Note(format!(
                "     {} {} → {location}",
                answer.status, answer.reason
            )),
        );
        target = resolve_location(&target, &location)?;
    }
    Err("redirecionamento sem fim".to_string())
}

/// Where a `Location` points, absolute or relative to where we were.
fn resolve_location(from: &Target, location: &str) -> Result<Target, String> {
    if location.starts_with("http://") || location.starts_with("https://") {
        return Target::parse(location);
    }
    let mut next = from.clone();
    next.path = if location.starts_with('/') {
        location.to_string()
    } else {
        // Relative to the current directory, which is the path up to its last slash.
        let base = from
            .path
            .rsplit_once('/')
            .map(|(base, _)| base)
            .unwrap_or("");
        format!("{base}/{location}")
    };
    Ok(next)
}

fn fetch(
    target: &Target,
    plan: &Plan,
    clients: &mut HashMap<String, Arc<super::tls::Client>>,
) -> Result<Answer, String> {
    let started = Instant::now();
    let mut timing = Timing::default();

    let address = (target.host.as_str(), target.port)
        .to_socket_addrs()
        .map_err(|e| format!("não resolveu {}: {e}", target.host))?
        .next()
        .ok_or_else(|| format!("{} não resolveu para nenhum endereço", target.host))?;
    timing.resolve = started.elapsed();

    let at_connect = Instant::now();
    let mut socket = TcpStream::connect_timeout(&address, plan.timeout)
        .map_err(|e| format!("não conectou em {address}: {e}"))?;
    timing.connect = at_connect.elapsed();
    let _ = socket.set_read_timeout(Some(plan.timeout));
    let _ = socket.set_write_timeout(Some(plan.timeout));
    // Without this the request — one small write — waits on Nagle for an ACK that the
    // other side is itself delaying, and the probe reports 40 ms of its own making as
    // the server's time to first byte. Every client that measures latency sets it.
    let _ = socket.set_nodelay(true);

    let request = format!(
        "{} {} HTTP/1.1\r\n\
         Host: {}\r\n\
         User-Agent: monitorzinho\r\n\
         Accept: */*\r\n\
         Accept-Encoding: identity\r\n\
         Connection: close\r\n\r\n",
        plan.method,
        target.path,
        host_header(target)
    );

    // `Connection: close` on purpose: the server ends the body by closing, so reading
    // to EOF measures it exactly without this having to understand chunked encoding.
    let (head, bytes, first_byte) = if target.https {
        // Built once per host and kept. Constructing it parses the whole system trust
        // store, which took longer than the request it was for — the probe was
        // measuring its own startup and calling it the server's latency.
        let client = match clients.get(&target.host) {
            Some(client) => Arc::clone(client),
            None => {
                let client = Arc::new(super::tls::Client::new(
                    &format!("{}:{}", target.host, target.port),
                    &target.host,
                    true,
                )?);
                clients.insert(target.host.clone(), Arc::clone(&client));
                client
            }
        };
        let mut session = client.session()?;
        // The handshake driven on its own, so what it costs is what gets reported. Left
        // inside the write, it would carry the request with it.
        let at_tls = Instant::now();
        session
            .complete_io(&mut socket)
            .map_err(|e| format!("handshake TLS falhou: {e}"))?;
        timing.tls = at_tls.elapsed();

        let mut stream = rustls::Stream::new(&mut session, &mut socket);
        stream
            .write_all(request.as_bytes())
            .map_err(|e| format!("erro ao enviar: {e}"))?;
        stream.flush().ok();
        read_response(&mut stream)?
    } else {
        socket
            .write_all(request.as_bytes())
            .map_err(|e| format!("erro ao enviar: {e}"))?;
        read_response(&mut socket)?
    };
    // Measured from the moment the request was on its way, which is what "time to first
    // byte" means everywhere else.
    timing.first_byte = first_byte;
    timing.total = started.elapsed();

    let (status, reason) = status_line(&head)?;
    Ok(Answer {
        status,
        reason,
        server: header(&head, "server"),
        location: header(&head, "location"),
        bytes,
        timing,
        address,
    })
}

/// The `Host` header: brackets back on for an IPv6 literal, and the port only when it
/// isn't the default one for the scheme.
fn host_header(target: &Target) -> String {
    let host = if target.host.contains(':') {
        format!("[{}]", target.host)
    } else {
        target.host.clone()
    };
    let default = if target.https { 443 } else { 80 };
    if target.port == default {
        host
    } else {
        format!("{host}:{}", target.port)
    }
}

/// Reads a whole response, returning the head, how many bytes came in total, and how
/// long until the first of them — which is the number that separates "slow network"
/// from "slow application".
fn read_response(stream: &mut impl Read) -> Result<(String, usize, Duration), String> {
    let started = Instant::now();
    let mut buffer = [0u8; 16 * 1024];
    let mut head = Vec::new();
    let mut total = 0usize;
    let mut first_byte = Duration::ZERO;

    loop {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                if total == 0 {
                    first_byte = started.elapsed();
                }
                total += read;
                if head.len() < 16 * 1024 {
                    head.extend_from_slice(&buffer[..read]);
                }
                if total >= MAX_BODY {
                    break;
                }
            }
            Err(e) => {
                if total > 0 {
                    // A truncated body still answers the question the probe asked.
                    break;
                }
                return Err(format!("erro ao ler a resposta: {e}"));
            }
        }
    }
    if total == 0 {
        return Err("o servidor fechou sem responder nada".to_string());
    }
    Ok((
        String::from_utf8_lossy(&head).to_string(),
        total,
        first_byte,
    ))
}

fn status_line(head: &str) -> Result<(u16, String), String> {
    let line = head.lines().next().unwrap_or_default();
    let mut parts = line.split_whitespace();
    let version = parts.next().unwrap_or_default();
    if !version.starts_with("HTTP/") {
        return Err(format!(
            "resposta não parece HTTP: «{}»",
            truncate(line, 60)
        ));
    }
    let status: u16 = parts
        .next()
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| format!("status ilegível em «{}»", truncate(line, 60)))?;
    Ok((status, parts.collect::<Vec<_>>().join(" ")))
}

fn header(head: &str, name: &str) -> Option<String> {
    head.lines()
        .take_while(|line| !line.trim().is_empty())
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim()
                .eq_ignore_ascii_case(name)
                .then(|| value.trim().to_string())
        })
}

/// Whether a status is the one asked for. `2xx` matches a class, `204` matches itself.
fn matches(expected: &str, status: u16) -> bool {
    let expected = expected.trim();
    if let Some(class) = expected.strip_suffix("xx")
        && let Ok(class) = class.parse::<u16>()
    {
        return status / 100 == class;
    }
    expected.parse::<u16>().map(|code| code == status) == Ok(true)
}

fn report(rec: &Recorder, request: u64, answer: &Answer, good: bool, plan: &Plan) {
    let ms = |d: Duration| format!("{:.0} ms", d.as_secs_f64() * 1000.0);
    let phases = format!(
        "dns {} · conexão {} · {}primeiro byte {} · total {}",
        ms(answer.timing.resolve),
        ms(answer.timing.connect),
        if plan.target.https {
            format!("tls {} · ", ms(answer.timing.tls))
        } else {
            String::new()
        },
        ms(answer.timing.first_byte),
        ms(answer.timing.total)
    );
    let line = format!(
        "#{request}  {} {}  ·  {}  ·  {}  ·  {}",
        answer.status,
        answer.reason,
        crate::format::human_bytes(answer.bytes as f64),
        phases,
        answer.server.clone().unwrap_or_else(|| "-".to_string())
    );
    if good {
        rec.record(request, EventKind::Note(line));
    } else {
        rec.record(
            request,
            EventKind::Error(format!("{line}   ← esperado {}", plan.expected)),
        );
    }
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    text.chars().take(width).collect::<String>() + "…"
}
