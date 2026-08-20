use std::collections::HashSet;

use sysinfo::{Pid, Signal};

use crate::history::{self, CAPACITY, History};
use crate::monitor::{self, Monitor, SystemState, TableMonitor, TableRow};

/// The two top-level views. Each is sampled only while it's the active tab — see
/// `App::tick` — so switching away from one stops spending resources on it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Processes,
}

impl Tab {
    pub const ALL: [Tab; 2] = [Tab::Overview, Tab::Processes];

    pub fn title(&self) -> &'static str {
        match self {
            Tab::Overview => "Visão Geral",
            Tab::Processes => "Processos",
        }
    }
}

const SAVE_EVERY_N_TICKS: u32 = 5;

/// a-z minus 'q' (always quits/exits) and 'x' (left free in case it's ever needed
/// again — a fullscreened table's search box swallows every other letter it's given).
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

/// What a shortcut key points at: chart panels on the Overview tab, table panels on
/// the Processes tab — see `App::shortcut_targets`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ShortcutTarget {
    Chart(usize),
    Table(usize),
}

/// A table panel that's been fullscreened: its row order/shape is frozen at the
/// moment of entry (re-sampling is skipped in `App::tick` while focused, in favor of
/// `TableMonitor::refresh_values` updating live values in place) so the list doesn't
/// shift under the user while they read it, navigate, or kill a process.
pub struct TableFocus {
    pub table_index: usize,
    pub rows: Vec<TableRow>,
    pub selected: usize,
    /// Free-text filter typed directly (no separate search mode to enter) — only rows
    /// with a cell matching this, case-insensitively, are shown/selectable.
    pub query: String,
    /// Pids of tree nodes currently expanded (showing their children) — seeded with
    /// every root pid on entry (roots open, everything deeper closed), toggled by
    /// `App::expand_selected`/`collapse_selected`. Unused (stays empty) for flat
    /// tables, since their rows all have `child_count == 0`.
    pub expanded: HashSet<u32>,
}

impl TableFocus {
    /// Indices into `rows` that are currently visible: rows whose full ancestor chain
    /// is expanded. `rows` is already a pre-order DFS flattening, so this is a single
    /// pass — once a row with children isn't expanded, skip everything deeper until a
    /// row back at its own depth (or shallower) appears again. Search doesn't filter
    /// this — see `match_indices` — it only moves `selected` between matches.
    pub fn visible_indices(&self) -> Vec<usize> {
        let mut result = Vec::new();
        let mut hide_below: Option<usize> = None;
        for (i, row) in self.rows.iter().enumerate() {
            if let Some(d) = hide_below {
                if row.depth > d {
                    continue;
                }
                hide_below = None;
            }
            result.push(i);
            if row.child_count > 0 && !self.expanded.contains(&row.pid) {
                hide_below = Some(row.depth);
            }
        }
        result
    }

