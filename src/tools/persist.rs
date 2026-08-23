//! Remembering which executions were running, so a restart picks them back up.
//!
//! Only the *configuration* is stored — which tool, and what it was given. Logs and
//! counters are deliberately not: they describe one run, and restoring them alongside a
//! freshly started thread would present stale traffic as if it were live.

use std::collections::BTreeMap;
use std::fs;

use serde::{Deserialize, Serialize};

use crate::history;

/// Everything needed to recreate one execution. `BTreeMap` so the file's key order is
/// stable between writes instead of shuffling with the hash seed on every save.
#[derive(Clone, Serialize, Deserialize)]
pub struct ExecutionSpec {
    /// `Tool::id`, not its display name — the name is free to change, this isn't.
    pub tool: String,
    pub params: BTreeMap<String, String>,
    /// Whether it should be running. Switched off stays switched off across restarts —
    /// the row is kept on purpose, and coming back up doing the very thing somebody
    /// turned off would be the opposite of what they asked for.
    ///
    /// Defaulted, so a file written by an older version reads as "on", which is what
    /// every execution in it was.
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
}

fn enabled_by_default() -> bool {
    true
}

fn path() -> std::path::PathBuf {
    history::data_file("tools.json")
}

/// The saved executions, or nothing at all if the file is missing or unreadable — a
/// corrupt config costs the saved list, never the app starting.
pub fn load() -> Vec<ExecutionSpec> {
    match fs::read_to_string(path()) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

pub fn save(specs: &[ExecutionSpec]) {
    if let Ok(content) = serde_json::to_string_pretty(specs) {
        let _ = fs::write(path(), content);
    }
}
