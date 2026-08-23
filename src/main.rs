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

/// Longest the loop sleeps before looking at whether a tool has written something. Also
/// the worst-case lag between a byte crossing a tunnel and its line appearing.
const REDRAW_SLICE: Duration = Duration::from_millis(60);

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

/// The two flags a program is expected to answer before it does anything else. There is
/// no configuration on the command line — everything monitorzinho does is decided from
/// inside it — but "which version is installed" is a question asked by installers,
/// scripts and bug reports alike, and a program that can only answer it by opening a
/// full-screen interface is a program that cannot answer it.
fn handle_flags() -> bool {
    let Some(flag) = std::env::args().nth(1) else {
        return false;
    };
    match flag.as_str() {
        "--version" | "-V" => println!("monitorzinho {}", env!("CARGO_PKG_VERSION")),
        // A stopwatch on the work that happens when you press Tab. Switching tabs
        // samples synchronously — the point is to show a filled screen rather than an
        // empty one — so anything slow in that path is felt as lag, and guessing which
        // part is slow on somebody else's machine is how the wrong thing gets optimised.
        "--bench" => app::bench(),
        "--help" | "-h" => {
            println!("monitorzinho {}", env!("CARGO_PKG_VERSION"));
            println!();
            println!("Monitor de terminal: CPU, memória, disco, rede e GPU em gráficos;");
            println!("processos, portas, conexões, sessões SSH e interfaces em tabelas;");
            println!("e ferramentas que rodam — túnel, scanner de portas, investigação");
            println!("DNS, scanner de rede, inspetor de certificado, receptor de");
            println!("requisições e seguidor de arquivo.");
            println!();
            println!("uso: monitorzinho [--version] [--help] [--bench]");
            println!();
            println!("--bench mede o custo de uma amostragem da aba Processos, que é o");
            println!("que acontece ao trocar de aba, e imprime onde o tempo foi.");
            println!();
            println!("Não há opções de configuração na linha de comando: tudo é escolhido");
            println!("de dentro, e as teclas de cada tela estão no rodapé dela. Tab troca");
            println!("de aba, Ctrl+C duas vezes sai.");
        }
        other => {
            eprintln!("monitorzinho: opção desconhecida «{other}» — tente --help");
            std::process::exit(2);
        }
    }
    true
}

