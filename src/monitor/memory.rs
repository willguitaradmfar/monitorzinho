use super::{Monitor, SystemState};
use crate::format;

pub struct MemoryMonitor;

impl Monitor for MemoryMonitor {
    fn id(&self) -> &'static str {
        "memory"
    }

    fn title(&self) -> &'static str {
        "Memory"
    }

    fn sample(&mut self, state: &SystemState) -> f64 {
        let total = state.sys.total_memory();
        if total == 0 {
            return 0.0;
        }
        (state.sys.used_memory() as f64 / total as f64) * 100.0
    }

    fn format(&self, value: f64) -> String {
        format!("{:.1}%", value)
    }

    fn limit(&self) -> Option<f64> {
        Some(100.0)
    }

    fn group(&self) -> &'static str {
        "System"
    }

    fn extra(&self, state: &SystemState) -> Option<String> {
        Some(format!(
            "{} / {}",
            format::human_bytes(state.sys.used_memory() as f64),
            format::human_bytes(state.sys.total_memory() as f64)
        ))
    }

    fn capacity(&self, state: &SystemState) -> Option<f64> {
        Some(state.sys.total_memory() as f64)
    }
}
