use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, Paragraph, RenderDirection, Row as UiRow, Sparkline, Table,
    TableState,
};

use crate::app::{
    App, DetailFocus, Focus, MATCH_CONTEXT, ParamField, RulesEditor, RulesMode, ShortcutTarget,
    Tab, TableFocus, ToolMonitorFocus, ToolWizard, WizardStep,
};
use crate::format;
use crate::history::History;
use crate::monitor::{Detail, Monitor, TableRow};
use crate::tools::rewrite::{self, Rule};
use crate::tools::{Direction as Flow, EventKind, Execution, ParamKind, lock_log};

const TAB_BAR_HEIGHT: u16 = 2;
const COLS: usize = 3;
/// Height of the two throughput sparklines at the top of a detail view — one line of
/// chart between its borders, which is all a rate needs to show its shape.
const RATE_PANEL_HEIGHT: u16 = 3;
/// Widest a detail's label column is allowed to get before values stop being aligned
/// against it. Sized to fit the longest label any monitor currently produces.
const MAX_LABEL_WIDTH: usize = 26;
/// Floor for the value column, so a very narrow terminal wraps hard instead of
/// computing a zero-width column and looping forever.
const MIN_VALUE_WIDTH: usize = 8;

/// A muted, low-saturation palette (One Dark-inspired) instead of the harsh basic
/// ANSI 16 colors — easier on the eyes and still clearly distinguishable per group.
mod palette {
    use ratatui::style::Color;

    pub const BLUE: Color = Color::Rgb(0x61, 0xAF, 0xEF);
    pub const PURPLE: Color = Color::Rgb(0xC6, 0x78, 0xDD);
    pub const GREEN: Color = Color::Rgb(0x98, 0xC3, 0x79);
    pub const CYAN: Color = Color::Rgb(0x56, 0xB6, 0xC2);
    pub const ORANGE: Color = Color::Rgb(0xD1, 0x9A, 0x66);
    pub const YELLOW: Color = Color::Rgb(0xE5, 0xC0, 0x7B);
    pub const RED: Color = Color::Rgb(0xE0, 0x6C, 0x75);
    /// Muted but still legible on a dark terminal — GitHub Dark's secondary-text gray.
    /// The original One Dark comment gray (`#5C6370`) was tuned for that theme's own
    /// background and read as barely-visible on plainer dark terminals.
    pub const DIM: Color = Color::Rgb(0x8B, 0x94, 0x9E);
}

/// Builds the shortcut-key badges shown per panel: which chart/table index maps to
/// which key, mirroring `App::shortcut_targets()`'s order (see `app::shortcut_key`).
struct ShortcutMap {
    chart: HashMap<usize, char>,
    table: HashMap<usize, char>,
}

impl ShortcutMap {
    fn build(app: &App) -> Self {
        let mut chart = HashMap::new();
        let mut table = HashMap::new();
        for (i, target) in app.shortcut_targets().into_iter().enumerate() {
            let Some(key) = crate::app::shortcut_key(i) else {
                continue;
            };
            match target {
                ShortcutTarget::Chart(idx) => {
                    chart.insert(idx, key);
                }
                ShortcutTarget::Table(idx) => {
                    table.insert(idx, key);
                }
            }
        }
        Self { chart, table }
    }
}

/// Right-aligned top-border badge showing a panel's shortcut key, e.g. `[3]` or `[a]`.
fn shortcut_badge(key: Option<char>) -> Option<Line<'static>> {
    key.map(|k| {
        Line::styled(
            format!(" [{}] ", k),
            Style::default()
                .fg(palette::DIM)
                .add_modifier(Modifier::BOLD),
        )
        .right_aligned()
    })
}

/// Bottom-border hint shown only in fullscreen (e.g. "Esc/q voltar").
fn hint_line(text: &str) -> Line<'static> {
    Line::styled(format!(" {} ", text), Style::default().fg(palette::DIM)).right_aligned()
}

pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();

    match &app.focus {
        Focus::Chart(idx) => {
            render_panel(
                frame,
                area,
                app.monitors[*idx].as_ref(),
                &app.histories[*idx],
                app.extras[*idx].as_deref(),
                app.capacities[*idx],
                PanelChrome {
                    shortcut: None,
                    hint: Some("Esc/q voltar"),
                },
            );
            return;
        }
        Focus::Table(table_focus) => {
            let monitor = app.table_monitors[table_focus.table_index].as_ref();
            render_fullscreen_table(
                frame,
                area,
                monitor.title(),
                monitor.headers(),
                monitor.has_detail(),
                table_focus,
            );
            return;
        }
        Focus::Detail(detail_focus) => {
            render_detail(frame, area, detail_focus);
            return;
        }
        Focus::Wizard(wizard) => {
            render_wizard(frame, area, app, wizard);
            return;
        }
        Focus::ToolMonitor(monitor) => {
            render_tool_monitor(frame, area, app, monitor);
            return;
        }
        Focus::None => {}
    }

    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(TAB_BAR_HEIGHT), Constraint::Min(0)])
        .split(area);
    render_tab_bar(frame, sections[0], app);

    match app.tab {
        Tab::Overview => render_overview_tab(frame, sections[1], app),
        Tab::Processes => render_processes_tab(frame, sections[1], app),
        Tab::Tools => render_tools_tab(frame, sections[1], app),
    }
}

/// Slim header showing the two tabs (the active one highlighted), plus the app
/// version and switch-tab hint on the right — always visible, on either tab.
fn render_tab_bar(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(Style::default().fg(palette::DIM));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut spans = vec![Span::raw(" ")];
    for (i, tab) in Tab::ALL.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("   "));
        }
        let style = if *tab == app.tab {
            Style::default()
                .fg(palette::CYAN)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            Style::default().fg(palette::DIM)
        };
        spans.push(Span::styled(format!(" {} ", tab.title()), style));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), inner);

    // With one Ctrl+C already pressed, the corner says what the second one does —
    // otherwise it's the usual version/tab hint.
    let hint = if app.quit_armed {
        Paragraph::new(Line::styled(
            " Ctrl+C de novo para sair · qualquer outra tecla cancela ",
            Style::default()
                .fg(palette::YELLOW)
                .add_modifier(Modifier::BOLD),
        ))
    } else {
        Paragraph::new(Line::styled(
            format!(
                " v{} · Tab/Shift+Tab alternar aba · Ctrl+C 2x sair ",
                env!("CARGO_PKG_VERSION")
            ),
            Style::default().fg(palette::DIM),
        ))
    }
    .alignment(Alignment::Right);
    frame.render_widget(hint, inner);
}

