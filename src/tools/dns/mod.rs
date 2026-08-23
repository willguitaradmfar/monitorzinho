//! A DNS investigation: everything a domain says about itself, and everything the
//! servers behind it say when asked directly.
//!
//! `dig` answers one question per invocation. What this does is the sweep you'd
//! otherwise run twenty times by hand and correlate in your head: the apex records, the
//! authoritative servers *asked individually* so a disagreement between them can't hide
//! behind a resolver's cache, mail and its policy records, DNSSEC at both ends of the
//! delegation, the registry's view from WHOIS, and whether any of the nameservers will
//! hand out the whole zone to a stranger.
//!
//! Like the port scanner, nothing runs until the execution is opened.

pub mod wire;

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::{EventKind, Execution, ParamSpec, Recorder, Tool};
use wire::{Rdata, Record, Response};

/// Names tried under the domain when subdomain discovery is on. Not a wordlist in the
/// brute-force sense — the point is the handful that are almost always there and
/// almost always interesting, not exhausting the namespace.
const SUBDOMAINS: &[&str] = &[
    "www",
    "mail",
    "smtp",
    "imap",
    "pop",
    "webmail",
    "autodiscover",
    "autoconfig",
    "mx",
    "mx1",
    "mx2",
    "ns1",
    "ns2",
    "ns3",
    "ftp",
    "sftp",
    "vpn",
    "remote",
    "api",
    "api-dev",
    "app",
    "admin",
    "portal",
    "intranet",
    "internal",
    "dev",
    "staging",
    "test",
    "qa",
    "beta",
    "demo",
    "docs",
    "help",
    "support",
    "status",
    "blog",
    "shop",
    "store",
    "pay",
    "cdn",
    "static",
    "assets",
    "media",
    "img",
    "files",
    "s3",
    "backup",
    "db",
    "mysql",
    "postgres",
    "redis",
    "git",
    "gitlab",
    "jenkins",
    "ci",
    "grafana",
    "kibana",
    "monitor",
    "metrics",
    "auth",
    "sso",
    "id",
    "login",
    "secure",
    "old",
    "new",
    "www2",
    "m",
    "mobile",
];

/// Concurrent lookups during the subdomain sweep. A resolver will happily take this
/// many; going much wider mostly earns rate limiting.
const SUBDOMAIN_WORKERS: usize = 24;
/// Lines of WHOIS text kept. Registries pad the answer with pages of legal notice, and
/// this is where the substance has always ended by.
const WHOIS_LINES: usize = 80;
/// Where the useful part of a WHOIS response stops. Everything past one of these is the
/// registry's terms of use, repeated identically for every domain.
const WHOIS_BOILERPLATE: &[&str] = &[
    ">>> last update",
    "terms of use",
    "by submitting",
    "the data contained",
    "notice and terms",
    "for more information",
    "url of the icann",
    "please register your domains",
];

/// The resolvers a propagation check asks by default: the ones with enough of the
/// world behind them that agreement actually means something.
const PUBLIC_RESOLVERS: &str =
    "8.8.8.8, 8.8.4.4, 1.1.1.1, 1.0.0.1, 9.9.9.9, 208.67.222.222, 94.140.14.14, 64.6.64.6";

/// Who is behind an address, so the propagation table reads as operators rather than
/// as eight numbers. Anything not listed simply shows its address.
const RESOLVER_NAMES: &[(&str, &str)] = &[
    ("8.8.8.8", "Google"),
    ("8.8.4.4", "Google 2"),
    ("1.1.1.1", "Cloudflare"),
    ("1.0.0.1", "Cloudflare 2"),
    ("9.9.9.9", "Quad9"),
    ("149.112.112.112", "Quad9 2"),
    ("208.67.222.222", "OpenDNS"),
    ("208.67.220.220", "OpenDNS 2"),
    ("94.140.14.14", "AdGuard"),
    ("64.6.64.6", "Verisign"),
    ("4.2.2.1", "Level3"),
    ("77.88.8.8", "Yandex"),
    ("185.228.168.9", "CleanBrowsing"),
];

/// Record types compared across resolvers. Enough to catch a migration in progress
/// without asking eight servers forty questions.
const PROPAGATION_TYPES: &[u16] = &[wire::A, wire::AAAA, wire::NS, wire::MX];

const YES_NO: &[&str] = &["sim", "não"];
/// How subdomains are found. The distinction the modes exist for: one asks the zone
/// what it contains, the other guesses names and sees what sticks.
const SUBDOMAIN_MODES: &[&str] = &["tudo", "sem adivinhação", "lista comum", "não"];

/// Names in the walk before it gives up. A zone with more than this isn't going to be
/// read in a terminal, and it's the guard against a chain that never closes.
const WALK_LIMIT: usize = 500;

pub struct DnsTool;

impl Tool for DnsTool {
    fn id(&self) -> &'static str {
        "dns"
    }

    fn name(&self) -> &'static str {
        "Investigação DNS"
    }

    fn description(&self) -> &'static str {
        "Varre tudo que um domínio publica: registros, servidores autoritativos, e-mail, DNSSEC, WHOIS e transferência de zona"
    }

    fn params(&self) -> Vec<ParamSpec> {
        vec![
            ParamSpec::text(
                "dominio",
                "Domínio",
                "example.com",
                "O domínio a investigar, sem protocolo. Subdomínios também servem",
            ),
            ParamSpec::text(
                "resolvedor",
                "Resolvedor",
                "",
                "Vazio usa os do /etc/resolv.conf. Aceita IP ou host: 1.1.1.1, 8.8.8.8:53",
            ),
            ParamSpec::choice(
                "subdominios",
                "Procurar subdomínios",
                SUBDOMAIN_MODES,
                "«sem adivinhação» lê os nomes que a própria zona publica; «lista comum» tenta ~70 nomes conhecidos",
            ),
            ParamSpec::choice(
                "propagacao",
                "Checar propagação",
                YES_NO,
                "Faz a mesma pergunta a vários resolvedores públicos e mostra quem já enxerga o valor novo",
            ),
            ParamSpec::text(
                "servidores",
                "Servidores da propagação",
                PUBLIC_RESOLVERS,
                "Lista fixa consultada na checagem, separada por vírgula. Aceita IP ou host, com porta opcional",
            ),
            ParamSpec::choice(
                "whois",
                "Consultar WHOIS",
                YES_NO,
                "Segue a cadeia IANA → registro do TLD → registrador para achar dono, datas e status",
            ),
            ParamSpec::choice(
                "axfr",
                "Tentar transferência de zona",
                YES_NO,
                "Pede a zona inteira a cada servidor autoritativo. Servidor bem configurado recusa — quem aceita é o achado",
            ),
            ParamSpec::text(
                "timeout",
                "Tempo por consulta (ms)",
                "3000",
                "Quanto esperar por cada resposta antes de desistir dela",
            ),
        ]
    }

    fn summarize(&self, params: &HashMap<&'static str, String>) -> String {
        let get = |key| params.get(key).map(String::as_str).unwrap_or("").trim();
        let domain = if get("dominio").is_empty() {
            "?"
        } else {
            get("dominio")
        };
        let via = match get("resolvedor") {
            "" => "resolvedor do sistema".to_string(),
            server => format!("via {server}"),
        };
        format!("{domain}  ·  {via}")
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
                "pronto para investigar {} via {}. Nada roda até você abrir",
                plan.domain, plan.resolver
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
            investigate(plan, &recorder);
            recorder.ran();
            finished.store(true, Ordering::Relaxed);
        });
    }

    fn columns(&self, execution: &Execution) -> (String, String) {
        execution.outcome()
    }
}

