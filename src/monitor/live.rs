//! A chart fed by a tool instead of by the machine.
//!
//! Every other monitor answers `sample()` by reading the system right then. A tool
//! can't work that way: a ping happens on its own thread, on its own schedule, and the
//! Overview tab only comes round when the user is looking at it. So the tool publishes
//! each measurement into a `LiveSeries` and the panel reads whatever the latest one is
//! — the chart is a sampling of the tool's stream, not the stream itself, which is why
//! the execution's own log keeps every measurement.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::{Monitor, SystemState};

/// The latest value a tool has published, shared with the thread producing it.
///
/// `f64` bits in an atomic: a sample is one number written by one thread and read by
/// another, which is the whole contract — no lock, and no chance of the UI blocking on
/// a tool's thread (or the other way round) for the sake of a chart.
#[derive(Default)]
pub struct LiveSeries {
    bits: AtomicU64,
}

impl LiveSeries {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn publish(&self, value: f64) {
        self.bits.store(value.to_bits(), Ordering::Relaxed);
    }

    pub fn latest(&self) -> f64 {
        f64::from_bits(self.bits.load(Ordering::Relaxed))
    }
}

/// A chart panel over a `LiveSeries`, named after whatever the tool was pointed at.
pub struct LiveMonitor {
    key: String,
    title: String,
    group: &'static str,
    /// How a sample reads on the panel. A plain function rather than a unit enum: what
    /// varies between tools is only the formatting, and every tool already knows how to
    /// write its own numbers.
    format: fn(f64) -> String,
    series: Arc<LiveSeries>,
}

impl LiveMonitor {
    pub fn new(
        key: impl Into<String>,
        title: impl Into<String>,
        group: &'static str,
        format: fn(f64) -> String,
        series: Arc<LiveSeries>,
    ) -> Self {
        Self {
            key: key.into(),
            title: title.into(),
            group,
            format,
            series,
        }
    }
}

impl Monitor for LiveMonitor {
    fn id(&self) -> &str {
        &self.key
    }

    fn title(&self) -> &str {
        &self.title
    }

    fn sample(&mut self, _state: &SystemState) -> f64 {
        self.series.latest()
    }

    fn format(&self, value: f64) -> String {
        (self.format)(value)
    }

    fn group(&self) -> &'static str {
        self.group
    }
}
