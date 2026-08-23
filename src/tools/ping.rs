//! Latency to one destination, measured over and over, drawn as a line.
//!
//! `ping` in a terminal answers "is it up right now"; this answers "what has it been
//! doing while I was looking at something else". Every measurement goes into the
//! execution's log, and the latest one into a chart panel on the Visão geral tab that
//! outlives the screen that started it — which is the point, because the packet loss
//! worth finding is the kind that happens for forty seconds twice an hour.
//!
//! Three ways to measure, because the classic one is not always available:
//!
//! * **ICMP** — a real echo request, on an unprivileged ICMP socket. The kernel only
//!   hands those out to the groups in `net.ipv4.ping_group_range`, which on plenty of
//!   distributions is empty, and then nothing but `ping` itself (setuid) can do it.
//! * **TCP** — the time to open a connection to a port, refused included: a RST is an
//!   answer from the host and times the round trip just as well as an accept does.
//!   Needs no privilege at all and passes through firewalls that drop ICMP.
//! * **UDP** — a datagram to a port nothing is listening on, timing the ICMP "port
//!   unreachable" it provokes. The same mechanism the traceroute here uses. Free of
//!   privilege too, but Linux rate-limits those replies (`net.ipv4.icmp_ratelimit`,
//!   1/s by default), so a fast interval will read as loss that isn't there.

use std::collections::HashMap;
use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use super::icmp::{Hop, Pinger, Tracer, unreachable_reason};
use super::{Chart, EventKind, Execution, ParamSpec, Recorder, Suggestion, Tool};
use crate::monitor::live::LiveSeries;

/// Port used by the UDP mode: the traceroute range, reserved by convention precisely
/// because nothing answers there.
const UDP_PORT: u16 = 33434;

/// How long the loop sleeps at a time while waiting for the next measurement. Short
/// enough that removing the execution stops it right away instead of at the end of a
/// long interval.
const SLICE: Duration = Duration::from_millis(100);

/// What a lost packet is worth on the chart. Zero rather than the timeout, because a
/// timeout is not a measurement: charting it as one would put a spike where there was
/// silence and drag the average up with numbers nobody measured. On the panel a zero
/// reads as "perdido"; the loss percentage lives in the execution's own columns.
const LOST: f64 = 0.0;

pub struct PingTool;

impl Tool for PingTool {
    fn id(&self) -> &'static str {
        "ping"
    }

    fn name(&self) -> &'static str {
        "Latência contínua"
    }

    fn description(&self) -> &'static str {
        "Mede o tempo de ida e volta até um destino sem parar e desenha a linha na Visão geral"
    }

    fn params(&self) -> Vec<ParamSpec> {
        vec![
            ParamSpec::text(
                "alvo",
                "Alvo",
                "1.1.1.1",
                "IP ou nome. Vira um gráfico próprio na aba Visão geral, com o histórico guardado",
            )
            .suggesting(suggested_targets()),
            ParamSpec::choice(
                "modo",
                "Como medir",
                MODES,
                "Automático usa ICMP se o sistema deixar e cai para TCP quando não deixa",
            ),
            ParamSpec::text(
                "porta",
                "Porta (modo TCP)",
                "443",
                "Só o modo TCP usa. Recusada serve igual: o RST veio do host e cronometra a volta",
            ),
            ParamSpec::text(
                "intervalo",
                "Intervalo (ms)",
                "1000",
                "Espera entre medições. Abaixo de 1000 no modo UDP o próprio Linux limita as respostas",
            ),
            ParamSpec::text(
                "timeout",
                "Tempo limite (ms)",
                "2000",
                "Depois disso a medição conta como perda",
            ),
        ]
    }

    fn summarize(&self, params: &HashMap<&'static str, String>) -> String {
        match Plan::from(params) {
            Ok(plan) => plan.summary(),
            Err(_) => params.get("alvo").cloned().unwrap_or_default(),
        }
    }

    fn start(&self, id: u64, params: &HashMap<&'static str, String>) -> Result<Execution, String> {
        let plan = Plan::from(params)?;
        let series = LiveSeries::new();
        let (execution, recorder) = Execution::new(id, self.name(), plan.summary());
        // Its own address is a finding like any other: from here the whole address menu
        // is one Ctrl+P away — varrer as portas, ler o certificado, traçar a rota.
        recorder.found("ip", plan.address.to_string());
        let chart = Chart {
            key: format!("ping:{}", plan.target),
            title: format!("Latência {}", plan.target),
            group: "Ferramentas",
            format: format_ms,
            series: Arc::clone(&series),
        };
        let finished = execution.finish_flag();
        let published = Arc::clone(&series);
        thread::spawn(move || {
            measure(plan, &recorder, &published);
            finished.store(true, Ordering::Relaxed);
        });
        Ok(execution.charting(chart))
    }

    fn columns(&self, execution: &Execution) -> (String, String) {
        execution.outcome()
    }
}

