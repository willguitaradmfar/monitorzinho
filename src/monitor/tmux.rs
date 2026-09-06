//! As tabelas da aba tmux: as sessões (em árvore), as janelas, e o resumo.
//!
//! Nada aqui cria processo nenhum. As três tabelas leem o retrato que `SystemState`
//! guarda, relido uma vez por tick enquanto a aba está na frente — ver
//! `SystemState::refresh_tmux`.

use crate::app::Tab;
use crate::format;
use crate::tmux::{Subject, Tmux};

use super::{Detail, DetailSection, SystemState, TableMonitor, TableRow, mark::MarkKind};

/// A vaga que uma linha sem processo ocupa no espaço de identidades.
///
/// `TableFocus` guarda o que está expandido por `pid`, e nem sessão nem janela têm um
/// processo só. Numerar do topo para baixo mantém cada uma distinta sem chegar perto de
/// um pid de verdade — o mesmo truque, pela mesma razão, que as linhas de projeto do
/// compose usam.
fn synthetic_key(index: usize) -> u32 {
    u32::MAX - index as u32
}

fn tmux(state: &SystemState) -> Option<&Tmux> {
    state.tmux.as_ref()
}

/// Os cabeçalhos da árvore. Um jogo só para os três níveis, e não um por nível: sessão,
/// janela e painel moram na mesma tabela, e uma coluna que mudasse de assunto conforme a
/// profundidade da linha seria uma coluna que não dá para ler de cima para baixo.
///
/// Cada uma responde a mesma pergunta em todo nível, e fica **vazia** onde o nível não
/// tem resposta — uma janela não tem janelas dentro, um painel não tem painéis dentro, e
/// só a sessão é datada pelo tmux. Vazio diz «esta pergunta não se faz aqui»; um zero
/// diria «a resposta é nenhum», que é outra coisa.
const SESSION_HEADERS: [&str; 7] = [
    "Sessão / janela / painel",
    "Janelas",
    "Painéis",
    "Estado",
    "Criada há",
    "Pasta",
    "Rodando",
];
/// O painel compacto para na pasta. Prefixo exato do ampliado, e não um
/// subconjunto salteado: as marcas guardam o índice da coluna que seguem, e suprimir uma
/// do meio faria a marca de pasta passar a comparar contra outra coisa.
const SESSION_COMPACT: usize = 6;

const SESSION_MARKS: [MarkKind; 2] = [
    MarkKind {
        name: "sessão",
        column: 0,
        numeric: false,
        help: "Segue uma sessão pelo nome — as janelas e painéis dela vêm junto.",
    },
    MarkKind {
        name: "pasta",
        column: 5,
        numeric: false,
        help: "Segue tudo que foi aberto sob um caminho — «/home/eu/git» pega o que está lá dentro.",
    },
];

