//! The machine's network interfaces: what they are, what they're called, and what's
//! moving through them.
//!
//! Every other network panel here answers a question *about* traffic — which ports are
//! open, which connections exist, where a login came from — and all of them were silent
//! about the hardware underneath. On a laptop with Wi-Fi, a wired port, a VPN and three
//! container bridges, "the network" is seven different things, and which one a number
//! belongs to is usually the first thing worth knowing.
//!
//! Read from `/sys/class/net`, one directory per interface, exactly as `ip` and
//! `ifconfig` do — every interface the kernel knows about is there, up or down,
//! addressed or not. The addresses themselves come through `sysinfo`, which asks
//! `getifaddrs` for them: it's the one part `/proc` and `/sys` can't answer, since the
//! kernel publishes addresses over netlink rather than as files.

use std::collections::HashMap;
use std::fs;
use std::time::Instant;

use sysinfo::Networks;

use super::{Detail, DetailSection, Rates, SystemState, TableMonitor, TableRow, mark};
use crate::format;

// No column for the kind: the name carries it in practice (`wlp3s0`, `docker0`,
// `br-...`), and an address column that gets squeezed out is a worse trade.
const HEADERS: [&str; 4] = ["Iface", "State", "Address", "Rate"];

const SYSFS_NET: &str = "/sys/class/net";

/// One interface, as complete a picture as the kernel gives without privileges.
pub(crate) struct Interface {
    pub name: String,
    pub kind: &'static str,
    /// `up`, `down`, `unknown` — the kernel's own word. A tunnel or loopback interface
    /// stays "unknown" for its whole life, which is why `carrier` is kept beside it.
    pub operstate: String,
    /// Whether the link is physically there: a cable in the socket, an association with
    /// an access point. `None` for interfaces where the question doesn't apply.
    pub carrier: Option<bool>,
    pub mac: Option<String>,
    pub mtu: Option<u64>,
    /// Negotiated speed in Mb/s. Only wired links report one — Wi-Fi rates change per
    /// frame and the file returns an error rather than a number.
    pub speed: Option<u64>,
    pub duplex: Option<String>,
    /// Kernel driver behind it, e.g. `ath10k_pci`, `r8169`, `wireguard`.
    pub driver: Option<String>,
    /// Addresses with their prefix length, IPv4 first.
    pub addresses: Vec<String>,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_packets: u64,
    pub tx_packets: u64,
    pub rx_errors: u64,
    pub tx_errors: u64,
    pub rx_dropped: u64,
    pub tx_dropped: u64,
}

impl Interface {
    /// Whether traffic can actually cross it right now. `unknown` counts as up: it's
    /// what every virtual interface reports, and a WireGuard tunnel carrying traffic is
    /// not "down" in any sense the reader means.
    pub fn is_up(&self) -> bool {
        self.operstate == "up" || self.operstate == "unknown"
    }

    /// Addresses on one line, or a word saying why there are none.
    pub fn address_summary(&self) -> String {
        if self.addresses.is_empty() {
            "sem endereço".to_string()
        } else {
            self.addresses.join(" · ")
        }
    }

    /// The first IPv4 address, which is what people mean by "the machine's IP".
    pub fn ipv4(&self) -> Option<&str> {
        self.addresses
            .iter()
            .find(|address| !address.contains(':'))
            .map(|address| address.split('/').next().unwrap_or(address))
    }
}