/// How a sample reads on the panel. Zero is the one value the loop never measures — see
/// `LOST`.
fn format_ms(value: f64) -> String {
    if value <= 0.0 {
        return "perdido".to_string();
    }
    format!("{value:.1} ms")
}

const MODES: &[&str] = &["automático", "ICMP", "TCP", "UDP"];

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Auto,
    Icmp,
    Tcp,
    Udp,
}

impl Mode {
    fn parse(text: &str) -> Self {
        match text {
            "ICMP" => Self::Icmp,
            "TCP" => Self::Tcp,
            "UDP" => Self::Udp,
            _ => Self::Auto,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Auto => "automático",
            Self::Icmp => "ICMP",
            Self::Tcp => "TCP",
            Self::Udp => "UDP",
        }
    }
}

struct Plan {
    /// What the user typed — the name if they gave a name, which is what the chart is
    /// called and what makes its history the same line tomorrow.
    target: String,
    address: Ipv4Addr,
    mode: Mode,
    port: u16,
    interval: Duration,
    timeout: Duration,
}

impl Plan {
    fn from(params: &HashMap<&'static str, String>) -> Result<Self, String> {
        let get = |key| params.get(key).map(String::as_str).unwrap_or("").trim();
        let target = match get("alvo") {
            "" => "1.1.1.1",
            text => text,
        }
        .to_string();
        let address = resolve(&target)?;
        let millis = |key, fallback, low, high| {
            Duration::from_millis(get(key).parse::<u64>().unwrap_or(fallback).clamp(low, high))
        };
        Ok(Self {
            target,
            address,
            mode: Mode::parse(get("modo")),
            port: get("porta").parse::<u16>().unwrap_or(443).max(1),
            interval: millis("intervalo", 1000, 100, 600_000),
            timeout: millis("timeout", 2000, 100, 60_000),
        })
    }

    fn summary(&self) -> String {
        let how = match self.mode {
            Mode::Tcp => format!("TCP {}", self.port),
            other => other.label().to_string(),
        };
        format!(
            "{} · {how} · a cada {:.0} ms",
            self.target,
            self.interval.as_secs_f64() * 1000.0
        )
    }
}

/// A name or an address to an address. IPv4 only, same as everything else built on the
/// raw sockets here.
fn resolve(target: &str) -> Result<Ipv4Addr, String> {
    if let Ok(address) = target.parse::<Ipv4Addr>() {
        return Ok(address);
    }
    let resolved = (target, 0u16)
        .to_socket_addrs()
        .map_err(|e| format!("não resolvi {target}: {e}"))?
        .find_map(|address| match address.ip() {
            IpAddr::V4(v4) => Some(v4),
            IpAddr::V6(_) => None,
        });
    resolved.ok_or_else(|| format!("{target} não tem endereço IPv4"))
}

/// One measurement's worth of answer.
enum Probe {
    /// The destination answered, in this long.
    Answered { rtt: Duration, note: Option<String> },
    /// Nothing usable came back, and why.
    Lost(String),
}

/// Everything that can measure, in the order `automático` tries them.
enum Prober {
    Icmp(Pinger),
    Tcp,
    Udp(Tracer),
}