/// Overview tab: just the sparkline grid, grouped by `Monitor::group()` (e.g. Disk
/// occupancy sits with its read/write throughput panels).
fn render_overview_tab(frame: &mut Frame, area: Rect, app: &App) {
    let shortcuts = ShortcutMap::build(app);
    render_charts(frame, area, app, &shortcuts);
}

/// Processes tab: Ports and Connections side by side on top (both are per-socket
/// listings, easiest to compare against each other), Top CPU and Top Memory stacked
/// full-width below, then SSH Sessions and System Info side by side at the bottom (who's
/// connected next to what they're connected to) — matches
/// `monitor::all_table_monitors()`'s order.
fn render_processes_tab(frame: &mut Frame, area: Rect, app: &App) {
    if app.table_monitors.len() < 6 {
        return;
    }

    let shortcuts = ShortcutMap::build(app);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Fill(1),
            Constraint::Fill(1),
            Constraint::Fill(1),
            Constraint::Fill(1),
        ])
        .split(area);
    let top_row = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Ratio(1, 2), Constraint::Ratio(1, 2)])
        .split(rows[0]);
    let bottom_row = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Ratio(1, 2), Constraint::Ratio(1, 2)])
        .split(rows[3]);

    let panels = [
        top_row[0],
        top_row[1],
        rows[1],
        rows[2],
        bottom_row[0],
        bottom_row[1],
    ];
    for (i, &panel_area) in panels.iter().enumerate() {
        let monitor = app.table_monitors[i].as_ref();
        render_table_panel(
            frame,
            panel_area,
            monitor.title(),
            monitor.headers(),
            &app.table_rows[i],
            shortcuts.table.get(&i).copied(),
        );
    }
}

/// Color used to visually tell monitor groups apart at a glance.
fn group_color(group: &str) -> Color {
    match group {
        "System" => palette::BLUE,
        "Disk" => palette::PURPLE,
        "Network" => palette::GREEN,
        "GPU" => palette::ORANGE,
        _ => palette::DIM,
    }
}

/// Escalates past the group color when a monitor has a limit and is approaching it.
/// Monitors without a limit (byte rates, etc.) always keep their group color.
fn signal_color(monitor: &dyn Monitor, value: f64) -> Color {
    let base = group_color(monitor.group());
    match monitor.limit() {
        Some(limit) if limit > 0.0 => {
            let ratio = value / limit;
            if ratio >= 0.9 {
                palette::RED
            } else if ratio >= 0.7 {
                palette::YELLOW
            } else {
                base
            }
        }
        _ => base,
    }
}

/// Packs `App::chart_monitor_order()` (already grouped so related panels like Net
/// down/up stay together) into a strict grid of `COLS` panels per row — a row is
/// topped up with the next group's panels instead of leaving a gap.
fn build_chart_rows(app: &App) -> Vec<Vec<usize>> {
    app.chart_monitor_order()
        .chunks(COLS)
        .map(<[usize]>::to_vec)
        .collect()
}

fn render_charts(frame: &mut Frame, area: Rect, app: &App, shortcuts: &ShortcutMap) {
    let rows = build_chart_rows(app);
    if rows.is_empty() {
        return;
    }

    let constraints: Vec<Constraint> = rows.iter().map(|_| Constraint::Fill(1)).collect();
    let row_areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    for (indices, &row_area) in rows.iter().zip(row_areas.iter()) {
        // Always split into a fixed `COLS`-wide grid — even a row with fewer panels
        // than `COLS` gets full-width column slots, leaving the rest blank, so the
        // grid reads as a consistent 3-column layout instead of stretching to fill.
        let col_constraints: Vec<Constraint> = (0..COLS)
            .map(|_| Constraint::Ratio(1, COLS as u32))
            .collect();
        let col_areas = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(col_constraints)
            .split(row_area);

        for (c, &idx) in indices.iter().enumerate() {
            render_panel(
                frame,
                col_areas[c],
                app.monitors[idx].as_ref(),
                &app.histories[idx],
                app.extras[idx].as_deref(),
                app.capacities[idx],
                PanelChrome {
                    shortcut: shortcuts.chart.get(&idx).copied(),
                    hint: None,
                },
            );
        }
    }
}

/// Panel-header extras that only apply outside the plain overview grid: a shortcut-key
/// badge, and/or a fullscreen-only footer hint.
#[derive(Default, Clone, Copy)]
struct PanelChrome<'a> {
    shortcut: Option<char>,
    hint: Option<&'a str>,
}

fn render_panel(
    frame: &mut Frame,
    area: Rect,
    monitor: &dyn Monitor,
    history: &History,
    extra: Option<&str>,
    capacity: Option<f64>,
    chrome: PanelChrome,
) {
    let last = history.last().unwrap_or(0.0);
    let color = signal_color(monitor, last);

    let title = match history.last() {
        Some(last) => {
            let peak = history
                .max()
                .map(|m| match capacity {
                    // `m` is a percentage of `capacity` here (only metrics measured
                    // against a fixed capacity, like Memory, set `capacity()`).
                    Some(cap) => format!(
                        " · máx {} ({})",
                        monitor.format(m),
                        format::human_bytes(m / 100.0 * cap)
                    ),
                    None => format!(" · máx {}", monitor.format(m)),
                })
                .unwrap_or_default();
            match extra {
                Some(extra) => format!(
                    " {} — {} ({}){} ",
                    monitor.title(),
                    monitor.format(last),
                    extra,
                    peak
                ),
                None => format!(" {} — {}{} ", monitor.title(), monitor.format(last), peak),
            }
        }
        None => format!(" {} ", monitor.title()),
    };

    let mut block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(color));
    if let Some(badge) = shortcut_badge(chrome.shortcut) {
        block = block.title_top(badge);
    }
    if let Some(text) = chrome.hint {
        block = block.title_bottom(hint_line(text));
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // `Sparkline` always draws its data starting at the render-direction's leading
    // edge, taking however many of the *first* items in the slice fit — it never skips
    // ahead to show the tail. So to keep the newest sample pinned to the right edge
    // (and only ever show blank space on the *left*, before history began, rather than
    // on the right when the panel is wider than our retained history) we reverse the
    // window to newest-first and render right-to-left.
    let data: Vec<u64> = history
        .values()
        .iter()
        .rev()
        .take(inner.width as usize)
        .map(|v| v.round().max(0.0) as u64)
        .collect();
    // Scale bars against the metric's real ceiling (e.g. 100%) rather than the max of
    // the current window — otherwise a value that's merely the local max renders as a
    // full bar even when it's nowhere near the actual limit.
    let mut sparkline = Sparkline::default()
        .data(&data)
        .direction(RenderDirection::RightToLeft)
        .style(Style::default().fg(color));
    if let Some(limit) = monitor.limit() {
        sparkline = sparkline.max(limit.round().max(0.0) as u64);
    }
    frame.render_widget(sparkline, inner);
}