#[derive(Clone)]
struct Plan {
    domain: String,
    resolver: SocketAddr,
    /// Resolvers the propagation check asks, with the label to show for each.
    propagation: Vec<(String, SocketAddr)>,
    /// Try a list of likely names and see which answer.
    guessing: bool,
    /// Read the names the zone itself publishes — no guessing involved.
    derived: bool,
    whois: bool,
    axfr: bool,
    timeout: Duration,
}

impl Plan {
    fn from(params: &HashMap<&'static str, String>) -> Result<Self, String> {
        let get = |key| params.get(key).map(String::as_str).unwrap_or("").trim();

        let domain = get("dominio")
            .trim_start_matches("http://")
            .trim_start_matches("https://")
            .trim_end_matches('/')
            .trim_end_matches('.')
            .to_ascii_lowercase();
        if domain.is_empty() || !domain.contains('.') {
            return Err("informe um domínio, como example.com".to_string());
        }
        if domain.contains(|c: char| c.is_whitespace() || c == '/') {
            return Err(format!("domínio inválido: «{domain}»"));
        }

        let resolver = match get("resolvedor") {
            "" => wire::system_resolvers()
                .into_iter()
                .next()
                .ok_or("nenhum resolvedor no /etc/resolv.conf — preencha o campo")?,
            text => wire::resolver_from(text)?,
        };

        let timeout = get("timeout")
            .parse::<u64>()
            .map_err(|_| format!("tempo por consulta: «{}» não é um número", get("timeout")))?
            .clamp(200, 60_000);

        // A server that doesn't resolve is dropped with a note rather than failing the
        // whole execution: one dead entry in a list of eight shouldn't stop the check.
        let propagation = if get("propagacao") == "não" {
            Vec::new()
        } else {
            get("servidores")
                .split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .filter_map(|entry| {
                    wire::resolver_from(entry)
                        .ok()
                        .map(|addr| (resolver_label(entry, &addr), addr))
                })
                .collect()
        };

        let mode = get("subdominios");
        Ok(Self {
            domain,
            resolver,
            propagation,
            guessing: mode == "tudo" || mode == "lista comum",
            derived: mode == "tudo" || mode == "sem adivinhação",
            whois: get("whois") != "não",
            axfr: get("axfr") != "não",
            timeout: Duration::from_millis(timeout),
        })
    }
}

/// `8.8.8.8` reads better as `Google (8.8.8.8)`, and a hostname keeps what was typed.
fn resolver_label(typed: &str, addr: &SocketAddr) -> String {
    let ip = addr.ip().to_string();
    match RESOLVER_NAMES.iter().find(|(address, _)| *address == ip) {
        Some((_, name)) => format!("{name} ({ip})"),
        None if typed == ip => ip,
        None => format!("{typed} ({ip})"),
    }
}

/// Running tally, so the summary at the end doesn't have to re-walk everything.
#[derive(Default)]
struct Tally {
    records: usize,
    nameservers: usize,
    mx: usize,
    subdomains: usize,
    dnssec: bool,
    /// Resolvers that agreed with the majority, out of those that answered.
    agreeing: Option<(usize, usize)>,
    /// The zone answers for names that don't exist.
    wildcard: bool,
    /// Names a nameserver handed over wholesale. When this is non-empty the zone's
    /// contents are known exactly, and nothing downstream needs to infer them.
    transferred: HashSet<String>,
    transferable: usize,
    addresses: Vec<IpAddr>,
    expiry: Option<String>,
}

fn investigate(plan: Plan, rec: &Recorder) {
    let started = Instant::now();
    let mut tally = Tally::default();

    rec.record(
        0,
        EventKind::Note(format!(
            "investigando {} via {} — resolvedor, apex, autoritativos, e-mail, DNSSEC{}{}",
            plan.domain,
            plan.resolver,
            if plan.whois { ", WHOIS" } else { "" },
            if plan.axfr { ", AXFR" } else { "" }
        )),
    );
    rec.report("investigando…", "");

    rec.found("dominio", plan.domain.clone());
    let apex = sweep_apex(&plan, rec, &mut tally);
    let servers = authoritative(&plan, rec, &mut tally);
    compare_serials(&plan, rec, &servers);
    if plan.axfr {
        try_transfers(&plan, rec, &servers, &mut tally);
    }
    if !plan.propagation.is_empty() {
        propagation(&plan, rec, &mut tally);
    }
    mail(&plan, rec, &apex, &mut tally);
    dnssec(&plan, rec, &mut tally);
    reverse(&plan, rec, &tally.addresses.clone());
    if plan.derived {
        derived_names(&plan, rec, &apex, &servers, &mut tally);
    }
    if plan.guessing {
        subdomains(&plan, rec, &mut tally);
    }
    if plan.whois {
        whois(&plan, rec, &mut tally);
    }

    // Published for the port scanner to pick up, de-duplicated and in a stable order.
    let mut unique: Vec<String> = tally
        .addresses
        .iter()
        .map(|ip| ip.to_string())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    unique.sort();
    for address in &unique {
        rec.found("ip", address);
    }

    let elapsed = started.elapsed();
    rec.record(
        0,
        EventKind::Note(format!(
            "investigação {}: {} registros, {} servidor(es) autoritativo(s), {} endereço(s), em {:.1}s",
            if rec.stopping() { "interrompida" } else { "concluída" },
            tally.records,
            tally.nameservers,
            unique.len(),
            elapsed.as_secs_f64()
        )),
    );
    rec.report(
        format!("{} registros", tally.records),
        summary_line(&tally, unique.len(), elapsed),
    );
}

