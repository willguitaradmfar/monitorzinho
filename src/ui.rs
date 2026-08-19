use std::collections::HashMap;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Paragraph, RenderDirection, Row as UiRow, Sparkline, Table, TableState,
};

use crate::app::{App, Focus, ShortcutTarget, TableFocus};
use crate::format;
use crate::history::History;
use crate::monitor::{Monitor, TableRow};

const TABLE_SECTION_HEIGHT: u16 = 14;
const NUMERIC_BAR_HEIGHT: u16 = 2;
const COLS: usize = 3;

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
    pub const DIM: Color = Color::Rgb(0x5C, 0x63, 0x70);
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
            render_fullscreen_table(frame, area, monitor.title(), monitor.headers(), table_focus);
            return;
        }
        Focus::None => {}
    }

    let numeric_indices: Vec<usize> = app
        .monitors
        .iter()
        .enumerate()
        .filter(|(_, m)| m.numeric_only())
        .map(|(i, _)| i)
        .collect();

    let mut constraints = Vec::new();
    if !numeric_indices.is_empty() {
        constraints.push(Constraint::Length(NUMERIC_BAR_HEIGHT));
    }
    constraints.push(Constraint::Min(0));
    if !app.table_monitors.is_empty() {
        constraints.push(Constraint::Length(TABLE_SECTION_HEIGHT));
    }

    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    let shortcuts = ShortcutMap::build(app);

    let mut cursor = 0;
    if !numeric_indices.is_empty() {
        render_numeric_bar(frame, sections[cursor], app, &numeric_indices);
        cursor += 1;
    }
    render_charts(frame, sections[cursor], app, &shortcuts);
    cursor += 1;
    if !app.table_monitors.is_empty() {
        render_tables(frame, sections[cursor], app, &shortcuts);
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

/// A slim status bar for metrics that change too slowly for a chart to be worth the
/// space (e.g. disk occupancy) — a single line, entries separated by a thin divider,
/// with a bottom border to set it apart from the chart sections below.
fn render_numeric_bar(frame: &mut Frame, area: Rect, app: &App, indices: &[usize]) {
    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(Style::default().fg(palette::DIM));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut spans = vec![Span::raw(" ")];
    for (n, &idx) in indices.iter().enumerate() {
        if n > 0 {
            spans.push(Span::styled("   │   ", Style::default().fg(palette::DIM)));
        }

        let monitor = app.monitors[idx].as_ref();
        let last = app.histories[idx].last().unwrap_or(0.0);
        let color = signal_color(monitor, last);

        spans.push(Span::styled(
            format!("{} ", monitor.title()),
            Style::default().fg(palette::DIM),
        ));
        let value = match app.extras[idx].as_deref() {
            Some(extra) => format!("{} ({})", monitor.format(last), extra),
            None => monitor.format(last),
        };
        spans.push(Span::styled(
            value,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), inner);

    let version = Paragraph::new(Line::styled(
        format!("v{} ", env!("CARGO_PKG_VERSION")),
        Style::default().fg(palette::DIM),
    ))
    .alignment(Alignment::Right);
    frame.render_widget(version, inner);
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
        ["Proto", "Port", "Process"] => vec![
            Constraint::Length(5),
            Constraint::Length(7),
            Constraint::Fill(1),
        ],
        _ => vec![
            Constraint::Fill(1),
            Constraint::Length(8),
            Constraint::Length(10),
        ],
    }
}

fn render_tables(frame: &mut Frame, area: Rect, app: &App, shortcuts: &ShortcutMap) {
    let n = app.table_monitors.len();
    if n == 0 {
        return;
    }

    let constraints: Vec<Constraint> = (0..n).map(|_| Constraint::Ratio(1, n as u32)).collect();
    let areas = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(constraints)
        .split(area);

    for (i, monitor) in app.table_monitors.iter().enumerate() {
        render_table_panel(
            frame,
            areas[i],
            monitor.title(),
            monitor.headers(),
            &app.table_rows[i],
            shortcuts.table.get(&i).copied(),
        );
    }
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
    let body: Vec<UiRow> = rows.iter().map(|r| UiRow::new(r.cells.clone())).collect();

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
        "↑/↓ navegar · Del matar (SIGKILL) · digite p/ buscar · Esc voltar"
    } else {
        "↑/↓ navegar · Del matar (SIGKILL) · digite p/ buscar · Esc limpar busca"
    };
    block = block.title_bottom(hint_line(hint));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let header = UiRow::new(headers.iter().map(|h| Cell::from(*h)).collect::<Vec<_>>())
        .style(Style::default().add_modifier(Modifier::BOLD));
    let body: Vec<UiRow> = table_focus
        .visible_indices()
        .into_iter()
        .map(|i| UiRow::new(table_focus.rows[i].cells.clone()))
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