/// Column widths for a table panel, keyed off its exact headers: the process tables
/// (Command/Time/Usage) want Command to take the leftover space with Time/Usage
/// fixed-width, while the ports table (Proto/Port/Process) wants Proto/Port fixed and
/// Process filling the rest.
fn table_col_widths(headers: &[&str]) -> Vec<Constraint> {
    match headers {
        ["Proto", "Port", "Process", "Age"] => vec![
            Constraint::Length(5),
            Constraint::Length(7),
            Constraint::Fill(1),
            Constraint::Length(8),
        ],
        ["Proto", "Process", "Connection", "Age", "Traffic", "Rate"] => vec![
            Constraint::Length(5),
            Constraint::Fill(2),
            Constraint::Fill(1),
            Constraint::Length(8),
            Constraint::Length(9),
            Constraint::Length(22),
        ],
        ["Field", "Value"] => vec![Constraint::Length(10), Constraint::Fill(1)],
        ["User", "Host", "TTY", "Time", "Folder", "Command"] => vec![
            Constraint::Length(10),
            Constraint::Length(16),
            Constraint::Length(9),
            Constraint::Length(8),
            Constraint::Fill(1),
            Constraint::Fill(1),
        ],
        _ => vec![
            Constraint::Fill(1),
            Constraint::Length(8),
            Constraint::Length(10),
        ],
    }
}

/// Builds a `TableRow`'s tree indentation prefix — per-ancestor-level guide bars, this
/// row's own branch connector, and an expand/collapse marker — then prepends it to a
/// rendering-only copy of the Command cell. Whether the row is "expanded" is inferred
/// from `next` (the row right after it in whatever list is actually being rendered)
/// rather than tracked separately: if `next` is one of this row's children (one level
/// deeper), its children are visibly present, so it reads as expanded. Rows with no
/// children (`child_count == 0`, e.g. every Ports row) get a blank marker and render
/// exactly as before.
fn tree_row(row: &TableRow, next: Option<&TableRow>) -> UiRow<'static> {
    let mut prefix = String::new();
    for &last_sibling in &row.guides {
        prefix.push_str(if last_sibling { "   " } else { "│  " });
    }
    if row.depth > 0 {
        prefix.push_str(if row.is_last_sibling {
            "└─"
        } else {
            "├─"
        });
    }
    prefix.push(if row.child_count == 0 {
        ' '
    } else if next.is_some_and(|n| n.depth > row.depth) {
        '▾'
    } else {
        '▸'
    });
    prefix.push(' ');

    let mut cells = row.cells.clone();
    if let Some(first) = cells.first_mut() {
        *first = format!("{}{}", prefix, first);
    }
    UiRow::new(cells)
}

fn render_table_panel(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    headers: &[&str],
    rows: &[TableRow],
    shortcut: Option<char>,
) {
    let mut block = Block::default()
        .title(format!(" {} ", title))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(palette::CYAN));
    if let Some(badge) = shortcut_badge(shortcut) {
        block = block.title_top(badge);
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let header = UiRow::new(headers.iter().map(|h| Cell::from(*h)).collect::<Vec<_>>())
        .style(Style::default().add_modifier(Modifier::BOLD));
    let body: Vec<UiRow> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| tree_row(r, rows.get(i + 1)))
        .collect();

    let table = Table::new(body, table_col_widths(headers)).header(header);
    frame.render_widget(table, inner);
}

/// Fullscreen view of a frozen table: same columns as the overview panel, plus a
/// selectable/highlighted row and a footer hint for navigate/kill/back.
fn render_fullscreen_table(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    headers: &[&str],
    has_detail: bool,
    table_focus: &TableFocus,
) {
    let title = if table_focus.query.is_empty() {
        format!(" {} ", title)
    } else {
        format!(" {} — busca: {} ", title, table_focus.query)
    };
    let mut block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(palette::CYAN));
    let hint = if table_focus.query.is_empty() {
        "↑/↓ navegar · ←/→ colapsar/expandir · Del matar (SIGKILL, com filhos) · digite p/ buscar · Esc voltar"
    } else {
        "↑/↓ ir ao próximo/anterior resultado · ←/→ colapsar/expandir · Del matar (SIGKILL, com filhos) · Esc limpar busca"
    };
    // Advertised only where Enter actually leads somewhere — see `TableMonitor::has_detail`.
    let hint = if has_detail {
        format!("Enter detalhar · {hint}")
    } else {
        hint.to_string()
    };
    block = block.title_bottom(hint_line(&hint));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let header = UiRow::new(headers.iter().map(|h| Cell::from(*h)).collect::<Vec<_>>())
        .style(Style::default().add_modifier(Modifier::BOLD));
    let visible = table_focus.visible_indices();
    // Peek at the *next visible* row, not the next row in the underlying full list —
    // a collapsed node's real children are still present right after it in the full,
    // unfiltered tree, which would otherwise make every collapsible row look expanded.
    let body: Vec<UiRow> = visible
        .iter()
        .enumerate()
        .map(|(vi, &i)| {
            let next = visible.get(vi + 1).map(|&ni| &table_focus.rows[ni]);
            tree_row(&table_focus.rows[i], next)
        })
        .collect();

    let table = Table::new(body, table_col_widths(headers))
        .header(header)
        .row_highlight_style(
            Style::default()
                .fg(palette::CYAN)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),
        )
        .highlight_symbol("▶ ");
    let mut state = TableState::default().with_selected(Some(table_focus.selected));
    frame.render_stateful_widget(table, inner, &mut state);
}

/// Greedy word wrap to `width` columns. A single token longer than the whole width (a
/// deep path, a URL in a command line) is hard-split rather than left to overflow.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_len = 0;
    for word in text.split_whitespace() {
        let word_len = word.chars().count();
        if word_len > width {
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
                current_len = 0;
            }
            let chars: Vec<char> = word.chars().collect();
            for chunk in chars.chunks(width) {
                lines.push(chunk.iter().collect());
            }
            continue;
        }
        // +1 for the space this word would need after an existing one.
        if current_len + word_len + usize::from(!current.is_empty()) > width {
            lines.push(std::mem::take(&mut current));
            current_len = 0;
        }
        if !current.is_empty() {
            current.push(' ');
            current_len += 1;
        }
        current.push_str(word);
        current_len += word_len;
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Flattens a `Detail` into renderable lines: a highlighted header per section, then
/// one `label   value` line per field with every value starting at the same column and
/// long ones wrapped underneath it, so the whole thing reads as two columns however
/// much any one value overruns.
fn detail_lines(detail: &Detail, width: usize) -> Vec<Line<'static>> {
    let label_width = detail
        .sections
        .iter()
        .flat_map(|s| s.fields.iter())
        .map(|(label, _)| label.chars().count())
        .max()
        .unwrap_or(0)
        .min(MAX_LABEL_WIDTH);
    // Two leading spaces of indent, two separating the columns.
    let value_col = 2 + label_width + 2;
    let value_width = width.saturating_sub(value_col).max(MIN_VALUE_WIDTH);

    let mut lines = Vec::new();
    for section in &detail.sections {
        if !lines.is_empty() {
            lines.push(Line::raw(""));
        }
        lines.push(Line::styled(
            section.title.to_string(),
            Style::default()
                .fg(palette::CYAN)
                .add_modifier(Modifier::BOLD),
        ));
        for (label, value) in &section.fields {
            for (i, chunk) in wrap(value, value_width).into_iter().enumerate() {
                let head = if i == 0 {
                    format!("  {:width$}  ", label, width = label_width)
                } else {
                    " ".repeat(value_col)
                };
                lines.push(Line::from(vec![
                    Span::styled(head, Style::default().fg(palette::DIM)),
                    Span::raw(chunk),
                ]));
            }
        }
    }
    lines
}

