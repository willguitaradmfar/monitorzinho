use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row as UiRow, Sparkline, Table};

use crate::app::App;
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

pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();

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

    let mut cursor = 0;
    if !numeric_indices.is_empty() {
        render_numeric_bar(frame, sections[cursor], app, &numeric_indices);
        cursor += 1;
    }
    render_charts(frame, sections[cursor], app);
    cursor += 1;
    if !app.table_monitors.is_empty() {
        render_tables(frame, sections[cursor], app);
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
}

/// Orders chart-worthy monitors by `Monitor::group()` (so related panels like Net
/// down/up still stay close together in the flow), then packs them into a strict grid
/// of `COLS` panels per row — a row is topped up with the next group's panels instead
/// of leaving a gap.
fn build_chart_rows(app: &App) -> Vec<Vec<usize>> {
    let mut groups: Vec<(&'static str, Vec<usize>)> = Vec::new();
    for (i, m) in app.monitors.iter().enumerate() {
        if m.numeric_only() {
            continue;
        }
        let g = m.group();
        match groups.iter_mut().find(|(name, _)| *name == g) {
            Some(entry) => entry.1.push(i),
            None => groups.push((g, vec![i])),
        }
    }

    let flat: Vec<usize> = groups
        .into_iter()
        .flat_map(|(_, indices)| indices)
        .collect();
    flat.chunks(COLS).map(<[usize]>::to_vec).collect()
}

fn render_charts(frame: &mut Frame, area: Rect, app: &App) {
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
            );
        }
    }
}

fn render_panel(
    frame: &mut Frame,
    area: Rect,
    monitor: &dyn Monitor,
    history: &History,
    extra: Option<&str>,
    capacity: Option<f64>,
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

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(color));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let values = history.values();
    let data: Vec<u64> = values.iter().map(|v| v.round().max(0.0) as u64).collect();
    // Scale bars against the metric's real ceiling (e.g. 100%) rather than the max of
    // the current window — otherwise a value that's merely the local max renders as a
    // full bar even when it's nowhere near the actual limit.
    let mut sparkline = Sparkline::default()
        .data(&data)
        .style(Style::default().fg(color));
    if let Some(limit) = monitor.limit() {
        sparkline = sparkline.max(limit.round().max(0.0) as u64);
    }
    frame.render_widget(sparkline, inner);
}

fn render_tables(frame: &mut Frame, area: Rect, app: &App) {
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
        );
    }
}

fn render_table_panel(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    headers: &[&str],
    rows: &[TableRow],
) {
    let block = Block::default()
        .title(format!(" {} ", title))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(palette::CYAN));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let header = UiRow::new(headers.iter().map(|h| Cell::from(*h)).collect::<Vec<_>>())
        .style(Style::default().add_modifier(Modifier::BOLD));
    let body: Vec<UiRow> = rows.iter().map(|r| UiRow::new(r.cells.clone())).collect();

    // Command gets the leftover space; Time/Usage are short and fixed-width.
    let table = Table::new(
        body,
        [
            Constraint::Fill(1),
            Constraint::Length(8),
            Constraint::Length(10),
        ],
    )
    .header(header);
    frame.render_widget(table, inner);
}