/// Há quanto tempo, em segundos desde a época, ou vazio quando o tmux não datou.
fn age(created: i64) -> String {
    if created <= 0 {
        return String::new();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    format::human_duration((now - created).max(0) as u64)
}

/// O estado de uma sessão: se há alguém olhando para ela agora, e quantos.
///
/// Por extenso e não «sim»/«não»: a coluna é compartilhada com janelas e painéis, que
/// dizem «ativa» e «ativo» nela. Um «sim» solto no meio disso não diz sim para quê.
fn attached_cell(attached: usize) -> String {
    match attached {
        0 => "sem cliente".to_string(),
        1 => "anexada".to_string(),
        n => format!("anexada ({n})"),
    }
}

// --- sessões -------------------------------------------------------------------------

#[derive(Default)]
pub struct SessionsMonitor {
    note: Option<String>,
}

impl TableMonitor for SessionsMonitor {
    fn id(&self) -> &'static str {
        "tmux_sessions"
    }

    fn title(&self) -> &'static str {
        "Sessões"
    }

    fn tab(&self) -> Tab {
        Tab::Tmux
    }

    fn actions_on_enter(&self) -> bool {
        true
    }

    fn tree(&self) -> bool {
        true
    }

    /// Aberta até o painel. São três níveis rasos, e o painel é o único que tem um
    /// processo — abrir a árvore pela metade esconderia justamente o que se veio ver.
    fn expand_all(&self) -> bool {
        true
    }

    fn has_detail(&self) -> bool {
        true
    }

    fn headers(&self) -> &'static [&'static str] {
        &SESSION_HEADERS
    }

    fn compact_headers(&self) -> &'static [&'static str] {
        &SESSION_HEADERS[..SESSION_COMPACT]
    }

    fn mark_kinds(&self) -> &'static [MarkKind] {
        &SESSION_MARKS
    }

    /// Quantas **sessões** o painel compacto pede — não quantas linhas.
    ///
    /// A distinção importa porque este painel mostra a árvore: trinta sessões podem
    /// render muito mais que trinta linhas, e o que sobra é recortado ao desenhar. O
    /// número é o teto de quantas sessões vale a pena montar, não do tamanho da lista.
    fn compact_rows(&self) -> usize {
        30
    }

    /// Árvore sessão → janela → painel nos dois tamanhos, aberta até o painel.
    ///
    /// Ao contrário das outras tabelas grandes daqui, o painel compacto **não** achata a
    /// árvore. Elas achatam porque agrupar custa linhas que o painel pequeno não tem — a
    /// de processos tem centenas de folhas. Esta tem três níveis rasos, e o nível de
    /// baixo é onde está o processo: achatá-la mostraria uma lista de nomes de sessão,
    /// que é a informação menos útil das três.
    ///
    /// O que o limite corta são **sessões**, não linhas — ver `compact_rows`.
    fn sample(&mut self, state: &SystemState, limit: Option<usize>) -> Vec<TableRow> {
        let Some(tmux) = tmux(state) else {
            return Vec::new();
        };
        self.note = Some(note_for(tmux));
        tree_rows(tmux, limit)
    }

    /// Em tela cheia a forma fica congelada e só os valores se mexem — a lista não pode
    /// reordenar debaixo de quem está lendo. Casadas pela chave, que é o que identifica
    /// uma linha desta tabela: sessão, janela e painel não compartilham espaço de pid.
    fn refresh_values(&mut self, state: &SystemState, rows: &mut [TableRow]) {
        let Some(tmux) = tmux(state) else {
            return;
        };
        for row in rows.iter_mut() {
            match parse_key(&row.key) {
                Some(RowKey::Session(name)) => {
                    if let Some(session) = tmux.snapshot.session_named(name) {
                        row.cells = session_cells(tmux, session);
                    }
                }
                Some(RowKey::Window(session, index)) => {
                    if let Some(window) =
                        tmux.snapshot.windows_of(session).find(|w| w.index == index)
                    {
                        row.cells = window_cells(&tmux.snapshot, window);
                    }
                }
                Some(RowKey::Pane(session, window, index)) => {
                    if let Some(pane) = tmux
                        .snapshot
                        .panes_of(session, window)
                        .find(|p| p.index == index)
                    {
                        row.cells = pane_cells(tmux, pane);
                    }
                }
                None => {}
            }
        }
    }

    fn note(&self) -> Option<String> {
        self.note.clone()
    }

    fn detail(&mut self, state: &SystemState, row: &TableRow) -> Option<Detail> {
        let tmux = tmux(state)?;
        match parse_key(&row.key)? {
            RowKey::Session(name) => {
                let session = tmux.snapshot.session_named(name)?;
                Some(session_detail(tmux, session))
            }
            RowKey::Window(session, index) => {
                let window = tmux
                    .snapshot
                    .windows_of(session)
                    .find(|w| w.index == index)?;
                Some(window_detail(tmux, window))
            }
            RowKey::Pane(session, window, index) => {
                let pane = tmux
                    .snapshot
                    .panes_of(session, window)
                    .find(|p| p.index == index)?;
                Some(pane_detail(pane))
            }
        }
    }
}

