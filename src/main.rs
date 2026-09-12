use std::io;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

mod action;
mod app;
mod container;
mod db;
mod format;
mod history;
mod invest;
mod legado;
mod monitor;
mod painel;
mod perfil;
mod tmux;
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

/// The flags a program is expected to answer before it does anything else. There is
/// almost no configuration on the command line — everything monitorzinho does is decided
/// from inside it — but "which version is installed" is a question asked by installers,
/// scripts and bug reports alike, and a program that can only answer it by opening a
/// full-screen interface is a program that cannot answer it.
///
/// `--perfil` is the exception, and it has to be one: which database to open is decided
/// *before* there is a program to decide it from inside of. `None` means the flag was
/// answered here and there is nothing left to run.
fn handle_flags() -> Option<perfil::Pedido> {
    let Some(flag) = std::env::args().nth(1) else {
        return Some(perfil::Pedido::Automatico);
    };
    match flag.as_str() {
        // Sem nome, abre a tela de escolha mesmo havendo um perfil só — que é como se
        // cria o segundo. Com nome, vai direto, criando se não existir: é o que serve
        // para um atalho ou um script.
        "--perfil" | "-p" => {
            return Some(match std::env::args().nth(2) {
                Some(nome) => perfil::Pedido::Nomeado(nome),
                None => perfil::Pedido::Escolher,
            });
        }
        "--perfis" => {
            let perfis = db::perfis();
            if perfis.is_empty() {
                println!("nenhum perfil ainda em {}", db::perfis_dir().display());
            }
            for p in perfis {
                println!("{}\t{}", p.nome, p.caminho.display());
            }
        }
        "--version" | "-V" => println!("monitorzinho {}", env!("CARGO_PKG_VERSION")),
        // A stopwatch on the work that happens when you press Tab. Switching tabs
        // samples synchronously — the point is to show a filled screen rather than an
        // empty one — so anything slow in that path is felt as lag, and guessing which
        // part is slow on somebody else's machine is how the wrong thing gets optimised.
        "--bench" => {
            // Precisa de um banco como qualquer outra execução, e não pode abrir tela
            // para perguntar qual: mede-se o perfil que uma execução sem flag abriria.
            if let Err(erro) = perfil::abrir_sem_tela() {
                eprintln!("monitorzinho: {erro}");
                std::process::exit(1);
            }
            app::bench()
        }
        "--help" | "-h" => {
            println!("monitorzinho {}", env!("CARGO_PKG_VERSION"));
            println!();
            println!("Monitor de terminal: CPU, memória, disco, rede e GPU em gráficos;");
            println!("processos, portas, conexões, sessões SSH e interfaces em tabelas;");
            println!("containers, volumes, imagens e redes — com logs, shell e as");
            println!("operações de cada um; sessões, janelas e painéis do tmux — com");
            println!("entrar, criar, renomear e matar; e ferramentas que rodam — túnel,");
            println!("scanner de portas, investigação DNS, scanner de rede, inspetor de");
            println!("certificado, receptor de requisições e seguidor de arquivo.");
            println!();
            println!("uso: monitorzinho [--version] [--help] [--bench]");
            println!("               [--perfil [nome]] [--perfis]");
            println!();
            println!("--bench mede o custo de uma amostragem de cada aba, que é o que");
            println!("acontece ao trocar de aba, e imprime onde o tempo foi.");
            println!();
            println!("Tudo que o programa guarda — histórico, ferramentas, marcas, a");
            println!("carteira — fica num arquivo SQLite por perfil, em");
            println!("~/.local/share/monitorzinho/db/. Copiar o arquivo é o backup;");
            println!("pôr o arquivo na mesma pasta de outra máquina é a restauração.");
            println!("Havendo mais de um perfil, o programa pergunta qual abrir.");
            println!("--perfis lista os que existem; --perfil abre a tela de escolha,");
            println!("que é onde se cria um novo; --perfil <nome> abre aquele direto.");
            println!();
            println!("Não há opções de configuração na linha de comando: tudo é escolhido");
            println!("de dentro, e as teclas de cada tela estão no rodapé dela. Tab troca");
            println!("de aba, Ctrl+C duas vezes sai. A aba Containers só aparece onde há");
            println!("uma engine que responda ou containers rodando, e a aba tmux só onde");
            println!("o tmux está instalado.");
        }
        other => {
            eprintln!("monitorzinho: opção desconhecida «{other}» — tente --help");
            std::process::exit(2);
        }
    }
    None
}