fn main() -> io::Result<()> {
    if handle_flags() {
        return Ok(());
    }
    // A full-screen interface needs somewhere to draw. Without this the failure is
    // `Os { code: 6, kind: Uncategorized }` from deep inside the terminal setup, which
    // tells a person piping the output nothing at all about what they did wrong.
    if !std::io::IsTerminal::is_terminal(&io::stdout()) {
        eprintln!("monitorzinho precisa de um terminal — a saída está redirecionada.");
        eprintln!("Para saber a versão instalada: monitorzinho --version");
        std::process::exit(1);
    }
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
    let mut drawn_activity = tools::activity();
    let mut dirty = true;

    loop {
        // Only when there's something new. The loop now wakes several times a second
        // rather than once a tick, and redrawing every one of those would burn a
        // measurable slice of a core to show an unchanged screen.
        if dirty {
            terminal.draw(|frame| ui::render(frame, &app))?;
            dirty = false;
        }

        // Waiting in slices instead of straight through to the next sample: a relay
        // thread appending to a log can't interrupt `poll`, so a long wait here is a
        // long wait before its line reaches the screen.
        let timeout = app
            .interval()
            .checked_sub(last_tick.elapsed())
            .unwrap_or(Duration::ZERO)
            .min(REDRAW_SLICE);

        // The tab was just switched and drawn with what it had; now fill it in. Doing
        // this after the draw rather than before is the whole difference between a
        // keypress that answers instantly and one that waits for /proc.
        if app.take_pending_sample() {
            app.tick();
            last_tick = Instant::now();
            dirty = true;
            continue;
        }

        if event::poll(timeout)? {
            let event = event::read()?;
            // Any event at all, not just keys: a resize redraws too, and reading one
            // without acting on it would leave the screen at the old size.
            dirty = true;
            if let Event::Key(key) = event {
                // Ctrl+C twice in a row is the one and only way out: the first press arms
                // the quit, the second confirms it, and any other key in between disarms
                // it. Nothing else closes the app — a monitor left running for hours
                // shouldn't die to a mistyped letter.
                let ctrl_c =
                    key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
                if ctrl_c {
                    if app.quit_armed {
                        break;
                    }
                    app.quit_armed = true;
                    continue;
                }
                app.quit_armed = false;

                match key.code {
                    // A destructive action waiting to be confirmed sits above every
                    // screen, including the rules one — nothing underneath may act on
                    // the key that answers it.
                    code if app.confirm_open() => app.confirm_key(code),
                    // The mark box sits over a fullscreened table, whose every letter is
                    // search input — so like the others, it has to see the keys first.
                    code if app.mark_editor_open() => app.mark_key(code),
                    // The rules screen sits on top of the wizard and takes every key,
                    // including Esc and 'q': in its Edit mode they're just characters.
                    code if app.rules_editor_open() => app.rules_key(code),
                    // Same reason: the hand-off picker sits over a log whose search box
                    // takes every letter, so it has to see the keys first.
                    code if app.handoff_open() => app.handoff_key(code),
                    // A fullscreened table's search box swallows every letter, including
                    // 'q' — so Esc is its only way out, and it first clears an active
                    // query rather than leaving fullscreen outright.
                    KeyCode::Esc => match &app.focus {
                        // Nothing is fullscreened, so there's nothing to back out of —
                        // and Esc deliberately doesn't quit.
                        Focus::None => {}
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
                    // 'q' closes whatever is fullscreened, and only that — on the plain
                    // dashboard it does nothing (it isn't a shortcut letter either).
                    KeyCode::Char('q')
                        if matches!(app.focus, Focus::Chart(_) | Focus::Detail(_)) =>
                    {
                        match app.focus {
                            Focus::Detail(_) => app.close_detail(),
                            _ => app.exit_focus(),
                        }
                    }

                    // --- add-an-execution wizard ---
                    KeyCode::Enter if matches!(app.focus, Focus::Wizard(_)) => app.wizard_advance(),
                    KeyCode::Up if matches!(app.focus, Focus::Wizard(_)) => app.wizard_move(-1),
                    KeyCode::Down if matches!(app.focus, Focus::Wizard(_)) => app.wizard_move(1),
                    // A form is shorter than a page, so these land on its first and last
                    // field — which is exactly what a fast move through a form means.
                    KeyCode::PageUp if matches!(app.focus, Focus::Wizard(_)) => {
                        app.wizard_move(-app::PAGE_ROWS);
                    }
                    KeyCode::PageDown if matches!(app.focus, Focus::Wizard(_)) => {
                        app.wizard_move(app::PAGE_ROWS);
                    }
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
                    // Ctrl rather than a bare letter: the log's search box is always on,
                    // and the same gesture should be the same key wherever it's offered.
                    KeyCode::Char('p')
                        if matches!(app.focus, Focus::ToolMonitor(_) | Focus::Detail(_))
                            && key.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        app.open_handoffs();
                    }
                    KeyCode::Tab if matches!(app.focus, Focus::ToolMonitor(_)) => {
                        app.tool_monitor_toggle_hex();
                    }
                    // Ctrl+L, the same gesture that clears a terminal.
                    KeyCode::Char('l')
                        if matches!(app.focus, Focus::ToolMonitor(_))
                            && key.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        app.tool_monitor_clear();
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
                    KeyCode::Delete if on_tools_tab(&app) => app.request_remove_execution(),
                    KeyCode::Char('e') if on_tools_tab(&app) => app.edit_selected_execution(),
                    KeyCode::Char('r') if on_tools_tab(&app) => app.restart_selected_execution(),
                    // Space, ahead of the global "refresh now" arm below: this tab has
                    // nothing to refresh — an execution's counters are atomics the UI
                    // already reads every frame — so the key is free for the one thing
                    // the list can do to a row without losing it.
                    KeyCode::Char(' ') if on_tools_tab(&app) => app.toggle_selected_execution(),
                    KeyCode::Up if on_tools_tab(&app) => app.move_tool_selection(-1),
                    KeyCode::Down if on_tools_tab(&app) => app.move_tool_selection(1),
                    KeyCode::PageUp if on_tools_tab(&app) => {
                        app.move_tool_selection(-app::PAGE_ROWS);
                    }
                    KeyCode::PageDown if on_tools_tab(&app) => {
                        app.move_tool_selection(app::PAGE_ROWS);
                    }
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
                    KeyCode::PageUp if matches!(app.focus, Focus::Detail(_)) => {
                        app.scroll_detail(-10)
                    }
                    KeyCode::PageDown if matches!(app.focus, Focus::Detail(_)) => {
                        app.scroll_detail(10)
                    }
                    KeyCode::Up if matches!(app.focus, Focus::Table(_)) => app.move_selection(-1),
                    KeyCode::Down if matches!(app.focus, Focus::Table(_)) => app.move_selection(1),
                    KeyCode::PageUp if matches!(app.focus, Focus::Table(_)) => {
                        app.page_selection(-app::PAGE_ROWS);
                    }
                    KeyCode::PageDown if matches!(app.focus, Focus::Table(_)) => {
                        app.page_selection(app::PAGE_ROWS);
                    }
                    KeyCode::Right if matches!(app.focus, Focus::Table(_)) => app.expand_selected(),
                    KeyCode::Left if matches!(app.focus, Focus::Table(_)) => {
                        app.collapse_selected()
                    }
                    KeyCode::Delete if matches!(app.focus, Focus::Table(_)) => {
                        app.request_kill_selected();
                    }
                    // Ctrl+E, not a bare letter: in a fullscreened table every letter is
                    // search input, and marking has to work *while* searching, since
                    // finding the row is usually how you got to it. Not Ctrl+M either —
                    // a terminal sends the same byte for that as for Enter.
                    KeyCode::Char('e')
                        if matches!(app.focus, Focus::Table(_))
                            && key.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        app.toggle_mark();
                    }
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
        }

        // A tool wrote something while nobody touched the keyboard. Only worth a redraw
        // where it would show: a busy tunnel is no reason to repaint a CPU chart, and
        // the counter is still read so the screen isn't stale on returning to the tab.
        let activity = tools::activity();
        if activity != drawn_activity {
            drawn_activity = activity;
            dirty |= app.shows_tools();
        }

        if last_tick.elapsed() >= app.interval() {
            app.tick();
            last_tick = Instant::now();
            dirty = true;
        }
    }

    app.persist();
    Ok(())
}