/// Characters the row's summary column gets. Everything that doesn't fit is dropped
/// from the end, which is why the parts below are built worst-news-first.
const SUMMARY_WIDTH: usize = 42;

/// The row's summary, in priority order: anything alarming, then the shape of the
/// domain, then the pleasantries. A column that fits three facts should be showing the
/// three that matter, not the three that happened to be computed first.
fn summary_line(tally: &Tally, addresses: usize, elapsed: Duration) -> String {
    let mut parts = Vec::new();
    if tally.transferable > 0 {
        parts.push(format!("⚠ {} AXFR aberto", tally.transferable));
    }
    if let Some((agree, total)) = tally.agreeing {
        parts.push(if agree == total {
            format!("propagado {agree}/{total}")
        } else {
            format!("⚠ propagação {agree}/{total}")
        });
    }
    parts.push(format!("{} NS", tally.nameservers));
    if tally.mx > 0 {
        parts.push(format!("{} MX", tally.mx));
    }
    if addresses > 0 {
        parts.push(format!("{addresses} IP"));
    }
    if tally.dnssec {
        parts.push("DNSSEC".to_string());
    }
    if tally.wildcard {
        parts.push("curinga *".to_string());
    }
    if tally.subdomains > 0 {
        parts.push(format!("{} sub", tally.subdomains));
    }
    if let Some(expiry) = &tally.expiry {
        parts.push(format!("expira {expiry}"));
    }
    parts.push(format!("{:.1}s", elapsed.as_secs_f64()));

    let mut line = String::new();
    for part in parts {
        let extra = if line.is_empty() { 0 } else { 3 };
        if line.chars().count() + extra + part.chars().count() > SUMMARY_WIDTH {
            continue;
        }
        if !line.is_empty() {
            line.push_str(" · ");
        }
        line.push_str(&part);
    }
    line
}

/// Announces a section, so the log reads as an investigation rather than as a dump.
fn section(rec: &Recorder, title: &str) {
    rec.record(0, EventKind::Note(format!("── {title} ──")));
    // Also the row's summary column: an investigation can take minutes, and "still
    // going" says less than which of nine steps it is on.
    rec.report("investigando…", title.to_string());
}

fn ask(plan: &Plan, name: &str, rtype: u16) -> Result<Response, String> {
    wire::query(plan.resolver, name, rtype, plan.timeout)
}

/// Records every answer and returns them, counting as it goes.
fn log_records(rec: &Recorder, records: &[Record], tally: &mut Tally) {
    for record in records {
        tally.records += 1;
        if let Rdata::Addr(ip) = record.data {
            tally.addresses.push(ip);
        }
        rec.record(0, EventKind::Note(format!("   {}", record.line())));
    }
}

fn sweep_apex(plan: &Plan, rec: &Recorder, tally: &mut Tally) -> Vec<Record> {
    section(rec, &format!("registros de {}", plan.domain));
    let mut all = Vec::new();
    for &rtype in wire::APEX_TYPES {
        if rec.stopping() {
            break;
        }
        match ask(plan, &plan.domain, rtype) {
            Ok(response) if response.rcode == 0 && !response.answers.is_empty() => {
                log_records(rec, &response.answers, tally);
                all.extend(response.answers);
            }
            Ok(response) if response.rcode != 0 => rec.record(
                0,
                EventKind::Note(format!(
                    "   {:<7} {}",
                    wire::type_name(rtype),
                    wire::rcode_name(response.rcode)
                )),
            ),
            Ok(_) => {}
            Err(e) => rec.record(
                0,
                EventKind::Error(format!(
                    "{} de {}: {e}",
                    wire::type_name(rtype),
                    plan.domain
                )),
            ),
        }
    }
    if all.is_empty() {
        rec.record(
            0,
            EventKind::Error(format!("{} não publicou nenhum registro", plan.domain)),
        );
        // The SOA a server returns in the authority section of an empty answer is what
        // says *which* zone is denying the name — the difference between "this domain
        // has no records" and "this domain isn't delegated at all".
        if let Ok(response) = ask(plan, &plan.domain, wire::SOA) {
            for record in &response.authority {
                rec.record(
                    0,
                    EventKind::Note(format!("   quem nega: {}", record.line())),
                );
            }
        }
    }
    all
}

