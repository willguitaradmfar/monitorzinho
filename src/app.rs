use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::KeyCode;
use sysinfo::{Pid, Signal};

use crate::format;
use crate::history::{self, CAPACITY, History};
use crate::monitor::mark::{self, Mark};
use crate::monitor::{
    self as monitors, Danger, Detail, Monitor, SystemState, TableMonitor, TableRow,
};
use crate::tools::persist::ExecutionSpec;
use crate::tools::rewrite::{self, Rule};
use crate::tools::{self, Execution, Handoff, ParamKind, ParamSpec, State, Tool};

/// The top-level views. Each is sampled only while it's the active tab — see
/// `App::tick` — so switching away from one stops spending resources on it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Processes,
    /// Unlike the other two this one doesn't watch anything: it lists the tool
    /// executions the user has started, which keep running regardless of which tab is
    /// on screen.
    Tools,
}

impl Tab {
    pub const ALL: [Tab; 3] = [Tab::Overview, Tab::Processes, Tab::Tools];

    pub fn title(&self) -> &'static str {
        match self {
            Tab::Overview => "Visão Geral",
            Tab::Processes => "Processos",
            Tab::Tools => "Ferramentas",
        }
    }
}

const SAVE_EVERY_N_TICKS: u32 = 5;

/// How long a restart waits between stopping an execution and starting its replacement.
/// Comfortably longer than a tool's socket poll interval, so the old listener has
/// actually released the port by the time the new one asks for it.
const RESTART_GRACE: Duration = Duration::from_millis(300);

/// a-z minus 'q' (closes a fullscreened chart/detail) and 'x' (left free in case it's
/// ever needed again — a fullscreened table's search box swallows every other letter
/// it's given). Quitting the app is Ctrl+C twice, never a letter.
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

/// Times one full sample of the Processes tab and prints where the time went.
///
/// That sample is exactly what a keypress on Tab pays for, so this is the measurement
/// that matters for how the app *feels*, as opposed to the steady-state CPU a `top`
/// would show. Run on the machine that feels slow: the answer differs by an order of
/// magnitude between a laptop and a node running hundreds of containers.
pub fn bench() {
    let mut state = SystemState::new();
    let mut monitors = monitors::all_table_monitors();

    println!(
        "monitorzinho {} — amostragem da aba Processos",
        env!("CARGO_PKG_VERSION")
    );
    println!();
    // Twice: the first pass fills every cache in the process and in the kernel, and the
    // second is what a running app actually pays each time.
    for pass in 1..=2 {
        let started = Instant::now();
        state.refresh_processes();
        let refreshed = started.elapsed();
        println!(
            "passagem {pass}{}",
            if pass == 1 {
                " (fria)"
            } else {
                " (quente — é esta que conta)"
            }
        );
        println!(
            "  {:<28}{:>8.1} ms",
            "refresh do /proc (sysinfo)",
            refreshed.as_secs_f64() * 1000.0
        );

        let mut total = refreshed;
        for monitor in monitors.iter_mut() {
            let at = Instant::now();
            let rows = monitor.sample(&state, Some(OVERVIEW_TABLE_ROWS));
            let elapsed = at.elapsed();
            total += elapsed;
            println!(
                "  {:<28}{:>8.1} ms   ({} linha(s))",
                monitor.title(),
                elapsed.as_secs_f64() * 1000.0,
                rows.len()
            );
        }
        println!("  {:<28}{:>8.1} ms", "TOTAL", total.as_secs_f64() * 1000.0);
        // What the loop does with a sample this expensive.
        println!(
            "  {:<28}{:>8.1} s    (intervalo escolhido para este custo)",
            "TICK",
            interval_for(total).as_secs_f64()
        );
        println!();
    }
}

/// What to put in the box when it opens over a given row: the port under the cursor,
/// the command under the cursor, the user under the cursor.
///
/// For a numeric kind it is the first number in the cell, which is the port itself
/// rather than the address around it. For a text one it is the cell, trimmed of the
/// container label and the tree drawing that belong to the display and not to the value.
fn suggested_value(row: &TableRow, kind: &mark::MarkKind) -> String {
    let Some(cell) = row.cells.get(kind.column) else {
        return String::new();
    };
    if kind.numeric {
        return cell
            .split(|c: char| !c.is_ascii_digit())
            .find(|part| !part.is_empty())
            .unwrap_or_default()
            .to_string();
    }
    let text = cell.trim();
    // A command line is long and its first word is what anyone would type.
    match text.split_whitespace().next() {
        Some(first) if text.len() > 40 => first.to_string(),
        _ => text.to_string(),
    }
}

/// The little form that writes one mark: what kind of thing to watch, the value, and —
/// where the table is a tree — whether the children come along.
///
/// It opens over a fullscreened table with the fields already filled from the row under
/// the cursor, because that is where the answer almost always is: you are looking at the
/// thing you want to follow when you decide to follow it.
pub struct MarkEditor {
    pub table_index: usize,
    /// Which of the table's kinds is selected, as an index into `mark_kinds()`.
    pub kind: usize,
    pub value: String,
    pub subtree: bool,
    /// Whether the table has a tree to extend a mark down — only then is the third
    /// field shown, since offering it on a flat list would be a question with one answer.
    pub tree: bool,
}

/// One row of a table, opened with Enter for everything its monitor knows about it —
/// the "wireshark-ish" view of a connection, minus the packets. Unlike `TableFocus`,
/// nothing here is frozen: the whole `detail` is rebuilt every tick so its values stay
/// live, which is the entire point of opening it.
pub struct DetailFocus {
    pub table_index: usize,
    /// The row the detail is about. Its `key` is what each tick re-queries, so this is
    /// kept rather than an index — the underlying table is free to reshape meanwhile.
    pub row: TableRow,
    pub detail: Detail,
    /// Set once the subject stops showing up in fresh samples (connection closed,
    /// process exited). The last known values stay on screen, flagged as stale —
    /// blanking the view at the exact moment something disappears would throw away
    /// what the user most likely opened it to see.
    pub gone: bool,
    /// Per-connection throughput, so the detail can sparkline just this one socket
    /// instead of the whole interface. Starts empty on entry — unlike the chart
    /// panels, there's no history to restore for a connection we've never seen.
    pub down: History,
    pub up: History,
    /// How far the field list is scrolled, in lines — a detail runs longer than a
    /// terminal on any real connection.
    pub scroll: u16,
    /// Largest offset `scroll` can usefully take: content height minus what fits on
    /// screen. Only the renderer knows either number, so it writes this back each
    /// frame and `App::scroll_detail` clamps against the last one it saw — a single
    /// frame of lag, invisible in practice, and much better than letting `scroll` run
    /// off past the end where several keypresses do nothing.
    pub max_scroll: Cell<u16>,
    /// The table view this was opened from, put back intact on Esc: same selection,
    /// same query, same expanded nodes.
    pub parent: TableFocus,
    /// The hand-off picker, while it's open over the detail.
    pub handoff: Option<HandoffPicker>,
}

/// The executions started from the Ferramentas tab, and which one is selected on it.
/// Selection lives here rather than in `Focus` because that screen isn't a fullscreen
/// mode — it *is* the tab.
pub struct ToolsState {
    pub executions: Vec<Execution>,
    pub selected: usize,
    /// Monotonic, never reused — an execution's id is what the monitor view holds onto,
    /// so recycling one would silently point it at a different execution.
    next_id: u64,
}

impl ToolsState {
    fn new() -> Self {
        Self {
            executions: Vec::new(),
            selected: 0,
            next_id: 0,
        }
    }

    fn take_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub fn selected(&self) -> Option<&Execution> {
        self.executions.get(self.selected)
    }

    fn index_of(&self, id: u64) -> Option<usize> {
        self.executions.iter().position(|e| e.id == id)
    }

    pub fn by_id(&self, id: u64) -> Option<&Execution> {
        self.executions.iter().find(|e| e.id == id)
    }

    /// Runs `tool` with `values`, returning the execution either way: one that failed
    /// to start is kept, carrying its error, rather than vanishing.
    fn launch(&mut self, tool: &dyn Tool, values: HashMap<&'static str, String>) -> Execution {
        let id = self.take_id();
        let spec = ExecutionSpec {
            tool: tool.id().to_string(),
            params: values
                .iter()
                .map(|(key, value)| (key.to_string(), value.clone()))
                .collect(),
        };
        match tool.start(id, &values) {
            Ok(execution) => execution,
            Err(error) => Execution::failed(id, tool.name(), tool.summarize(&values), error),
        }
        .with_spec(spec)
    }

    /// Writes the current list of executions to disk. Called on every change rather
    /// than on a timer, so a crash can't lose one that was added seconds earlier.
    fn persist(&self) {
        let specs: Vec<ExecutionSpec> = self
            .executions
            .iter()
            .filter_map(|execution| execution.spec().cloned())
            .collect();
        tools::persist::save(&specs);
    }
}

/// Rebuilds a saved execution's parameters against the tool's *current* declaration:
/// a parameter added since the file was written gets its default, and one that no
/// longer exists is dropped. That way an upgrade never rejects an old config outright.
fn restore_params(tool: &dyn Tool, saved: &ExecutionSpec) -> HashMap<&'static str, String> {
    tool.params()
        .into_iter()
        .map(|spec| {
            let value = saved
                .params
                .get(spec.key)
                .cloned()
                .unwrap_or_else(|| spec.default.to_string());
            (spec.key, value)
        })
        .collect()
}