fn attribute(name: &str, file: &str) -> Option<String> {
    let content = fs::read_to_string(format!("{SYSFS_NET}/{name}/{file}")).ok()?;
    let trimmed = content.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn number(name: &str, file: &str) -> Option<u64> {
    attribute(name, file)?.parse().ok()
}

fn counter(name: &str, file: &str) -> u64 {
    number(name, &format!("statistics/{file}")).unwrap_or(0)
}

/// One value out of the interface's `uevent`, which is where the kernel writes what
/// kind of device it made — `wlan`, `bridge`, `vlan`, `wireguard`, and so on.
fn uevent(name: &str, key: &str) -> Option<String> {
    let content = fs::read_to_string(format!("{SYSFS_NET}/{name}/uevent")).ok()?;
    content
        .lines()
        .find_map(|line| Some(line.strip_prefix(&format!("{key}="))?.to_string()))
}

/// What sort of interface this is, in the words someone would use for it.
///
/// `DEVTYPE` covers most of it; `type` (the ARPHRD number) catches loopback and the
/// tunnels that don't declare a devtype; and the presence of a `wireless` directory is
/// the last word on Wi-Fi, since some drivers set neither.
fn kind_of(name: &str) -> &'static str {
    if fs::metadata(format!("{SYSFS_NET}/{name}/wireless")).is_ok() {
        return "Wi-Fi";
    }
    match uevent(name, "DEVTYPE").as_deref() {
        Some("wlan") => return "Wi-Fi",
        Some("bridge") => return "ponte",
        Some("vlan") => return "VLAN",
        Some("bond") => return "agregado",
        Some("wireguard") => return "VPN WireGuard",
        Some("veth") => return "par virtual",
        _ => {}
    }
    match number(name, "type") {
        // ARPHRD_LOOPBACK / ARPHRD_NONE / ARPHRD_PPP / ARPHRD_SIT / ARPHRD_TUNNEL.
        Some(772) => "loopback",
        Some(65534) => "túnel",
        Some(512) => "PPP",
        Some(776) | Some(768) => "túnel IP",
        Some(1) if fs::metadata(format!("{SYSFS_NET}/{name}/bridge")).is_ok() => "ponte",
        Some(1) => "Ethernet",
        _ => "outro",
    }
}

/// Wi-Fi link quality and signal, from `/proc/net/wireless` — the same two numbers
/// `iwconfig` prints, and the only ones available without talking nl80211.
fn wireless_signal(name: &str) -> Option<(String, String)> {
    let content = fs::read_to_string("/proc/net/wireless").ok()?;
    content.lines().find_map(|line| {
        let (interface, rest) = line.split_once(':')?;
        if interface.trim() != name {
            return None;
        }
        let fields: Vec<&str> = rest.split_whitespace().collect();
        Some((
            fields.get(1)?.trim_end_matches('.').to_string(),
            fields.get(2)?.trim_end_matches('.').to_string(),
        ))
    })
}

/// Addresses per interface, from whatever `getifaddrs` reported into `networks`.
/// IPv4 before IPv6, since that's the one people are looking for.
fn addresses_of(networks: &Networks, name: &str) -> Vec<String> {
    let Some(data) = networks.list().get(name) else {
        return Vec::new();
    };
    let mut addresses: Vec<String> = data
        .ip_networks()
        .iter()
        .map(|network| network.to_string())
        .collect();
    addresses.sort_by_key(|address| address.contains(':'));
    addresses
}

