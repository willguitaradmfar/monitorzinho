use nvml_wrapper::Nvml;

use super::{Monitor, SystemState};
use crate::format;

pub struct GpuMonitor {
    nvml: Nvml,
}

impl GpuMonitor {
    /// `Nvml::init()` dynamically loads the NVIDIA driver library and fails cleanly
    /// (no panic) if it isn't present — so this returns `None` on any machine without
    /// an NVIDIA GPU, and the caller simply skips registering the monitor.
    pub fn probe() -> Option<Self> {
        Nvml::init().ok().map(|nvml| Self { nvml })
    }
}

impl Monitor for GpuMonitor {
    fn id(&self) -> &'static str {
        "gpu"
    }

    fn title(&self) -> &'static str {
        "GPU"
    }

    fn sample(&mut self, _state: &SystemState) -> f64 {
        self.nvml
            .device_by_index(0)
            .and_then(|d| d.utilization_rates())
            .map(|u| u.gpu as f64)
            .unwrap_or(0.0)
    }

    fn format(&self, value: f64) -> String {
        format!("{:.1}%", value)
    }

    fn limit(&self) -> Option<f64> {
        Some(100.0)
    }

    fn group(&self) -> &'static str {
        "GPU"
    }

    fn extra(&self, _state: &SystemState) -> Option<String> {
        let device = self.nvml.device_by_index(0).ok()?;
        let mem = device.memory_info().ok()?;
        Some(format!(
            "{} / {}",
            format::human_bytes(mem.used as f64),
            format::human_bytes(mem.total as f64)
        ))
    }
}
