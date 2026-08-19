use sysinfo::{Pid, Signal};

use crate::history::{self, CAPACITY, History};
use crate::monitor::{self, Monitor, SystemState, TableMonitor, TableRow};

const SAVE_EVERY_N_TICKS: u32 = 5;

/// a-z minus 'q' (quit) and 'x' (kill, inside a fullscreened table) — those two stay
/// reserved everywhere so they always mean the same thing regardless of focus.
const SHORTCUT_LETTERS: &[char] = &[
    'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o', 'p', 'r', 's', 't',
    'u', 'v', 'w', 'y', 'z',
];
/// Shortcut keys are '1'..='9' then `SHORTCUT_LETTERS`, in that order — so only the
/// first `MAX_SHORTCUTS` panels (charts first, then tables) get one.
pub const MAX_SHORTCUTS: usize = 9 + SHORTCUT_LETTERS.len();

/// The key that activates the panel at `index` (0-indexed), in `shortcut_targets()`
/// order — mirrored by `ui::ShortcutMap` to label each panel with the same key.
pub fn shortcut_key(index: usize) -> Option<char> {
    if index < 9 {
        Some((b'1' + index as u8) as char)
    } else {
        SHORTCUT_LETTERS.get(index - 9).copied()
    }
}

/// Inverse of `shortcut_key`: which panel index a pressed key activates, if any.
fn shortcut_index(key: char) -> Option<usize> {
    if key.is_ascii_digit() && key != '0' {
        Some((key as u8 - b'1') as usize)
    } else {
        SHORTCUT_LETTERS
            .iter()
            .position(|&l| l == key)
            .map(|p| p + 9)
    }
}

/// Row cap for a table panel's compact, in-grid rendering. Fullscreening it takes a
/// fresh, uncapped sample instead — see `App::activate_shortcut`.
const OVERVIEW_TABLE_ROWS: usize = 10;

/// What a shortcut key points at, in the flattened order shown in the UI: chart
/// panels first (same order as the chart grid), then table panels.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ShortcutTarget {
    Chart(usize),
    Table(usize),
}

/// A table panel that's been fullscreened: its rows are frozen at the moment of
/// entry (re-sampling is skipped in `App::tick` while focused) so the list doesn't
/// shift under the user while they read it, navigate, or kill a process.
pub struct TableFocus {
    pub table_index: usize,
    pub rows: Vec<TableRow>,
    pub selected: usize,
}

pub enum Focus {
    None,
    Chart(usize),
    Table(TableFocus),
}

pub struct App {
    pub monitors: Vec<Box<dyn Monitor>>,
    pub histories: Vec<History>,
    pub extras: Vec<Option<String>>,
    pub capacities: Vec<Option<f64>>,
    pub table_monitors: Vec<Box<dyn TableMonitor>>,
    pub table_rows: Vec<Vec<TableRow>>,
    pub focus: Focus,
    state: SystemState,
    ticks_since_save: u32,
}

impl App {
    pub fn new() -> Self {
        let monitors = monitor::all_monitors();
        let saved = history::load_all();
        let histories = monitors
            .iter()
            .map(|m| match saved.get(m.id()) {
                Some(values) => History::from_saved(values.clone(), CAPACITY),
                None => History::new(CAPACITY),
            })
            .collect();
        let extras = monitors.iter().map(|_| None).collect();
        let capacities = monitors.iter().map(|_| None).collect();

        let table_monitors = monitor::all_table_monitors();
        let table_rows = table_monitors.iter().map(|_| Vec::new()).collect();

        Self {
            monitors,
            histories,
            extras,
            capacities,
            table_monitors,
            table_rows,
            focus: Focus::None,
            state: SystemState::new(),
            ticks_since_save: 0,
        }
    }

