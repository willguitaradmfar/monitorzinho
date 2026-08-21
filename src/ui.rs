use std::collections::HashMap;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Paragraph, RenderDirection, Row as UiRow, Sparkline, Table, TableState,
};

use crate::app::{App, DetailFocus, Focus, ShortcutTarget, Tab, TableFocus};
use crate::format;
use crate::history::History;
use crate::monitor::{Detail, Monitor, TableRow};

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

    let hint = Paragraph::new(Line::styled(
        format!(
            " v{} · Tab/Shift+Tab alternar aba ",
            env!("CARGO_PKG_VERSION")
        ),
        Style::default().fg(palette::DIM),
    ))
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
