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
mod ui;

use app::{App, Focus};

const TICK_RATE: Duration = Duration::from_secs(2);

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
                    Focus::Table(tf) if !tf.query.is_empty() => app.clear_search(),
                    _ => app.exit_focus(),
                },
                KeyCode::Char('q') if !matches!(app.focus, Focus::Table(_)) => match app.focus {
                    Focus::None => break,
                    _ => app.exit_focus(),
                },
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