/// One of the two throughput sparklines above a detail. Unlike the chart panels these
/// are scaled to the window's own peak (a connection has no natural ceiling), so the
/// shape is what to read here, not the bar height against a limit.
fn render_rate(
    frame: &mut Frame,
    area: Rect,
    label: &str,
    value: f64,
    history: &History,
    color: Color,
) {
    let title = format!(
        " {} — {} · máx {} ",
        label,
        format::human_bytes_per_sec(value),
        format::human_bytes_per_sec(history.max().unwrap_or(0.0))
    );
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(color));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Newest-first + right-to-left, same as `render_panel`, so the live edge stays
    // pinned to the right and the blank space sits before history began.
    let data: Vec<u64> = history
        .values()
        .iter()
        .rev()
        .take(inner.width as usize)
        .map(|v| v.round().max(0.0) as u64)
        .collect();
    frame.render_widget(
        Sparkline::default()
            .data(&data)
            .direction(RenderDirection::RightToLeft)
            .style(Style::default().fg(color)),
        inner,
    );
}

/// Fullscreen detail for one selected row (Enter): this single connection's throughput
/// sparklined on top, and everything its monitor could tell us about it listed below.
fn render_detail(frame: &mut Frame, area: Rect, focus: &DetailFocus) {
    // Dimmed and labelled once the subject is gone, rather than emptied — the last
    // known state of a connection that just closed is usually the interesting one.
    let (title, color) = if focus.gone {
        (
            format!(" {} — encerrada ", focus.detail.title),
            palette::DIM,
        )
    } else {
        (format!(" {} ", focus.detail.title), palette::CYAN)
    };
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(color))
        .title_bottom(hint_line(
            "↑/↓ rolar · PgUp/PgDn rolar rápido · Esc voltar à lista",
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let body = match focus.detail.rates {
        Some((down, up)) => {
            let split = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(RATE_PANEL_HEIGHT), Constraint::Min(0)])
                .split(inner);
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Ratio(1, 2), Constraint::Ratio(1, 2)])
                .split(split[0]);
            render_rate(
                frame,
                cols[0],
                "↓ Recebendo",
                down,
                &focus.down,
                palette::GREEN,
            );
            render_rate(frame, cols[1], "↑ Enviando", up, &focus.up, palette::BLUE);
            split[1]
        }
        None => inner,
    };

    let lines = detail_lines(&focus.detail, body.width as usize);
    // Fed back so the next keypress can't scroll past the end — see
    // `DetailFocus::max_scroll`.
    let max_scroll = (lines.len() as u16).saturating_sub(body.height);
    focus.max_scroll.set(max_scroll);
    frame.render_widget(
        Paragraph::new(lines).scroll((focus.scroll.min(max_scroll), 0)),
        body,
    );
}

// --- Ferramentas tab ---------------------------------------------------------------

const TOOLS_HEADERS: [&str; 6] = [
    "Ferramenta",
    "Detalhe",
    "Tempo",
    "Conexões",
    "Tráfego",
    "Estado",
];

/// Carves a fixed-size box out of the middle of `area`, shrinking to fit rather than
/// overflowing when the terminal is smaller than the box wants to be.
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