impl Prober {
    /// Builds what the chosen mode needs, or explains what stopped it. `automático`
    /// prefers ICMP — it measures the host rather than a service on it — and falls back
    /// to TCP, which no configuration can take away.
    fn open(mode: Mode) -> Result<(Self, Mode), String> {
        match mode {
            Mode::Icmp => Pinger::new()
                .map(|p| (Self::Icmp(p), Mode::Icmp))
                .ok_or_else(|| {
                    "o sistema não deu um socket ICMP (veja net.ipv4.ping_group_range)".to_string()
                }),
            Mode::Tcp => Ok((Self::Tcp, Mode::Tcp)),
            Mode::Udp => Tracer::new()
                .map(|t| (Self::Udp(t), Mode::Udp))
                .ok_or_else(|| "o sistema não deu um socket UDP".to_string()),
            Mode::Auto => match Pinger::new() {
                Some(pinger) => Ok((Self::Icmp(pinger), Mode::Icmp)),
                None => Ok((Self::Tcp, Mode::Tcp)),
            },
        }
    }

    fn probe(&self, plan: &Plan, sequence: u16) -> Probe {
        match self {
            Self::Icmp(pinger) => match pinger.ping(plan.address, sequence, plan.timeout) {
                Hop::Reply { rtt, .. } => Probe::Answered { rtt, note: None },
                Hop::Unreachable { rtt, code, .. } => Probe::Answered {
                    rtt,
                    note: Some(unreachable_reason(code).to_string()),
                },
                Hop::Exceeded { .. } => Probe::Lost("TTL esgotado no caminho".to_string()),
                Hop::Silent => Probe::Lost("sem resposta".to_string()),
            },
            Self::Tcp => tcp_probe(plan),
            Self::Udp(tracer) => {
                // Full distance: this is a question about the host, not about the path.
                match tracer.hop(plan.address, 255, UDP_PORT, plan.timeout) {
                    Hop::Unreachable { rtt, .. } => Probe::Answered { rtt, note: None },
                    Hop::Reply { rtt, .. } => Probe::Answered { rtt, note: None },
                    Hop::Exceeded { .. } => Probe::Lost("TTL esgotado no caminho".to_string()),
                    Hop::Silent => Probe::Lost("sem resposta".to_string()),
                }
            }
        }
    }
}

/// The time to open a connection — or to be told there is nothing to open. Both are the
/// host answering; only silence is loss.
fn tcp_probe(plan: &Plan) -> Probe {
    let address = SocketAddr::new(IpAddr::V4(plan.address), plan.port);
    let started = Instant::now();
    match TcpStream::connect_timeout(&address, plan.timeout) {
        Ok(_) => Probe::Answered {
            rtt: started.elapsed(),
            note: None,
        },
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => Probe::Answered {
            rtt: started.elapsed(),
            note: Some(format!("porta {} recusada", plan.port)),
        },
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
            Probe::Lost("sem resposta".to_string())
        }
        Err(e) => Probe::Lost(e.to_string()),
    }
}

/// Running totals, kept here rather than recomputed from the log — the log forgets old
/// events, and the whole point of leaving this running is the hour it has been up.
#[derive(Default)]
struct Tally {
    sent: u64,
    answered: u64,
    min: f64,
    max: f64,
    total: f64,
    /// Consecutive losses right now, so a gap reads as a gap instead of as a percentage
    /// that barely moved.
    streak: u64,
    worst_streak: u64,
}

impl Tally {
    fn answered(&mut self, millis: f64) {
        self.sent += 1;
        self.answered += 1;
        self.total += millis;
        self.min = if self.answered == 1 {
            millis
        } else {
            self.min.min(millis)
        };
        self.max = self.max.max(millis);
        self.streak = 0;
    }

    fn lost(&mut self) {
        self.sent += 1;
        self.streak += 1;
        self.worst_streak = self.worst_streak.max(self.streak);
    }

    fn loss_percent(&self) -> f64 {
        if self.sent == 0 {
            return 0.0;
        }
        (self.sent - self.answered) as f64 * 100.0 / self.sent as f64
    }

