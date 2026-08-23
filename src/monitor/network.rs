use sysinfo::NetworkData;

use super::{Monitor, SystemState};
use crate::format;

/// Which interface carried most of this sample, and how much of it.
///
/// The chart sums every interface, which answers "how much is moving" and says nothing
/// about "over what" — and on a machine with Wi-Fi, a VPN and three bridges, the second
/// question is usually the one being asked. Names the interface next to the figure, the
/// same place a percentage-of-capacity monitor puts its absolute quantity.
fn busiest(state: &SystemState, bytes: fn(&NetworkData) -> u64) -> Option<String> {
    let total: u64 = state.networks.list().values().map(bytes).sum();
    if total == 0 {
        return None;
    }
    let (name, moved) = state
        .networks
        .list()
        .iter()
        .map(|(name, data)| (name, bytes(data)))
        .max_by_key(|(_, moved)| *moved)?;
    Some(format!(
        "{name} {:.0}%",
        moved as f64 / total as f64 * 100.0
    ))
}

pub struct NetRxMonitor;

impl Monitor for NetRxMonitor {
    fn id(&self) -> &str {
        "net_rx"
    }

    fn title(&self) -> &str {
        "Net down"
    }

    fn sample(&mut self, state: &SystemState) -> f64 {
        let bytes: u64 = state.networks.list().values().map(|n| n.received()).sum();
        bytes as f64
    }

    fn format(&self, value: f64) -> String {
        format::human_bytes_per_sec(value)
    }

    fn group(&self) -> &'static str {
        "Network"
    }

    fn extra(&self, state: &SystemState) -> Option<String> {
        busiest(state, NetworkData::received)
    }
}

pub struct NetTxMonitor;

impl Monitor for NetTxMonitor {
    fn id(&self) -> &str {
        "net_tx"
    }

    fn title(&self) -> &str {
        "Net up"
    }

    fn sample(&mut self, state: &SystemState) -> f64 {
        let bytes: u64 = state
            .networks
            .list()
            .values()
            .map(|n| n.transmitted())
            .sum();
        bytes as f64
    }

    fn format(&self, value: f64) -> String {
        format::human_bytes_per_sec(value)
    }

    fn group(&self) -> &'static str {
        "Network"
    }

    fn extra(&self, state: &SystemState) -> Option<String> {
        busiest(state, NetworkData::transmitted)
    }
}
