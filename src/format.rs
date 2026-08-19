const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];

fn scale(mut value: f64, suffix: &str) -> String {
    let mut unit_idx = 0;
    while value.abs() >= 1024.0 && unit_idx < UNITS.len() - 1 {
        value /= 1024.0;
        unit_idx += 1;
    }
    format!("{:.1} {}{}", value, UNITS[unit_idx], suffix)
}

/// Formats a byte count with an auto-scaled unit (B/KB/MB/GB/TB), e.g. "482.0 MB".
pub fn human_bytes(bytes: f64) -> String {
    scale(bytes, "")
}

/// Formats a byte rate with an auto-scaled unit, e.g. "1.3 MB/s".
pub fn human_bytes_per_sec(bytes_per_sec: f64) -> String {
    scale(bytes_per_sec, "/s")
}

/// Formats a duration in seconds as a compact human-readable string, e.g. "2h15m".
pub fn human_duration(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let mins = (seconds % 3_600) / 60;
    let secs = seconds % 60;

    if days > 0 {
        format!("{days}d{hours}h")
    } else if hours > 0 {
        format!("{hours}h{mins}m")
    } else if mins > 0 {
        format!("{mins}m{secs}s")
    } else {
        format!("{secs}s")
    }
}