    /// The two columns of the execution's row: where it stands, and what it has seen.
    fn columns(&self, last: &Probe) -> (String, String) {
        let headline = match last {
            Probe::Answered { rtt, .. } => format!("{:.1} ms", rtt.as_secs_f64() * 1000.0),
            Probe::Lost(_) if self.streak > 1 => format!("perdido ×{}", self.streak),
            Probe::Lost(_) => "perdido".to_string(),
        };
        if self.answered == 0 {
            return (
                headline,
                format!("{} enviados, nenhum respondido", self.sent),
            );
        }
        let mut summary = format!(
            "mín {:.1} · méd {:.1} · máx {:.1} ms · {:.1}% de perda ({} pacotes)",
            self.min,
            self.total / self.answered as f64,
            self.max,
            self.loss_percent(),
            self.sent
        );
        if self.worst_streak > 1 {
            summary.push_str(&format!(" · pior sequência {} perdidos", self.worst_streak));
        }
        (headline, summary)
    }
}

fn measure(plan: Plan, rec: &Recorder, series: &LiveSeries) {
    let (prober, mode) = match Prober::open(plan.mode) {
        Ok(ready) => ready,
        Err(reason) => {
            rec.record(0, EventKind::Error(reason.clone()));
            rec.report("sem medição", reason);
            return;
        }
    };
    let how = match mode {
        Mode::Tcp => format!("TCP porta {}", plan.port),
        Mode::Udp => format!("UDP porta {UDP_PORT} (esperando \"porta inacessível\")"),
        other => other.label().to_string(),
    };
    let chosen = if plan.mode == Mode::Auto && mode != Mode::Icmp {
        format!("{how} — ICMP indisponível neste sistema")
    } else {
        how
    };
    rec.record(
        0,
        EventKind::Note(format!(
            "medindo {} ({}) por {chosen}, a cada {:.0} ms",
            plan.target,
            plan.address,
            plan.interval.as_secs_f64() * 1000.0
        )),
    );

    let mut tally = Tally::default();
    let mut sequence: u16 = 0;
    while !rec.stopping() {
        let round = Instant::now();
        sequence = sequence.wrapping_add(1);
        let probe = prober.probe(&plan, sequence);
        match &probe {
            Probe::Answered { rtt, note } => {
                let millis = rtt.as_secs_f64() * 1000.0;
                tally.answered(millis);
                series.publish(millis);
                let suffix = note
                    .as_ref()
                    .map(|n| format!("  ({n})"))
                    .unwrap_or_default();
                rec.record(
                    0,
                    EventKind::Note(format!("#{sequence:<5} {millis:>8.1} ms{suffix}")),
                );
            }
            Probe::Lost(reason) => {
                tally.lost();
                series.publish(LOST);
                rec.record(0, EventKind::Error(format!("#{sequence:<5} {reason}")));
            }
        }
        let (headline, summary) = tally.columns(&probe);
        rec.report(headline, summary);

        // Sliced, so removing the execution stops it now rather than at the end of the
        // interval — which for a ping every five minutes is the difference between
        // closing the app and waiting for it.
        while !rec.stopping() && round.elapsed() < plan.interval {
            let left = plan.interval.saturating_sub(round.elapsed());
            thread::sleep(left.min(SLICE));
        }
    }
}

/// Destinations already worth asking about: the default gateway (the first hop that can
/// be at fault) and the resolvers this machine uses (the ones every other program waits
/// on), plus a public address to tell "my link" from "the internet".
fn suggested_targets() -> Vec<Suggestion> {
    let mut suggestions = Vec::new();
    if let Some(gateway) = crate::monitor::summary::default_gateway() {
        suggestions.push(Suggestion::new(gateway.to_string(), "gateway padrão"));
    }
    for resolver in resolvers() {
        suggestions.push(Suggestion::new(resolver.to_string(), "servidor DNS em uso"));
    }
    suggestions.push(Suggestion::new(
        "1.1.1.1",
        "Cloudflare — referência pública",
    ));
    suggestions.push(Suggestion::new("8.8.8.8", "Google — referência pública"));
    suggestions
}

/// The nameservers in `/etc/resolv.conf`, IPv4 only.
fn resolvers() -> Vec<Ipv4Addr> {
    let Ok(content) = fs::read_to_string("/etc/resolv.conf") else {
        return Vec::new();
    };
    content
        .lines()
        .filter_map(|line| line.strip_prefix("nameserver"))
        .filter_map(|rest| rest.trim().parse::<Ipv4Addr>().ok())
        .collect()
}
