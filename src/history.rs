use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;

// Sized to still fill a wide fullscreened panel (each sample is one column) rather than
// just the overview grid's narrower one-of-three-columns panels.
pub const CAPACITY: usize = 300;

pub type HistoryMap = HashMap<String, Vec<f64>>;

#[derive(Debug, Clone)]
pub struct History {
    buf: VecDeque<f64>,
    capacity: usize,
}

impl History {
    pub fn new(capacity: usize) -> Self {
        Self {
            buf: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    pub fn from_saved(values: Vec<f64>, capacity: usize) -> Self {
        let mut buf: VecDeque<f64> = values.into_iter().collect();
        while buf.len() > capacity {
            buf.pop_front();
        }
        Self { buf, capacity }
    }

    pub fn push(&mut self, value: f64) {
        if self.buf.len() == self.capacity {
            self.buf.pop_front();
        }
        self.buf.push_back(value);
    }

    pub fn last(&self) -> Option<f64> {
        self.buf.back().copied()
    }

    /// Peak value within the currently retained window (not all-time — old samples
    /// fall off as the ring buffer fills).
    pub fn max(&self) -> Option<f64> {
        self.buf.iter().copied().fold(None, |acc, v| match acc {
            Some(m) => Some(v.max(m)),
            None => Some(v),
        })
    }

    pub fn values(&self) -> Vec<f64> {
        self.buf.iter().copied().collect()
    }
}

/// Path of one of monitorzinho's state files, creating the directory if needed. Shared
/// with `tools::persist` — this module happens to own the "where we keep things on
/// disk" logic, and a second copy of it would be one more place to get wrong.
pub fn data_file(name: &str) -> PathBuf {
    let mut dir = dirs::data_dir().unwrap_or_else(std::env::temp_dir);
    dir.push("monitorzinho");
    let _ = std::fs::create_dir_all(&dir);
    dir.push(name);
    dir
}

fn data_file_path() -> PathBuf {
    data_file("history.json")
}

pub fn load_all() -> HistoryMap {
    let path = data_file_path();
    match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => HistoryMap::new(),
    }
}

pub fn save_all(map: &HistoryMap) {
    let path = data_file_path();
    if let Ok(content) = serde_json::to_string_pretty(map) {
        let _ = std::fs::write(path, content);
    }
}