/// A linha da borda: com quem estamos falando, quantas sessões há, e onde estamos.
fn note_for(tmux: &Tmux) -> String {
    let mut parts = vec![format!("tmux {}", tmux.version)];
    parts.push(format!(
        "{} sessão(ões), {} painel(is)",
        tmux.snapshot.sessions.len(),
        tmux.snapshot.pane_count()
    ));
    // O que muda o que «entrar» faz. Dito onde a decisão é tomada, e não escondido no
    // resumo: é a diferença entre uma tecla que aninha e uma que troca a sessão de fora.
    if let Some(inside) = &tmux.inside {
        parts.push(format!("estamos dentro de «{inside}»"));
    }
    if let Some(error) = &tmux.error {
        parts.push(error.clone());
    }
    if let Some((message, ok)) = &tmux.outcome {
        parts.push(if *ok {
            message.clone()
        } else {
            format!("falhou: {message}")
        });
    }
    parts.join(" · ")
}

/// O painel ativo de uma janela — de onde saem a pasta e o comando que representam a
/// janela inteira nas colunas que os três níveis dividem.
fn active_pane<'a>(
    snapshot: &'a crate::tmux::Snapshot,
    session: &str,
    window: u32,
) -> Option<&'a crate::tmux::Pane> {
    snapshot.panes_of(session, window).find(|p| p.active)
}

fn session_cells(tmux: &Tmux, session: &crate::tmux::Session) -> Vec<String> {
    let panes: usize = tmux
        .snapshot
        .windows_of(&session.name)
        .map(|w| w.panes)
        .sum();
    // O painel ativo da janela ativa: o que reconhece a sessão sem entrar nela.
    let active = tmux
        .snapshot
        .windows_of(&session.name)
        .find(|w| w.active)
        .and_then(|w| active_pane(&tmux.snapshot, &session.name, w.index));
    let name = match tmux.inside.as_deref() == Some(session.name.as_str()) {
        // Marcada na própria célula, porque é a linha em que metade das ações está
        // bloqueada — e descobrir isso só ao abrir o menu seria descobrir tarde demais.
        true => format!("{} (aqui)", session.name),
        false => session.name.clone(),
    };
    vec![
        name,
        session.windows.to_string(),
        panes.to_string(),
        attached_cell(session.attached),
        age(session.created),
        session.path.clone(),
        active.map(|p| p.command.clone()).unwrap_or_default(),
    ]
}

fn window_cells(snapshot: &crate::tmux::Snapshot, window: &crate::tmux::Window) -> Vec<String> {
    let active = active_pane(snapshot, &window.session, window.index);
    vec![
        format!("{}: {}", window.index, window.name),
        // Uma janela não tem janelas dentro.
        String::new(),
        window.panes.to_string(),
        if window.active {
            "ativa".to_string()
        } else {
            String::new()
        },
        // Só a sessão é datada pelo tmux.
        String::new(),
        active.map(|p| p.path.clone()).unwrap_or_default(),
        active.map(|p| p.command.clone()).unwrap_or_default(),
    ]
}

fn pane_cells(tmux: &Tmux, pane: &crate::tmux::Pane) -> Vec<String> {
    let id = match tmux.inside_pane.as_deref() == Some(pane.id.as_str()) {
        // O painel em que o próprio programa está desenhando, dito na linha: é o único
        // que não dá para matar, e é onde a pessoa está olhando.
        true => format!("{} (aqui)", pane.id),
        false => pane.id.clone(),
    };
    vec![
        id,
        String::new(),
        String::new(),
        if pane.active {
            "ativo".to_string()
        } else {
            String::new()
        },
        String::new(),
        pane.path.clone(),
        pane.command.clone(),
    ]
}