/// The nameservers, resolved to addresses so the next steps can talk to them directly
/// instead of through a cache.
fn authoritative(plan: &Plan, rec: &Recorder, tally: &mut Tally) -> Vec<(String, SocketAddr)> {
    section(rec, "servidores autoritativos");
    let mut servers = Vec::new();
    let response = match ask(plan, &plan.domain, wire::NS) {
        Ok(response) => response,
        Err(e) => {
            rec.record(0, EventKind::Error(format!("NS de {}: {e}", plan.domain)));
            return servers;
        }
    };
    for record in response.of_type(wire::NS) {
        let Rdata::Name(host) = &record.data else {
            continue;
        };
        tally.nameservers += 1;
        // Glue first: a delegation usually ships the nameservers' addresses in the
        // additional section, and those are the ones resolution actually uses.
        let mut addresses: Vec<IpAddr> = response
            .additional
            .iter()
            .filter(|extra| extra.name.eq_ignore_ascii_case(host))
            .filter_map(|extra| match extra.data {
                Rdata::Addr(ip) => Some(ip),
                _ => None,
            })
            .collect();
        if addresses.is_empty() {
            for rtype in [wire::A, wire::AAAA] {
                if let Ok(answer) = ask(plan, host, rtype) {
                    for found in answer.of_type(rtype) {
                        if let Rdata::Addr(ip) = found.data {
                            addresses.push(ip);
                        }
                    }
                }
            }
        }
        tally.addresses.extend(addresses.iter().copied());
        rec.record(
            0,
            EventKind::Note(format!(
                "   {host} → {}",
                if addresses.is_empty() {
                    "não resolveu".to_string()
                } else {
                    addresses
                        .iter()
                        .map(|ip| ip.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            )),
        );
        if let Some(&ip) = addresses.first() {
            servers.push((host.clone(), SocketAddr::new(ip, 53)));
        }
    }
    servers
}

/// The check a resolver can't do for you: ask each authoritative server for the zone's
/// serial and see whether they agree. A stale secondary is invisible until you look.
fn compare_serials(plan: &Plan, rec: &Recorder, servers: &[(String, SocketAddr)]) {
    if servers.len() < 2 {
        return;
    }
    section(rec, "os autoritativos concordam?");
    let mut serials: Vec<(String, String)> = Vec::new();
    for (host, addr) in servers {
        if rec.stopping() {
            return;
        }
        let answer = match wire::query(*addr, &plan.domain, wire::SOA, plan.timeout) {
            Ok(answer) => answer,
            Err(e) => {
                rec.record(0, EventKind::Error(format!("   {host}: {e}")));
                serials.push((host.clone(), "sem resposta".to_string()));
                continue;
            }
        };
        let serial = answer
            .of_type(wire::SOA)
            .first()
            .and_then(|record| match &record.data {
                Rdata::Soa { serial, .. } => Some(serial.to_string()),
                _ => None,
            })
            .unwrap_or_else(|| wire::rcode_name(answer.rcode).to_string());
        rec.record(
            0,
            EventKind::Note(format!(
                "   {host:<32} serial {serial}{}",
                if answer.authoritative {
                    ""
                } else {
                    "  (não autoritativo!)"
                }
            )),
        );
        serials.push((host.clone(), serial));
    }
    let distinct: HashSet<&String> = serials.iter().map(|(_, serial)| serial).collect();
    if distinct.len() > 1 {
        rec.record(
            0,
            EventKind::Error(format!(
                "servidores fora de sincronia — {} seriais diferentes",
                distinct.len()
            )),
        );
    } else {
        rec.record(
            0,
            EventKind::Note("   todos com o mesmo serial".to_string()),
        );
    }
}

fn try_transfers(plan: &Plan, rec: &Recorder, servers: &[(String, SocketAddr)], tally: &mut Tally) {
    section(rec, "transferência de zona (AXFR)");
    for (host, addr) in servers {
        if rec.stopping() {
            return;
        }
        match wire::zone_transfer(*addr, &plan.domain, plan.timeout) {
            wire::Transfer::Zone(records) => {
                tally.transferable += 1;
                rec.record(
                    0,
                    EventKind::Error(format!(
                        "   {host} ENTREGOU a zona inteira — {} registros expostos",
                        records.len()
                    )),
                );
                // The actual list, straight from the server. Nothing discovered later
                // needs to be guessed at or walked to when this worked.
                let suffix = format!(".{}", plan.domain);
                for record in &records {
                    let name = record.name.trim_end_matches('.').to_ascii_lowercase();
                    if name.ends_with(&suffix) {
                        tally.transferred.insert(name);
                    }
                }
                for record in records.iter().take(200) {
                    tally.records += 1;
                    rec.record(0, EventKind::Note(format!("      {}", record.line())));
                }
                if records.len() > 200 {
                    rec.record(
                        0,
                        EventKind::Note(format!(
                            "      … e mais {} registros, todos contados na descoberta",
                            records.len() - 200
                        )),
                    );
                }
            }
            wire::Transfer::Refused(rcode) => rec.record(
                0,
                EventKind::Note(format!("   {host}: recusou com {rcode} — correto")),
            ),
            // Not the same as a refusal: nobody said no, the question never landed.
            wire::Transfer::NoAnswer(why) => rec.record(
                0,
                EventKind::Note(format!("   {host}: sem resposta em TCP/53 ({why})")),
            ),
        }
    }
}

/// Asks the same question of every listed resolver and compares the answers.
///
/// This is the check you can't do from one machine with `dig`: a record that changed an
/// hour ago is live for whoever's resolver has expired its cache and stale for everyone
/// else, and the only way to know how far along that is, is to go and ask. Divergence
/// isn't necessarily wrong — mid-migration it's expected — but it's the thing worth
/// seeing, so a resolver that disagrees with the majority is called out by name.
fn propagation(plan: &Plan, rec: &Recorder, tally: &mut Tally) {
    section(rec, "propagação");

    let mut answers: HashMap<String, Vec<(String, String)>> = HashMap::new();
    let handles: Vec<_> = plan
        .propagation
        .iter()
        .map(|(label, addr)| {
            let (label, addr, plan, rec) = (label.clone(), *addr, plan.clone(), rec.clone());
            thread::spawn(move || {
                let mut seen = Vec::new();
                for &rtype in PROPAGATION_TYPES {
                    if rec.stopping() {
                        break;
                    }
                    let value = match wire::query(addr, &plan.domain, rtype, plan.timeout) {
                        Ok(response) if response.rcode == 0 => normalize(&response, rtype),
                        Ok(response) => wire::rcode_name(response.rcode).to_string(),
                        Err(_) => "sem resposta".to_string(),
                    };
                    seen.push((wire::type_name(rtype), value));
                }
                (label, seen)
            })
        })
        .collect();
    for handle in handles {
        if let Ok((label, seen)) = handle.join() {
            for (rtype, value) in seen {
                answers
                    .entry(rtype)
                    .or_default()
                    .push((label.clone(), value));
            }
        }
    }

    let mut agree_total = 0usize;
    let mut answered_total = 0usize;
    for &rtype in PROPAGATION_TYPES {
        let name = wire::type_name(rtype);
        let Some(rows) = answers.get(&name) else {
            continue;
        };
        // "Nothing published" is unanimous agreement about nothing; saying so for every
        // type a domain doesn't use would bury the types it does.
        if rows.iter().all(|(_, value)| value.is_empty()) {
            continue;
        }

        let mut counts: HashMap<&String, usize> = HashMap::new();
        for (_, value) in rows {
            *counts.entry(value).or_default() += 1;
        }
        let majority = counts
            .iter()
            .max_by_key(|(_, count)| **count)
            .map(|(value, _)| (*value).clone())
            .unwrap_or_default();

        rec.record(0, EventKind::Note(format!("   {name}")));
        let mut sorted = rows.clone();
        sorted.sort();
        for (label, value) in &sorted {
            let agrees = *value == majority;
            answered_total += 1;
            if agrees {
                agree_total += 1;
            }
            let shown = if value.is_empty() {
                "(vazio)".to_string()
            } else {
                value.clone()
            };
            let line = format!(
                "      {:<22} {}  {shown}",
                label,
                if agrees { "·" } else { "✗" }
            );
            if agrees {
                rec.record(0, EventKind::Note(line));
            } else {
                rec.record(0, EventKind::Error(line));
            }
        }
        if counts.len() > 1 {
            rec.record(
                0,
                EventKind::Error(format!(
                    "   {name}: {} respostas diferentes — ainda propagando ou zona inconsistente",
                    counts.len()
                )),
            );
        }
    }

    if answered_total > 0 {
        tally.agreeing = Some((agree_total, answered_total));
        rec.record(
            0,
            EventKind::Note(format!(
                "   {agree_total} de {answered_total} respostas batem com a maioria"
            )),
        );
    }
}

/// One answer reduced to a value comparable across servers: sorted, so the order a
/// resolver happens to rotate its records in isn't mistaken for a difference.
fn normalize(response: &Response, rtype: u16) -> String {
    let mut values: Vec<String> = response
        .of_type(rtype)
        .iter()
        .map(|record| record.data.text())
        .collect();
    values.sort();
    values.join(", ")
}

fn mail(plan: &Plan, rec: &Recorder, apex: &[Record], tally: &mut Tally) {
    section(rec, "e-mail");
    let hosts: Vec<(u16, String)> = apex
        .iter()
        .filter_map(|record| match &record.data {
            Rdata::Mx { preference, host } => Some((*preference, host.clone())),
            _ => None,
        })
        .collect();
    tally.mx = hosts.len();

    if hosts.is_empty() {
        rec.record(
            0,
            EventKind::Note("   sem MX — domínio não recebe e-mail".to_string()),
        );
    }
    for (preference, host) in &hosts {
        // Published so the certificate reader can be pointed at it: a mail exchanger
        // has a certificate too, and nobody ever remembers to check it.
        rec.found("mx", host.trim_end_matches('.'));
        let mut addresses = Vec::new();
        for rtype in [wire::A, wire::AAAA] {
            if let Ok(answer) = ask(plan, host, rtype) {
                for record in answer.of_type(rtype) {
                    if let Rdata::Addr(ip) = record.data {
                        addresses.push(ip.to_string());
                        tally.addresses.push(ip);
                    }
                }
            }
        }
        rec.record(
            0,
            EventKind::Note(format!(
                "   MX {preference:>3}  {host} → {}",
                if addresses.is_empty() {
                    "não resolveu (MX quebrado)".to_string()
                } else {
                    addresses.join(", ")
                }
            )),
        );
    }

    let spf: Vec<String> = apex
        .iter()
        .filter_map(|record| match &record.data {
            Rdata::Txt(parts) => {
                let joined = parts.join("");
                joined.starts_with("v=spf1").then_some(joined)
            }
            _ => None,
        })
        .collect();
    match spf.len() {
        0 => rec.record(0, EventKind::Note("   sem SPF".to_string())),
        1 => rec.record(0, EventKind::Note(format!("   SPF   {}", spf[0]))),
        n => rec.record(
            0,
            EventKind::Error(format!("   {n} registros SPF — só um é válido")),
        ),
    }

    for (label, name) in [
        ("DMARC", format!("_dmarc.{}", plan.domain)),
        ("MTA-STS", format!("_mta-sts.{}", plan.domain)),
    ] {
        match ask(plan, &name, wire::TXT) {
            Ok(answer) if !answer.answers.is_empty() => {
                for record in answer.of_type(wire::TXT) {
                    tally.records += 1;
                    rec.record(
                        0,
                        EventKind::Note(format!("   {label} {}", record.data.text())),
                    );
                }
            }
            _ if label == "DMARC" => {
                rec.record(0, EventKind::Note("   sem DMARC".to_string()));
            }
            _ => {}
        }
    }
}

fn dnssec(plan: &Plan, rec: &Recorder, tally: &mut Tally) {
    section(rec, "DNSSEC");
    let ds = ask(plan, &plan.domain, wire::DS)
        .map(|answer| answer.of_type(wire::DS).len())
        .unwrap_or(0);
    let keys = ask(plan, &plan.domain, wire::DNSKEY)
        .map(|answer| answer.of_type(wire::DNSKEY).len())
        .unwrap_or(0);
    tally.dnssec = ds > 0 && keys > 0;
    rec.record(
        0,
        EventKind::Note(match (ds, keys) {
            (0, 0) => "   não assinado — sem DS no pai e sem DNSKEY na zona".to_string(),
            (0, k) => format!("   {k} DNSKEY na zona, mas nenhum DS no pai — a cadeia não fecha"),
            (d, 0) => format!("   {d} DS no pai, mas nenhum DNSKEY na zona — resolução vai falhar"),
            (d, k) => format!("   assinado — {d} DS no pai, {k} DNSKEY na zona"),
        }),
    );
}

fn reverse(plan: &Plan, rec: &Recorder, addresses: &[IpAddr]) {
    let mut unique: Vec<IpAddr> = addresses
        .iter()
        .copied()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    unique.sort();
    if unique.is_empty() {
        return;
    }
    section(rec, "DNS reverso");
    for ip in unique.iter().take(32) {
        if rec.stopping() {
            return;
        }
        let name = reverse_name(ip);
        let answer = ask(plan, &name, wire::PTR)
            .ok()
            .and_then(|answer| answer.of_type(wire::PTR).first().map(|r| r.data.text()));
        rec.record(
            0,
            EventKind::Note(format!("   {ip:<40} {}", name_or_none(answer.as_deref()))),
        );
    }
}

fn name_or_none(name: Option<&str>) -> String {
    name.unwrap_or("(sem PTR)").to_string()
}

/// `1.2.3.4` becomes `4.3.2.1.in-addr.arpa`; v6 becomes a nibble-per-label ip6.arpa.
fn reverse_name(ip: &IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            format!("{}.{}.{}.{}.in-addr.arpa", o[3], o[2], o[1], o[0])
        }
        IpAddr::V6(v6) => {
            let mut name = String::new();
            for byte in v6.octets().iter().rev() {
                let _ = std::fmt::Write::write_fmt(
                    &mut name,
                    format_args!("{:x}.{:x}.", byte & 0x0F, byte >> 4),
                );
            }
            name.push_str("ip6.arpa");
            name
        }
    }
}

