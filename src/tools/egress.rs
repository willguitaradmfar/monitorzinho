//! Which ports this network actually lets out.
//!
//! Everything else here asks what a remote host will accept. This asks the opposite, and
//! it is the question behind a whole category of afternoons: the service is up, the
//! address is right, the firewall on the far side is open, and the connection still
//! never arrives — because the network *this* machine is on drops everything except 80
//! and 443. Corporate networks, hotel wifi, restrictive clouds.
//!
//! It works by connecting outward to a host that answers on every port. Anything that
//! connects is a port that leaves; anything that hangs is a port that doesn't.

use std::collections::HashMap;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use super::{EventKind, Execution, ParamSpec, Recorder, Tool};

/// The ports worth knowing about, and why each one is in the list: mail, remote access,
/// databases, message brokers, VPN, and the two everyone assumes work.
const COMMON: &str = "22,25,53,80,110,143,443,465,587,993,995,1194,1433,1723,3306,3389,5432,5672,5900,6379,8080,8443,9418,27017";
/// Answers on every TCP port by design, which is what makes this measurable at all.
const DEFAULT_TARGET: &str = "portquiz.net";

pub struct EgressTool;

impl Tool for EgressTool {
    fn id(&self) -> &'static str {
        "egress"
    }

    fn name(&self) -> &'static str {
        "Portas de saída"
    }

    fn description(&self) -> &'static str {
        "Descobre quais portas esta rede deixa sair — a pergunta ao contrário: não o que o destino aceita, mas o que daqui consegue partir"
    }

    fn params(&self) -> Vec<ParamSpec> {
        vec![
            ParamSpec::text(
                "alvo",
                "Alvo",
                DEFAULT_TARGET,
                "Precisa ser um host que atenda em qualquer porta. portquiz.net existe para isso; um servidor seu com um listener genérico serve igual",
            ),
            ParamSpec::text(
                "portas",
                "Portas",
                COMMON,
                "Lista e intervalos: 22,80,443,8000-8010",
            ),
            ParamSpec::text(
                "concorrencia",
                "Tentativas simultâneas",
                "32",
                "Quantas portas tentar ao mesmo tempo",
            ),
            ParamSpec::text(
                "timeout",
                "Tempo por porta (ms)",
                "3000",
                "Uma porta bloqueada normalmente não recusa: ela silencia, e só o tempo limite revela isso",
            ),
        ]
    }

    fn summarize(&self, params: &HashMap<&'static str, String>) -> String {
        let get = |key| params.get(key).map(String::as_str).unwrap_or("").trim();
        format!(
            "{} · {} porta(s)",
            get("alvo"),
            parse_ports(get("portas")).len()
        )
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
                "pronto para testar {} porta(s) de saída contra {}. Nada roda até você abrir",
                plan.ports.len(),
                plan.target
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
            test(plan, &recorder);
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
    address: SocketAddr,
    ports: Vec<u16>,
    workers: usize,
    timeout: Duration,
}

impl Plan {
    fn from(params: &HashMap<&'static str, String>) -> Result<Self, String> {
        let get = |key| params.get(key).map(String::as_str).unwrap_or("").trim();
        let target = match get("alvo") {
            "" => DEFAULT_TARGET.to_string(),
            host => host.to_string(),
        };
        let address = (target.as_str(), 80)
            .to_socket_addrs()
            .map_err(|e| format!("não consegui resolver {target}: {e}"))?
            .next()
            .ok_or_else(|| format!("{target} não resolveu"))?;
        let ports = parse_ports(match get("portas") {
            "" => COMMON,
            text => text,
        });
        if ports.is_empty() {
            return Err("informe ao menos uma porta".to_string());
        }
        Ok(Self {
            target,
            address,
            ports,
            workers: get("concorrencia").parse().unwrap_or(32).clamp(1, 256),
            timeout: Duration::from_millis(
                get("timeout")
                    .parse::<u64>()
                    .unwrap_or(3000)
                    .clamp(200, 30_000),
            ),
        })
    }
}