/// Sessão → janela → painel, na forma que a máquina de árvore da tabela já desenha.
///
/// `max_sessions` corta pela sessão e não pela linha: metade de uma árvore é um painel
/// pendurado numa janela cujo pai não está na tela.
fn tree_rows(tmux: &Tmux, max_sessions: Option<usize>) -> Vec<TableRow> {
    let mut rows = Vec::new();
    let mut synthetic = 0usize;
    let all = &tmux.snapshot.sessions;
    let sessions = &all[..max_sessions.unwrap_or(all.len()).min(all.len())];
    let last_session = sessions.len().saturating_sub(1);

    for (s_index, session) in sessions.iter().enumerate() {
        let windows: Vec<&crate::tmux::Window> = tmux.snapshot.windows_of(&session.name).collect();
        let session_last = s_index == last_session;
        let key = synthetic_key(synthetic);
        synthetic += 1;
        rows.push(TableRow {
            cells: session_cells(tmux, session),
            pid: key,
            depth: 0,
            is_last_sibling: session_last,
            guides: Vec::new(),
            mark: None,
            child_count: windows.len(),
            descendant_pids: Vec::new(),
            key: format!("s\x1f{}", session.name),
        });

        let last_window = windows.len().saturating_sub(1);
        for (w_index, window) in windows.iter().enumerate() {
            let panes: Vec<&crate::tmux::Pane> = tmux
                .snapshot
                .panes_of(&session.name, window.index)
                .collect();
            let window_last = w_index == last_window;
            let key = synthetic_key(synthetic);
            synthetic += 1;
            rows.push(TableRow {
                cells: window_cells(&tmux.snapshot, window),
                pid: key,
                depth: 1,
                is_last_sibling: window_last,
                guides: vec![session_last],
                mark: None,
                child_count: panes.len(),
                descendant_pids: panes.iter().map(|p| p.pid).collect(),
                key: format!("w\x1f{}\x1f{}", session.name, window.index),
            });

            let last_pane = panes.len().saturating_sub(1);
            for (p_index, pane) in panes.iter().enumerate() {
                rows.push(TableRow {
                    cells: pane_cells(tmux, pane),
                    pid: pane.pid,
                    depth: 2,
                    is_last_sibling: p_index == last_pane,
                    guides: vec![session_last, window_last],
                    mark: None,
                    child_count: 0,
                    descendant_pids: Vec::new(),
                    key: format!(
                        "p\x1f{}\x1f{}\x1f{}",
                        session.name, window.index, pane.index
                    ),
                });
            }
        }
    }
    rows
}

