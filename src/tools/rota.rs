//! The path to a host: every router between here and there, and how far each one is.
//!
//! Two panels answer "is that host alive" and a third answers "what is it running", and
//! between the two there was nothing. A host that doesn't answer is a host that could be
//! off, unreachable, firewalled at its own door, or sitting behind a link that goes
//! nowhere three hops from here — and those are four different afternoons.
//!
//! It works the way `traceroute -I` does when it isn't setuid: an ICMP echo sent with a
//! deliberately small hop limit, and the router that throws it away announces itself by
//! saying so. Those announcements arrive on the socket's error queue rather than its
//! receive queue, which is the one detail that makes this possible without a raw socket
//! — see `icmp`.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, ToSocketAddrs};
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use super::icmp::{Hop, Tracer, unreachable_reason};
use super::{EventKind, Execution, ParamSpec, Recorder, Tool};
use crate::monitor::resolve;

/// Hops past which nothing on the public internet is: the far side of the world is
/// twenty-something, and anything beyond thirty is a routing loop.
const MAX_TTL: u8 = 30;
/// Probes per hop. Three is what every traceroute sends, and for the same reason: one
/// silent probe means nothing, three silent probes mean something.
const PROBES: u8 = 3;

pub struct RotaTool;

impl Tool for RotaTool {
    fn id(&self) -> &'static str {
        "rota"
    }

    fn name(&self) -> &'static str {
        "Rota até o host"
    }

    fn description(&self) -> &'static str {
        "Mostra cada roteador entre aqui e um host, com a latência de cada salto — responde onde o caminho para, não só que parou"
    }

    fn params(&self) -> Vec<ParamSpec> {
        vec![
            ParamSpec::text(
                "alvo",
                "Alvo",
                "1.1.1.1",
                "Host ou IP de destino. O nome é resolvido uma vez, na criação",
            ),
            ParamSpec::text(
                "saltos",
                "Saltos no máximo",
                "30",
                "Onde desistir. Nada na internet pública está além de uns 25 saltos",
            ),
            ParamSpec::text(
                "timeout",
                "Tempo por sonda (ms)",
                "1000",
                "Quanto esperar cada resposta. Roteador que não responde é comum e não quer dizer que o caminho parou ali",
            ),
            ParamSpec::choice(
                "nomes",
                "Resolver nomes",
                &["sim", "não"],
                "Faz DNS reverso de cada salto — é o que transforma um IP em «gru09s26-in-f14.1e100.net»",
            ),
        ]
    }

    fn summarize(&self, params: &HashMap<&'static str, String>) -> String {
        let get = |key| params.get(key).map(String::as_str).unwrap_or("").trim();
        match get("alvo") {
            "" => "?".to_string(),
            target => target.to_string(),
        }
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
                "pronto para traçar a rota até {} ({}). Nada roda até você abrir",
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
        finished.store(false, Ordering::Relaxed);
        thread::spawn(move || {
            trace(plan, &recorder);
            recorder.ran();
            finished.store(true, Ordering::Relaxed);
        });
    }

    fn columns(&self, execution: &Execution) -> (String, String) {
        execution.outcome()
    }
}

struct Plan {
    target: String,
    address: Ipv4Addr,
    max_ttl: u8,
    timeout: Duration,
    names: bool,
}

impl Plan {
    fn from(params: &HashMap<&'static str, String>) -> Result<Self, String> {
        let get = |key| params.get(key).map(String::as_str).unwrap_or("").trim();
        let target = get("alvo").to_string();
        if target.is_empty() {
            return Err("informe o host ou IP de destino".to_string());
        }
        // Resolved here so a name that doesn't exist is an error on the form.
        let address = (target.as_str(), 0)
            .to_socket_addrs()
            .map_err(|e| format!("não consegui resolver {target}: {e}"))?
            .find_map(|addr| match addr.ip() {
                IpAddr::V4(v4) => Some(v4),
                // ICMP here is IPv4 only, and saying so beats tracing the wrong thing.
                IpAddr::V6(_) => None,
            })
            .ok_or_else(|| format!("{target} não tem endereço IPv4 — só IPv4 por enquanto"))?;
        Ok(Self {
            target,
            address,
            max_ttl: get("saltos").parse::<u8>().unwrap_or(MAX_TTL).clamp(1, 64),
            timeout: Duration::from_millis(
                get("timeout")
                    .parse::<u64>()
                    .unwrap_or(1000)
                    .clamp(100, 10_000),
            ),
            names: get("nomes") != "não",
        })
    }
}