    pub fn tick(&mut self) {
        self.state.refresh();
        for (((monitor, history), extra), capacity) in self
            .monitors
            .iter_mut()
            .zip(self.histories.iter_mut())
            .zip(self.extras.iter_mut())
            .zip(self.capacities.iter_mut())
        {
            let value = monitor.sample(&self.state);
            history.push(value);
            *extra = monitor.extra(&self.state);
            *capacity = monitor.capacity(&self.state);
        }
        for (i, (monitor, rows)) in self
            .table_monitors
            .iter_mut()
            .zip(self.table_rows.iter_mut())
            .enumerate()
        {
            // The fullscreened table (if any) is frozen — leave its rows untouched so
            // the list doesn't shift while the user is reading/navigating/killing.
            let frozen = matches!(&self.focus, Focus::Table(tf) if tf.table_index == i);
            if !frozen {
                *rows = monitor.sample(&self.state, Some(OVERVIEW_TABLE_ROWS));
            }
        }

        self.ticks_since_save += 1;
        if self.ticks_since_save >= SAVE_EVERY_N_TICKS {
            self.persist();
            self.ticks_since_save = 0;
        }
    }

    /// Chart-worthy monitor indices ordered by `Monitor::group()` — the same order the
    /// UI lays out the chart grid in. Shared so shortcut numbering always matches what's
    /// on screen.
    pub fn chart_monitor_order(&self) -> Vec<usize> {
        let mut groups: Vec<(&'static str, Vec<usize>)> = Vec::new();
        for (i, m) in self.monitors.iter().enumerate() {
            if m.numeric_only() {
                continue;
            }
            let g = m.group();
            match groups.iter_mut().find(|(name, _)| *name == g) {
                Some(entry) => entry.1.push(i),
                None => groups.push((g, vec![i])),
            }
        }
        groups
            .into_iter()
            .flat_map(|(_, indices)| indices)
            .collect()
    }

    /// Shortcut-able panels in display order: charts first, then tables. The key
    /// returned by `shortcut_key(i)` activates `shortcut_targets()[i]`.
    pub fn shortcut_targets(&self) -> Vec<ShortcutTarget> {
        let mut targets: Vec<ShortcutTarget> = self
            .chart_monitor_order()
            .into_iter()
            .map(ShortcutTarget::Chart)
            .collect();
        targets.extend((0..self.table_monitors.len()).map(ShortcutTarget::Table));
        targets.truncate(MAX_SHORTCUTS);
        targets
    }

    /// Enter fullscreen for the panel bound to `key`, if any.
    pub fn activate_shortcut(&mut self, key: char) {
        let Some(index) = shortcut_index(key) else {
            return;
        };
        let targets = self.shortcut_targets();
        let Some(&target) = targets.get(index) else {
            return;
        };
        self.focus = match target {
            ShortcutTarget::Chart(idx) => Focus::Chart(idx),
            ShortcutTarget::Table(idx) => {
                // Fullscreen shows every ranked row, not just the compact grid panel's
                // top `OVERVIEW_TABLE_ROWS` — take a fresh, uncapped sample rather than
                // reusing the already-truncated `table_rows` snapshot.
                let rows = self.table_monitors[idx].sample(&self.state, None);
                Focus::Table(TableFocus {
                    table_index: idx,
                    rows,
                    selected: 0,
                })
            }
        };
    }

    pub fn exit_focus(&mut self) {
        self.focus = Focus::None;
    }

    /// Moves the selection in a fullscreened, frozen table by `delta` rows, wrapping
    /// around. No-op outside `Focus::Table`.
    pub fn move_selection(&mut self, delta: i32) {
        if let Focus::Table(tf) = &mut self.focus {
            if tf.rows.is_empty() {
                return;
            }
            let len = tf.rows.len() as i32;
            tf.selected = (tf.selected as i32 + delta).rem_euclid(len) as usize;
        }
    }

    /// Sends SIGKILL to the currently selected process in a fullscreened table, then
    /// drops it from the frozen snapshot. No-op outside `Focus::Table`.
    pub fn kill_selected(&mut self) {
        let Focus::Table(tf) = &mut self.focus else {
            return;
        };
        let Some(row) = tf.rows.get(tf.selected) else {
            return;
        };
        if let Some(process) = self.state.sys.process(Pid::from_u32(row.pid)) {
            process.kill_with(Signal::Kill);
        }
        tf.rows.remove(tf.selected);
        if !tf.rows.is_empty() {
            tf.selected = tf.selected.min(tf.rows.len() - 1);
        }
    }

    pub fn persist(&self) {
        let mut map = history::HistoryMap::new();
        for (monitor, history) in self.monitors.iter().zip(self.histories.iter()) {
            map.insert(monitor.id().to_string(), history.values());
        }
        history::save_all(&map);
    }
}