/// Every interface the kernel knows about, in a stable order: the ones carrying real
/// traffic first, loopback and dead interfaces last.
pub(super) fn interfaces(networks: &Networks) -> Vec<Interface> {
    let Ok(dir) = fs::read_dir(SYSFS_NET) else {
        return Vec::new();
    };
    let mut list: Vec<Interface> = dir
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            Some(Interface {
                kind: kind_of(&name),
                operstate: attribute(&name, "operstate").unwrap_or_else(|| "?".to_string()),
                carrier: attribute(&name, "carrier").map(|value| value == "1"),
                mac: attribute(&name, "address").filter(|mac| mac != "00:00:00:00:00:00"),
                mtu: number(&name, "mtu"),
                // Both files return EINVAL rather than a value on a link that has no
                // fixed speed (Wi-Fi) or no link at all (an unplugged cable).
                speed: number(&name, "speed"),
                duplex: attribute(&name, "duplex"),
                driver: fs::read_link(format!("{SYSFS_NET}/{name}/device/driver"))
                    .ok()
                    .and_then(|path| Some(path.file_name()?.to_string_lossy().into_owned())),
                addresses: addresses_of(networks, &name),
                rx_bytes: counter(&name, "rx_bytes"),
                tx_bytes: counter(&name, "tx_bytes"),
                rx_packets: counter(&name, "rx_packets"),
                tx_packets: counter(&name, "tx_packets"),
                rx_errors: counter(&name, "rx_errors"),
                tx_errors: counter(&name, "tx_errors"),
                rx_dropped: counter(&name, "rx_dropped"),
                tx_dropped: counter(&name, "tx_dropped"),
                name,
            })
        })
        .collect();
    list.sort_by(|a, b| {
        let rank = |i: &Interface| {
            (
                i.name == "lo",
                !i.is_up(),
                i.addresses.is_empty(),
                u64::MAX - (i.rx_bytes + i.tx_bytes),
            )
        };
        rank(a).cmp(&rank(b))
    });
    list
}

/// The interface an address belongs to. Every panel that shows a local address can say
/// which card it's on, and none of them should have to know how that's found out.
pub(super) fn interface_of(networks: &Networks, ip: &str) -> Option<String> {
    // A wildcard bind isn't on one interface, it's on all of them at once — saying
    // "wlp3s0" there would be a smaller truth than saying nothing.
    if ip.is_empty() || ip == "0.0.0.0" || ip == "::" {
        return None;
    }
    networks.list().iter().find_map(|(name, data)| {
        data.ip_networks()
            .iter()
            .any(|network| network.addr.to_string() == ip)
            .then(|| name.clone())
    })
}

/// Interfaces with an address, named and summarised for a one-line answer to "what is
/// this machine on the network". Used by the System Info panel, which has one row to
/// say it in.
pub(super) fn addressed_summary(networks: &Networks) -> String {
    let list = interfaces(networks);
    let addressed: Vec<&Interface> = list
        .iter()
        .filter(|interface| interface.name != "lo" && !interface.addresses.is_empty())
        .collect();
    if addressed.is_empty() {
        return format!("{} interfaces, nenhuma com endereço", list.len());
    }
    let named: Vec<String> = addressed
        .iter()
        .take(SUMMARY_INTERFACES)
        .map(|interface| match interface.ipv4() {
            Some(ip) => format!("{} {ip}", interface.name),
            None => interface.name.clone(),
        })
        .collect();
    let mut summary = named.join("  ·  ");
    if addressed.len() > SUMMARY_INTERFACES {
        summary.push_str(&format!("  ·  +{}", addressed.len() - SUMMARY_INTERFACES));
    }
    summary.push_str(&format!(
        "   ({} de {} no total)",
        addressed.len(),
        list.len()
    ));
    summary
}

/// Interfaces named on the System Info row before the rest becomes a count.
const SUMMARY_INTERFACES: usize = 3;

/// Routes the kernel sends through one interface — how to tell a bridge nothing uses
/// from the one carrying the default route.
fn routes_of(name: &str) -> Vec<String> {
    let Ok(content) = fs::read_to_string("/proc/net/route") else {
        return Vec::new();
    };
    content
        .lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if *fields.first()? != name {
                return None;
            }
            let destination = super::summary::hex_ipv4(fields.get(1)?)?;
            let mask = super::summary::hex_ipv4(fields.get(7)?)?;
            let gateway = super::summary::hex_ipv4(fields.get(2)?)?;
            let prefix = u32::from(mask).count_ones();
            let target = if destination.is_unspecified() && prefix == 0 {
                "padrão".to_string()
            } else {
                format!("{destination}/{prefix}")
            };
            Some(if gateway.is_unspecified() {
                format!("{target} (direto)")
            } else {
                format!("{target} via {gateway}")
            })
        })
        .collect()
}