fn trace(plan: Plan, rec: &Recorder) {
    let started = Instant::now();
    let Some(tracer) = Tracer::new() else {
        rec.record(
            0,
            EventKind::Error("não consegui abrir o socket para traçar a rota".to_string()),
        );
        rec.report("sem socket", "o sistema recusou um socket UDP");
        return;
    };

    rec.record(
        0,
        EventKind::Note(format!(
            "traçando até {} ({}) — até {} saltos, {} sondas cada, {} ms por sonda",
            plan.target,
            plan.address,
            plan.max_ttl,
            PROBES,
            plan.timeout.as_millis()
        )),
    );

    let mut arrived = false;
    let mut reached_at = 0u8;
    let mut silent_run = 0u8;

    for ttl in 1..=plan.max_ttl {
        if rec.stopping() {
            break;
        }
        rec.report(format!("salto {ttl}"), format!("até {}", plan.target));

        // Every probe of a hop, then one line: the same shape as traceroute's output,
        // and the reason for it is that one probe answering and two not is a fact about
        // the hop worth seeing in one glance.
        let mut answers: Vec<(Ipv4Addr, Duration)> = Vec::new();
        let mut unreachable: Option<(Ipv4Addr, u8)> = None;
        let mut silent = 0u8;
        for probe in 0..PROBES {
            // The classic reserved range: a port nothing listens on, so the
            // destination answers "port unreachable" and that answer means "arrived".
            let port = 33_434 + ttl as u16 * PROBES as u16 + probe as u16;
            match tracer.hop(plan.address, ttl, port, plan.timeout) {
                Hop::Reply { from, rtt } => {
                    arrived = true;
                    answers.push((from, rtt));
                }
                Hop::Exceeded { from, rtt } => answers.push((from, rtt)),
                Hop::Unreachable { from, rtt, code } => {
                    answers.push((from, rtt));
                    unreachable = Some((from, code));
                }
                Hop::Silent => silent += 1,
            }
        }

        if answers.is_empty() {
            silent_run += 1;
            rec.record(0, EventKind::Note(format!("{ttl:>3}   * * *")));
            // Five silent hops in a row is a wall, not a shy router. Saying so beats
            // filling the screen with stars up to the limit.
            if silent_run >= 5 {
                rec.record(
                    0,
                    EventKind::Note(
                        "   cinco saltos seguidos sem resposta — o caminho para aqui".to_string(),
                    ),
                );
                break;
            }
            continue;
        }
        silent_run = 0;

        let address = answers[0].0;
        let name = if plan.names {
            resolve::reverse_now(&address.to_string()).unwrap_or_default()
        } else {
            String::new()
        };
        let times = answers
            .iter()
            .map(|(_, rtt)| format!("{:.1} ms", rtt.as_secs_f64() * 1000.0))
            .collect::<Vec<_>>()
            .join("  ");
        let lost = match silent {
            0 => String::new(),
            n => format!("   ({n} de {PROBES} sem resposta)"),
        };
        rec.record(
            0,
            EventKind::Note(format!(
                "{ttl:>3}   {address:<15} {:<40} {times}{lost}",
                truncate(&name, 40)
            )),
        );
        // Every hop is an address in its own right, worth scanning or reading a
        // certificate off — the picker decides that from the kind, not from here.
        rec.found("ip", address.to_string());

        if let Some((from, code)) = unreachable {
            rec.record(
                0,
                EventKind::Error(format!(
                    "   {from} respondeu «{}» — o caminho termina aqui",
                    unreachable_reason(code)
                )),
            );
            reached_at = ttl;
            break;
        }
        if arrived {
            reached_at = ttl;
            break;
        }
    }

    let elapsed = started.elapsed();
    let (headline, summary) = if arrived {
        (
            format!("{reached_at} saltos"),
            format!("{} em {:.1}s", plan.target, elapsed.as_secs_f64()),
        )
    } else if reached_at > 0 {
        (
            "não chegou".to_string(),
            format!("parou no salto {reached_at}"),
        )
    } else {
        (
            "não chegou".to_string(),
            format!("{} saltos sem resposta do destino", plan.max_ttl),
        )
    };
    rec.record(
        0,
        EventKind::Note(format!(
            "{} em {:.1}s",
            if arrived {
                format!("chegou em {reached_at} saltos")
            } else {
                "não chegou ao destino".to_string()
            },
            elapsed.as_secs_f64()
        )),
    );
    rec.report(headline, summary);
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    text.chars()
        .take(width.saturating_sub(1))
        .collect::<String>()
        + "…"
}
