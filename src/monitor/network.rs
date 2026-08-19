use super::{Monitor, SystemState};
use crate::format;

pub struct NetRxMonitor;

impl Monitor for NetRxMonitor {
    fn id(&self) -> &'static str {
        "net_rx"
    }

    fn title(&self) -> &'static str {
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
}

pub struct NetTxMonitor;

impl Monitor for NetTxMonitor {
    fn id(&self) -> &'static str {
        "net_tx"
    }

    fn title(&self) -> &'static str {
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
}