/// `22,80,443,8000-8010` → the ports, deduplicated and in order.
fn parse_ports(text: &str) -> Vec<u16> {
    let mut ports: Vec<u16> = Vec::new();
    for part in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        match part.split_once('-') {
            Some((from, to)) => {
                if let (Ok(from), Ok(to)) = (from.trim().parse::<u16>(), to.trim().parse::<u16>()) {
                    ports.extend(from.min(to)..=from.max(to));
                }
            }
            None => {
                if let Ok(port) = part.parse() {
                    ports.push(port);
                }
            }
        }
    }
    ports.sort_unstable();
    ports.dedup();
    ports
}

/// What one port did.
enum Result_ {
    Left(u16, Duration),
    Refused(u16),
    Blocked(u16),
}

fn test(plan: Plan, rec: &Recorder) {
    let started = Instant::now();
    rec.record(
        0,
        EventKind::Note(format!(
            "testando {} porta(s) contra {} ({}) — {} ms por porta",
            plan.ports.len(),
            plan.target,
            plan.address.ip(),
            plan.timeout.as_millis()
        )),
    );

    let plan = Arc::new(plan);
    let cursor = Arc::new(AtomicUsize::new(0));
    let workers: Vec<_> = (0..plan.workers.min(plan.ports.len()))
        .map(|_| {
            let (plan, cursor, rec) = (Arc::clone(&plan), Arc::clone(&cursor), rec.clone());
            thread::spawn(move || {
                let mut results = Vec::new();
                loop {
                    if rec.stopping() {
                        break;
                    }
                    let index = cursor.fetch_add(1, Ordering::Relaxed);
                    let Some(&port) = plan.ports.get(index) else {
                        break;
                    };
                    let address = SocketAddr::new(plan.address.ip(), port);
                    let at = Instant::now();
                    results.push(match TcpStream::connect_timeout(&address, plan.timeout) {
                        Ok(_) => Result_::Left(port, at.elapsed()),
                        // A refusal is a reply, and a reply had to leave this network:
                        // the port is open on the way out even though nothing answered
                        // on the other end.
                        Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                            Result_::Refused(port)
                        }
                        Err(_) => Result_::Blocked(port),
                    });
                }
                results
            })
        })
        .collect();

    let mut results: Vec<Result_> = workers
        .into_iter()
        .filter_map(|worker| worker.join().ok())
        .flatten()
        .collect();
    results.sort_by_key(|result| match result {
        Result_::Left(port, _) | Result_::Refused(port) | Result_::Blocked(port) => *port,
    });

    let (mut out, mut blocked) = (Vec::new(), Vec::new());
    for result in &results {
        match result {
            Result_::Left(port, rtt) => {
                out.push(*port);
                rec.record(
                    0,
                    EventKind::Note(format!(
                        "{port:>6}  sai   ({:.0} ms)",
                        rtt.as_secs_f64() * 1000.0
                    )),
                );
            }
            Result_::Refused(port) => {
                out.push(*port);
                rec.record(
                    0,
                    EventKind::Note(format!(
                        "{port:>6}  sai   (recusada no destino — a saída funciona)"
                    )),
                );
            }
            Result_::Blocked(port) => {
                blocked.push(*port);
                rec.record(0, EventKind::Error(format!("{port:>6}  BLOQUEADA")));
            }
        }
    }

    rec.record(
        0,
        EventKind::Note(format!(
            "{} de {} porta(s) saem; {} bloqueada(s) em {:.1}s",
            out.len(),
            results.len(),
            blocked.len(),
            started.elapsed().as_secs_f64()
        )),
    );
    if !blocked.is_empty() {
        rec.record(
            0,
            EventKind::Note(format!(
                "bloqueadas: {}",
                blocked
                    .iter()
                    .map(u16::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        );
    }
    rec.report(
        format!("{} de {} saem", out.len(), results.len()),
        match blocked.len() {
            0 => "nada bloqueado".to_string(),
            n => format!("{n} bloqueada(s)"),
        },
    );
}