pub struct InterfacesMonitor {
    /// Last byte counters per interface, with when they were read — the kernel only
    /// publishes totals, and a total is not what anyone wants to look at.
    last: HashMap<String, (u64, u64, Instant)>,
    rates: HashMap<String, (f64, f64)>,
}

impl InterfacesMonitor {
    pub fn new() -> Self {
        Self {
            last: HashMap::new(),
            rates: HashMap::new(),
        }
    }

    /// Updates the per-interface rates from a fresh reading, and returns them.
    fn measure(&mut self, list: &[Interface]) -> HashMap<String, (f64, f64)> {
        let now = Instant::now();
        for interface in list {
            if let Some((rx, tx, at)) = self.last.get(&interface.name) {
                let seconds = now.duration_since(*at).as_secs_f64();
                if seconds > 0.0 {
                    self.rates.insert(
                        interface.name.clone(),
                        (
                            interface.rx_bytes.saturating_sub(*rx) as f64 / seconds,
                            interface.tx_bytes.saturating_sub(*tx) as f64 / seconds,
                        ),
                    );
                }
            }
            self.last.insert(
                interface.name.clone(),
                (interface.rx_bytes, interface.tx_bytes, now),
            );
        }
        self.rates.clone()
    }

    fn rate_cells(&self, name: &str) -> String {
        match self.rates.get(name) {
            Some((rx, tx)) => format!(
                "↓{} ↑{}",
                format::human_bytes_per_sec(*rx),
                format::human_bytes_per_sec(*tx)
            ),
            // Nothing to say until a second reading gives us an interval to divide by.
            None => "-".to_string(),
        }
    }

    fn row_of(&self, interface: &Interface) -> TableRow {
        let mut row = TableRow::leaf(
            vec![
                interface.name.clone(),
                state_word(interface),
                interface.address_summary(),
                self.rate_cells(&interface.name),
            ],
            0,
        );
        row.key = interface.name.clone();
        row
    }
}

/// State in one word, folding the carrier in: an Ethernet port that's down because
/// nothing is plugged into it is a different situation from one that's been turned off.
fn state_word(interface: &Interface) -> String {
    match (interface.operstate.as_str(), interface.carrier) {
        // "unknown" is what every virtual interface reports for its whole life, so it
        // means active here; the detail keeps the kernel's own word for whoever wants it.
        ("up", _) | ("unknown", _) => "ativa".to_string(),
        ("down", Some(false)) => "sem link".to_string(),
        ("down", _) => "desligada".to_string(),
        (other, _) => other.to_string(),
    }
}