fn main() -> io::Result<()> {
    let Some(pedido) = handle_flags() else {
        return Ok(());
    };
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

    let result = run(&mut terminal, pedido);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

/// Entrega o terminal a um shell dentro do container e o toma de volta quando acaba.
///
/// A tela alternativa é abandonada de propósito: o shell tem que escrever na tela normal,
/// onde o que ele imprimiu continua no histórico do terminal depois que a sessão fecha —
/// que é metade da razão de abrir um shell. O modo bruto continua ligado, porque é
/// exatamente o que um terminal interativo quer.
fn open_shell(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    container: &container::Container,
) -> io::Result<()> {
    execute!(io::stdout(), LeaveAlternateScreen)?;
    let size = crossterm::terminal::size().unwrap_or((80, 24));
    let outcome = app.run_shell(container, size);

    // Volta para a tela alternativa e reconstrói o quadro do zero: o shell escreveu por
    // toda a tela, e o `ratatui` só redesenha o que ele acha que mudou.
    execute!(io::stdout(), EnterAlternateScreen)?;
    terminal.clear()?;
    if let Err(error) = outcome {
        app.report_shell_failure(error);
    }
    Ok(())
}

/// Entrega o terminal a uma sessão de tmux e o toma de volta quando alguém desanexar.
///
/// Irmão do `open_shell`, e mais simples que ele por uma razão de fundo: o tmux é um
/// processo filho que **herda o terminal de verdade**, enquanto o shell de um container
/// está do outro lado de um socket e precisa de alguém copiando bytes nos dois sentidos.
/// Aqui não há relay, não há thread, e o redimensionamento não é problema nosso — o
/// `SIGWINCH` vai direto para quem está desenhando.
///
/// O modo bruto é desligado antes e religado depois. O tmux configura o terminal do jeito
/// dele e restaura o que encontrou ao sair; entregá-lo já em modo bruto faria ele
/// restaurar o modo bruto, e a interface voltaria para um terminal que ela acha que
/// acabou de configurar mas não configurou.
fn open_attach(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    request: &tmux::Attach,
) -> io::Result<()> {
    execute!(io::stdout(), LeaveAlternateScreen)?;
    disable_raw_mode()?;
    let outcome = app.run_attach(request);
    enable_raw_mode()?;
    // Volta para a tela alternativa e reconstrói o quadro do zero: o tmux escreveu por
    // toda a tela, e o `ratatui` só redesenha o que ele acha que mudou.
    execute!(io::stdout(), EnterAlternateScreen)?;
    terminal.clear()?;
    if let Err(error) = outcome {
        app.report_attach_failure(error);
    }
    Ok(())
}

fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    pedido: perfil::Pedido,
) -> io::Result<()> {
    // Antes de tudo, porque tudo depende disto: `App::new` lê o histórico, as
    // ferramentas, as marcas e a carteira, e todos eles vêm do banco do perfil.
    let aberto = match perfil::abrir(terminal, pedido)? {
        Some(aberto) => aberto,
        // Esc na tela de escolha. Sair sem ter aberto nada é o que ele significa.
        None => return Ok(()),
    };
    match aberto {
        // A importação dos arquivos da versão anterior acontece uma vez na vida da
        // instalação, e move a carteira de alguém de lugar. Ela é mostrada e esperada:
        // uma linha de rodapé que some no primeiro tick não é onde isso se conta.
        perfil::Aberto::Pronto {
            legado: Some(resumo),
        } if resumo.houve_algo() => {
            perfil::mostrar_legado(terminal, &resumo)?;
        }
        perfil::Aberto::Pronto { .. } => {}
        perfil::Aberto::Falhou { caminho, erro } => {
            return perfil::mostrar_falha(terminal, &caminho, &erro);
        }
    }

    let mut app = App::new();
    app.tick();
    let mut last_tick = Instant::now();
    let mut drawn_activity = tools::activity();
    let mut drawn_containers = app.container_revision();
    let mut drawn_invest = app.invest_revision();
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

        // Um shell pedido no menu da aba Containers. Aqui, entre um quadro e o próximo,
        // e não dentro do tratamento da tecla: enquanto ele está aberto o terminal é
        // dele, e o laço não pode desenhar por cima.
        if let Some(container) = app.take_pending_shell() {
            open_shell(terminal, &mut app, &container)?;
            last_tick = Instant::now();
            dirty = true;
            continue;
        }

        // Uma sessão de tmux pedida no menu da aba tmux, ou recém-criada no formulário.
        // Aqui pelo mesmo motivo que o shell: enquanto ela está aberta o terminal é dela.
        if let Some(request) = app.take_pending_attach() {
            open_attach(terminal, &mut app, &request)?;
            last_tick = Instant::now();
            dirty = true;
            continue;
        }

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
                    // Above the marks screen too: the box is opened from it, to edit the
                    // row the list has the cursor on.
                    code if app.mark_editor_open() => app.mark_key(code),
                    // A caixa do endereço engole todas as letras: é um campo de texto,
                    // e 'q' faz parte de um nome de host como qualquer outra.
                    code if app.endpoint_editor_open() => app.endpoint_key(code),
                    // Mesma razão, e sobre uma tabela em tela cheia cujas letras seriam
                    // busca: a caixa que cria uma sessão tem que ver as teclas primeiro.
                    code if app.session_editor_open() => app.session_key(code),
                    // The marks screen sits over whatever it was opened from, table or
                    // dashboard, and takes every key including 'q' and the letters that
                    // would otherwise be search input or panel shortcuts.
                    code if app.marks_screen_open() => app.marks_key(code),
                    // The rules screen sits on top of the wizard and takes every key,
                    // including Esc and 'q': in its Edit mode they're just characters.
                    code if app.rules_editor_open() => app.rules_key(code),
                    // Same reason: the hand-off picker sits over a log whose search box
                    // takes every letter, so it has to see the keys first.
                    code if app.handoff_open() => app.handoff_key(code),
                    // O menu de operações e a tela de texto tomam todas as teclas
                    // enquanto estão abertos, incluindo 'q' e as setas: são telas
                    // inteiras, não caixas sobre a tabela.
                    code if app.actions_open() => app.actions_key(code),
                    code if app.text_open() => app.text_key(code),
                    // --- o painel de uma execução ---
                    // Antes do Esc e do Tab globais: numa grade de cartões essas duas
                    // teclas são da grade (voltar um nível, ver o log), e não da janela.
                    KeyCode::Esc if matches!(app.focus, Focus::Board(_)) => app.board_escape(),
                    KeyCode::Char('q')
                        if matches!(app.focus, Focus::Board(_))
                            && !key.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        app.board_escape();
                    }
                    KeyCode::Tab if matches!(app.focus, Focus::Board(_)) => app.board_log(),
                    KeyCode::Char('r')
                        if matches!(app.focus, Focus::Board(_))
                            && key.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        app.board_refresh();
                    }
                    KeyCode::Up if matches!(app.focus, Focus::Board(_)) => app.board_scroll(-1),
                    KeyCode::Down if matches!(app.focus, Focus::Board(_)) => app.board_scroll(1),
                    KeyCode::PageUp if matches!(app.focus, Focus::Board(_)) => {
                        app.board_scroll(-15);
                    }
                    KeyCode::PageDown if matches!(app.focus, Focus::Board(_)) => {
                        app.board_scroll(15);
                    }
                    // As letras e os números são dos cartões. Com Ctrl não: aqueles são
                    // os gestos do programa inteiro, e eles continuam valendo aqui.
                    KeyCode::Char(c)
                        if matches!(app.focus, Focus::Board(_))
                            && !key.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        app.board_open_card(c);
                    }

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
                        // Um módulo desfaz uma camada por vez e, quando não tem mais o
                        // que desfazer, **pergunta** em vez de sair. `Esc` é uma tecla
                        // que a mão aperta sozinha ao voltar de qualquer outra coisa, e
                        // um módulo é uma tela em que se fica.
                        Focus::Module(_) => app.module_escape(),
                        _ => app.exit_focus(),
                    },

                    // Ctrl+E, not a bare letter: in a fullscreened table every letter is
                    // search input, and marking has to work *while* searching, since
                    // finding the row is usually how you got to it. Not Ctrl+M either —
                    // a terminal sends the same byte for that as for Enter.
                    //
                    // Above the module arm below, and that is the whole point: marking is
                    // the app's gesture, not one panel's. Two keys are reserved from every
                    // screen so that the same gesture is the same key in every tab — and
                    // the three Invest modules that had claimed Ctrl+E for themselves gave
                    // it up rather than make the standard have exceptions.
                    KeyCode::Char('e')
                        if matches!(app.focus, Focus::Table(_) | Focus::Module(_))
                            && key.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        app.toggle_mark();
                    }

                    // Ctrl+G, next door to the key that makes a mark: the list of what
                    // has been marked. From anywhere at all — marks span every table of
                    // every tab and outlive all of it, so the one screen that answers
                    // "what am I following" can't be reachable only from some of them.
                    KeyCode::Char('g') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.open_marks_screen();
                    }

                    // Todo o resto vai para o módulo aberto: letras, setas, Enter, Del e
                    // combinações com Ctrl. Um módulo pode ter busca digitada direto, e
                    // por isso não pode haver letra reservada pelo app **sozinha** — as
                    // duas exceções são as de cima, e são com Ctrl.
                    _ if app.in_module() => app.module_key(key),
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

                    // Ctrl+N cria uma sessão de tmux. Com Ctrl e não com uma letra pela
                    // mesma razão do Ctrl+E que marca: numa tabela em tela cheia toda
                    // letra é entrada de busca, e criar tem que funcionar *enquanto* se
                    // procura, que é geralmente como se descobre que a sessão não existe.
                    KeyCode::Char('n')
                        if key.modifiers.contains(KeyModifiers::CONTROL) && app.on_tmux_tab() =>
                    {
                        app.open_session_editor();
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
                    // Ctrl+A rather than 'a': the search box owns every bare letter.
                    KeyCode::Char('a')
                        if matches!(app.focus, Focus::ToolMonitor(_))
                            && key.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        app.tool_monitor_toggle_alerts();
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
                    // The bare letter, and only the bare letter: `Ctrl+E` is the mark key
                    // everywhere in the app, and this tab has no list to mark. Without
                    // the guard it opened the execution editor instead — a reserved
                    // gesture doing something else on one tab is worse than doing nothing.
                    KeyCode::Char('e')
                        if on_tools_tab(&app) && !key.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        app.edit_selected_execution()
                    }
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
                    // A grade da Invest dá tecla aos módulos que cabem na tela. Enter
                    // abre a lista completa, buscável, para chegar aos outros.
                    KeyCode::Enter if matches!(app.focus, Focus::None) => {
                        app.abrir_lista_de_modulos();
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
                    // tables with no detail to give simply ignore it. Nas tabelas da aba
                    // Containers abre o menu de operações — ver `App::open_row`.
                    KeyCode::Enter if matches!(app.focus, Focus::Table(_)) => app.open_row(),
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

        // O mesmo, para o retrato dos containers: ele muda a cada segundo e uma ação
        // termina quando termina, enquanto o tick só vem a cada dois. Redesenhar só onde
        // isso apareceria — repintar um gráfico de CPU porque um container mudou de
        // estado é gastar um núcleo para mostrar a mesma tela.
        let containers = app.container_revision();
        if containers != drawn_containers {
            drawn_containers = containers;
            dirty |= app.shows_containers();
        }

        // O mesmo, para o retrato de mercado: um preço chega quando a thread o publica, e
        // não no ritmo do tique. Redesenhar só onde isso apareceria — repintar um gráfico
        // de CPU porque o dólar mexeu é gastar um núcleo para mostrar a mesma tela.
        let invest = app.invest_revision();
        if invest != drawn_invest {
            drawn_invest = invest;
            dirty |= app.shows_invest();
        }

        if last_tick.elapsed() >= app.interval() {
            app.tick();
            last_tick = Instant::now();
            dirty = true;
        }
    }

    app.persist();
    app.persist_invest();
    app.stop_invest();
    Ok(())
}