/// Which step of adding an execution the user is on. Deliberately linear — pick a
/// tool, fill in what it needs, look at it once before anything starts listening.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum WizardStep {
    SelectTool,
    Params,
    Confirm,
}

/// One parameter being filled in, i.e. its spec plus what's been typed so far.
pub struct ParamField {
    pub spec: ParamSpec,
    pub value: String,
}

impl ParamField {
    /// Moves a `Choice` field to the next/previous option, wrapping — and a text field
    /// that has suggestions through those, since walking a list the machine already
    /// knows beats typing it back in. A text field with nothing to suggest is edited by
    /// typing instead, and ←/→ do nothing to it.
    fn cycle(&mut self, delta: i32) {
        if let ParamKind::Choice(options) = self.spec.kind {
            let current = options.iter().position(|o| *o == self.value).unwrap_or(0);
            let len = options.len() as i32;
            let next = (current as i32 + delta).rem_euclid(len) as usize;
            self.value = options[next].to_string();
            return;
        }
        if !matches!(self.spec.kind, ParamKind::Text) || self.spec.suggestions.is_empty() {
            return;
        }
        let suggestions = &self.spec.suggestions;
        // The value may well be something typed rather than picked, in which case there
        // is no position in the list to move from: → enters it at the top and ← at the
        // bottom, so a suggestion is always one keypress away from anything typed.
        let next = match suggestions.iter().position(|s| s.value == self.value) {
            Some(current) => (current as i32 + delta).rem_euclid(suggestions.len() as i32) as usize,
            None if delta >= 0 => 0,
            None => suggestions.len() - 1,
        };
        self.value = suggestions[next].value.clone();
    }
}

/// The add-an-execution wizard.
pub struct ToolWizard {
    pub step: WizardStep,
    /// Index into `App::tools_available`.
    pub tool: usize,
    pub fields: Vec<ParamField>,
    /// Which field is focused during `Params`.
    pub field: usize,
    /// Why the last attempt to start failed — shown in place until the user changes
    /// something. `Tool::start` does the validating, so this is whatever it said.
    pub error: Option<String>,
    /// The rules screen, while it's open on top of the form.
    pub editor: Option<RulesEditor>,
    /// The execution being reconfigured, or `None` when adding a new one. An edit skips
    /// the tool-picking step — the tool is what it already is — and replaces that
    /// execution instead of appending one.
    pub editing: Option<u64>,
}

/// Editing one execution's list of rewrite rules.
///
/// It lives inside the wizard rather than beside it: the list belongs to the parameter
/// being filled in, and closing it writes the encoded value straight back into that
/// field. Nothing is committed to the execution until the wizard itself is confirmed.
pub struct RulesEditor {
    /// Which wizard field this list belongs to.
    field: usize,
    pub rules: Vec<Rule>,
    pub selected: usize,
    pub mode: RulesMode,
    pub error: Option<String>,
}

pub enum RulesMode {
    /// Looking at this execution's rules.
    List,
    /// Typing one rule. `editing` is the index being replaced, or `None` for a new one.
    Edit {
        find: String,
        replace: String,
        on_replace: bool,
        editing: Option<usize>,
    },
    /// Picking from the rules saved by every execution that ever had one.
    History { entries: Vec<Rule>, selected: usize },
}

impl RulesEditor {
    fn new(field: usize, encoded: &str) -> Self {
        Self {
            field,
            rules: rewrite::decode(encoded),
            selected: 0,
            mode: RulesMode::List,
            error: None,
        }
    }

    fn edit_new(&mut self) {
        self.mode = RulesMode::Edit {
            find: String::new(),
            replace: String::new(),
            on_replace: false,
            editing: None,
        };
        self.error = None;
    }

    fn edit_selected(&mut self) {
        let Some(rule) = self.rules.get(self.selected) else {
            return;
        };
        self.mode = RulesMode::Edit {
            find: rule.find.clone(),
            replace: rule.replace.clone(),
            on_replace: false,
            editing: Some(self.selected),
        };
        self.error = None;
    }

    /// Validates the typed pattern and files it, both in this list and in the shared
    /// history. Compiling here is the point: a rule that can't compile would otherwise
    /// only fail much later, when the execution refuses to start.
    fn commit(&mut self) {
        let RulesMode::Edit {
            find,
            replace,
            editing,
            ..
        } = &self.mode
        else {
            return;
        };
        if find.is_empty() {
            self.error = Some("informe o que procurar".to_string());
            return;
        }
        let rule = Rule {
            find: find.clone(),
            replace: replace.clone(),
        };
        if let Err(e) = rewrite::Rules::parse(&rewrite::encode(std::slice::from_ref(&rule))) {
            self.error = Some(e);
            return;
        }
        match editing {
            Some(index) => {
                let index = *index;
                self.rules[index] = rule.clone();
                self.selected = index;
            }
            None => {
                self.rules.push(rule.clone());
                self.selected = self.rules.len() - 1;
            }
        }
        rewrite::remember(&rule);
        self.mode = RulesMode::List;
        self.error = None;
    }

    fn open_history(&mut self) {
        self.mode = RulesMode::History {
            entries: rewrite::history(),
            selected: 0,
        };
        self.error = None;
    }

    fn move_selection(&mut self, delta: i32) {
        match &mut self.mode {
            RulesMode::List => {
                if !self.rules.is_empty() {
                    let last = self.rules.len() as i32 - 1;
                    self.selected = (self.selected as i32 + delta).clamp(0, last) as usize;
                }
            }
            RulesMode::History { entries, selected } => {
                if !entries.is_empty() {
                    let last = entries.len() as i32 - 1;
                    *selected = (*selected as i32 + delta).clamp(0, last) as usize;
                }
            }
            // Up/Down move between the two lines of the form instead.
            RulesMode::Edit { on_replace, .. } => *on_replace = delta > 0,
        }
    }
}

/// Choosing which of an execution's findings to turn into a new execution.
pub struct HandoffPicker {
    /// Named by whichever view opened it, since "what these offers are" differs: a
    /// sweep's addresses and a connection's two ends are not the same kind of thing.
    pub title: &'static str,
    pub options: Vec<Handoff>,
    /// Index into the rendered list, which starts with the "all of them" row when there
    /// are enough offers for that to save anything.
    pub selected: usize,
    pub bulk: bool,
    /// Live search, typed straight in. A sweep of a /24 comes back with a hundred
    /// addresses and the one being looked for is somewhere in the middle.
    ///
    /// Unlike the tables', this search never hides a row. This is a list of things
    /// about to be *acted* on, with a "all of them at once" row sitting at the top of
    /// it: quietly narrowing what "all" means, or hiding rows a keypress away from
    /// creating an execution, is how someone ends up with forty executions they never
    /// saw. It moves the cursor to the match and marks it instead — and where a
    /// narrowed "all" is genuinely wanted, the bulk row says so in as many words.
    pub query: String,
}

/// Offers below this many aren't worth a bulk row: picking one of two is already one
/// keypress, and the row would only push the real choices down.
const BULK_THRESHOLD: usize = 3;

impl HandoffPicker {
    fn new(title: &'static str, options: Vec<Handoff>) -> Self {
        Self {
            title,
            bulk: options.len() >= BULK_THRESHOLD,
            options,
            selected: 0,
            query: String::new(),
        }
    }

    /// Whether this row's offer matches the current search. The bulk row never does —
    /// it isn't one of the findings, and letting it match would put the cursor on
    /// "create all of them" as the answer to a search for one.
    pub fn matches(&self, row: usize) -> bool {
        !self.query.is_empty()
            && self
                .at(row)
                .is_some_and(|offer| format::contains_ci(&offer.label, &self.query))
    }

    pub fn match_count(&self) -> usize {
        (0..self.rows()).filter(|&row| self.matches(row)).count()
    }

    /// The offers a narrowed bulk row would create — those matching the search, or all
    /// of them when nothing is being searched for.
    fn matching(&self) -> Vec<&Handoff> {
        if self.query.is_empty() {
            return self.options.iter().collect();
        }
        (0..self.rows())
            .filter(|&row| self.matches(row))
            .filter_map(|row| self.at(row))
            .collect()
    }

    /// Moves the cursor to the first match at or after `start`, wrapping once. A row
    /// that still matches keeps the cursor where it is, so typing more of a word never
    /// walks away from what's already found.
    fn focus_match_from(&mut self, start: usize) {
        let rows = self.rows();
        if self.query.is_empty() || rows == 0 {
            return;
        }
        if let Some(row) = (0..rows)
            .map(|step| (start + step) % rows)
            .find(|&row| self.matches(row))
        {
            self.selected = row;
        }
    }

    /// Next/previous match, wrapping — what ↑/↓ mean while a search is running, the
    /// same as in the tables and the log.
    fn jump_match(&mut self, delta: i32) {
        let hits: Vec<usize> = (0..self.rows()).filter(|&row| self.matches(row)).collect();
        if hits.is_empty() {
            return;
        }
        let current = hits
            .iter()
            .position(|&row| row >= self.selected)
            .unwrap_or(0) as i32;
        let next = if delta > 0 && hits.get(current as usize) == Some(&self.selected) {
            current + delta
        } else if delta > 0 {
            current
        } else {
            current + delta
        };
        self.selected = hits[next.rem_euclid(hits.len() as i32) as usize];
    }