impl Default for InterfacesMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl TableMonitor for InterfacesMonitor {
    fn id(&self) -> &'static str {
        "interfaces"
    }

    fn title(&self) -> &'static str {
        "Interfaces"
    }

    fn headers(&self) -> &'static [&'static str] {
        &HEADERS
    }

    fn mark_kinds(&self) -> &'static [mark::MarkKind] {
        &[mark::MarkKind {
            name: "interface",
            column: 0,
            numeric: false,
            help: "nome da interface, ou uma expressão regular",
        }]
    }

    fn sample(&mut self, state: &SystemState, limit: Option<usize>) -> Vec<TableRow> {
        let list = interfaces(&state.networks);
        self.measure(&list);
        list.iter()
            .take(limit.unwrap_or(usize::MAX))
            .map(|interface| self.row_of(interface))
            .collect()
    }

    fn refresh_values(&mut self, state: &SystemState, rows: &mut [TableRow]) {
        let list = interfaces(&state.networks);
        self.measure(&list);
        for row in rows.iter_mut() {
            if let Some(interface) = list.iter().find(|i| i.name == row.key) {
                *row = self.row_of(interface);
            }
        }
    }

    fn detail(&mut self, state: &SystemState, row: &TableRow) -> Option<Detail> {
        let list = interfaces(&state.networks);
        self.measure(&list);
        let interface = list.iter().find(|i| i.name == row.key)?;

        let mut identity = DetailSection::new("Interface");
        identity.push("Nome", interface.name.clone());
        identity.push("Tipo", interface.kind);
        identity.push(
            "Estado",
            match interface.operstate.as_str() {
                "unknown" => {
                    "ativa  (operstate «unknown», o normal numa interface virtual)".to_string()
                }
                _ => format!(
                    "{}  (operstate «{}»)",
                    state_word(interface),
                    interface.operstate
                ),
            },
        );
        if let Some(carrier) = interface.carrier {
            identity.push(
                "Link",
                if carrier {
                    "presente"
                } else {
                    "ausente — cabo desconectado ou sem associação"
                },
            );
        }
        if let Some(mac) = &interface.mac {
            identity.push("MAC", mac.clone());
        }
        if let Some(mtu) = interface.mtu {
            identity.push("MTU", format!("{mtu} bytes"));
        }
        if let Some(speed) = interface.speed {
            identity.push("Velocidade negociada", format!("{speed} Mb/s"));
        }
        if let Some(duplex) = &interface.duplex {
            identity.push("Duplex", duplex.clone());
        }
        if let Some(driver) = &interface.driver {
            identity.push("Driver", driver.clone());
        }
        if let Some((quality, level)) = wireless_signal(&interface.name) {
            identity.push("Sinal", format!("qualidade {quality} · {level} dBm"));
        }

        let mut addressing = DetailSection::new("Endereços");
        if interface.addresses.is_empty() {
            addressing.push("Endereços", "nenhum configurado");
        }
        for address in &interface.addresses {
            addressing.push(
                if address.contains(':') {
                    "IPv6"
                } else {
                    "IPv4"
                },
                address.clone(),
            );
        }
        let routes = routes_of(&interface.name);
        if routes.is_empty() {
            addressing.push("Rotas", "nenhuma passa por aqui");
        }
        for route in routes {
            addressing.push("Rota", route);
        }

        let mut traffic = DetailSection::new("Tráfego");
        let (rx_rate, tx_rate) = self
            .rates
            .get(&interface.name)
            .copied()
            .unwrap_or((0.0, 0.0));
        traffic.push(
            "Taxa atual",
            format!(
                "↓ {} · ↑ {}",
                format::human_bytes_per_sec(rx_rate),
                format::human_bytes_per_sec(tx_rate)
            ),
        );
        traffic.push("Recebido", format::human_bytes(interface.rx_bytes as f64));
        traffic.push("Enviado", format::human_bytes(interface.tx_bytes as f64));
        traffic.push(
            "Pacotes",
            format!(
                "{} recebidos · {} enviados",
                interface.rx_packets, interface.tx_packets
            ),
        );
        traffic.push(
            "Erros",
            format!(
                "{} na recepção · {} no envio",
                interface.rx_errors, interface.tx_errors
            ),
        );
        traffic.push(
            "Descartados",
            format!(
                "{} na recepção · {} no envio",
                interface.rx_dropped, interface.tx_dropped
            ),
        );

        // A sweep of the network this interface is on: the addresses are right here, and
        // typing a CIDR back in by hand while looking at it is work the app can spare.
        // What a network is worth doing is decided where it is for every other tool.
        let handoffs = interface
            .addresses
            .iter()
            .filter(|address| !address.contains(':'))
            .flat_map(|address| crate::tools::offers_for("rede", address))
            .collect();

        Some(Detail {
            title: format!("{} · {}", interface.name, interface.kind),
            gone_note: "removida",
            sections: vec![identity, addressing, traffic],
            rates: Some(Rates {
                labels: ("↓ Recebendo", "↑ Enviando"),
                values: (rx_rate, tx_rate),
            }),
            handoffs,
            handoff_title: "Varrer a rede desta interface",
        })
    }

    fn has_detail(&self) -> bool {
        true
    }
}
