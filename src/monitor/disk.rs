use std::path::Path;

use sysinfo::Disk;

use super::{Monitor, SystemState};
use crate::format;

fn primary_disk(state: &SystemState) -> Option<&Disk> {
    state
        .disks
        .list()
        .iter()
        .find(|d| d.mount_point() == Path::new("/"))
        .or_else(|| state.disks.list().first())
}

pub struct DiskMonitor;

impl Monitor for DiskMonitor {
    fn id(&self) -> &'static str {
        "disk"
    }

    fn title(&self) -> &'static str {
        "Disk (/)"
    }

    fn sample(&mut self, state: &SystemState) -> f64 {
        match primary_disk(state) {
            Some(d) => {
                let total = d.total_space();
                if total == 0 {
                    return 0.0;
                }
                let used = total.saturating_sub(d.available_space());
                (used as f64 / total as f64) * 100.0
            }
            None => 0.0,
        }
    }

    fn format(&self, value: f64) -> String {
        format!("{:.1}%", value)
    }

    fn limit(&self) -> Option<f64> {
        Some(100.0)
    }

    fn group(&self) -> &'static str {
        "Disk"
    }

    fn extra(&self, state: &SystemState) -> Option<String> {
        let d = primary_disk(state)?;
        let total = d.total_space();
        let used = total.saturating_sub(d.available_space());
        Some(format!(
            "{} / {}",
            format::human_bytes(used as f64),
            format::human_bytes(total as f64)
        ))
    }

    fn numeric_only(&self) -> bool {
        true
    }
}

pub struct DiskReadMonitor;

impl Monitor for DiskReadMonitor {
    fn id(&self) -> &'static str {
        "disk_read"
    }

    fn title(&self) -> &'static str {
        "Disk read"
    }

    fn sample(&mut self, state: &SystemState) -> f64 {
        primary_disk(state)
            .map(|d| d.usage().read_bytes as f64)
            .unwrap_or(0.0)
    }

    fn format(&self, value: f64) -> String {
        format::human_bytes_per_sec(value)
    }

    fn group(&self) -> &'static str {
        "Disk"
    }
}

pub struct DiskWriteMonitor;

impl Monitor for DiskWriteMonitor {
    fn id(&self) -> &'static str {
        "disk_write"
    }

    fn title(&self) -> &'static str {
        "Disk write"
    }

    fn sample(&mut self, state: &SystemState) -> f64 {
        primary_disk(state)
            .map(|d| d.usage().written_bytes as f64)
            .unwrap_or(0.0)
    }

    fn format(&self, value: f64) -> String {
        format::human_bytes_per_sec(value)
    }

    fn group(&self) -> &'static str {
        "Disk"
    }
}