    /// How many rows are shown, including the bulk row.
    pub fn rows(&self) -> usize {
        self.options.len() + usize::from(self.bulk)
    }

    /// Moves the highlight, clamped to the ends. Shared by ↑/↓ and PgUp/PgDn — the
    /// only difference between them is how far they ask to go.
    fn move_selection(&mut self, delta: i32) {
        let last = self.rows().saturating_sub(1) as i32;
        self.selected = (self.selected as i32 + delta).clamp(0, last) as usize;
    }

    /// The finding a row stands for, or `None` for the bulk row.
    pub fn at(&self, row: usize) -> Option<&Handoff> {
        match (self.bulk, row) {
            (true, 0) => None,
            (true, row) => self.options.get(row - 1),
            (false, row) => self.options.get(row),
        }
    }
}

/// How many lines of context are kept above a match when jumping to it, so a hit never
/// lands flush against the top border with nothing before it.
pub const MATCH_CONTEXT: u16 = 2;

/// The interval a sample of this cost earns.
///
/// Kept apart from `App` so it can be reasoned about — and printed by `--bench` — on
/// its own: it is a function of one number and nothing else.
pub fn interval_for(cost: Duration) -> Duration {
    let budget = SAMPLE_BUDGET.as_secs_f64();
    let cost = cost.as_secs_f64();
    if cost <= budget {
        return TICK_RATE;
    }
    // Keeps sampling at roughly the share of the machine it would have at two seconds
    // and a cheap sample: cost over interval stays near budget over TICK_RATE.
    TICK_RATE
        .mul_f64((cost / budget).min(MAX_SLOWDOWN))
        .min(MAX_TICK_RATE)
}

/// How often the active tab is resampled on a machine where that is cheap.
pub const TICK_RATE: Duration = Duration::from_secs(2);
/// What one sample may cost before the interval starts stretching. A tenth of a second
/// is under what anyone notices and a twentieth of the interval.
const SAMPLE_BUDGET: Duration = Duration::from_millis(100);
/// How much slower the tick may get, and the hard ceiling on the interval. Even the
/// most expensive machine gets fresh numbers within a handful of seconds.
const MAX_SLOWDOWN: f64 = 4.0;
const MAX_TICK_RATE: Duration = Duration::from_secs(8);

/// Rows a PgUp/PgDn moves a selection by, in every list that has one. A fixed step
/// rather than the panel's own height: only the renderer knows that, and a page that
/// means the same distance everywhere is easier to build a feel for than one that
/// changes with the size of what it's moving through. Unlike a single ↑/↓, a page
/// stops at the ends instead of wrapping — a fast gesture that silently teleports from
/// the bottom of a list to the top is a fast gesture that loses your place.
pub const PAGE_ROWS: i32 = 10;

/// The live log of one execution, opened with Enter from the Ferramentas tab.
///
/// Events are shown newest-first, so the live edge is the top of the screen and new
/// traffic pushes older lines downward. Several fields are `Cell`s written by the
/// renderer: it's the only place that knows how many lines the log currently occupies,
/// which of them match, and how tall the viewport is.
pub struct ToolMonitorFocus {
    /// Held by id, not index: the list can be reordered or shortened underneath.
    pub execution_id: u64,
    /// Free-text search, typed directly like the tables'. Matching text is highlighted
    /// wherever it appears rather than the non-matching lines being hidden — in a relay
    /// log the lines *around* a hit are usually the point.
    pub query: String,
    /// Show only lines that match, instead of highlighting in place.
    pub only_matches: bool,
    /// Render payloads as hex + ASCII rather than text. Off by default, since most of
    /// what a tunnel carries while debugging is text.
    pub hex: bool,
    pub scroll: Cell<u16>,
    /// Whether the view is pinned to the newest event — which, newest-first, means
    /// simply sitting at the top. Scrolling down into history releases it; End re-pins.
    pub follow: bool,
    pub max_scroll: Cell<u16>,
    /// Line indices of the current search's hits, ascending, as of the last frame.
    /// The hand-off picker, while it's open over the log.
    pub handoff: Option<HandoffPicker>,
    pub matches: RefCell<Vec<u16>>,
    /// Which of `matches` the view is parked on. `None` means the search hasn't been
    /// navigated yet, which the renderer takes as "jump to the first hit".
    pub match_index: Cell<Option<usize>>,
    /// Sequence number of the newest event at the last frame — see `Event::seq`.
    pub anchor_seq: Cell<u64>,
    /// How many lines into the anchored event's block the viewport top sits, so a block
    /// several lines tall is put back exactly rather than approximately.
    pub anchor_offset: Cell<u16>,
}

impl ToolMonitorFocus {
    /// Moves to the next (`delta > 0`, further down the screen and so further back in
    /// time) or previous hit, wrapping at either end. No-op with nothing to jump to.
    /// Puts the viewport somewhere on purpose.
    ///
    /// Clearing the anchor is the point: the anchor exists to hold the view still while
    /// the *content* moves underneath it, and without dropping it here the next frame
    /// would faithfully restore the position the key just moved away from.
    fn move_to(&self, position: u16) {
        self.scroll.set(position.min(self.max_scroll.get()));
        self.anchor_seq.set(0);
        self.anchor_offset.set(0);
    }

    fn jump_match(&mut self, delta: i32) {
        let matches = self.matches.borrow();
        if matches.is_empty() {
            return;
        }
        let len = matches.len() as i32;
        let next = match self.match_index.get() {
            Some(current) => (current as i32 + delta).rem_euclid(len) as usize,
            // A first press with no current hit lands on the nearest one in that
            // direction rather than doing nothing.
            None if delta >= 0 => 0,
            None => (len - 1) as usize,
        };
        self.match_index.set(Some(next));
        self.follow = false;
        self.move_to(matches[next].saturating_sub(MATCH_CONTEXT));
    }

    /// Forgets where the search was, so the next frame re-anchors on the first hit.
    /// Called whenever the query text changes.
    fn reset_search(&mut self) {
        self.match_index.set(None);
    }
}

pub enum Focus {
    None,
    Chart(usize),
    Table(TableFocus),
    /// Boxed because a `DetailFocus` carries the whole `TableFocus` it came from, and
    /// every `Focus` value would otherwise be that big.
    Detail(Box<DetailFocus>),
    Wizard(ToolWizard),
    ToolMonitor(ToolMonitorFocus),
}

/// One chart on the Overview tab: what it measures, and everything the panel knows
/// about it.
///
/// Kept together rather than as four vectors indexed in parallel, because panels are no
/// longer a fixed set — a tool that measures something over time adds one while the app
/// runs and takes it away again when its execution is removed, and four vectors that
/// have to be inserted into and removed from in lockstep is a bug waiting for the first
/// place that forgets one of them.
pub struct ChartPanel {
    pub monitor: Box<dyn Monitor>,
    pub history: History,
    /// The absolute quantity shown beside the value, sampled with it (e.g. "5.6 GB / 16.0 GB").
    pub extra: Option<String>,
    /// Total capacity behind a percentage metric, sampled with the value.
    pub capacity: Option<f64>,
    /// The execution this panel belongs to, for one created by a tool. `None` for the
    /// machine's own panels, which nothing can remove.
    pub execution: Option<u64>,
}

impl ChartPanel {
    fn new(monitor: Box<dyn Monitor>, history: History, execution: Option<u64>) -> Self {
        Self {
            monitor,
            history,
            extra: None,
            capacity: None,
            execution,
        }
    }
}

pub struct App {
    pub charts: Vec<ChartPanel>,
    /// Histories for charts that aren't on screen: what was read from disk at launch,
    /// plus what a removed panel left behind. Keyed by `Monitor::id`, same as the file.
    known_histories: history::HistoryMap,
    pub table_monitors: Vec<Box<dyn TableMonitor>>,
    pub table_rows: Vec<Vec<TableRow>>,
    /// What the user asked to keep an eye on, across restarts — see `monitor::mark`.
    pub marks: mark::Marks,
    /// The mark being written, while its box is open.
    pub mark_editor: Option<MarkEditor>,
    pub tools_available: Vec<Box<dyn Tool>>,
    pub tools: ToolsState,
    pub focus: Focus,
    pub tab: Tab,
    /// Set by the first Ctrl+C and cleared by any other key: the app only closes on a
    /// second Ctrl+C pressed straight after the first, so a stray one never kills a
    /// session that's carrying live executions.
    pub quit_armed: bool,
    /// Set when the tab changed and its data hasn't been refreshed yet — see
    /// `switch_tab`.
    pending_sample: bool,
    /// How long the last sample took. What the interval is chosen from — see `interval`.
    last_sample: Duration,
    /// A destructive key waiting to be confirmed. Sits above every screen and takes
    /// every key while it's open, so nothing underneath can act on the keypress that
    /// dismisses it.
    pub pending: Option<Pending>,
    state: SystemState,
    ticks_since_save: u32,
}

