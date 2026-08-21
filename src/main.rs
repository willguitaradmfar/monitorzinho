use std::io;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

mod app;
mod format;
mod history;
mod monitor;
mod tools;
mod ui;

use app::{App, Focus, Tab};

const TICK_RATE: Duration = Duration::from_secs(2);

/// Whether the Ferramentas tab's own list is what the keyboard should be driving —
/// i.e. that tab is showing and nothing is fullscreened on top of it.
fn on_tools_tab(app: &App) -> bool {
    matches!(app.focus, Focus::None) && app.tab == Tab::Tools
}

fn install_panic_hook() {
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        original_hook(panic_info);
    }));
}

fn main() -> io::Result<()> {
    install_panic_hook();

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run(&mut terminal);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    let mut app = App::new();
    app.tick();
    let mut last_tick = Instant::now();

    loop {
        terminal.draw(|frame| ui::render(frame, &app))?;

        let timeout = TICK_RATE
            .checked_sub(last_tick.elapsed())
            .unwrap_or(Duration::from_millis(0));

        if event::poll(timeout)?
            && let Event::Key(key) = event::read()?
        {
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                break;
            }
            match key.code {
                // A fullscreened table's search box swallows every letter, including
                // 'q' — so Esc is its only way out, and it first clears an active
                // query rather than leaving fullscreen outright.
                KeyCode::Esc => match &app.focus {
                    Focus::None => break,
                    // A detail view goes back to the table it came from, not all the
                    // way out — that table is the thing it was opened on top of.
                    Focus::Detail(_) => app.close_detail(),
                    // The wizard steps backwards one stage at a time, and the monitor
                    // drops its search before it drops the view. Both keep their own
                    // logic rather than exiting outright.
                    Focus::Wizard(_) => app.wizard_back(),
                    Focus::ToolMonitor(_) => app.tool_monitor_escape(),
                    Focus::Table(tf) if !tf.query.is_empty() => app.clear_search(),
                    _ => app.exit_focus(),
                },
                // 'q' only quits where no text is being typed — the tables' search box,
                // the wizard's fields and the monitor's search all need the letter.
                KeyCode::Char('q')
                    if matches!(app.focus, Focus::None | Focus::Chart(_) | Focus::Detail(_)) =>
                {
                    match app.focus {
                        Focus::None => break,
                        Focus::Detail(_) => app.close_detail(),
                        _ => app.exit_focus(),
                    }
                }

                // --- add-an-execution wizard ---
                KeyCode::Enter if matches!(app.focus, Focus::Wizard(_)) => app.wizard_advance(),
                KeyCode::Up if matches!(app.focus, Focus::Wizard(_)) => app.wizard_move(-1),
                KeyCode::Down if matches!(app.focus, Focus::Wizard(_)) => app.wizard_move(1),
                KeyCode::Left if matches!(app.focus, Focus::Wizard(_)) => app.wizard_cycle(-1),
                KeyCode::Right if matches!(app.focus, Focus::Wizard(_)) => app.wizard_cycle(1),
                KeyCode::Backspace if matches!(app.focus, Focus::Wizard(_)) => {
                    app.wizard_backspace();
                }
                KeyCode::Char(c) if matches!(app.focus, Focus::Wizard(_)) => app.wizard_type(c),

                // --- one execution's live log ---
                KeyCode::Char('f')
                    if matches!(app.focus, Focus::ToolMonitor(_))
                        && key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    app.tool_monitor_toggle_filter();
                }
                KeyCode::Tab if matches!(app.focus, Focus::ToolMonitor(_)) => {
                    app.tool_monitor_toggle_hex();
                }
                KeyCode::End if matches!(app.focus, Focus::ToolMonitor(_)) => {
                    app.tool_monitor_follow();
                }
                KeyCode::Up if matches!(app.focus, Focus::ToolMonitor(_)) => {
                    app.tool_monitor_scroll(-1);
                }
                KeyCode::Down if matches!(app.focus, Focus::ToolMonitor(_)) => {
                    app.tool_monitor_scroll(1);
                }
                KeyCode::PageUp if matches!(app.focus, Focus::ToolMonitor(_)) => {
                    app.tool_monitor_scroll(-15);
                }
                KeyCode::PageDown if matches!(app.focus, Focus::ToolMonitor(_)) => {
                    app.tool_monitor_scroll(15);
                }
                KeyCode::Backspace if matches!(app.focus, Focus::ToolMonitor(_)) => {
                    app.tool_monitor_backspace();
                }
                KeyCode::Char(c) if matches!(app.focus, Focus::ToolMonitor(_)) => {
                    app.tool_monitor_type(c);
                }

                // --- Ferramentas tab, nothing fullscreened ---
                // Ahead of the shortcut arm below: this tab has no shortcut-able
                // panels, so its letters are free for its own bindings.
                KeyCode::Char('a') if on_tools_tab(&app) => app.open_wizard(),
                KeyCode::Enter if on_tools_tab(&app) => app.open_tool_monitor(),
                KeyCode::Delete if on_tools_tab(&app) => app.remove_selected_execution(),
                KeyCode::Char('r') if on_tools_tab(&app) => app.restart_selected_execution(),
                KeyCode::Up if on_tools_tab(&app) => app.move_tool_selection(-1),
                KeyCode::Down if on_tools_tab(&app) => app.move_tool_selection(1),
                KeyCode::Tab if matches!(app.focus, Focus::None) => app.next_tab(),
                KeyCode::BackTab if matches!(app.focus, Focus::None) => app.prev_tab(),
                // Like top's spacebar: force an immediate refresh without waiting for
                // the next tick, and restart the tick timer so it doesn't double-fire.
                KeyCode::Char(' ') if matches!(app.focus, Focus::None) => {
                    app.tick();
                    last_tick = Instant::now();
                }
                KeyCode::Char(c) if matches!(app.focus, Focus::None) => {
                    app.activate_shortcut(c);
                }
                // Enter opens whatever the selected row's monitor can say about it;
                // tables with no detail to give simply ignore it.
                KeyCode::Enter if matches!(app.focus, Focus::Table(_)) => app.open_detail(),
                KeyCode::Up if matches!(app.focus, Focus::Detail(_)) => app.scroll_detail(-1),
                KeyCode::Down if matches!(app.focus, Focus::Detail(_)) => app.scroll_detail(1),
                KeyCode::PageUp if matches!(app.focus, Focus::Detail(_)) => app.scroll_detail(-10),
                KeyCode::PageDown if matches!(app.focus, Focus::Detail(_)) => app.scroll_detail(10),
                KeyCode::Up if matches!(app.focus, Focus::Table(_)) => app.move_selection(-1),
                KeyCode::Down if matches!(app.focus, Focus::Table(_)) => app.move_selection(1),
                KeyCode::Right if matches!(app.focus, Focus::Table(_)) => app.expand_selected(),
                KeyCode::Left if matches!(app.focus, Focus::Table(_)) => app.collapse_selected(),
                KeyCode::Delete if matches!(app.focus, Focus::Table(_)) => app.kill_selected(),
                KeyCode::Backspace if matches!(app.focus, Focus::Table(_)) => {
                    app.search_backspace();
                }
                // Anything else typed while a table is fullscreened is search input —
                // no separate search mode to enter first.
                KeyCode::Char(c) if matches!(app.focus, Focus::Table(_)) => {
                    app.search_push(c);
                }
                _ => {}
            }
        }

        if last_tick.elapsed() >= TICK_RATE {
            app.tick();
            last_tick = Instant::now();
        }
    }

    app.persist();
    Ok(())
}