/// O que a chave de uma linha diz sobre ela.
///
/// As três coisas moram na mesma tabela e não compartilham espaço de identidade — um
/// índice de janela e um de painel são ambos `0` o tempo todo —, então a chave carrega o
/// que a linha é junto com onde ela está.
///
/// O `\x1f` aqui é nosso e nunca sai do programa — não tem relação com o separador que
/// `tmux::cli` usa para falar com o binário, que é uma tabulação por razões que só valem
/// lá.
pub enum RowKey<'a> {
    Session(&'a str),
    Window(&'a str, u32),
    Pane(&'a str, u32, u32),
}

pub fn parse_key(key: &str) -> Option<RowKey<'_>> {
    let mut parts = key.split('\x1f');
    match parts.next()? {
        "s" => Some(RowKey::Session(parts.next()?)),
        "w" => {
            let session = parts.next()?;
            Some(RowKey::Window(session, parts.next()?.parse().ok()?))
        }
        "p" => {
            let session = parts.next()?;
            let window = parts.next()?.parse().ok()?;
            Some(RowKey::Pane(session, window, parts.next()?.parse().ok()?))
        }
        _ => None,
    }
}

/// O sujeito por trás de uma linha, para o menu do `Enter`.
///
/// Os três níveis respondem, porque nos três há o que fazer: uma sessão se entra e se
/// mata, uma janela se renomeia e se mata, e um painel é onde de fato existe um processo
/// para matar. Parar na janela deixaria a única linha que aponta para um processo sendo a
/// única sem menu.
pub fn subject_of(tmux: &Tmux, key: &str) -> Option<Subject> {
    match parse_key(key)? {
        RowKey::Session(name) => tmux
            .snapshot
            .session_named(name)
            .cloned()
            .map(Subject::Session),
        RowKey::Window(session, index) => tmux
            .snapshot
            .windows_of(session)
            .find(|w| w.index == index)
            .cloned()
            .map(Subject::Window),
        RowKey::Pane(session, window, index) => tmux
            .snapshot
            .panes_of(session, window)
            .find(|p| p.index == index)
            .cloned()
            .map(Subject::Pane),
    }
}

fn session_detail(tmux: &Tmux, session: &crate::tmux::Session) -> Detail {
    let mut about = DetailSection::new("Sessão");
    about.push("nome", session.name.clone());
    about.push("id", session.id.clone());
    about.push("pasta base", session.path.clone());
    about.push("criada há", age(session.created));
    about.push("clientes anexados", session.attached.to_string());
    if tmux.inside.as_deref() == Some(session.name.as_str()) {
        about.push("nota", "é a sessão onde o monitorzinho está rodando");
    }

    let mut layout = DetailSection::new("Janelas");
    for window in tmux.snapshot.windows_of(&session.name) {
        let running: Vec<String> = tmux
            .snapshot
            .panes_of(&session.name, window.index)
            .map(|pane| pane.command.clone())
            .filter(|command| !command.is_empty())
            .collect();
        layout.push(
            &format!("{}: {}", window.index, window.name),
            format!(
                "{} painel(is){}{}",
                window.panes,
                if window.active { " · ativa" } else { "" },
                match running.is_empty() {
                    true => String::new(),
                    false => format!(" · {}", running.join(", ")),
                }
            ),
        );
    }

    Detail {
        title: format!("sessão {}", session.name),
        gone_note: "encerrada",
        sections: vec![about, layout],
        rates: None,
        handoffs: Vec::new(),
        handoff_title: "",
    }
}

fn window_detail(tmux: &Tmux, window: &crate::tmux::Window) -> Detail {
    let mut about = DetailSection::new("Janela");
    about.push("sessão", window.session.clone());
    about.push("índice", window.index.to_string());
    about.push("nome", window.name.clone());
    about.push("ativa", if window.active { "sim" } else { "não" });

    let mut panes = DetailSection::new("Painéis");
    for pane in tmux.snapshot.panes_of(&window.session, window.index) {
        panes.push(
            &format!("{} {}", pane.id, if pane.active { "•" } else { "" }),
            format!(
                "{} · pid {} · {}×{} · {}",
                pane.command, pane.pid, pane.width, pane.height, pane.path
            ),
        );
    }

    Detail {
        title: format!("janela {}:{} {}", window.session, window.index, window.name),
        gone_note: "fechada",
        sections: vec![about, panes],
        rates: None,
        handoffs: Vec::new(),
        handoff_title: "",
    }
}

fn pane_detail(pane: &crate::tmux::Pane) -> Detail {
    let mut about = DetailSection::new("Painel");
    about.push("id", pane.id.clone());
    about.push("sessão", pane.session.clone());
    about.push("janela", pane.window_index.to_string());
    about.push("posição", pane.index.to_string());
    about.push("ativo", if pane.active { "sim" } else { "não" });
    about.push("tamanho", format!("{}×{}", pane.width, pane.height));

    let mut process = DetailSection::new("Processo");
    process.push("comando", pane.command.clone());
    // O pid é o que liga esta linha à aba de Processos: o painel é o único nível do tmux
    // que tem um, e é por ele que se acha o mesmo processo do outro lado do programa.
    if pane.pid != 0 {
        process.push("pid", pane.pid.to_string());
    }
    process.push("pasta", pane.path.clone());

    Detail {
        title: format!("painel {} de {}", pane.id, pane.session),
        gone_note: "fechado",
        sections: vec![about, process],
        rates: None,
        handoffs: Vec::new(),
        handoff_title: "",
    }
}

// --- janelas -------------------------------------------------------------------------

const WINDOW_HEADERS: [&str; 6] = ["Sessão", "Janela", "Painéis", "Ativa", "Rodando", "Pasta"];

/// Todas as janelas de todas as sessões, planas.
///
/// A árvore da tabela de cima responde «o que tem dentro desta sessão». Esta responde a
/// pergunta oposta, que é a que se faz quando não se lembra onde uma coisa ficou: «em que
/// sessão é que aquilo estava rodando».
#[derive(Default)]
pub struct WindowsMonitor;

impl TableMonitor for WindowsMonitor {
    fn id(&self) -> &'static str {
        "tmux_windows"
    }

    fn title(&self) -> &'static str {
        "Janelas"
    }

    fn tab(&self) -> Tab {
        Tab::Tmux
    }

    fn actions_on_enter(&self) -> bool {
        true
    }

    fn has_detail(&self) -> bool {
        true
    }

    fn headers(&self) -> &'static [&'static str] {
        &WINDOW_HEADERS
    }

    fn sample(&mut self, state: &SystemState, limit: Option<usize>) -> Vec<TableRow> {
        let Some(tmux) = tmux(state) else {
            return Vec::new();
        };
        let take = limit.unwrap_or(usize::MAX);
        tmux.snapshot
            .windows
            .iter()
            .take(take)
            .map(|window| {
                let active = tmux
                    .snapshot
                    .panes_of(&window.session, window.index)
                    .find(|p| p.active);
                let mut row = TableRow::leaf(
                    vec![
                        window.session.clone(),
                        format!("{}: {}", window.index, window.name),
                        window.panes.to_string(),
                        if window.active {
                            "sim".to_string()
                        } else {
                            String::new()
                        },
                        active.map(|p| p.command.clone()).unwrap_or_default(),
                        active.map(|p| p.path.clone()).unwrap_or_default(),
                    ],
                    active.map(|p| p.pid).unwrap_or(0),
                );
                row.key = format!("w\x1f{}\x1f{}", window.session, window.index);
                row
            })
            .collect()
    }

    fn detail(&mut self, state: &SystemState, row: &TableRow) -> Option<Detail> {
        let tmux = tmux(state)?;
        let RowKey::Window(session, index) = parse_key(&row.key)? else {
            return None;
        };
        let window = tmux
            .snapshot
            .windows_of(session)
            .find(|w| w.index == index)?;
        Some(window_detail(tmux, window))
    }
}