/// The Ferramentas tab: every execution the user has started, live. Nothing is sampled
/// here — the counters are atomics the tools' own threads keep updating.
fn render_tools_tab(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .title(" Execuções ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(palette::CYAN))
        .title_bottom(hint_line(
            "a adicionar · Enter monitorar · e editar · r reiniciar · Del remover · ↑/↓ navegar",
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.tools.executions.is_empty() {
        let empty = Paragraph::new(vec![
            Line::raw(""),
            Line::styled(
                "  Nenhuma execução rodando.",
                Style::default().fg(palette::DIM),
            ),
            Line::styled(
                "  Tecle 'a' para adicionar uma.",
                Style::default().fg(palette::DIM),
            ),
        ]);
        frame.render_widget(empty, inner);
        return;
    }

    let header = UiRow::new(
        TOOLS_HEADERS
            .iter()
            .map(|h| Cell::from(*h))
            .collect::<Vec<_>>(),
    )
    .style(Style::default().add_modifier(Modifier::BOLD));

    let body: Vec<UiRow> = app
        .tools
        .executions
        .iter()
        .map(|execution| {
            let stats = &execution.stats;
            let (state, style) = if execution.is_running() {
                ("rodando", Style::default().fg(palette::GREEN))
            } else {
                ("parada", Style::default().fg(palette::RED))
            };
            UiRow::new(vec![
                Cell::from(execution.tool),
                Cell::from(execution.summary.clone()),
                Cell::from(format::human_duration(
                    execution.started.elapsed().as_secs(),
                )),
                Cell::from(format!(
                    "{} ({} ativas)",
                    stats.connections.load(Ordering::Relaxed),
                    stats.active.load(Ordering::Relaxed)
                )),
                Cell::from(format!(
                    "→{} ←{}",
                    format::human_bytes(stats.to_target.load(Ordering::Relaxed) as f64),
                    format::human_bytes(stats.from_target.load(Ordering::Relaxed) as f64)
                )),
                Cell::from(Span::styled(state, style)),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(16),
        Constraint::Fill(1),
        Constraint::Length(8),
        Constraint::Length(16),
        Constraint::Length(22),
        Constraint::Length(8),
    ];
    let table = Table::new(body, widths)
        .header(header)
        .row_highlight_style(
            Style::default()
                .fg(palette::CYAN)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),
        )
        .highlight_symbol("▶ ");
    let mut state = TableState::default().with_selected(Some(app.tools.selected));
    frame.render_stateful_widget(table, inner, &mut state);
}

/// The add-an-execution wizard: pick a tool, fill in what it needs, look at it once,
/// then start it. Rendered as one centered box whose contents change per step.
fn render_wizard(frame: &mut Frame, area: Rect, app: &App, wizard: &ToolWizard) {
    // The rules screen replaces the form rather than sitting on top of it: two centred
    // boxes of different sizes only ever read as one box with a hole cut in it.
    if let Some(editor) = &wizard.editor {
        render_rules(frame, area, editor);
        return;
    }
    let tool = app.tools_available.get(wizard.tool);
    let editing = wizard.editing.is_some();
    let (subtitle, hint) = match wizard.step {
        WizardStep::SelectTool => (
            "escolher ferramenta".to_string(),
            "↑/↓ escolher · Enter continuar · Esc cancelar",
        ),
        WizardStep::Params => (
            tool.map(|t| t.name().to_string()).unwrap_or_default(),
            if editing {
                "↑/↓ campo · ←/→ alternar opção · digite para editar · Enter continuar · Esc descartar"
            } else {
                "↑/↓ campo · ←/→ alternar opção · digite para editar · Enter continuar · Esc voltar"
            },
        ),
        WizardStep::Confirm => (
            "confirmar".to_string(),
            if editing {
                "Enter aplica as mudanças · Esc voltar"
            } else {
                "Enter inicia a execução · Esc voltar"
            },
        ),
    };

    // Width is settled before the lines are built, since the prose in them has to be
    // wrapped to it — and only then is the height known.
    let width = WIZARD_WIDTH.min(area.width);
    let text_width = (width as usize).saturating_sub(WIZARD_TEXT_MARGIN);
    let lines = match wizard.step {
        WizardStep::SelectTool => wizard_tool_lines(app, wizard, text_width),
        WizardStep::Params => wizard_param_lines(wizard, text_width),
        WizardStep::Confirm => wizard_confirm_lines(app, wizard),
    };

    // Two rows of border plus a blank line of breathing room at each end.
    let height = (lines.len() as u16).saturating_add(4).min(area.height);
    let box_area = centered(area, width, height);
    let heading = if editing {
        "Editar execução"
    } else {
        "Nova execução"
    };
    let block = Block::default()
        .title(format!(" {heading} — {subtitle} "))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(palette::PURPLE))
        .title_bottom(hint_line(hint));
    let inner = block.inner(box_area);
    frame.render_widget(Clear, box_area);
    frame.render_widget(block, box_area);
    frame.render_widget(Paragraph::new(lines), inner);
}

/// The rewrite rules of one execution, plus the shared history to pull from.
fn render_rules(frame: &mut Frame, area: Rect, editor: &RulesEditor) {
    let (subtitle, hint) = match &editor.mode {
        RulesMode::List => (
            "regex/replace",
            "a nova · e editar · h histórico · Del remover · Esc concluir",
        ),
        RulesMode::Edit { editing, .. } => (
            if editing.is_some() {
                "editar regra"
            } else {
                "nova regra"
            },
            "Tab/↑/↓ trocar de linha · Enter salvar · Esc cancelar",
        ),
        RulesMode::History { .. } => (
            "histórico de regras",
            "↑/↓ escolher · Enter usar nesta execução · Del apagar do histórico · Esc voltar",
        ),
    };

    let width = WIZARD_WIDTH.min(area.width);
    let text_width = (width as usize).saturating_sub(WIZARD_TEXT_MARGIN);
    let mut lines = match &editor.mode {
        RulesMode::List => rules_list_lines(editor, text_width),
        RulesMode::Edit {
            find,
            replace,
            on_replace,
            ..
        } => rules_edit_lines(find, replace, *on_replace, text_width),
        RulesMode::History { entries, selected } => {
            rules_history_lines(entries, *selected, text_width)
        }
    };
    if let Some(error) = &editor.error {
        lines.push(Line::raw(""));
        for line in wrap(&format!("⚠ {error}"), text_width) {
            lines.push(Line::styled(
                format!("   {line}"),
                Style::default().fg(palette::RED),
            ));
        }
    }

    let height = (lines.len() as u16).saturating_add(4).min(area.height);
    let box_area = centered(area, width, height);
    let block = Block::default()
        .title(format!(" {subtitle} "))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(palette::CYAN))
        .title_bottom(hint_line(hint));
    let inner = block.inner(box_area);
    frame.render_widget(Clear, box_area);
    frame.render_widget(block, box_area);
    frame.render_widget(Paragraph::new(lines), inner);
}

/// One rule as `procurado → substituto`, which is how they read in the file too.
fn rule_lines(rule: &Rule, marker: &str, style: Style, width: usize) -> Vec<Line<'static>> {
    let replace = if rule.replace.is_empty() {
        "(apaga)".to_string()
    } else {
        rule.replace.clone()
    };
    wrap(&format!("{} → {}", rule.find, replace), width)
        .into_iter()
        .enumerate()
        .map(|(i, text)| {
            let prefix = if i == 0 { marker } else { "   " };
            Line::styled(format!(" {prefix}{text}"), style)
        })
        .collect()
}

fn rules_list_lines(editor: &RulesEditor, width: usize) -> Vec<Line<'static>> {
    let mut lines = vec![Line::raw("")];
    if editor.rules.is_empty() {
        lines.push(Line::styled(
            "   Nenhuma regra. 'a' escreve uma, 'h' pega uma já usada antes.",
            Style::default().fg(palette::DIM),
        ));
    }
    for (i, rule) in editor.rules.iter().enumerate() {
        let selected = i == editor.selected;
        let style = if selected {
            Style::default()
                .fg(palette::YELLOW)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let marker = if selected { "▶ " } else { "  " };
        lines.extend(rule_lines(rule, marker, style, width));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "   Aplicadas em ordem, ao que o cliente manda, antes de sair para o destino.",
        Style::default().fg(palette::DIM),
    ));
    lines
}

fn rules_edit_lines(
    find: &str,
    replace: &str,
    on_replace: bool,
    width: usize,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::raw("")];
    for (label, value, focused) in [
        ("Procurar (regex)", find, !on_replace),
        ("Substituir por", replace, on_replace),
    ] {
        let marker = if focused { "▶ " } else { "  " };
        let shown = if focused {
            format!("{value}▏")
        } else {
            value.to_string()
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {marker}{label:PARAM_LABEL_WIDTH$}  "),
                Style::default().fg(palette::DIM),
            ),
            Span::styled(shown, value_style(focused)),
        ]));
    }
    lines.push(Line::raw(""));
    let help = if on_replace {
        "Texto puro; $1 e ${nome} trazem o que os grupos capturaram. Vazio apaga o trecho."
    } else {
        "Sintaxe regex, casada sobre os bytes crus — vale para payload binário também."
    };
    for line in wrap(help, width) {
        lines.push(Line::styled(
            format!("   {line}"),
            Style::default().fg(palette::DIM),
        ));
    }
    lines.push(Line::raw(""));
    for line in wrap(
        "A regra roda em cima de cada pedaço lido: casa o que chega numa leitura só, e trocar por algo de tamanho diferente muda o tamanho do que o destino recebe.",
        width,
    ) {
        lines.push(Line::styled(
            format!("   {line}"),
            Style::default().fg(palette::DIM),
        ));
    }
    lines
}