/// A destructive action, described and held until it's confirmed.
pub struct Pending {
    pub danger: Danger,
    pub action: PendingAction,
}

/// What to carry out once the confirmation is accepted. Each variant re-reads what it
/// needs at that moment rather than carrying a snapshot: a table can reshape, and a
/// stored index would then point at the wrong row.
pub enum PendingAction {
    /// SIGKILL the selected row's process and its subtree, in the fullscreened table.
    KillRow,
    /// Stop and forget the selected execution on the Ferramentas tab.
    RemoveExecution,
    /// Drop one rule from the shared rewrite history, which lives on disk.
    ForgetRule(Rule),
}

/// The saved line for `key`, or a blank one. A chart that has been running before picks
/// up where it left off; a new one starts empty.
fn restored_history(saved: &history::HistoryMap, key: &str) -> History {
    match saved.get(key) {
        Some(values) => History::from_saved(values.clone(), CAPACITY),
        None => History::new(CAPACITY),
    }
}

impl App {
    pub fn new() -> Self {
        let known_histories = history::load_all();
        let charts = monitors::all_monitors()
            .into_iter()
            .map(|m| {
                let history = restored_history(&known_histories, m.id());
                ChartPanel::new(m, history, None)
            })
            .collect();

        let table_monitors = monitors::all_table_monitors();
        let table_rows = table_monitors.iter().map(|_| Vec::new()).collect();
        let marks = mark::Marks::load();

        // Executions come back up before the first frame: whatever was listening when
        // the app was last closed is listening again by the time the user sees the tab.
        let tools_available = tools::all_tools();
        let mut tools = ToolsState::new();
        for saved in tools::persist::load() {
            // A tool that no longer exists in this build is skipped, but its entry is
            // left in the file — downgrading and re-running shouldn't have silently
            // thrown the configuration away.
            let Some(tool) = tools_available.iter().find(|t| t.id() == saved.tool) else {
                continue;
            };
            let values = restore_params(tool.as_ref(), &saved);
            let execution = tools.launch(tool.as_ref(), values);
            tools.executions.push(execution);
        }

        let mut app = Self {
            charts,
            known_histories,
            table_monitors,
            table_rows,
            marks,
            mark_editor: None,
            tools_available,
            tools,
            focus: Focus::None,
            tab: Tab::Overview,
            quit_armed: false,
            pending_sample: false,
            last_sample: Duration::ZERO,
            pending: None,
            state: SystemState::new(),
            ticks_since_save: 0,
        };
        // A restored execution that charts something gets its panel back here, so the
        // Overview tab looks the same as it did when the app was closed.
        app.sync_tool_charts();
        app
    }

    /// Brings the chart panels in line with the executions that exist right now: one
    /// panel for every execution publishing a series, and none for an execution that
    /// has gone away.
    ///
    /// Reconciled rather than hooked onto each place an execution is created or removed
    /// — there are five of those, and the failure mode of missing one is a chart that
    /// keeps drawing for something that stopped existing.
    fn sync_tool_charts(&mut self) {
        let live: Vec<u64> = self.tools.executions.iter().map(|e| e.id).collect();
        let mut removed = Vec::new();
        self.charts.retain(|panel| {
            let keep = panel.execution.is_none_or(|id| live.contains(&id));
            if !keep {
                removed.push((panel.monitor.id().to_string(), panel.history.values()));
            }
            keep
        });
        // A panel that goes away leaves its line behind: pointing a tool at the same
        // target again in the same session continues where it stopped rather than
        // starting blank, which is the whole reason the key names the target.
        self.known_histories.extend(removed);

        for index in 0..self.tools.executions.len() {
            let id = self.tools.executions[index].id;
            if self.charts.iter().any(|p| p.execution == Some(id)) {
                continue;
            }
            let Some(monitor) = self.tools.executions[index].chart_monitor() else {
                continue;
            };
            let history = restored_history(&self.known_histories, monitor.id());
            self.charts
                .push(ChartPanel::new(monitor, history, Some(id)));
        }
    }

    /// Whether what's on screen is fed by the tools' own threads. Decides whether their
    /// writing something is worth a redraw between samples — anywhere else the next
    /// tick is soon enough, because nothing on screen changed.
    pub fn shows_tools(&self) -> bool {
        self.tab == Tab::Tools || matches!(self.focus, Focus::ToolMonitor(_))
    }

    /// Switches to `tab` and immediately samples it (rather than waiting for the next
    /// tick), so the newly focused tab isn't stale for up to a second. No-op if `tab`
    /// is already active.
    pub fn switch_tab(&mut self, tab: Tab) {
        if tab == self.tab {
            return;
        }
        self.tab = tab;
        // Not sampled here. Sampling the Processes tab means reading /proc for every
        // process on the machine, which on a busy server is a third of a second — and
        // doing it before the first draw is what turns a keypress into a wait. The tab
        // is drawn with what it already has and `pending_sample` makes the loop fill it
        // in immediately afterwards, so the key answers at once and the numbers land a
        // moment later.
        self.pending_sample = true;
    }

    /// How long to wait before sampling again.
    ///
    /// Two seconds on any ordinary machine. But sampling costs what the machine *has* —
    /// a Kubernetes node with eight hundred processes and forty-four network namespaces
    /// takes a third of a second to answer, and spending a sixth of every second on
    /// that is a monitor that competes with what it is monitoring. So a sample that
    /// takes longer than `SAMPLE_BUDGET` buys itself proportionally more room, up to a
    /// ceiling: the machine that is cheap to read stays live, and the one that is
    /// expensive to read stops charging for it twice a second.
    pub fn interval(&self) -> Duration {
        interval_for(self.last_sample)
    }

    /// Whether the loop owes the newly-shown tab a sample. Taken, not peeked: asking is
    /// what clears it.
    pub fn take_pending_sample(&mut self) -> bool {
        std::mem::take(&mut self.pending_sample)
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
        let started = Instant::now();
        // Executions come and go between ticks — from the wizard, from a hand-off, from
        // being removed — and the panels follow whatever exists now.
        self.sync_tool_charts();
        match self.tab {
            Tab::Overview => self.state.refresh_overview(),
            Tab::Processes => self.state.refresh_processes(),
            // Nothing to refresh: an execution's counters are atomics the UI reads
            // directly, and its log is appended to by the tool's own threads.
            Tab::Tools => {}
        }
        self.sample_active_tab();
        self.last_sample = started.elapsed();

        self.ticks_since_save += 1;
        if self.ticks_since_save >= SAVE_EVERY_N_TICKS {
            self.persist();
            self.ticks_since_save = 0;
        }
    }

    /// Samples only the monitors backing the currently active tab — the point of
    /// having tabs at all: an unfocused tab's monitors don't run.
    fn sample_active_tab(&mut self) {
        // A panel a tool feeds is sampled on every tab, not just its own: the value is
        // already measured and reading it costs one atomic load, and the whole point of
        // leaving a measurement running is that its line keeps being drawn while the
        // user is looking at something else. The machine's own panels are the expensive
        // ones, and those still only run while their tab is up.
        if self.tab != Tab::Overview {
            for panel in self.charts.iter_mut() {
                if panel.execution.is_some() {
                    let value = panel.monitor.sample(&self.state);
                    panel.history.push(value);
                }
            }
        }
        match self.tab {
            Tab::Overview => {
                for panel in self.charts.iter_mut() {
                    let value = panel.monitor.sample(&self.state);
                    panel.history.push(value);
                    panel.extra = panel.monitor.extra(&self.state);
                    panel.capacity = panel.monitor.capacity(&self.state);
                }
            }
            Tab::Processes => {
                // The fullscreened table (if any) keeps its row order/shape frozen —
                // re-sampling would re-rank and reshape it out from under whatever the
                // user is reading, searching, or has expanded — but its live values
                // (e.g. CPU%/memory) still refresh in place every tick.
                let frozen_idx = match &self.focus {
                    Focus::Table(tf) => Some(tf.table_index),
                    // A detail view's table is frozen too — it's still there behind it,
                    // waiting to be restored exactly as it was left.
                    Focus::Detail(df) => Some(df.table_index),
                    _ => None,
                };
                if let Some(idx) = frozen_idx {
                    // Before the monitor is borrowed: a detail view is about one process
                    // and can afford to know everything about it, including the fields
                    // the machine-wide refresh skips because they cost a syscall each,
                    // across every process, on every tick.
                    if let Focus::Detail(df) = &self.focus
                        && df.row.pid != 0
                    {
                        let pid = df.row.pid;
                        self.state.refresh_one(pid);
                    }
                    let monitor = self.table_monitors[idx].as_mut();
                    match &mut self.focus {
                        Focus::Table(tf) => {
                            monitor.refresh_values(&self.state, &mut tf.rows);
                            self.marks
                                .apply(monitor.id(), monitor.mark_kinds(), &mut tf.rows);
                        }
                        // Rebuilt rather than patched in place: a detail is a few dozen
                        // formatted strings, cheap enough that tracking which of them
                        // changed would cost more than just building them again.
                        Focus::Detail(df) => {
                            let detail = monitor.detail(&self.state, &df.row);
                            match detail {
                                Some(detail) => {
                                    if let Some(rates) = &detail.rates {
                                        df.down.push(rates.values.0);
                                        df.up.push(rates.values.1);
                                    }
                                    df.detail = detail;
                                    df.gone = false;
                                }
                                None => df.gone = true,
                            }
                        }
                        _ => {}
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
                        self.marks.apply(monitor.id(), monitor.mark_kinds(), rows);
                    }
                }
            }
            Tab::Tools => {}
        }
    }