// --- resumo --------------------------------------------------------------------------

/// Os mesmos cabeçalhos do resumo da aba Containers, de propósito: são a mesma coisa —
/// uma lista de fatos — e compartilhar o par dá a elas as mesmas larguras sem uma regra
/// nova.
const SUMMARY_HEADERS: [&str; 2] = ["Item", "Situação"];

/// Os fatos sobre o servidor, não sobre uma sessão.
///
/// O que está aqui é o que explica o comportamento do resto da aba — em particular se
/// estamos dentro do tmux, que é o que decide se «entrar» aninha ou troca de sessão.
pub struct SummaryMonitor;

impl TableMonitor for SummaryMonitor {
    fn id(&self) -> &'static str {
        "tmux_summary"
    }

    fn title(&self) -> &'static str {
        "Resumo"
    }

    fn tab(&self) -> Tab {
        Tab::Tmux
    }

    fn headers(&self) -> &'static [&'static str] {
        &SUMMARY_HEADERS
    }

    fn sample(&mut self, state: &SystemState, _limit: Option<usize>) -> Vec<TableRow> {
        let Some(tmux) = tmux(state) else {
            return Vec::new();
        };
        let snapshot = &tmux.snapshot;
        let attached = snapshot.sessions.iter().filter(|s| s.attached > 0).count();
        let mut rows = vec![
            ("versão", format!("tmux {}", tmux.version)),
            ("socket", tmux.socket_label()),
            ("sessões", snapshot.sessions.len().to_string()),
            (
                "anexadas",
                format!("{attached} de {}", snapshot.sessions.len()),
            ),
            ("janelas", snapshot.windows.len().to_string()),
            ("painéis", snapshot.pane_count().to_string()),
        ];
        rows.push((
            "monitorzinho",
            match &tmux.inside {
                // Escrito por extenso porque é a explicação de por que o menu de uma
                // sessão oferece duas maneiras de entrar em vez de uma.
                Some(session) => format!("dentro da sessão «{session}» — entrar aninha"),
                None => "fora do tmux — entrar anexa direto".to_string(),
            },
        ));
        if let Some(error) = &tmux.error {
            rows.push(("erro", error.clone()));
        }
        rows.into_iter()
            .map(|(label, value)| TableRow::leaf(vec![label.to_string(), value], 0))
            .collect()
    }
}