/// Names probed to find out whether the zone answers for everything. Three rather than
/// one, because a wildcard behind round-robin hands out a different address each time
/// and a single probe would only learn part of the set.
const WILDCARD_PROBES: usize = 3;

/// What a zone with a `*` record answers with, so real names can be told from the
/// wildcard answering on their behalf.
struct Wildcard {
    /// Every value seen across the probes — a name whose answer is drawn only from
    /// this set is indistinguishable from a name that doesn't exist.
    values: HashSet<String>,
}

impl Wildcard {
    /// Whether this answer is nothing but the wildcard talking.
    ///
    /// Subset, not equality: a round-robin wildcard returns a rotating slice of the
    /// same pool, so "everything it said came from the pool" is the test that holds.
    fn covers(&self, records: &[Record]) -> bool {
        !records.is_empty()
            && records
                .iter()
                .all(|record| self.values.contains(&record.data.text()))
    }
}

/// Asks the zone about names that cannot exist. If it answers, there's a `*` record and
/// every subsequent lookup would otherwise "succeed".
fn detect_wildcard(plan: &Plan, rec: &Recorder) -> Option<Wildcard> {
    let mut values = HashSet::new();
    for probe in 0..WILDCARD_PROBES {
        // Nothing about this label can be a real name: a fixed prefix nobody registers
        // plus the clock, so two runs never ask the same thing and hit a cache.
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(probe as u128);
        let name = format!("mz-inexistente-{nonce:x}-{probe}.{}", plan.domain);
        for rtype in [wire::A, wire::AAAA] {
            if let Ok(response) = wire::query(plan.resolver, &name, rtype, plan.timeout) {
                for record in &response.answers {
                    if matches!(record.data, Rdata::Addr(_) | Rdata::Name(_)) {
                        values.insert(record.data.text());
                    }
                }
            }
        }
    }
    if values.is_empty() {
        return None;
    }
    let mut listed: Vec<&String> = values.iter().collect();
    listed.sort();
    rec.record(
        0,
        EventKind::Error(format!(
            "   curinga detectado — *.{} responde {}",
            plan.domain,
            listed
                .iter()
                .map(|value| value.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    );
    rec.record(
        0,
        EventKind::Note(
            "   com curinga qualquer nome «resolve», então só entram na lista os que respondem algo diferente dele".to_string(),
        ),
    );
    Some(Wildcard { values })
}

fn subdomains(plan: &Plan, rec: &Recorder, tally: &mut Tally) {
    section(rec, "subdomínios");
    // Before anything else: a zone that answers for every name would otherwise turn
    // this sweep into seventy false positives.
    let wildcard = detect_wildcard(plan, rec);
    tally.wildcard = wildcard.is_some();
    let wildcard = Arc::new(wildcard);
    let cursor = Arc::new(AtomicUsize::new(0));
    let plan = Arc::new(plan.clone());

    let workers: Vec<_> = (0..SUBDOMAIN_WORKERS.min(SUBDOMAINS.len()))
        .map(|_| {
            let (plan, cursor, rec) = (Arc::clone(&plan), Arc::clone(&cursor), rec.clone());
            let wildcard = Arc::clone(&wildcard);
            thread::spawn(move || {
                let (mut found, mut masked) = (Vec::new(), Vec::new());
                loop {
                    if rec.stopping() {
                        break;
                    }
                    let index = cursor.fetch_add(1, Ordering::Relaxed);
                    let Some(label) = SUBDOMAINS.get(index) else {
                        break;
                    };
                    let name = format!("{label}.{}", plan.domain);
                    let mut answers = Vec::new();
                    for rtype in [wire::A, wire::AAAA] {
                        if let Ok(answer) = wire::query(plan.resolver, &name, rtype, plan.timeout) {
                            for record in answer.answers {
                                if matches!(record.data, Rdata::Addr(_) | Rdata::Name(_)) {
                                    answers.push(record);
                                }
                            }
                        }
                    }
                    match wildcard.as_ref() {
                        Some(wildcard) if wildcard.covers(&answers) => masked.push(*label),
                        _ => found.extend(answers),
                    }
                }
                (found, masked)
            })
        })
        .collect();

    let (mut found, mut masked): (Vec<Record>, Vec<&str>) = (Vec::new(), Vec::new());
    for worker in workers {
        if let Ok((records, hidden)) = worker.join() {
            found.extend(records);
            masked.extend(hidden);
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name).then(a.rtype.cmp(&b.rtype)));
    found.dedup_by(|a, b| a.name == b.name && a.rtype == b.rtype && a.data == b.data);

    let names: HashSet<&String> = found.iter().map(|record| &record.name).collect();
    tally.subdomains = names.len();
    // Each name that exists is a domain in its own right — worth its own investigation
    // or its own certificate, and the hand-off picker is where that gets decided.
    for name in &names {
        rec.found("dominio", name.trim_end_matches('.'));
    }
    if found.is_empty() {
        rec.record(
            0,
            EventKind::Note("   nenhum nome testado existe de verdade".to_string()),
        );
    }
    log_records(rec, &found, tally);

    // Named rather than merely counted: with a wildcard, "api didn't show up" and "api
    // was hidden because everything resolves" are different facts, and only one of them
    // means the name isn't there.
    if !masked.is_empty() {
        masked.sort_unstable();
        rec.record(
            0,
            EventKind::Note(format!(
                "   {} nome(s) responderam só o curinga, então não contam: {}",
                masked.len(),
                masked.join(", ")
            )),
        );
    }
}

/// Subdomains without guessing at any of them.
///
/// DNS has no "list everything" for the public — that's deliberate — but three things
/// come close, and none of them involves inventing a name:
///
/// * **NSEC walking.** A zone signed with NSEC proves a name doesn't exist by naming
///   the next one that does, so following the chain reads the zone off the wire, name
///   by name, exactly as it is. NSEC3 hashes those names and closes the door.
/// * **What the records already say.** Nameservers, mail exchangers, CNAME targets, the
///   hosts an SPF record authorises, the address DMARC reports go to — every one is a
///   real name the domain published about itself.
/// * **AXFR**, which is the actual list operation, and which the zone-transfer section
///   already tries.
fn derived_names(
    plan: &Plan,
    rec: &Recorder,
    apex: &[Record],
    servers: &[(String, SocketAddr)],
    tally: &mut Tally,
) {
    section(rec, "nomes publicados pela própria zona");

    // A zone transfer, if one worked, is the list itself — every other source here is
    // an inference, and this one isn't.
    let mut names: HashSet<String> = tally.transferred.clone();
    if !names.is_empty() {
        rec.record(
            0,
            EventKind::Note(format!(
                "   {} nome(s) vieram da transferência de zona — lista completa, sem inferência",
                names.len()
            )),
        );
    }
    for record in apex {
        collect_names(&record.data, &plan.domain, &mut names);
    }
    // The mail policy records name hosts too, and they're fetched separately.
    for extra in [
        format!("_dmarc.{}", plan.domain),
        format!("_mta-sts.{}", plan.domain),
    ] {
        if let Ok(response) = ask(plan, &extra, wire::TXT) {
            for record in &response.answers {
                collect_names(&record.data, &plan.domain, &mut names);
            }
        }
    }
    let published = names.len();
    if published > 0 {
        rec.record(
            0,
            EventKind::Note(format!(
                "   {published} nome(s) citados nos registros do domínio"
            )),
        );
    }
    for name in &names {
        rec.found("dominio", name.trim_end_matches('.'));
    }

    let mut external: HashSet<String> = HashSet::new();
    for record in apex {
        collect_external(&record.data, &plan.domain, &mut external);
    }
    if !external.is_empty() {
        let mut listed: Vec<String> = external.into_iter().collect();
        listed.sort();
        rec.record(
            0,
            EventKind::Note(format!("   aponta para {} host(s) de fora:", listed.len())),
        );
        for host in &listed {
            rec.record(0, EventKind::Note(format!("      {host}")));
        }
    }

    let walked = nsec_walk(plan, rec, servers, &mut names);
    names.remove(&plan.domain);
    if names.is_empty() {
        rec.record(
            0,
            EventKind::Note("   a zona não expõe nome nenhum sem adivinhação".to_string()),
        );
        return;
    }

    let mut sorted: Vec<String> = names.into_iter().collect();
    sorted.sort();
    // A walked zone can be hundreds of names, and resolving them one at a time is the
    // difference between seconds and minutes. Only A and AAAA are asked: a name that is
    // a CNAME answers both with the alias in front of the target.
    let found = resolve_all(plan, rec, &sorted);
    let distinct: HashSet<&String> = found.iter().map(|record| &record.name).collect();
    tally.subdomains += distinct.len();
    log_records(rec, &found, tally);

    rec.record(
        0,
        EventKind::Note(format!(
            "   {} nome(s) reais: {} da transferência, {published} dos registros, {walked} da caminhada NSEC — nenhum adivinhado",
            sorted.len(),
            tally.transferred.len()
        )),
    );
}

/// Resolves a list of names concurrently, returning what actually answered.
fn resolve_all(plan: &Plan, rec: &Recorder, names: &[String]) -> Vec<Record> {
    let cursor = Arc::new(AtomicUsize::new(0));
    let names = Arc::new(names.to_vec());
    let plan = Arc::new(plan.clone());
    let total = names.len();
    let done = Arc::new(AtomicUsize::new(0));

    let workers: Vec<_> = (0..SUBDOMAIN_WORKERS.min(total.max(1)))
        .map(|_| {
            let (plan, names, cursor, done, rec) = (
                Arc::clone(&plan),
                Arc::clone(&names),
                Arc::clone(&cursor),
                Arc::clone(&done),
                rec.clone(),
            );
            thread::spawn(move || {
                let mut found = Vec::new();
                loop {
                    if rec.stopping() {
                        break;
                    }
                    let index = cursor.fetch_add(1, Ordering::Relaxed);
                    let Some(name) = names.get(index) else {
                        break;
                    };
                    for rtype in [wire::A, wire::AAAA] {
                        if let Ok(response) = wire::query(plan.resolver, name, rtype, plan.timeout)
                        {
                            for record in response.answers {
                                if matches!(record.data, Rdata::Addr(_) | Rdata::Name(_)) {
                                    found.push(record);
                                }
                            }
                        }
                    }
                    let seen = done.fetch_add(1, Ordering::Relaxed) + 1;
                    if seen.is_multiple_of(25) {
                        rec.report("investigando…", format!("resolvendo {seen}/{total}"));
                    }
                }
                found
            })
        })
        .collect();

    let mut found: Vec<Record> = workers
        .into_iter()
        .filter_map(|worker| worker.join().ok())
        .flatten()
        .collect();
    found.sort_by(|a, b| a.name.cmp(&b.name).then(a.rtype.cmp(&b.rtype)));
    found.dedup_by(|a, b| a.name == b.name && a.rtype == b.rtype && a.data == b.data);
    found
}

/// Every in-zone hostname a record points at.
fn collect_names(data: &Rdata, domain: &str, names: &mut HashSet<String>) {
    let mut push = |candidate: &str| {
        let candidate = candidate.trim_end_matches('.').to_ascii_lowercase();
        if candidate.ends_with(&format!(".{domain}")) {
            names.insert(candidate);
        }
    };
    match data {
        Rdata::Name(name) => push(name),
        Rdata::Mx { host, .. } => push(host),
        Rdata::Srv { target, .. } => push(target),
        Rdata::Soa { primary, .. } => push(primary),
        Rdata::Txt(parts) => {
            let text = parts.join("");
            // SPF authorises hosts by name, and DMARC sends reports to one. Both are
            // the domain telling you about its own infrastructure.
            for token in text.split_whitespace() {
                for prefix in ["include:", "a:", "mx:", "redirect=", "ptr:", "exists:"] {
                    if let Some(value) = token.strip_prefix(prefix) {
                        push(value);
                    }
                }
                if let Some(address) = token
                    .trim_start_matches("rua=")
                    .trim_start_matches("ruf=")
                    .split(':')
                    .next_back()
                    && let Some((_, host)) = address.split_once('@')
                {
                    push(host.trim_end_matches([',', ';']));
                }
            }
        }
        _ => {}
    }
}

/// Hosts the domain points at that live somewhere else — whoever runs its DNS, its
/// mail, whatever its SPF trusts. Not subdomains, but the answer to "who actually
/// operates this domain's infrastructure".
fn collect_external(data: &Rdata, domain: &str, hosts: &mut HashSet<String>) {
    let suffix = format!(".{domain}");
    let mut push = |candidate: &str| {
        let candidate = candidate.trim_end_matches('.').to_ascii_lowercase();
        if candidate.is_empty() || candidate.ends_with(&suffix) || candidate == domain {
            return;
        }
        // The host as published, not a guess at which part of it names a company.
        // Reducing `ns-1707.awsdns-21.co.uk` to an organisation needs the public suffix
        // list to be right, and a hardcoded slice of it is wrong for exactly the domains
        // nobody tests against. The full name is unambiguous and reads fine.
        hosts.insert(candidate);
    };
    match data {
        Rdata::Name(name) => push(name),
        Rdata::Mx { host, .. } => push(host),
        Rdata::Soa { primary, .. } => push(primary),
        Rdata::Txt(parts) => {
            for token in parts.join("").split_whitespace() {
                if let Some(value) = token.strip_prefix("include:") {
                    push(value);
                }
            }
        }
        _ => {}
    }
}

/// Follows the NSEC chain: every "this name doesn't exist" answer in a signed zone
/// names the next one that does, so the chain visits every name in order.
fn nsec_walk(
    plan: &Plan,
    rec: &Recorder,
    servers: &[(String, SocketAddr)],
    names: &mut HashSet<String>,
) -> usize {
    // The zone's own servers, not the machine's resolver. A stub like systemd-resolved
    // answers SERVFAIL to NSEC outright, and a recursor may strip what it treats as
    // internal to validation — the authoritative server publishes the record.
    let sources: Vec<(String, SocketAddr)> = if servers.is_empty() {
        vec![(plan.resolver.to_string(), plan.resolver)]
    } else {
        servers.to_vec()
    };

    // NSEC3 hashes the names in the chain, so walking it yields hashes rather than
    // names — which is the whole reason NSEC3 exists.
    for (_, addr) in &sources {
        match wire::query(*addr, &plan.domain, wire::NSEC3PARAM, plan.timeout) {
            Ok(response) if !response.answers.is_empty() => {
                rec.record(
                    0,
                    EventKind::Note(
                        "   zona usa NSEC3 — a cadeia é de hashes, não dá para caminhar"
                            .to_string(),
                    ),
                );
                return 0;
            }
            Ok(response) if response.rcode == 0 => break,
            _ => continue,
        }
    }

    // Every server is tried, and the longest chain wins. They do not all answer the
    // same: a secondary can be configured to synthesise a minimal NSEC that covers only
    // the name asked about, which truthfully denies existence and tells you nothing
    // about the zone. Taking whichever server happened to be listed first would make
    // the result depend on the order of an NS record set.
    let (mut best, mut best_from, mut complete) = (HashSet::new(), String::new(), false);
    for (host, addr) in &sources {
        if rec.stopping() {
            break;
        }
        let (found, finished) = walk_from(plan, rec, *addr);
        if found.len() > best.len() {
            (best, best_from, complete) = (found, host.clone(), finished);
        }
        // A chain that closed the loop back to the apex has read the whole zone; asking
        // anyone else can only repeat it.
        if complete {
            break;
        }
    }

    let walked = best.len();
    if walked > 0 {
        rec.record(
            0,
            EventKind::Note(format!(
                "   caminhada NSEC leu {walked} nome(s) direto de {best_from}{}",
                if complete {
                    ", zona inteira"
                } else if walked >= WALK_LIMIT {
                    " — parou no limite, a zona tem mais"
                } else {
                    " — a cadeia parou antes de fechar"
                }
            )),
        );
        names.extend(best);
    }
    walked
}

/// One walk against one server. Returns the names read and whether the chain closed the
/// loop back to the apex, which is what proves the whole zone was seen.
fn walk_from(plan: &Plan, rec: &Recorder, source: SocketAddr) -> (HashSet<String>, bool) {
    let mut found = HashSet::new();
    let mut current = plan.domain.clone();
    let suffix = format!(".{}", plan.domain);

    for _ in 0..WALK_LIMIT {
        if rec.stopping() {
            break;
        }
        let Ok(response) = wire::query_signed(source, &current, wire::NSEC, plan.timeout) else {
            break;
        };
        if response.rcode != 0 {
            break;
        }
        let next = response
            .answers
            .iter()
            .chain(response.authority.iter())
            .filter(|record| record.rtype == wire::NSEC)
            .find_map(|record| match &record.data {
                Rdata::Name(next) => Some(next.trim_end_matches('.').to_ascii_lowercase()),
                _ => None,
            });
        let Some(next) = next else {
            break;
        };
        // Back at the apex: the chain wrapped, and the zone is fully read.
        if next == plan.domain {
            return (found, true);
        }
        // Off the end of the zone.
        if !next.ends_with(&suffix) || next == current {
            break;
        }
        // A server can answer "this name doesn't exist" with an NSEC covering only the
        // name asked about — the next name it gives is a synthetic one just below the
        // query, spelled with a NUL label. That denies existence truthfully and says
        // nothing about the zone, so following it would walk forever inventing names.
        if next
            .split('.')
            .next()
            .is_some_and(|label| label.chars().any(char::is_control))
        {
            break;
        }
        if !found.insert(next.clone()) {
            break;
        }
        current = next;
    }
    (found, false)
}

fn whois(plan: &Plan, rec: &Recorder, tally: &mut Tally) {
    section(rec, "WHOIS");
    let Some(tld) = plan.domain.rsplit('.').next() else {
        return;
    };

    // IANA knows which registry runs each TLD, which saves hardcoding a table that goes
    // stale every time a new TLD is delegated.
    let registry = match wire::whois("whois.iana.org", tld, plan.timeout) {
        Ok(text) => field(&text, "whois"),
        Err(e) => {
            rec.record(0, EventKind::Error(format!("   IANA: {e}")));
            None
        }
    };
    let Some(registry) = registry else {
        rec.record(
            0,
            EventKind::Note(format!("   IANA não indicou servidor WHOIS para .{tld}")),
        );
        return;
    };
    rec.record(
        0,
        EventKind::Note(format!("   registro de .{tld}: {registry}")),
    );

    let text = match wire::whois(&registry, &plan.domain, plan.timeout) {
        Ok(text) => text,
        Err(e) => {
            rec.record(0, EventKind::Error(format!("   {registry}: {e}")));
            return;
        }
    };
    // The registry often only knows who the registrar is; the registrar has the detail.
    let referral = field(&text, "registrar whois server");
    let full = match &referral {
        Some(server) if !server.eq_ignore_ascii_case(&registry) => {
            rec.record(0, EventKind::Note(format!("   registrador: {server}")));
            wire::whois(server, &plan.domain, plan.timeout).unwrap_or(text)
        }
        _ => text,
    };

    // Everything the registry said, rather than the subset a hardcoded field list
    // happens to know the name of — registries spell their keys differently and a
    // filter tuned to .com quietly empties the answer for .br or .de.
    let mut shown = 0;
    for line in full.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('%') || trimmed.starts_with('#') {
            continue;
        }
        let lowered = trimmed.to_ascii_lowercase();
        if WHOIS_BOILERPLATE
            .iter()
            .any(|marker| lowered.starts_with(marker))
        {
            break;
        }
        if let Some((key, value)) = trimmed.split_once(':') {
            let key = key.trim().to_ascii_lowercase();
            // The one field the row's summary wants, whatever the registry calls it.
            if tally.expiry.is_none() && (key.contains("expir") || key.contains("paid-till")) {
                tally.expiry = Some(
                    value
                        .trim()
                        .split(['T', ' '])
                        .next()
                        .unwrap_or("")
                        .to_string(),
                );
            }
            rec.record(0, EventKind::Note(format!("   {}: {}", key, value.trim())));
        } else {
            rec.record(0, EventKind::Note(format!("   {trimmed}")));
        }
        shown += 1;
        if shown >= WHOIS_LINES {
            rec.record(
                0,
                EventKind::Note("   … resposta truncada aqui".to_string()),
            );
            break;
        }
    }
    if shown == 0 {
        rec.record(0, EventKind::Note("   WHOIS respondeu vazio".to_string()));
    }
}

/// The first value of a `key: value` line, case-insensitively.
fn field(text: &str, key: &str) -> Option<String> {
    text.lines()
        .filter_map(|line| line.trim().split_once(':'))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case(key))
        .map(|(_, value)| value.trim().to_string())
        .filter(|value| !value.is_empty())
}