fn rules_history_lines(entries: &[Rule], selected: usize, width: usize) -> Vec<Line<'static>> {
    let mut lines = vec![Line::raw("")];
    if entries.is_empty() {
        lines.push(Line::styled(
            "   Nada guardado ainda. Toda regra escrita entra aqui e fica.",
            Style::default().fg(palette::DIM),
        ));
        return lines;
    }
    for (i, rule) in entries.iter().enumerate() {
        let is_selected = i == selected;
        let style = if is_selected {
            Style::default()
                .fg(palette::YELLOW)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let marker = if is_selected { "▶ " } else { "  " };
        lines.extend(rule_lines(rule, marker, style, width));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "   Guardado por máquina, não por execução: remover a execução não apaga daqui.",
        Style::default().fg(palette::DIM),
    ));
    lines
}

fn wizard_tool_lines(app: &App, wizard: &ToolWizard, text_width: usize) -> Vec<Line<'static>> {
    let mut lines = vec![Line::raw("")];
    for (i, tool) in app.tools_available.iter().enumerate() {
        let selected = i == wizard.tool;
        let marker = if selected { "▶ " } else { "  " };
        let style = if selected {
            Style::default()
                .fg(palette::CYAN)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" {marker}"), style),
            Span::styled(tool.name().to_string(), style),
        ]));
        for chunk in wrap(tool.description(), text_width) {
            lines.push(Line::styled(
                format!("     {chunk}"),
                Style::default().fg(palette::DIM),
            ));
        }
        lines.push(Line::raw(""));
    }
    lines
}

/// Label column for the parameter form, wide enough for the longest label any tool
/// currently declares.
const PARAM_LABEL_WIDTH: usize = 20;
const WIZARD_WIDTH: u16 = 92;
/// Borders plus the indent the wizard's prose sits at — subtracted from the box width
/// to get the column that help text and descriptions wrap to.
const WIZARD_TEXT_MARGIN: usize = 8;

fn wizard_param_lines(wizard: &ToolWizard, text_width: usize) -> Vec<Line<'static>> {
    let mut lines = vec![Line::raw("")];
    for (i, field) in wizard.fields.iter().enumerate() {
        let focused = i == wizard.field;
        let marker = if focused { "▶ " } else { "  " };
        let label_style = if focused {
            Style::default()
                .fg(palette::CYAN)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette::DIM)
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" {marker}"), label_style),
            Span::styled(
                format!("{:width$}  ", field.spec.label, width = PARAM_LABEL_WIDTH),
                label_style,
            ),
            Span::styled(field_value(field, focused), value_style(focused)),
        ]));
    }
    // The focused field's help sits below the whole form rather than beside its row, so
    // a long explanation never pushes the value column around as focus moves.
    if let Some(field) = wizard.fields.get(wizard.field) {
        lines.push(Line::raw(""));
        for chunk in wrap(field.spec.help, text_width) {
            lines.push(Line::styled(
                format!("   {chunk}"),
                Style::default().fg(palette::DIM),
            ));
        }
    }
    if let Some(error) = &wizard.error {
        lines.push(Line::raw(""));
        for (i, chunk) in wrap(error, text_width).into_iter().enumerate() {
            let prefix = if i == 0 { "   ⚠ " } else { "     " };
            lines.push(Line::styled(
                format!("{prefix}{chunk}"),
                Style::default()
                    .fg(palette::RED)
                    .add_modifier(Modifier::BOLD),
            ));
        }
    }
    lines
}

/// A choice shows its arrows so it's visibly cycled rather than typed; a text field
/// shows a caret while focused so it's visibly editable.
fn field_value(field: &ParamField, focused: bool) -> String {
    match field.spec.kind {
        ParamKind::Choice(_) => format!("◂ {} ▸", field.value),
        ParamKind::Rules => rules_summary(&field.value),
        ParamKind::Text if focused => format!("{}▏", field.value),
        ParamKind::Text => field.value.clone(),
    }
}

/// A rules field shows its size, not its contents — the list lives on its own screen.
fn rules_summary(encoded: &str) -> String {
    match rewrite::decode(encoded).len() {
        0 => "nenhuma  ⏎ editar".to_string(),
        1 => "1 regra  ⏎ editar".to_string(),
        n => format!("{n} regras  ⏎ editar"),
    }
}

fn value_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(palette::YELLOW)
    } else {
        Style::default()
    }
}

fn wizard_confirm_lines(app: &App, wizard: &ToolWizard) -> Vec<Line<'static>> {
    let mut lines = vec![Line::raw("")];
    if let Some(tool) = app.tools_available.get(wizard.tool) {
        lines.push(Line::styled(
            format!("  {}", tool.name()),
            Style::default()
                .fg(palette::CYAN)
                .add_modifier(Modifier::BOLD),
        ));
        lines.push(Line::raw(""));
    }
    for field in &wizard.fields {
        // A rules field has no single value to show, so the count goes on the label's
        // line and the rules themselves follow it — this is the last screen before
        // anything starts, so it's worth reading them once.
        if matches!(field.spec.kind, ParamKind::Rules) {
            let rules = rewrite::decode(&field.value);
            lines.push(Line::from(vec![
                Span::styled(
                    format!("   {:PARAM_LABEL_WIDTH$}  ", field.spec.label),
                    Style::default().fg(palette::DIM),
                ),
                match rules.len() {
                    0 => Span::styled("(nenhuma)", Style::default().fg(palette::DIM)),
                    1 => Span::raw("1 regra"),
                    n => Span::raw(format!("{n} regras")),
                },
            ]));
            for rule in &rules {
                lines.extend(rule_lines(
                    rule,
                    "  ",
                    Style::default().fg(palette::DIM),
                    WIZARD_WIDTH as usize - WIZARD_TEXT_MARGIN - PARAM_LABEL_WIDTH,
                ));
            }
            continue;
        }
        lines.push(Line::from(vec![
            Span::styled(
                format!(
                    "   {:width$}  ",
                    field.spec.label,
                    width = PARAM_LABEL_WIDTH
                ),
                Style::default().fg(palette::DIM),
            ),
            if field.value.is_empty() {
                // An optional field left blank still gets a line, so the confirmation
                // shows the whole form rather than quietly hiding part of it.
                Span::styled("(vazio)", Style::default().fg(palette::DIM))
            } else {
                Span::raw(field.value.clone())
            },
        ]));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        if wizard.editing.is_some() {
            "   Enter aplica agora — a execução atual para e recomeça com estes parâmetros."
        } else {
            "   Enter inicia agora — a porta passa a ser ouvida imediatamente."
        },
        Style::default().fg(palette::DIM),
    ));
    lines
}