    /// Indices into `rows` (regardless of current visibility) with a cell matching
    /// `query`, case-insensitively, in tree order. Empty when there's no query.
    fn match_indices(&self) -> Vec<usize> {
        if self.query.is_empty() {
            return Vec::new();
        }
        let needle = self.query.to_lowercase();
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.cells.iter().any(|c| c.to_lowercase().contains(&needle)))
            .map(|(i, _)| i)
            .collect()
    }

    /// Indices into `rows` of every ancestor of `idx`, nearest first. `rows` is a
    /// pre-order DFS flattening, so an ancestor at `depth - 1` is always the nearest
    /// preceding row at exactly that depth — nothing shallower can appear between a
    /// node and its parent.
    fn ancestor_indices(&self, idx: usize) -> Vec<usize> {
        let mut result = Vec::new();
        let mut depth = self.rows[idx].depth;
        let mut i = idx;
        while depth > 0 && i > 0 {
            i -= 1;
            if self.rows[i].depth == depth - 1 {
                result.push(i);
                depth -= 1;
            }
        }
        result
    }

    /// Expands every ancestor of `idx` so it's visible, then moves `selected` onto it.
    fn focus_row(&mut self, idx: usize) {
        for a in self.ancestor_indices(idx) {
            self.expanded.insert(self.rows[a].pid);
        }
        if let Some(pos) = self.visible_indices().iter().position(|&i| i == idx) {
            self.selected = pos;
        }
    }

    /// Focuses the first match, if any. Called whenever the query changes, so editing
    /// the search box always jumps back to its first hit — same as before search
    /// stopped filtering the row list.
    fn focus_first_match(&mut self) {
        if let Some(idx) = self.match_indices().first().copied() {
            self.focus_row(idx);
        }
    }

    /// Moves `selected` to the next (`delta > 0`) or previous (`delta < 0`) match,
    /// wrapping around. If the current selection isn't itself a match, `delta > 0`
    /// lands on the first match and `delta < 0` on the last, so a single press always
    /// reaches the nearest hit in that direction. No-op with no matches.
    fn focus_relative_match(&mut self, delta: i32) {
        let matches = self.match_indices();
        if matches.is_empty() {
            return;
        }
        let current = self.visible_indices().get(self.selected).copied();
        let len = matches.len() as i32;
        let new_pos = match current.and_then(|c| matches.iter().position(|&m| m == c)) {
            Some(p) => (p as i32 + delta).rem_euclid(len),
            None => {
                if delta >= 0 {
                    0
                } else {
                    len - 1
                }
            }
        };
        self.focus_row(matches[new_pos as usize]);
    }
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
    pub tab: Tab,
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
            tab: Tab::Overview,
            state: SystemState::new(),
            ticks_since_save: 0,
        }
    }

    /// Switches to `tab` and immediately samples it (rather than waiting for the next
    /// tick), so the newly focused tab isn't stale for up to a second. No-op if `tab`
    /// is already active.
    pub fn switch_tab(&mut self, tab: Tab) {
        if tab == self.tab {
            return;
        }
        self.tab = tab;
        self.sample_active_tab();
    }

    /// Cycles to the next/previous tab, wrapping around.
    pub fn next_tab(&mut self) {
        let i = Tab::ALL.iter().position(|&t| t == self.tab).unwrap_or(0);
        self.switch_tab(Tab::ALL[(i + 1) % Tab::ALL.len()]);
    }

    pub fn prev_tab(&mut self) {
        let i = Tab::ALL.iter().position(|&t| t == self.tab).unwrap_or(0);
        self.switch_tab(Tab::ALL[(i + Tab::ALL.len() - 1) % Tab::ALL.len()]);
    }

    pub fn tick(&mut self) {
        match self.tab {
            Tab::Overview => self.state.refresh_overview(),
            Tab::Processes => self.state.refresh_processes(),
        }
        self.sample_active_tab();

        self.ticks_since_save += 1;
        if self.ticks_since_save >= SAVE_EVERY_N_TICKS {
            self.persist();
            self.ticks_since_save = 0;
        }
    }

    /// Samples only the monitors backing the currently active tab — the point of
    /// having tabs at all: an unfocused tab's monitors don't run.
    fn sample_active_tab(&mut self) {
        match self.tab {
            Tab::Overview => {
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
            }
            Tab::Processes => {
                // The fullscreened table (if any) keeps its row order/shape frozen —
                // re-sampling would re-rank and reshape it out from under whatever the
                // user is reading, searching, or has expanded — but its live values
                // (e.g. CPU%/memory) still refresh in place every tick.
                let frozen_idx = match &self.focus {
                    Focus::Table(tf) => Some(tf.table_index),
                    _ => None,
                };
                if let Some(idx) = frozen_idx {
                    let monitor = self.table_monitors[idx].as_mut();
                    if let Focus::Table(tf) = &mut self.focus {
                        monitor.refresh_values(&self.state, &mut tf.rows);
                    }
                }
                for (i, (monitor, rows)) in self
                    .table_monitors
                    .iter_mut()
                    .zip(self.table_rows.iter_mut())
                    .enumerate()
                {
                    if Some(i) != frozen_idx {
                        *rows = monitor.sample(&self.state, Some(OVERVIEW_TABLE_ROWS));
                    }
                }
            }
        }
    }

    /// Chart-worthy monitor indices ordered by `Monitor::group()` — the same order the
    /// UI lays out the chart grid in. Shared so shortcut numbering always matches what's
    /// on screen.
    pub fn chart_monitor_order(&self) -> Vec<usize> {
        let mut groups: Vec<(&'static str, Vec<usize>)> = Vec::new();
        for (i, m) in self.monitors.iter().enumerate() {
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

    /// Shortcut-able panels on the active tab, in display order — charts on Overview,
    /// tables on Processes. Each tab has its own independent `1..9,a..z` numbering, so
    /// switching tabs never changes what a given key does within it. The key returned
    /// by `shortcut_key(i)` activates `shortcut_targets()[i]`.
    pub fn shortcut_targets(&self) -> Vec<ShortcutTarget> {
        let mut targets: Vec<ShortcutTarget> = match self.tab {
            Tab::Overview => self
                .chart_monitor_order()
                .into_iter()
                .map(ShortcutTarget::Chart)
                .collect(),
            Tab::Processes => (0..self.table_monitors.len())
                .map(ShortcutTarget::Table)
                .collect(),
        };
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
                // Roots open by default (showing their direct children), everything
                // deeper closed — same "2nd level" policy the compact panel uses.
                let expanded = rows
                    .iter()
                    .filter(|r| r.depth == 0)
                    .map(|r| r.pid)
                    .collect();
                Focus::Table(TableFocus {
                    table_index: idx,
                    rows,
                    selected: 0,
                    query: String::new(),
                    expanded,
                })
            }
        };
    }

    pub fn exit_focus(&mut self) {
        self.focus = Focus::None;
    }

    /// Moves the selection by `delta`, wrapping around. While searching, this instead
    /// jumps between matches (see `TableFocus::focus_relative_match`) — the row list
    /// itself is never filtered. No-op outside `Focus::Table`.
    pub fn move_selection(&mut self, delta: i32) {
        if let Focus::Table(tf) = &mut self.focus {
            if !tf.query.is_empty() {
                tf.focus_relative_match(delta);
                return;
            }
            let indices = tf.visible_indices();
            if indices.is_empty() {
                return;
            }
            let len = indices.len() as i32;
            tf.selected = (tf.selected as i32 + delta).rem_euclid(len) as usize;
        }
    }

    /// Sends SIGKILL to the currently selected process *and* every descendant in its
    /// subtree, then drops all of them from the frozen snapshot. No-op outside
    /// `Focus::Table`.
    pub fn kill_selected(&mut self) {
        let Focus::Table(tf) = &mut self.focus else {
            return;
        };
        let indices = tf.visible_indices();
        let Some(&row_idx) = indices.get(tf.selected) else {
            return;
        };
        let Some(row) = tf.rows.get(row_idx) else {
            return;
        };
        let mut dead: HashSet<u32> = row.descendant_pids.iter().copied().collect();
        dead.insert(row.pid);
        for &pid in &dead {
            if let Some(process) = self.state.sys.process(Pid::from_u32(pid)) {
                process.kill_with(Signal::Kill);
            }
        }
        tf.rows.retain(|r| !dead.contains(&r.pid));
        tf.expanded.retain(|pid| !dead.contains(pid));
        let indices = tf.visible_indices();
        tf.selected = if indices.is_empty() {
            0
        } else {
            tf.selected.min(indices.len() - 1)
        };
    }

    /// Expands the selected row's children (Right arrow). No-op if it's a leaf or
    /// already expanded, or outside `Focus::Table`.
    pub fn expand_selected(&mut self) {
        if let Focus::Table(tf) = &mut self.focus {
            let indices = tf.visible_indices();
            if let Some(&row_idx) = indices.get(tf.selected)
                && let Some(row) = tf.rows.get(row_idx)
                && row.child_count > 0
            {
                tf.expanded.insert(row.pid);
            }
        }
    }

    /// Collapses the selected row's children (Left arrow). No-op if it's a leaf or
    /// already collapsed, or outside `Focus::Table`.
    pub fn collapse_selected(&mut self) {
        if let Focus::Table(tf) = &mut self.focus {
            let indices = tf.visible_indices();
            if let Some(&row_idx) = indices.get(tf.selected)
                && let Some(row) = tf.rows.get(row_idx)
            {
                tf.expanded.remove(&row.pid);
            }
        }
    }

    /// Appends a typed character to the active fullscreen table's search box and
    /// jumps the selection to its first match (expanding ancestors as needed to reveal
    /// it) — the row list itself is never filtered. No-op outside `Focus::Table`.
    pub fn search_push(&mut self, c: char) {
        if let Focus::Table(tf) = &mut self.focus {
            tf.query.push(c);
            tf.focus_first_match();
        }
    }

    /// Removes the last character from the active search box and re-focuses the first
    /// match of what remains. No-op outside `Focus::Table`.
    pub fn search_backspace(&mut self) {
        if let Focus::Table(tf) = &mut self.focus {
            tf.query.pop();
            tf.focus_first_match();
        }
    }

    /// Clears the active search box, leaving the selection wherever it landed. No-op
    /// outside `Focus::Table`.
    pub fn clear_search(&mut self) {
        if let Focus::Table(tf) = &mut self.focus {
            tf.query.clear();
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