    /// Chart-worthy monitor indices ordered by `Monitor::group()` — the same order the
    /// UI lays out the chart grid in. Shared so shortcut numbering always matches what's
    /// on screen.
    pub fn chart_monitor_order(&self) -> Vec<usize> {
        let mut groups: Vec<(&'static str, Vec<usize>)> = Vec::new();
        for (i, panel) in self.charts.iter().enumerate() {
            let g = panel.monitor.group();
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
            // No shortcut-able panels here, which is also what frees the letter keys
            // on this tab for its own bindings ('a' to add an execution).
            Tab::Tools => Vec::new(),
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
                let mut rows = self.table_monitors[idx].sample(&self.state, None);
                let monitor = self.table_monitors[idx].as_ref();
                self.marks
                    .apply(monitor.id(), monitor.mark_kinds(), &mut rows);
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

    /// Opens the detail view for the selected row (Enter). No-op outside a fullscreen
    /// table, on an empty selection, or on a table whose monitor has no detail to give
    /// — `TableMonitor::detail` returning `None` is how a table opts out.
    pub fn open_detail(&mut self) {
        let Focus::Table(tf) = &self.focus else {
            return;
        };
        let Some(&row_idx) = tf.visible_indices().get(tf.selected) else {
            return;
        };
        let (table_index, row) = (tf.table_index, tf.rows[row_idx].clone());

        let monitor = self.table_monitors[table_index].as_mut();
        let Some(detail) = monitor.detail(&self.state, &row) else {
            return;
        };

        let mut down = History::new(CAPACITY);
        let mut up = History::new(CAPACITY);
        if let Some(rates) = &detail.rates {
            down.push(rates.values.0);
            up.push(rates.values.1);
        }
        // Only now that we know there's a detail to show does the table get taken —
        // bailing out above must leave the focus untouched.
        let Focus::Table(parent) = std::mem::replace(&mut self.focus, Focus::None) else {
            return;
        };
        self.focus = Focus::Detail(Box::new(DetailFocus {
            table_index,
            row,
            detail,
            gone: false,
            down,
            up,
            scroll: 0,
            max_scroll: Cell::new(0),
            parent,
            handoff: None,
        }));
    }

    /// Returns from a detail view to the table it was opened from, exactly as it was
    /// left (Esc/q). No-op outside `Focus::Detail`.
    pub fn close_detail(&mut self) {
        if let Focus::Detail(df) = std::mem::replace(&mut self.focus, Focus::None) {
            self.focus = Focus::Table(df.parent);
        }
    }

    /// Scrolls the detail's field list by `delta` lines, clamped to what's actually
    /// scrollable (see `DetailFocus::max_scroll`). No-op outside `Focus::Detail`.
    pub fn scroll_detail(&mut self, delta: i32) {
        if let Focus::Detail(df) = &mut self.focus {
            let limit = df.max_scroll.get() as i32;
            df.scroll = (df.scroll as i32 + delta).clamp(0, limit) as u16;
        }
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

    /// Moves the selection a whole page (PgUp/PgDn), stopping at the ends rather than
    /// wrapping the way a single step does — see `PAGE_ROWS`. While searching it steps
    /// between matches instead, same as ↑/↓, since that's what the keys mean there.
    pub fn page_selection(&mut self, delta: i32) {
        let Focus::Table(tf) = &mut self.focus else {
            return;
        };
        if !tf.query.is_empty() {
            tf.focus_relative_match(delta.signum());
            return;
        }
        let indices = tf.visible_indices();
        if indices.is_empty() {
            return;
        }
        let last = indices.len() as i32 - 1;
        tf.selected = (tf.selected as i32 + delta).clamp(0, last) as usize;
    }

    /// Opens the mark box for the selected row, or clears the marks that already match
    /// it — pressing the same key on something already followed means stop following it.
    pub fn toggle_mark(&mut self) {
        let Focus::Table(tf) = &self.focus else {
            return;
        };
        let index = tf.table_index;
        let monitor = self.table_monitors[index].as_ref();
        let kinds = monitor.mark_kinds();
        if kinds.is_empty() {
            return;
        }
        let Some(&row_idx) = tf.visible_indices().get(tf.selected) else {
            return;
        };
        let Some(row) = tf.rows.get(row_idx).cloned() else {
            return;
        };
        if self.marks.hit(monitor.id(), kinds, &row).is_some() {
            self.marks.remove_matching(monitor.id(), kinds, &row);
            self.reapply_marks();
            return;
        }
        // Filled in from the row: the value someone wants is almost always the one they
        // are looking at.
        let tree = tf.rows.iter().any(|row| row.child_count > 0);
        let value = suggested_value(&row, &kinds[0]);
        self.mark_editor = Some(MarkEditor {
            table_index: index,
            kind: 0,
            value,
            subtree: tree,
            tree,
        });
    }

    pub fn mark_editor_open(&self) -> bool {
        self.mark_editor.is_some()
    }

    /// Keys while the mark box is open. Same shape as every other small form here:
    /// ←/→ change the kind, typing edits the value, Enter saves, Esc gives up.
    pub fn mark_key(&mut self, code: KeyCode) {
        let Some(editor) = &mut self.mark_editor else {
            return;
        };
        let kinds = self.table_monitors[editor.table_index].mark_kinds();
        match code {
            KeyCode::Left | KeyCode::Right => {
                let delta = if code == KeyCode::Right { 1 } else { -1 };
                let count = kinds.len() as i32;
                editor.kind = (editor.kind as i32 + delta).rem_euclid(count) as usize;
                // The value follows the kind: switching from "porta" to "processo" with
                // a port number still in the box would save a mark that matches nothing.
                if let Focus::Table(tf) = &self.focus
                    && let Some(&row_idx) = tf.visible_indices().get(tf.selected)
                    && let Some(row) = tf.rows.get(row_idx)
                {
                    editor.value = suggested_value(row, &kinds[editor.kind]);
                }
            }
            KeyCode::Up | KeyCode::Down if editor.tree => editor.subtree = !editor.subtree,
            KeyCode::Char(c) => editor.value.push(c),
            KeyCode::Backspace => {
                editor.value.pop();
            }
            KeyCode::Enter => {
                let mark = Mark {
                    table: self.table_monitors[editor.table_index].id().to_string(),
                    kind: kinds[editor.kind].name.to_string(),
                    value: editor.value.trim().to_string(),
                    subtree: editor.tree && editor.subtree,
                };
                if !mark.value.is_empty() {
                    self.marks.add(mark);
                }
                self.mark_editor = None;
                self.reapply_marks();
            }
            KeyCode::Esc => self.mark_editor = None,
            _ => {}
        }
    }

    /// Re-runs the marks over whatever rows are on screen, so a mark added or dropped
    /// shows up now rather than at the next tick.
    fn reapply_marks(&mut self) {
        for (index, monitor) in self.table_monitors.iter().enumerate() {
            let kinds = monitor.mark_kinds();
            if let Some(rows) = self.table_rows.get_mut(index) {
                self.marks.apply(monitor.id(), kinds, rows);
            }
        }
        if let Focus::Table(tf) = &mut self.focus {
            let monitor = self.table_monitors[tf.table_index].as_ref();
            self.marks
                .apply(monitor.id(), monitor.mark_kinds(), &mut tf.rows);
        }
    }

    /// What `Del` would do to the selected row, or `None` where it would do nothing —
    /// which is also what the footer asks before offering the key at all.
    pub fn selected_danger(&self) -> Option<Danger> {
        let Focus::Table(tf) = &self.focus else {
            return None;
        };
        let &row_idx = tf.visible_indices().get(tf.selected)?;
        let row = tf.rows.get(row_idx)?;
        self.table_monitors[tf.table_index].danger(&self.state, row)
    }

    /// `Del` on a fullscreened table. Nothing dies here: it puts up what would happen
    /// and waits. A table with nothing to kill (interfaces, machine facts) returns no
    /// danger, and the key is then simply ignored.
    pub fn request_kill_selected(&mut self) {
        if let Some(danger) = self.selected_danger() {
            self.pending = Some(Pending {
                danger,
                action: PendingAction::KillRow,
            });
        }
    }

    /// Sends SIGKILL to the confirmed row's process *and* every descendant in its
    /// subtree, then drops all of them from the frozen snapshot. Reached only through
    /// `Pending`, never straight from a keypress.
    fn kill_selected(&mut self) {
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

    // --- Ferramentas tab ---------------------------------------------------------

    /// Moves the selection in the execution list, clamped (not wrapped — a short list
    /// that jumps from top to bottom under an arrow key reads as a glitch).
    pub fn move_tool_selection(&mut self, delta: i32) {
        let len = self.tools.executions.len();
        if len == 0 {
            return;
        }
        let next = (self.tools.selected as i32 + delta).clamp(0, len as i32 - 1);
        self.tools.selected = next as usize;
    }

    /// Opens the add-an-execution wizard at its first step ('a').
    pub fn open_wizard(&mut self) {
        if self.tools_available.is_empty() {
            return;
        }
        self.focus = Focus::Wizard(ToolWizard {
            step: WizardStep::SelectTool,
            tool: 0,
            fields: Vec::new(),
            field: 0,
            error: None,
            editor: None,
            editing: None,
        });
    }

    /// Moves within whatever the current wizard step is showing: the tool list, or the
    /// parameter fields.
    pub fn wizard_move(&mut self, delta: i32) {
        let tool_count = self.tools_available.len();
        let Focus::Wizard(wizard) = &mut self.focus else {
            return;
        };
        match wizard.step {
            WizardStep::SelectTool => {
                if tool_count > 0 {
                    let next = (wizard.tool as i32 + delta).clamp(0, tool_count as i32 - 1);
                    wizard.tool = next as usize;
                }
            }
            WizardStep::Params => {
                if !wizard.fields.is_empty() {
                    let next =
                        (wizard.field as i32 + delta).clamp(0, wizard.fields.len() as i32 - 1);
                    wizard.field = next as usize;
                }
            }
            WizardStep::Confirm => {}
        }
    }

    /// ←/→ on a multiple-choice parameter, or on a text field with suggestions to walk
    /// them. Nothing else in the wizard uses them, so a stray press elsewhere is simply
    /// ignored.
    pub fn wizard_cycle(&mut self, delta: i32) {
        if let Focus::Wizard(wizard) = &mut self.focus
            && wizard.step == WizardStep::Params
            && let Some(field) = wizard.fields.get_mut(wizard.field)
        {
            field.cycle(delta);
            wizard.error = None;
        }
    }

    pub fn wizard_type(&mut self, c: char) {
        if let Focus::Wizard(wizard) = &mut self.focus
            && wizard.step == WizardStep::Params
            && let Some(field) = wizard.fields.get_mut(wizard.field)
            && matches!(field.spec.kind, ParamKind::Text)
        {
            field.value.push(c);
            wizard.error = None;
        }
    }

    pub fn wizard_backspace(&mut self) {
        if let Focus::Wizard(wizard) = &mut self.focus
            && wizard.step == WizardStep::Params
            && let Some(field) = wizard.fields.get_mut(wizard.field)
            && matches!(field.spec.kind, ParamKind::Text)
        {
            field.value.pop();
            wizard.error = None;
        }
    }

    /// True while the rules screen is on top of the wizard, so key handling can go
    /// there first instead of to the form underneath.
    pub fn rules_editor_open(&self) -> bool {
        matches!(&self.focus, Focus::Wizard(wizard) if wizard.editor.is_some())
    }

    /// Every key while the rules screen is open. One entry point rather than an arm per
    /// binding, because what a letter means depends on which of the three modes is
    /// showing — in `Edit` they're all just text.
    pub fn rules_key(&mut self, code: KeyCode) {
        let Focus::Wizard(wizard) = &mut self.focus else {
            return;
        };
        let Some(editor) = &mut wizard.editor else {
            return;
        };

        if let RulesMode::Edit {
            find,
            replace,
            on_replace,
            ..
        } = &mut editor.mode
        {
            let line = if *on_replace { replace } else { find };
            match code {
                KeyCode::Char(c) => {
                    line.push(c);
                    editor.error = None;
                }
                KeyCode::Backspace => {
                    line.pop();
                    editor.error = None;
                }
                KeyCode::Tab | KeyCode::Up | KeyCode::Down => {
                    *on_replace = !*on_replace;
                }
                KeyCode::Enter => editor.commit(),
                KeyCode::Esc => {
                    editor.mode = RulesMode::List;
                    editor.error = None;
                }
                _ => {}
            }
            return;
        }

        if let RulesMode::History { entries, selected } = &editor.mode {
            match code {
                KeyCode::Up => editor.move_selection(-1),
                KeyCode::Down => editor.move_selection(1),
                KeyCode::PageUp => editor.move_selection(-PAGE_ROWS),
                KeyCode::PageDown => editor.move_selection(PAGE_ROWS),
                KeyCode::Enter => {
                    if let Some(rule) = entries.get(*selected).cloned() {
                        // Re-filed as it's picked, so the history keeps ordering itself
                        // by what's actually being used.
                        rewrite::remember(&rule);
                        editor.rules.push(rule);
                        editor.selected = editor.rules.len() - 1;
                        editor.mode = RulesMode::List;
                    }
                }
                KeyCode::Delete => {
                    // The history is shared by every execution and lives in a file:
                    // this is the one Del in the rules screen that leaves the current
                    // execution and touches something permanent.
                    if let Some(rule) = entries.get(*selected).cloned() {
                        let described = format!("«{}»  →  «{}»", rule.find, rule.replace);
                        self.pending = Some(Pending {
                            danger: Danger {
                                action: "apagar do histórico",
                                title: "Apagar esta regra do histórico?".to_string(),
                                lines: vec![
                                    described,
                                    "Some do histórico compartilhado, em disco, para todas as \
                                     execuções — as regras já aplicadas nesta continuam onde \
                                     estão."
                                        .to_string(),
                                ],
                            },
                            action: PendingAction::ForgetRule(rule),
                        });
                    }
                }
                KeyCode::Esc => editor.mode = RulesMode::List,
                _ => {}
            }
            return;
        }

        match code {
            KeyCode::Up => editor.move_selection(-1),
            KeyCode::Down => editor.move_selection(1),
            KeyCode::PageUp => editor.move_selection(-PAGE_ROWS),
            KeyCode::PageDown => editor.move_selection(PAGE_ROWS),
            KeyCode::Char('a') => editor.edit_new(),
            KeyCode::Char('e') | KeyCode::Enter => editor.edit_selected(),
            KeyCode::Char('h') => editor.open_history(),
            KeyCode::Delete => {
                // Only from this execution. The shared history is deliberately left
                // alone — that's the whole reason it's a separate list.
                if editor.selected < editor.rules.len() {
                    editor.rules.remove(editor.selected);
                    editor.selected = editor.selected.saturating_sub(1);
                }
            }
            KeyCode::Esc => self.close_rules_editor(),
            _ => {}
        }
    }

    /// Closes the rules screen, writing the list back into the field it belongs to.
    fn close_rules_editor(&mut self) {
        let Focus::Wizard(wizard) = &mut self.focus else {
            return;
        };
        let Some(editor) = wizard.editor.take() else {
            return;
        };
        if let Some(field) = wizard.fields.get_mut(editor.field) {
            field.value = rewrite::encode(&editor.rules);
        }
        // Step off the rules field on the way out. Leaving the cursor on it would mean
        // the next Enter reopens the list the user just closed, with no way forward
        // that doesn't look like the form is stuck.
        if editor.field + 1 < wizard.fields.len() {
            wizard.field = editor.field + 1;
        }
        wizard.error = None;
    }

    /// Enter: advance a step, or — on the last one — actually start the execution.
    /// Starting is the only step that can refuse to advance, and it says why.
    pub fn wizard_advance(&mut self) {
        let Focus::Wizard(wizard) = &mut self.focus else {
            return;
        };
        match wizard.step {
            WizardStep::SelectTool => {
                let Some(tool) = self.tools_available.get(wizard.tool) else {
                    return;
                };
                // Rebuilt from the spec on every entry, so backing out to pick a
                // different tool can't leave the previous one's fields behind.
                wizard.fields = tool
                    .params()
                    .into_iter()
                    .map(|spec| ParamField {
                        value: spec.default.to_string(),
                        spec,
                    })
                    .collect();
                wizard.field = 0;
                wizard.error = None;
                wizard.step = WizardStep::Params;
            }
            WizardStep::Params => {
                // A rules field is a list, not a value: Enter on it opens that list
                // rather than moving the wizard along.
                if let Some(field) = wizard.fields.get(wizard.field)
                    && matches!(field.spec.kind, ParamKind::Rules)
                {
                    wizard.editor = Some(RulesEditor::new(wizard.field, &field.value));
                    return;
                }
                wizard.error = None;
                wizard.step = WizardStep::Confirm;
            }
            WizardStep::Confirm => self.start_execution(),
        }
    }

    /// Esc: back up one step, or leave the wizard entirely from the first one. Nothing
    /// has started yet at any point here, so backing out is always safe.
    pub fn wizard_back(&mut self) {
        let Focus::Wizard(wizard) = &mut self.focus else {
            return;
        };
        match wizard.step {
            WizardStep::SelectTool => self.focus = Focus::None,
            // There's no tool-picking step behind an edit to go back to.
            WizardStep::Params if wizard.editing.is_some() => self.focus = Focus::None,
            WizardStep::Params => {
                wizard.error = None;
                wizard.step = WizardStep::SelectTool;
            }
            WizardStep::Confirm => {
                wizard.error = None;
                wizard.step = WizardStep::Params;
            }
        }
    }

    /// Runs the configured tool. On success the wizard closes and the new execution is
    /// selected in the list; on failure the wizard drops back to the form with the
    /// tool's own message, so the user can fix the field that was wrong.
    fn start_execution(&mut self) {
        let Focus::Wizard(wizard) = &mut self.focus else {
            return;
        };
        let Some(tool) = self.tools_available.get(wizard.tool) else {
            return;
        };
        let params: HashMap<&'static str, String> = wizard
            .fields
            .iter()
            .map(|f| (f.spec.key, f.value.trim().to_string()))
            .collect();

        let editing = wizard.editing;
        // Reconfiguring means the old execution has to let go of its port before the
        // new one can ask for it, exactly like a restart — and for the same reason it's
        // a stop, wait, start rather than a swap.
        let replacing = editing.and_then(|id| self.tools.index_of(id));
        let previous = replacing.and_then(|index| {
            let existing = &self.tools.executions[index];
            let saved = existing.spec().cloned();
            existing.stop();
            saved
        });
        if previous.is_some() {
            thread::sleep(RESTART_GRACE);
        }

        // Unlike a restored execution, one being added by hand shouldn't be accepted
        // when it can't start — the user is right there and can fix the field.
        let id = self.tools.take_id();
        match tool.start(id, &params) {
            Ok(execution) => {
                let spec = ExecutionSpec {
                    tool: tool.id().to_string(),
                    params: params
                        .iter()
                        .map(|(key, value)| (key.to_string(), value.clone()))
                        .collect(),
                };
                let execution = execution.with_spec(spec);
                match replacing {
                    Some(index) => {
                        self.tools.executions[index] = execution;
                        self.tools.selected = index;
                    }
                    None => {
                        self.tools.executions.push(execution);
                        self.tools.selected = self.tools.executions.len() - 1;
                    }
                }
                self.tools.persist();
                self.focus = Focus::None;
            }
            Err(message) => {
                // The old one was already stopped to free the port, so a rejected edit
                // would otherwise cost a working execution over a typo. Put it back the
                // way it was and let the wizard say what was wrong.
                if let (Some(index), Some(saved)) = (replacing, previous)
                    && let Some(tool) = self.tools_available.iter().find(|t| t.id() == saved.tool)
                {
                    let values = restore_params(tool.as_ref(), &saved);
                    let restored = self.tools.launch(tool.as_ref(), values);
                    self.tools.executions[index] = restored;
                }
                let Focus::Wizard(wizard) = &mut self.focus else {
                    return;
                };
                wizard.error = Some(message);
                wizard.step = WizardStep::Params;
            }
        }
    }

    /// The tool that owns an execution, found by the stable id its configuration
    /// carries — not by display name, which is free to change.
    pub fn tool_for(&self, execution: &Execution) -> Option<&dyn Tool> {
        let spec = execution.spec()?;
        self.tools_available
            .iter()
            .find(|tool| tool.id() == spec.tool)
            .map(|tool| tool.as_ref())
    }

    /// The parameters an execution was started with, in the shape a tool expects.
    fn params_of(
        &self,
        execution: &Execution,
    ) -> Option<(&dyn Tool, HashMap<&'static str, String>)> {
        let tool = self.tool_for(execution)?;
        let spec = execution.spec()?;
        Some((tool, restore_params(tool, spec)))
    }

    /// Opens the wizard on an execution that already exists ('e'), pre-filled with what
    /// it was started with.
    pub fn edit_selected_execution(&mut self) {
        let Some(existing) = self.tools.selected() else {
            return;
        };
        let (id, Some(saved)) = (existing.id, existing.spec().cloned()) else {
            return;
        };
        let Some(index) = self
            .tools_available
            .iter()
            .position(|t| t.id() == saved.tool)
        else {
            return;
        };
        let tool = &self.tools_available[index];
        let fields = tool
            .params()
            .into_iter()
            .map(|spec| ParamField {
                value: saved
                    .params
                    .get(spec.key)
                    .cloned()
                    .unwrap_or_else(|| spec.default.to_string()),
                spec,
            })
            .collect();
        self.focus = Focus::Wizard(ToolWizard {
            // Straight to the form: the tool of an existing execution isn't in question.
            step: WizardStep::Params,
            tool: index,
            fields,
            field: 0,
            error: None,
            editor: None,
            editing: Some(id),
        });
    }

    /// Restarts the selected execution from its saved configuration ('r'). The point
    /// is the one that failed to come back on startup — its port was busy at boot and
    /// is free now — but it doubles as a way to bounce a live one.
    pub fn restart_selected_execution(&mut self) {
        let index = self.tools.selected;
        let Some(existing) = self.tools.executions.get(index) else {
            return;
        };
        // Nothing to recreate for an on-demand execution — it holds no threads and no
        // port. 'r' there means "do it again", against the same target.
        if let Some((tool, params)) = self.params_of(existing)
            && tool.on_demand(&params)
        {
            tool.rerun(existing, &params);
            return;
        }
        let Some(saved) = existing.spec().cloned() else {
            return;
        };
        // The old one has to let go of its port before the new one can take it, and its
        // threads only notice the stop flag on their next poll — so this is a
        // stop, wait, start, not an atomic swap. It blocks the UI for that beat, which
        // is acceptable for an explicit keypress and honest about what's happening.
        existing.stop();
        thread::sleep(RESTART_GRACE);

        let Some(tool) = self.tools_available.iter().find(|t| t.id() == saved.tool) else {
            return;
        };
        let values = restore_params(tool.as_ref(), &saved);
        let replacement = self.tools.launch(tool.as_ref(), values);
        self.tools.executions[index] = replacement;
        self.tools.persist();
    }

    /// Stops and forgets the selected execution (Del). The threads wind down on their
    /// own within a poll interval; nothing here waits for them, so the UI never stalls
    /// behind a socket.
    /// `Del` on the Ferramentas tab. Stopping a tunnel drops whatever is connected
    /// through it and throws away its log, and the row doesn't come back on the next
    /// launch — so this asks first, naming what it is.
    pub fn request_remove_execution(&mut self) {
        let Some(execution) = self.tools.executions.get(self.tools.selected) else {
            return;
        };
        let mut lines = vec![format!("{} — {}", execution.tool, execution.summary)];
        if matches!(execution.state(), State::Running) {
            lines.push("Está rodando agora: para na hora.".to_string());
        }
        // Only said when it's true: a tunnel with people connected through it is a very
        // different loss from a probe that is merely between measurements, and a
        // warning that cries wolf on every row stops being read.
        let open = execution.stats.active.load(Ordering::Relaxed);
        if open > 0 {
            lines.push(format!(
                "{open} conexão(ões) aberta(s) através dela caem junto."
            ));
        }
        if execution.chart_monitor().is_some() {
            lines.push(
                "O gráfico dele sai da Visão geral. A linha fica guardada enquanto o monitorzinho estiver aberto, então recriar a mesma medição continua de onde parou."
                    .to_string(),
            );
        }
        lines.push(
            "O log gravado até aqui é descartado, e a execução não volta no próximo início."
                .to_string(),
        );
        self.pending = Some(Pending {
            danger: Danger {
                action: "remover execução",
                title: "Remover esta execução?".to_string(),
                lines,
            },
            action: PendingAction::RemoveExecution,
        });
    }

    fn remove_selected_execution(&mut self) {
        if self.tools.selected >= self.tools.executions.len() {
            return;
        }
        let execution = self.tools.executions.remove(self.tools.selected);
        execution.stop();
        self.tools.selected = self
            .tools
            .selected
            .min(self.tools.executions.len().saturating_sub(1));
        self.tools.persist();
    }

    /// Opens the live log of the selected execution (Enter).
    pub fn open_tool_monitor(&mut self) {
        let Some(execution) = self.tools.selected() else {
            return;
        };
        // Opening is the trigger for a tool that only works on demand — the scan starts
        // here, on the keypress, rather than at launch behind the user's back.
        if let Some((tool, params)) = self.params_of(execution) {
            tool.open(execution, &params);
        }
        let Some(execution) = self.tools.selected() else {
            return;
        };
        self.focus = Focus::ToolMonitor(ToolMonitorFocus {
            execution_id: execution.id,
            query: String::new(),
            only_matches: false,
            hex: false,
            scroll: Cell::new(0),
            follow: true,
            max_scroll: Cell::new(0),
            handoff: None,
            matches: RefCell::new(Vec::new()),
            match_index: Cell::new(None),
            anchor_seq: Cell::new(0),
            anchor_offset: Cell::new(0),
        });
    }

    /// Where the picker lives for whichever view is open. Two surfaces offer the
    /// gesture — an execution's log, and a table row's detail — and everything below
    /// works the same on both.
    fn handoff_slot(&mut self) -> Option<&mut Option<HandoffPicker>> {
        match &mut self.focus {
            Focus::ToolMonitor(monitor) => Some(&mut monitor.handoff),
            Focus::Detail(detail) => Some(&mut detail.handoff),
            _ => None,
        }
    }

    /// Offers what the open view found as new executions (Ctrl+P). Silent when there's
    /// nothing another tool could be pointed at.
    pub fn open_handoffs(&mut self) {
        // Each detail names its own picker: what a connection offers (a tunnel to
        // either end) and what a listening port offers are different gestures.
        let title = match &self.focus {
            Focus::Detail(detail) => detail.detail.handoff_title,
            _ => "Achados desta execução",
        };
        let options = match &self.focus {
            Focus::ToolMonitor(monitor) => self
                .tools
                .by_id(monitor.execution_id)
                .and_then(|execution| {
                    self.tool_for(execution)
                        .map(|tool| tool.handoffs(execution))
                })
                .unwrap_or_default(),
            // A detail already holds everything the offered execution needs — a
            // connection names both ends and the protocol, a port names its service.
            Focus::Detail(detail) => detail
                .detail
                .handoffs
                .iter()
                .map(|offer| Handoff {
                    label: offer.label.clone(),
                    tool: offer.tool,
                    params: offer.params.clone(),
                })
                .collect(),
            _ => Vec::new(),
        };
        if options.is_empty() {
            return;
        }
        if let Some(slot) = self.handoff_slot() {
            *slot = Some(HandoffPicker::new(title, options));
        }
    }

    /// True while a destructive action is waiting to be confirmed. Checked before every
    /// other handler, including the rules screen's — the box sits over all of them.
    pub fn confirm_open(&self) -> bool {
        self.pending.is_some()
    }

    /// Enter goes through with it, Esc calls it off, and every other key is ignored
    /// rather than taken as an answer — a confirmation that any keypress can satisfy is
    /// not a confirmation.
    pub fn confirm_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Enter => {
                let Some(pending) = self.pending.take() else {
                    return;
                };
                match pending.action {
                    PendingAction::KillRow => self.kill_selected(),
                    PendingAction::RemoveExecution => self.remove_selected_execution(),
                    PendingAction::ForgetRule(rule) => {
                        rewrite::forget(&rule);
                        if let Focus::Wizard(wizard) = &mut self.focus
                            && let Some(editor) = &mut wizard.editor
                        {
                            editor.open_history();
                        }
                    }
                }
            }
            KeyCode::Esc => self.pending = None,
            _ => {}
        }
    }

    /// True while the picker is open, so keys go there rather than to whatever is
    /// underneath — a log's search box would otherwise swallow every letter.
    pub fn handoff_open(&self) -> bool {
        match &self.focus {
            Focus::ToolMonitor(monitor) => monitor.handoff.is_some(),
            Focus::Detail(detail) => detail.handoff.is_some(),
            _ => false,
        }
    }

    pub fn handoff_key(&mut self, code: KeyCode) {
        let Some(slot) = self.handoff_slot() else {
            return;
        };
        let Some(picker) = slot else {
            return;
        };
        match code {
            // While searching, the arrows step between hits — the list is still all
            // there, and PgUp/PgDn stay the way through it row by row.
            KeyCode::Up if !picker.query.is_empty() => picker.jump_match(-1),
            KeyCode::Down if !picker.query.is_empty() => picker.jump_match(1),
            KeyCode::Up => picker.move_selection(-1),
            KeyCode::Down => picker.move_selection(1),
            KeyCode::PageUp => picker.move_selection(-PAGE_ROWS),
            KeyCode::PageDown => picker.move_selection(PAGE_ROWS),
            KeyCode::Enter => self.create_from_handoff(),
            KeyCode::Backspace => {
                picker.query.pop();
                let from = picker.selected;
                picker.focus_match_from(from);
            }
            // Esc drops the search before it drops the picker, same as everywhere else
            // — one key, one level at a time.
            KeyCode::Esc if !picker.query.is_empty() => picker.query.clear(),
            KeyCode::Esc => *slot = None,
            // Typing searches straight away; there's no mode to enter first, and the
            // picker has no other use for letters.
            KeyCode::Char(c) => {
                picker.query.push(c);
                let from = picker.selected;
                picker.focus_match_from(from);
            }
            _ => {}
        }
    }

    /// Builds the offered execution and starts it, leaving the user looking at the list
    /// with the new row selected — the thing they asked for is the thing they should be
    /// looking at.
    fn create_from_handoff(&mut self) {
        let Some(slot) = self.handoff_slot() else {
            return;
        };
        let Some(picker) = slot else {
            return;
        };
        // The bulk row creates one execution per finding — every finding, or, with a
        // search running, the ones it matches, which is what its label promises at that
        // moment. Any other row creates the one it names. Both go through the same path.
        let chosen: Vec<Handoff> = match picker.at(picker.selected) {
            Some(handoff) => vec![Handoff {
                label: handoff.label.clone(),
                tool: handoff.tool,
                params: handoff.params.clone(),
            }],
            None => picker
                .matching()
                .into_iter()
                .map(|handoff| Handoff {
                    label: handoff.label.clone(),
                    tool: handoff.tool,
                    params: handoff.params.clone(),
                })
                .collect(),
        };
        if chosen.is_empty() {
            return;
        }

        for handoff in &chosen {
            let Some(index) = self
                .tools_available
                .iter()
                .position(|tool| tool.id() == handoff.tool)
            else {
                continue;
            };
            let tool = &self.tools_available[index];
            // Defaults first, then whatever the offer named — so a tool that grows a
            // parameter later doesn't leave it empty here.
            let mut values: HashMap<&'static str, String> = tool
                .params()
                .into_iter()
                .map(|spec| (spec.key, spec.default.to_string()))
                .collect();
            for (key, value) in &handoff.params {
                values.insert(key, value.clone());
            }
            let execution = self.tools.launch(tool.as_ref(), values);
            self.tools.executions.push(execution);
        }
        self.tools.selected = self.tools.executions.len().saturating_sub(1);
        self.tools.persist();
        // Land on what was just created rather than on the view it came from — getting
        // to the new execution is the point of the gesture.
        self.focus = Focus::None;
        self.switch_tab(Tab::Tools);
        // An offer that can't run as it stands opens its form instead of sitting there
        // as a dead row: everything the offer carried is already in the fields, and what
        // is missing is exactly what only the user can say — repeating a request a
        // receiver caught, for instance, needs somewhere to send it, and no finding can
        // know where. Only for a single offer: a bulk creation has no one form to open.
        if chosen.len() == 1
            && self
                .tools
                .selected()
                .is_some_and(|execution| execution.failed_to_start())
        {
            self.edit_selected_execution();
        }
    }

    /// ↑/↓ in the monitor. With a search active these step between hits instead of
    /// between lines — in a log you're searching, jumping is the whole point, and it's
    /// what the fullscreen tables already do with the same keys.
    pub fn tool_monitor_scroll(&mut self, delta: i32) {
        if let Focus::ToolMonitor(monitor) = &mut self.focus {
            if !monitor.query.is_empty() {
                monitor.jump_match(delta);
                return;
            }
            let limit = monitor.max_scroll.get() as i32;
            let next = (monitor.scroll.get() as i32 + delta).clamp(0, limit) as u16;
            monitor.move_to(next);
            // Oldest-first, so the live edge is the bottom: following means being there.
            monitor.follow = next as i32 == limit;
        }
    }

    /// Jumps back to the newest event and resumes following it (End).
    pub fn tool_monitor_follow(&mut self) {
        if let Focus::ToolMonitor(monitor) = &mut self.focus {
            monitor.follow = true;
            monitor.move_to(monitor.max_scroll.get());
            monitor.match_index.set(None);
        }
    }

    /// Throws away what this execution has logged so far (Ctrl+L).
    ///
    /// Counters are left alone: they describe the execution's whole life, while the log
    /// is a scrollback, and someone clearing it wants a clean surface to watch the next
    /// request on — not their traffic totals reset.
    pub fn tool_monitor_clear(&mut self) {
        let Focus::ToolMonitor(monitor) = &mut self.focus else {
            return;
        };
        let Some(execution) = self.tools.by_id(monitor.execution_id) else {
            return;
        };
        let mut log = tools::lock_log(&execution.log);
        log.clear();
        log.note(execution.started.elapsed(), "log limpo".to_string());
        drop(log);
        monitor.scroll.set(0);
        monitor.follow = true;
        monitor.anchor_seq.set(0);
        monitor.anchor_offset.set(0);
        monitor.match_index.set(None);
    }

    pub fn tool_monitor_toggle_hex(&mut self) {
        if let Focus::ToolMonitor(monitor) = &mut self.focus {
            monitor.hex = !monitor.hex;
        }
    }

    /// Switches between highlighting matches in place and hiding everything else.
    pub fn tool_monitor_toggle_filter(&mut self) {
        if let Focus::ToolMonitor(monitor) = &mut self.focus {
            monitor.only_matches = !monitor.only_matches;
        }
    }

    pub fn tool_monitor_type(&mut self, c: char) {
        if let Focus::ToolMonitor(monitor) = &mut self.focus {
            monitor.query.push(c);
            monitor.reset_search();
        }
    }

    pub fn tool_monitor_backspace(&mut self) {
        if let Focus::ToolMonitor(monitor) = &mut self.focus {
            monitor.query.pop();
            monitor.reset_search();
        }
    }

    /// Esc in the monitor: drop the search first, leave only once there's none.
    pub fn tool_monitor_escape(&mut self) {
        if let Focus::ToolMonitor(monitor) = &mut self.focus {
            if monitor.query.is_empty() {
                self.focus = Focus::None;
            } else {
                monitor.query.clear();
                monitor.only_matches = false;
                monitor.reset_search();
            }
        }
    }

    pub fn persist(&self) {
        let mut map = history::HistoryMap::new();
        for panel in &self.charts {
            map.insert(panel.monitor.id().to_string(), panel.history.values());
        }
        history::save_all(&map);
    }
}