// --- Monitor de uma execução -------------------------------------------------------

/// Width of the monitor's left gutter (`mm:ss.mmm` + connection number), which every
/// continuation line of a payload is indented past.
const GUTTER_WIDTH: usize = 17;
/// Bytes per line in hex view — the classic 16, which keeps hex + ASCII inside 80
/// columns once the gutter is accounted for.
const HEX_COLUMNS: usize = 16;

/// One rendered line of an execution's log. Split from the ratatui `Line` so the
/// search pass can look at the plain text before styling anything.
struct LogLine {
    /// Sequence number of the event this line came from — see `Event::seq`.
    seq: u64,
    gutter: String,
    text: String,
    style: Style,
}

/// `mm:ss.mmm` since the execution started.
fn stamp(at: Duration) -> String {
    let secs = at.as_secs();
    format!(
        "{:02}:{:02}.{:03}",
        secs / 60,
        secs % 60,
        at.subsec_millis()
    )
}

/// Renders a payload as text: control characters that carry structure (newline) split
/// lines, the rest become `·` so a binary blob still shows its printable islands
/// instead of scrambling the terminal.
fn payload_text_lines(bytes: &[u8], width: usize) -> Vec<String> {
    let decoded = String::from_utf8_lossy(bytes);
    let mut out = Vec::new();
    for raw in decoded.split('\n') {
        // Drop the CR of a CRLF: the line break already says what it meant, and
        // marking every header end with a `·` buries the text under punctuation. A CR
        // anywhere else is a real oddity and still shows up below.
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        let cleaned: String = raw
            .chars()
            .map(|c| {
                if c == '\t' || !c.is_control() {
                    c
                } else {
                    '·'
                }
            })
            .collect();
        out.extend(wrap(&cleaned, width));
    }
    out
}

/// Classic hexdump: offset, 16 bytes of hex split into two groups, then the printable
/// rendering of the same bytes.
fn payload_hex_lines(bytes: &[u8]) -> Vec<String> {
    bytes
        .chunks(HEX_COLUMNS)
        .enumerate()
        .map(|(row, chunk)| {
            let mut hex = String::new();
            for i in 0..HEX_COLUMNS {
                if i == HEX_COLUMNS / 2 {
                    hex.push(' ');
                }
                match chunk.get(i) {
                    Some(b) => hex.push_str(&format!("{b:02x} ")),
                    None => hex.push_str("   "),
                }
            }
            let ascii: String = chunk
                .iter()
                .map(|&b| {
                    if b.is_ascii_graphic() || b == b' ' {
                        b as char
                    } else {
                        '.'
                    }
                })
                .collect();
            format!("{:04x}  {hex} |{ascii}|", row * HEX_COLUMNS)
        })
        .collect()
}

/// Flattens an execution's log into displayable lines. Every event contributes one
/// header line, and a data event additionally contributes its payload, indented under
/// the gutter so the two read as one block.
fn log_lines(execution: &Execution, hex: bool, width: usize) -> Vec<LogLine> {
    let log = lock_log(&execution.log);
    let payload_width = width.saturating_sub(GUTTER_WIDTH + 2).max(MIN_VALUE_WIDTH);
    let mut lines = Vec::new();

    // Newest event first, so the live edge is the top of the screen and new traffic
    // pushes history downward. Each event's own block stays in its natural order —
    // reversing *inside* one would scramble the request it represents.
    for event in log.iter().rev() {
        let conn = if event.conn == 0 {
            "  · ".to_string()
        } else {
            format!("#{:<3}", event.conn)
        };
        let gutter = format!("{}  {}  ", stamp(event.at), conn);
        let blank = " ".repeat(gutter.chars().count());

        let (text, style) = match &event.kind {
            EventKind::Opened { peer } => (
                format!("conectado de {peer}"),
                Style::default().fg(palette::GREEN),
            ),
            EventKind::Closed { reason } => (reason.clone(), Style::default().fg(palette::DIM)),
            EventKind::Note(text) => (text.clone(), Style::default().fg(palette::CYAN)),
            EventKind::Error(text) => (format!("⚠ {text}"), Style::default().fg(palette::RED)),
            EventKind::Data { dir, len, preview } => {
                let color = match dir {
                    Flow::ToTarget => palette::YELLOW,
                    Flow::FromTarget => palette::BLUE,
                };
                let dropped = len.saturating_sub(preview.len());
                let suffix = if dropped > 0 {
                    format!(
                        " (+{} não registrados)",
                        format::human_bytes(dropped as f64)
                    )
                } else {
                    String::new()
                };
                lines.push(LogLine {
                    seq: event.seq,
                    gutter: gutter.clone(),
                    text: format!(
                        "{} {}{suffix}",
                        dir.arrow(),
                        format::human_bytes(*len as f64)
                    ),
                    style: Style::default().fg(color).add_modifier(Modifier::BOLD),
                });
                let payload = if hex {
                    payload_hex_lines(preview)
                } else {
                    payload_text_lines(preview, payload_width)
                };
                for line in payload {
                    lines.push(LogLine {
                        seq: event.seq,
                        gutter: blank.clone(),
                        text: format!("  {line}"),
                        style: Style::default().fg(color),
                    });
                }
                continue;
            }
        };
        lines.push(LogLine {
            seq: event.seq,
            gutter,
            text,
            style,
        });
    }

    // Oldest thing there is, so newest-first puts it at the very bottom.
    if log.dropped() > 0 {
        lines.push(LogLine {
            seq: 0,
            gutter: " ".repeat(GUTTER_WIDTH),
            text: format!(
                "… {} eventos mais antigos foram descartados do buffer",
                log.dropped()
            ),
            style: Style::default().fg(palette::DIM),
        });
    }
    lines
}

/// Case-insensitive substring search over ASCII, returning a byte offset. Comparing
/// raw bytes keeps the returned indices valid for slicing: an ASCII byte can never
/// match a UTF-8 continuation byte, so a match can only ever start and end on a
/// character boundary.
fn find_ci(haystack: &str, needle: &str, from: usize) -> Option<usize> {
    let (hay, need) = (haystack.as_bytes(), needle.as_bytes());
    if need.is_empty() || hay.len() < need.len() || from > hay.len() - need.len() {
        return None;
    }
    (from..=hay.len() - need.len()).find(|&i| hay[i..i + need.len()].eq_ignore_ascii_case(need))
}

