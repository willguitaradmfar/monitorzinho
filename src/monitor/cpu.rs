use super::{Monitor, SystemState};

pub struct CpuMonitor;

impl Monitor for CpuMonitor {
    fn id(&self) -> &str {
        "cpu"
    }

    fn title(&self) -> &str {
        "CPU"
    }

    fn sample(&mut self, state: &SystemState) -> f64 {
        state.sys.global_cpu_usage() as f64
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
        Some(format!("{} cores", state.sys.cpus().len()))
    }
}