/// Splits `text` into spans with every occurrence of `query` picked out, so a search
/// hit is visible in place rather than only by the line having survived a filter.
fn highlight(text: &str, query: &str, base: Style) -> Vec<Span<'static>> {
    if query.is_empty() {
        return vec![Span::styled(text.to_string(), base)];
    }
    let hit = base
        .fg(palette::ORANGE)
        .add_modifier(Modifier::BOLD | Modifier::REVERSED);
    let mut spans = Vec::new();
    let mut pos = 0;
    while let Some(found) = find_ci(text, query, pos) {
        if found > pos {
            spans.push(Span::styled(text[pos..found].to_string(), base));
        }
        let end = found + query.len();
        spans.push(Span::styled(text[found..end].to_string(), hit));
        pos = end;
    }
    if pos < text.len() {
        spans.push(Span::styled(text[pos..].to_string(), base));
    }
    spans
}

/// The live log of one execution, with search, in-place highlighting, and an optional
/// hex rendering of the payloads.
fn render_tool_monitor(frame: &mut Frame, area: Rect, app: &App, monitor: &ToolMonitorFocus) {
    let Some(execution) = app.tools.by_id(monitor.execution_id) else {
        let block = Block::default()
            .title(" Execução removida ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(palette::DIM))
            .title_bottom(hint_line("Esc voltar"));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        frame.render_widget(
            Paragraph::new(Line::styled(
                "  Esta execução não existe mais.",
                Style::default().fg(palette::DIM),
            )),
            inner,
        );
        return;
    };

    let hint = if monitor.query.is_empty() {
        "digite p/ buscar · Tab hex · ↑/↓ rolar · End ir ao mais recente · Esc voltar"
    } else {
        "↑/↓ resultado anterior/próximo · Ctrl+F filtrar · Tab hex · End mais recente · Esc limpar busca"
    };
    // The borders are drawn only once the title is known, and the title carries the
    // match count — so the inner area comes from the same block shape up front.
    let outer = Block::default().borders(Borders::ALL);
    let inner = outer.inner(area);

    let mut lines = log_lines(execution, monitor.hex, inner.width as usize);
    if monitor.only_matches && !monitor.query.is_empty() {
        lines.retain(|line| find_ci(&line.text, &monitor.query, 0).is_some());
    }
    let matches: Vec<u16> = if monitor.query.is_empty() {
        Vec::new()
    } else {
        lines
            .iter()
            .enumerate()
            .filter(|(_, line)| find_ci(&line.text, &monitor.query, 0).is_some())
            .map(|(i, _)| i as u16)
            .collect()
    };

    let max_scroll = (lines.len() as u16).saturating_sub(inner.height);
    monitor.max_scroll.set(max_scroll);
    let current = settle_scroll(monitor, &lines, &matches, max_scroll);
    let match_count = matches.len();
    let current_line = current.and_then(|i| matches.get(i).copied());
    monitor.matches.replace(matches);

    let mut title = format!(" {} — {} ", execution.tool, execution.summary);
    if !monitor.query.is_empty() {
        let position = match (current, match_count) {
            (_, 0) => " (nenhum resultado)".to_string(),
            (Some(i), total) => format!(" ({}/{total})", i + 1),
            (None, total) => format!(" ({total} resultados)"),
        };
        let filtered = if monitor.only_matches {
            " · só correspondências"
        } else {
            ""
        };
        title = format!("{title}— busca: {}{position}{filtered} ", monitor.query);
    }
    let block = outer
        .title(title)
        .border_style(Style::default().fg(if execution.is_running() {
            palette::CYAN
        } else {
            palette::DIM
        }))
        .title_bottom(hint_line(hint));
    frame.render_widget(block, area);

    if lines.is_empty() {
        let message = if monitor.query.is_empty() {
            "  Nada registrado ainda — aponte um cliente para a porta de escuta."
        } else {
            "  Nenhuma correspondência."
        };
        frame.render_widget(
            Paragraph::new(Line::styled(message, Style::default().fg(palette::DIM))),
            inner,
        );
        return;
    }

    let rendered: Vec<Line> = lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            // The hit the arrows are parked on gets a marker in the gutter: with a
            // dozen highlighted matches on screen, the highlighting alone doesn't say
            // which one ↑/↓ will move away from.
            let on_current = current_line == Some(i as u16);
            let (gutter, gutter_style) = if on_current {
                let mut marked = line.gutter.clone();
                marked.replace_range(..1, "▸");
                (
                    marked,
                    Style::default()
                        .fg(palette::ORANGE)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                (line.gutter.clone(), Style::default().fg(palette::DIM))
            };
            let mut spans = vec![Span::styled(gutter, gutter_style)];
            spans.extend(highlight(&line.text, &monitor.query, line.style));
            Line::from(spans)
        })
        .collect();

    frame.render_widget(
        Paragraph::new(rendered).scroll((monitor.scroll.get(), 0)),
        inner,
    );
}

/// Works out where the viewport should sit this frame, and returns which match — as an
/// index into `matches` — it ends up parked on.
///
/// Three things can move it, in order: following pins it to the newest event; events
/// that arrived since the last frame are counted so a reader scrolled back into history
/// stays on the same line instead of being slid downward by new traffic; and a search
/// that hasn't been navigated yet jumps to its first hit.
fn settle_scroll(
    monitor: &ToolMonitorFocus,
    lines: &[LogLine],
    matches: &[u16],
    max_scroll: u16,
) -> Option<usize> {
    let newest = lines.first().map(|line| line.seq).unwrap_or(0);
    let anchor = monitor.anchor_seq.replace(newest);

    if monitor.follow {
        monitor.scroll.set(0);
    } else {
        let arrived = lines.iter().take_while(|line| line.seq > anchor).count() as u16;
        if arrived > 0 {
            monitor
                .scroll
                .set(monitor.scroll.get().saturating_add(arrived));
            // The same shift applies to the hit the arrows are on: new matches landing
            // above it push its index further down the list.
            if let Some(current) = monitor.match_index.get() {
                let above = matches.iter().take_while(|&&line| line < arrived).count();
                monitor.match_index.set(Some(current + above));
            }
        }
    }

    if matches.is_empty() {
        monitor.match_index.set(None);
    } else if monitor.match_index.get().is_none() {
        // A freshly typed search lands on its first hit without needing an arrow key,
        // same as the fullscreen tables do.
        monitor.match_index.set(Some(0));
        monitor.scroll.set(matches[0].saturating_sub(MATCH_CONTEXT));
    }

    monitor.scroll.set(monitor.scroll.get().min(max_scroll));
    monitor.match_index.get().filter(|&i| i < matches.len())
}
