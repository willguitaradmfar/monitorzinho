//! Sherlock Database: lendo um banco de produção até ele confessar.
//!
//! Tudo aqui é pergunta, nunca ordem. Nenhum `CREATE`, nenhum `ANALYZE`, nenhum
//! `pg_stat_statements_reset`, nenhum profiler ligado pelas costas de ninguém — uma
//! ferramenta apontada para produção ganha o lugar dela sendo impossível de culpar, e o
//! único jeito de ser impossível de culpar é nunca ter tido como mudar nada. O lado
//! Postgres diz isso ao próprio servidor (`pg::Conn::harden`) e pede confirmação; o lado
//! MongoDB só manda comandos de uma lista fixa de leitores.
//!
//! O custo é a outra metade da promessa. Ler catálogo é barato; medir o tamanho de cada
//! tabela e estimar o inchaço não é, e num servidor ocupado a diferença importa mais que
//! os achados a mais. Por isso a profundidade é escolhida na criação e cada verificação
//! declara a que profundidade pertence.
//!
//! A tela não é um log: é um quadro de cartões — ver `tools::Board`. Ele é preenchido por
//! uma investigação inteira quando a tela abre, e depois **só** quando alguém pede outra
//! com Ctrl+R. Não há laço de atualização: um painel que se reinvestigasse sozinho a cada
//! poucos segundos seria, num banco de produção, exatamente o tipo de carga que ele
//! existe para encontrar. O que está na tela é o retrato da última leitura, e o cabeçalho
//! diz de quando ela é.
//!
//! Fechou a tela, nada mais é perguntado e a conexão é fechada: uma ferramenta que
//! continuasse falando com seis bancos de produção porque as execuções ficaram salvas
//! seria pior do que ferramenta nenhuma.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::painel::{Layout, Pane, Row, Tone};

use super::{
    Board, Card, EventKind, Execution, MARK_ALERTA, MARK_AVISO, MARK_SUGESTAO, ParamSpec, Recorder,
    Tool, lock_board,
};

mod bson;
mod mongo;
mod mongo_checks;
mod pg;
mod pg_checks;
pub(crate) mod scram;
mod url;

const ENGINES: &[&str] = &["PostgreSQL", "MongoDB"];
const DEPTHS: &[&str] = &["raso", "médio", "profundo"];

/// How long a single question may take before the server is told to abandon it. Also the
/// socket timeout. Nothing this tool asks should come close on a healthy server — on an
/// unhealthy one, which is where it will actually be pointed, it is the difference
/// between a check that gives up and a session that sits there.
const STATEMENT_TIMEOUT: Duration = Duration::from_secs(15);
/// How long the loop waits between two looks at "was I asked to investigate again?".
/// Nothing is asked of the database in between — this is a sleeping thread, not a poll.
const IDLE_SLICE: Duration = Duration::from_millis(120);

/// Which engine is on the other end. Chosen in the form rather than sniffed from the URL:
/// the scheme would give it away, but a wrong guess would be discovered halfway through a
/// handshake against a production server, and saying which one you meant costs one
/// keypress.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    Postgres,
    Mongo,
}

/// How hard to look, and therefore how much of the server's time to spend.
///
/// Ordered, so a check asks `depth >= Depth::Medio` rather than matching every variant —
/// each level is the one below it plus more.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Depth {
    /// Catalogue and counters only. Indexes, settings, connections, locks — everything
    /// the server already knows without touching a table.
    Raso,
    /// Plus the statistics that need the extensions and views a busy server keeps: slow
    /// queries, vacuum debt, replication, temp files.
    Medio,
    /// Plus the measurements that cost: every relation's size, sequence exhaustion,
    /// foreign keys with no index behind them.
    Profundo,
}

impl Depth {
    fn from(text: &str) -> Self {
        match text {
            "profundo" => Depth::Profundo,
            "médio" | "medio" => Depth::Medio,
            _ => Depth::Raso,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Depth::Raso => "raso",
            Depth::Medio => "médio",
            Depth::Profundo => "profundo",
        }
    }
}

/// What to do about TLS, in libpq's vocabulary because that is what connection strings
/// are written in.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Ssl {
    /// Encrypt if the server offers it, carry on in the clear if it doesn't.
    Prefer,
    /// Encrypt or fail, without judging the certificate — what most managed databases
    /// expect, since their certificates are signed by their own CA.
    Require,
    /// Encrypt, and check the certificate really belongs to this host.
    Verify,
    Disable,
}

impl Ssl {
    fn from_sslmode(mode: &str) -> Self {
        match mode.trim().to_ascii_lowercase().as_str() {
            "disable" | "false" | "off" => Ssl::Disable,
            "require" => Ssl::Require,
            "verify-ca" | "verify-full" | "true" => Ssl::Verify,
            _ => Ssl::Prefer,
        }
    }
}

pub struct SherlockTool;

impl Tool for SherlockTool {
    fn id(&self) -> &'static str {
        "sherlock"
    }

    fn name(&self) -> &'static str {
        "Sherlock Database"
    }

    fn description(&self) -> &'static str {
        "Investiga um Postgres ou MongoDB inteiro sem escrever nada: queries lentas, índices que faltam e que sobram, vacuum, bloqueios, volumes e ajustes — num painel que se atualiza sozinho enquanto estiver aberto"
    }

    fn params(&self) -> Vec<ParamSpec> {
        vec![
            ParamSpec::choice(
                "motor",
                "Banco de dados",
                ENGINES,
                "Qual servidor está do outro lado. Decide o protocolo e o repertório de perguntas",
            ),
            ParamSpec::text(
                "url",
                "String de conexão",
                "",
                "postgres://usuario:senha@host:5432/base ou mongodb://usuario:senha@host:27017/base. Fica gravada junto com a execução, senha inclusive",
            ),
            ParamSpec::text(
                "base",
                "Base (opcional)",
                "",
                "Vazio usa a base que vier na URL. Preencha para investigar outra base do mesmo servidor sem reescrever a string",
            ),
            ParamSpec::choice(
                "nivel",
                "Profundidade",
                DEPTHS,
                "Quanto do servidor investigar. 'raso' lê catálogo e contadores (índices, ajustes, conexões); 'médio' soma queries lentas, vacuum e replicação; 'profundo' mede tamanho de cada tabela, chaves sem índice e sequências — mais achados, mais leitura no servidor",
            ),
        ]
    }

    fn summarize(&self, params: &HashMap<&'static str, String>) -> String {
        let plan = Plan::from(params);
        let target = match &plan {
            Ok(plan) => plan.target(),
            // Never the URL itself: it carries a password, and this string goes on a row
            // that is on screen whenever the tab is.
            Err(_) => "string de conexão inválida".to_string(),
        };
        let depth = params.get("nivel").map(String::as_str).unwrap_or("raso");
        format!("{target} · {depth}")
    }

    /// Always. A full inspection is somebody else's production server being read, and the
    /// one thing it must never do is happen because the app started.
    fn on_demand(&self, _params: &HashMap<&'static str, String>) -> bool {
        true
    }

    fn off_note(&self, _params: &HashMap<&'static str, String>) -> Option<String> {
        Some("desligada: nenhuma conexão é aberta, nem quando a tela dela é aberta".to_string())
    }

    fn start(&self, id: u64, params: &HashMap<&'static str, String>) -> Result<Execution, String> {
        let plan = Plan::from(params)?;
        let (execution, recorder) = Execution::new(id, self.name(), self.summarize(params));
        recorder.record(
            0,
            EventKind::Note(format!(
                "pronto para investigar {} em nível {}. Nada roda até você abrir — e nada é alterado quando rodar",
                plan.target(),
                plan.depth.label()
            )),
        );
        let board = Arc::new(Mutex::new(Board {
            note: "nada investigado ainda — abra para começar".to_string(),
            ..Board::default()
        }));
        Ok(execution.on_demand().with_board(board))
    }

    /// Opening doesn't start anything by itself: `set_watched` does, on the tick right
    /// after, and it is also what stops it. Keeping both halves in one place is what
    /// makes "só enquanto a tela está aberta" true rather than nearly true.
    fn open(&self, _execution: &Execution, _params: &HashMap<&'static str, String>) {}

    /// Ctrl+R, ou 'r' na lista: investiga de novo, agora.
    ///
    /// Com a tela aberta é um recado para o laço que já tem a conexão na mão — resposta
    /// imediata, sem handshake. Sem a tela aberta, abre uma conexão, investiga uma vez e
    /// fecha.
    fn rerun(&self, execution: &Execution, params: &HashMap<&'static str, String>) {
        if let Some(driver) = drivers().get(&execution.id) {
            driver.again.store(true, Ordering::Relaxed);
            return;
        }
        let Ok(plan) = Plan::from(params) else {
            return;
        };
        let (Some(board), false) = (execution.board().cloned(), execution.is_off()) else {
            return;
        };
        let recorder = execution.recorder();
        let finished = execution.finish_flag();
        finished.store(false, Ordering::Relaxed);
        thread::spawn(move || {
            let stop = AtomicBool::new(false);
            investigate(&plan, &recorder, &board, &stop);
            recorder.ran();
            finished.store(true, Ordering::Relaxed);
        });
    }

    /// The screen is the switch. While it is up, the board refreshes; when it goes away,
    /// the connection is closed and nothing else is asked of the server.
    fn set_watched(
        &self,
        execution: &Execution,
        params: &HashMap<&'static str, String>,
        watched: bool,
    ) {
        if !watched {
            stop_driver(execution.id);
            return;
        }
        let Ok(plan) = Plan::from(params) else {
            return;
        };
        let (Some(board), false) = (execution.board().cloned(), execution.is_off()) else {
            return;
        };
        let Some(driver) = claim_driver(execution.id) else {
            // Already refreshing: coming back to the same screen shouldn't start a second
            // one against the same database.
            return;
        };
        let recorder = execution.recorder();
        let finished = execution.finish_flag();
        finished.store(false, Ordering::Relaxed);
        thread::spawn(move || {
            drive(&plan, &recorder, &board, &driver);
            recorder.ran();
            finished.store(true, Ordering::Relaxed);
            release_driver(&driver);
        });
    }

    fn columns(&self, execution: &Execution) -> (String, String) {
        execution.outcome()
    }
}

/// Everything the form amounts to, already validated.
pub struct Plan {
    engine: Engine,
    url: String,
    database: String,
    depth: Depth,
}

impl Plan {
    fn from(params: &HashMap<&'static str, String>) -> Result<Self, String> {
        let get = |key| params.get(key).map(String::as_str).unwrap_or("").trim();
        let engine = match get("motor") {
            "MongoDB" => Engine::Mongo,
            _ => Engine::Postgres,
        };
        let url = get("url").to_string();
        if url.is_empty() {
            return Err("informe a string de conexão".to_string());
        }
        let plan = Self {
            engine,
            url,
            database: get("base").to_string(),
            depth: Depth::from(get("nivel")),
        };
        // Parsed here, while the user is still looking at the form: a URL that can't be
        // read is the one failure that must never wait until someone opens the module.
        plan.check()?;
        Ok(plan)
    }

    fn check(&self) -> Result<(), String> {
        match self.engine {
            Engine::Postgres => pg::Target::parse(&self.url, &self.database).map(|_| ()),
            Engine::Mongo => mongo::Target::parse(&self.url, &self.database).map(|_| ()),
        }
    }

    /// Who and where, never the password.
    fn target(&self) -> String {
        match self.engine {
            Engine::Postgres => pg::Target::parse(&self.url, &self.database)
                .map(|target| format!("pg {}", target.summary()))
                .unwrap_or_default(),
            Engine::Mongo => mongo::Target::parse(&self.url, &self.database)
                .map(|target| format!("mongo {}", target.summary()))
                .unwrap_or_default(),
        }
    }
}

/// A tela está aberta: uma investigação agora, e depois só quando pedirem.
///
/// A conexão fica de pé entre uma e outra, parada. É o que faz o Ctrl+R responder na
/// hora, e é também o que dá sentido ao cartão «Movimento»: ele compara os contadores
/// desta investigação com os da anterior, e sem a mesma sessão não haveria «anterior».
fn drive(plan: &Plan, rec: &Recorder, board: &Arc<Mutex<Board>>, driver: &Driver) {
    let mut report = Report::new(rec, Arc::clone(board));
    let mut session = match Session::open(plan) {
        Ok(session) => session,
        Err(problem) => {
            report.failed(&problem);
            rec.record(0, EventKind::Error(problem.clone()));
            rec.report("falhou", problem);
            // Não há o que tentar de novo sozinho: o endereço está errado, a senha está
            // errada, ou o servidor não está lá. Ctrl+R tenta outra vez quando quem está
            // olhando decidir.
            return;
        }
    };

    loop {
        let started = Instant::now();
        report.begin();
        match session.pass(&mut report) {
            Ok(()) => report.end(started.elapsed()),
            Err(problem) => {
                // A conexão caiu no meio — um restart do outro lado, uma queda de rede,
                // um pooler nos reciclando. A próxima investigação abre outra.
                report.failed(&problem);
                rec.record(0, EventKind::Error(format!("{problem} — conexão perdida")));
                match Session::open(plan) {
                    Ok(fresh) => session = fresh,
                    Err(_) => return,
                }
            }
        }
        // Daqui até o próximo Ctrl+R, nada é perguntado ao banco.
        while !driver.stop.load(Ordering::Relaxed) && !rec.stopping() {
            if driver.again.swap(false, Ordering::Relaxed) {
                break;
            }
            thread::sleep(IDLE_SLICE);
        }
        if driver.stop.load(Ordering::Relaxed) || rec.stopping() {
            lock_board(board).working = false;
            return;
        }
    }
}

/// Uma investigação avulsa, sem tela aberta: conecta, lê, fecha.
fn investigate(plan: &Plan, rec: &Recorder, board: &Arc<Mutex<Board>>, stop: &AtomicBool) {
    let mut report = Report::new(rec, Arc::clone(board));
    let mut session = match Session::open(plan) {
        Ok(session) => session,
        Err(problem) => {
            report.failed(&problem);
            rec.record(0, EventKind::Error(problem.clone()));
            rec.report("falhou", problem);
            return;
        }
    };
    if stop.load(Ordering::Relaxed) {
        return;
    }
    let started = Instant::now();
    report.begin();
    match session.pass(&mut report) {
        Ok(()) => report.end(started.elapsed()),
        Err(problem) => report.failed(&problem),
    }
}

/// One live inspection, whichever engine it is. Holds the connection and everything a
/// pass needs to compare against the pass before it.
enum Session {
    Postgres(Box<pg_checks::Session>),
    Mongo(Box<mongo_checks::Session>),
}

impl Session {
    fn open(plan: &Plan) -> Result<Self, String> {
        Ok(match plan.engine {
            Engine::Postgres => Session::Postgres(Box::new(pg_checks::Session::open(plan)?)),
            Engine::Mongo => Session::Mongo(Box::new(mongo_checks::Session::open(plan)?)),
        })
    }

    fn pass(&mut self, report: &mut Report) -> Result<(), String> {
        match self {
            Session::Postgres(session) => session.pass(report),
            Session::Mongo(session) => session.pass(report),
        }
    }
}

/// O que comanda um laço aberto: a bandeira que o encerra, e a que pede outra
/// investigação.
struct Driver {
    stop: Arc<AtomicBool>,
    again: Arc<AtomicBool>,
}

/// The refresh loops running right now, by execution.
///
/// A `Tool` is one shared object with no room to keep anything per execution, and this is
/// the one thing that has to be kept: the handle that stops a loop when its screen closes.
static DRIVERS: LazyLock<Mutex<HashMap<u64, Driver>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn drivers() -> std::sync::MutexGuard<'static, HashMap<u64, Driver>> {
    DRIVERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn claim_driver(execution: u64) -> Option<Driver> {
    let mut drivers = drivers();
    if drivers.contains_key(&execution) {
        return None;
    }
    let driver = Driver {
        stop: Arc::new(AtomicBool::new(false)),
        again: Arc::new(AtomicBool::new(false)),
    };
    drivers.insert(
        execution,
        Driver {
            stop: Arc::clone(&driver.stop),
            again: Arc::clone(&driver.again),
        },
    );
    Some(driver)
}

fn stop_driver(execution: u64) {
    if let Some(driver) = drivers().remove(&execution) {
        driver.stop.store(true, Ordering::Relaxed);
    }
}

/// Drops a finished loop's registration, unless it has already been replaced by a newer
/// one — which is what `stop_driver` leaves behind when a screen is closed and reopened
/// before the old loop noticed.
fn release_driver(driver: &Driver) {
    drivers().retain(|_, registered| !Arc::ptr_eq(&registered.stop, &driver.stop));
}

/// The pass being written: the card under construction, the tally, and the board it all
/// lands on.
///
/// Severity is three words and they are used literally: `grave` is something hurting the
/// database now or about to take it down, `aviso` is something worth fixing on a calm
/// afternoon, and everything else is a measurement. Each problem carries what to do about
/// it — a finding a reader has to go and research is half a finding.
pub struct Report<'a> {
    rec: &'a Recorder,
    board: Arc<Mutex<Board>>,
    graves: usize,
    avisos: usize,
    /// The worst thing seen this pass, for the row's second column.
    headline: String,
    open: Option<Building>,
    /// Whether findings go to the log. The first pass writes the whole report there;
    /// after that only what is new, since the same report every three seconds would bury
    /// the log it is written next to.
    quiet: bool,
    /// Findings already written to the log, so a problem that is still true on the next
    /// pass doesn't get announced again.
    logged: HashSet<String>,
}

/// A card being filled in.
struct Building {
    key: String,
    title: String,
    /// What the grid will show: the few lines that matter.
    facts: Vec<(String, String, Tone)>,
    /// A listagem da seção, em colunas. Vazia numa seção que não tem o que tabular.
    headers: Vec<String>,
    rows: Vec<Row>,
    /// A prosa: o que a listagem quer dizer, e o que fazer a respeito.
    lines: Vec<(String, Tone)>,
    tone: Tone,
}

impl<'a> Report<'a> {
    fn new(rec: &'a Recorder, board: Arc<Mutex<Board>>) -> Self {
        Self {
            rec,
            board,
            graves: 0,
            avisos: 0,
            headline: String::new(),
            open: None,
            quiet: false,
            logged: HashSet::new(),
        }
    }

    /// Starts a pass. The summary card is put on the board first so it keeps the first
    /// position — and therefore the first shortcut key — for the life of the execution.
    fn begin(&mut self) {
        self.graves = 0;
        self.avisos = 0;
        self.headline.clear();
        self.open = None;
        let mut board = lock_board(&self.board);
        board.working = true;
        if board.cards.is_empty() {
            let mut card = Card::new("resumo", "Resumo");
            card.summary = Pane::Facts {
                title: "Resumo".to_string(),
                rows: vec![("estado".to_string(), "investigando…".to_string(), Tone::Dim)],
            };
            card.height = 6;
            board.cards.push(card);
        }
        drop(board);
        if !self.quiet {
            self.note(String::new());
        }
        // O log recebe a investigação inteira uma vez; da segunda em diante, só o que for
        // novo. Ver `log_finding`.
        self.quiet = true;
    }

    /// Announces a section: closes the card being built and opens the next one.
    pub fn section(&mut self, title: &str) {
        self.flush();
        self.open = Some(Building {
            key: slug(title),
            title: title.to_string(),
            facts: Vec::new(),
            headers: Vec::new(),
            rows: Vec::new(),
            lines: Vec::new(),
            tone: Tone::Normal,
        });
        if !self.quiet {
            self.note(format!("── {title} ──"));
        }
        self.rec.report(self.tally(), title.to_string());
    }

    /// A line of plain narration. Detail only: the grid has no room for prose.
    pub fn line(&mut self, text: impl Into<String>) {
        let text = text.into();
        self.detail(format!("  {text}"), Tone::Dim);
        self.log(format!("  {text}"));
    }

    /// A labelled line. The card's bread and butter: it shows on the grid *and* in the
    /// detail, which is what makes a card readable without being opened.
    pub fn field(&mut self, label: impl AsRef<str>, value: impl AsRef<str>) {
        let (label, value) = (label.as_ref().to_string(), value.as_ref().to_string());
        if value.is_empty() {
            return;
        }
        if let Some(open) = &mut self.open {
            open.facts
                .push((label.clone(), value.clone(), Tone::Normal));
        }
        self.detail(format!("  {label:<26}{value}"), Tone::Normal);
        self.log(format!("  {label:<26}{value}"));
    }

    /// Declara que esta seção tem uma listagem, e quais são as colunas dela.
    ///
    /// O que é lista vira tabela: uma coluna de tempo alinhada à direita ao lado de uma de
    /// texto é a diferença entre correr o olho e ler linha por linha. O que **não** é
    /// lista — o que a listagem quer dizer, e o que fazer a respeito — continua sendo
    /// prosa, embaixo dela.
    pub fn table(&mut self, headers: &[&str]) {
        if let Some(open) = &mut self.open {
            open.headers = headers.iter().map(|h| h.to_string()).collect();
        }
    }

    /// Uma linha da listagem declarada por `table`.
    pub fn cells(&mut self, cells: Vec<String>, tone: Tone) {
        let texto = cells.join("  ");
        if let Some(open) = &mut self.open {
            let mut row = Row::new(cells);
            row.tone = tone;
            open.rows.push(row);
        }
        self.log(format!("      {texto}"));
    }

    /// Uma linha da listagem que também merece aparecer no cartão da grade.
    pub fn cells_headline(&mut self, cells: Vec<String>, tone: Tone) {
        if let Some(open) = &mut self.open
            && open.facts.len() < 8
        {
            let (primeira, resto) = cells.split_at(1.min(cells.len()));
            open.facts.push((
                primeira.first().cloned().unwrap_or_default(),
                resto.join("  "),
                tone,
            ));
        }
        self.cells(cells, tone);
    }

    /// A row of a listing, indented under whatever introduced it.
    pub fn row(&mut self, text: impl Into<String>) {
        let text = text.into();
        self.detail(format!("      {text}"), Tone::Normal);
        self.log(format!("      {text}"));
    }

    /// Nothing wrong here — said out loud, because a card that prints nothing reads as a
    /// card that failed.
    pub fn ok(&mut self, text: impl Into<String>) {
        let text = text.into();
        if let Some(open) = &mut self.open
            && open.facts.len() < 6
        {
            open.facts
                .push((String::new(), format!("✓ {text}"), Tone::Bom));
        }
        self.detail(format!("  ✓ {text}"), Tone::Bom);
        self.log(format!("  ✓ {text}"));
    }

    /// Hurting now.
    pub fn grave(&mut self, what: impl Into<String>, fix: impl Into<String>) {
        let what = what.into();
        self.graves += 1;
        if self.headline.is_empty() || self.graves == 1 {
            self.headline = what.clone();
        }
        if let Some(open) = &mut self.open {
            open.facts.push((String::new(), what.clone(), Tone::Ruim));
            open.tone = Tone::Ruim;
        }
        self.detail(format!("  GRAVE  {what}"), Tone::Ruim);
        self.log_finding(format!("GRAVE  {what}"), true);
        self.suggest(fix);
    }

    /// Worth fixing, not worth waking anyone up for.
    pub fn aviso(&mut self, what: impl Into<String>, fix: impl Into<String>) {
        let what = what.into();
        self.avisos += 1;
        if self.headline.is_empty() {
            self.headline = what.clone();
        }
        if let Some(open) = &mut self.open {
            open.facts
                .push((String::new(), format!("{MARK_AVISO} {what}"), Tone::Aviso));
            if open.tone == Tone::Normal {
                open.tone = Tone::Aviso;
            }
        }
        self.detail(format!("  {MARK_AVISO} {what}"), Tone::Aviso);
        self.log_finding(format!("  {MARK_AVISO} {what}"), false);
        self.suggest(fix);
    }

    /// Something happening right now that this pass caught.
    pub fn alerta(&mut self, what: impl Into<String>, fix: impl Into<String>) {
        let what = what.into();
        if let Some(open) = &mut self.open {
            open.facts.push((
                String::new(),
                format!("{MARK_ALERTA} {what}"),
                Tone::Destaque,
            ));
        }
        self.detail(format!("  {MARK_ALERTA} {what}"), Tone::Destaque);
        self.log_finding(format!("  {MARK_ALERTA} {what}"), false);
        self.suggest(fix);
    }

    fn suggest(&mut self, fix: impl Into<String>) {
        let fix = fix.into();
        if fix.is_empty() {
            return;
        }
        // Spelled out as something to run elsewhere, deliberately: this tool will not run
        // it, and the wording is the reminder of that.
        self.detail(format!("      {MARK_SUGESTAO} {fix}"), Tone::Dim);
        self.log_finding(format!("      {MARK_SUGESTAO} {fix}"), false);
    }

    pub fn stopping(&self) -> bool {
        self.rec.stopping()
    }

    fn detail(&mut self, text: String, tone: Tone) {
        if let Some(open) = &mut self.open {
            open.lines.push((text, tone));
        }
    }

    /// Narration to the log, on the passes that write narration.
    fn log(&mut self, text: String) {
        if !self.quiet {
            self.note(text);
        }
    }

    /// A finding to the log — on every pass, but only the first time it is true. What
    /// makes the log the history of this database instead of a transcript of the tool.
    fn log_finding(&mut self, text: String, bad: bool) {
        if !self.logged.insert(text.clone()) {
            return;
        }
        self.rec.record(
            0,
            if bad {
                EventKind::Error(text)
            } else {
                EventKind::Note(text)
            },
        );
    }

    fn note(&self, text: String) {
        self.rec.record(0, EventKind::Note(text));
    }

    /// Publishes the card under construction.
    fn flush(&mut self) {
        let Some(mut open) = self.open.take() else {
            return;
        };
        // Uma seção que só produziu listagem não pode virar uma moldura vazia na grade:
        // a primeira linha dela vira o resumo, que é melhor que nada e é verdade.
        if open.facts.is_empty() {
            let primeira = open
                .lines
                .iter()
                .find(|(text, _)| !text.trim().is_empty())
                .map(|(text, tone)| (text.trim().to_string(), *tone));
            open.facts.push(match primeira {
                Some((text, tone)) => (String::new(), text, tone),
                None => (String::new(), "nada a apontar".to_string(), Tone::Dim),
            });
        }
        let mut card = Card::new(open.key, &open.title);
        // Quanto o cartão **tem** para mostrar. Quem decide quanto disso cabe é a grade,
        // que sabe o tamanho da tela; um cartão que já chegasse cortado seria um cartão
        // que nunca aproveita uma tela grande.
        card.height = open.facts.len().max(1) as u16;
        card.tone = open.tone;
        // The mark rides the title, which is the one piece of a card that is legible
        // from across the room: a grid of a dozen cards should say which one to open
        // without any of them being opened.
        let marked = match open.tone {
            Tone::Ruim => format!("{} {MARK_ALERTA}", open.title),
            Tone::Aviso => format!("{} {MARK_AVISO}", open.title),
            _ => open.title.clone(),
        };
        card.summary = Pane::Facts {
            title: marked,
            rows: open.facts,
        };
        card.detail = detail(open.title, open.headers, open.rows, open.lines);
        lock_board(&self.board).put(card);
    }

    /// Counts the findings on the board as it stands.
    ///
    /// Not the findings of this pass: a quick pass only looks at five of the fifteen
    /// cards, and a summary that counted only those would say "0 graves" while three
    /// cards on the same screen were red. The board is the state; the pass is just the
    /// last thing that touched it.
    fn survey(&self) -> (usize, usize, String) {
        let board = lock_board(&self.board);
        let (mut graves, mut avisos) = (0, 0);
        let (mut worst, mut first_warning) = (String::new(), String::new());
        for card in &board.cards {
            if card.key == "resumo" {
                continue;
            }
            let Pane::Facts { rows, .. } = &card.summary else {
                continue;
            };
            for (_, value, tone) in rows {
                match tone {
                    Tone::Ruim => {
                        graves += 1;
                        if worst.is_empty() {
                            worst = value.clone();
                        }
                    }
                    Tone::Aviso => {
                        avisos += 1;
                        if first_warning.is_empty() {
                            first_warning = value.clone();
                        }
                    }
                    _ => {}
                }
            }
        }
        let headline = match (worst.is_empty(), first_warning.is_empty()) {
            (false, _) => worst,
            (true, false) => first_warning,
            _ => String::new(),
        };
        (graves, avisos, headline)
    }

    /// The pass is over: close the last card and rewrite the summary.
    fn end(&mut self, elapsed: Duration) {
        self.flush();
        let (graves, avisos, headline) = self.survey();
        self.graves = graves;
        self.avisos = avisos;
        if !headline.is_empty() {
            self.headline = headline;
        }
        let verdict = match (self.graves, self.avisos) {
            (0, 0) => "nada a apontar — este banco está em ordem".to_string(),
            (0, avisos) => format!("{avisos} ponto(s) de atenção, nada grave"),
            (graves, avisos) => format!("{graves} grave(s) e {avisos} aviso(s)"),
        };
        let mut card = Card::new("resumo", "Resumo");
        card.height = 6;
        card.tone = match (self.graves, self.avisos) {
            (0, 0) => Tone::Bom,
            (0, _) => Tone::Aviso,
            _ => Tone::Ruim,
        };
        card.summary = Pane::Facts {
            title: "Resumo".to_string(),
            rows: vec![
                (
                    "graves".to_string(),
                    self.graves.to_string(),
                    match self.graves {
                        0 => Tone::Bom,
                        _ => Tone::Ruim,
                    },
                ),
                (
                    "avisos".to_string(),
                    self.avisos.to_string(),
                    match self.avisos {
                        0 => Tone::Bom,
                        _ => Tone::Aviso,
                    },
                ),
                (
                    "o pior agora".to_string(),
                    match self.headline.is_empty() {
                        true => "nada".to_string(),
                        false => self.headline.clone(),
                    },
                    Tone::Normal,
                ),
                (
                    "última leitura".to_string(),
                    format!("{:.1}s de perguntas", elapsed.as_secs_f64()),
                    Tone::Dim,
                ),
                (
                    "alterações feitas".to_string(),
                    "nenhuma — só leitura".to_string(),
                    Tone::Dim,
                ),
            ],
        };
        card.detail = Layout::one(Pane::Text {
            title: "Resumo".to_string(),
            lines: vec![
                (format!("  {verdict}"), Tone::Destaque),
                (String::new(), Tone::Normal),
                (
                    "  Cada cartão deste painel é uma seção da investigação, e a tecla de cada"
                        .to_string(),
                    Tone::Normal,
                ),
                (
                    "  um abre o que ele encontrou por inteiro. Ctrl+T põe tudo em texto corrido,"
                        .to_string(),
                    Tone::Normal,
                ),
                (
                    "  e lá o `c` copia o relatório inteiro.".to_string(),
                    Tone::Normal,
                ),
                (String::new(), Tone::Normal),
                (
                    "  Isto é um retrato, não um monitor: nada é perguntado ao banco entre uma"
                        .to_string(),
                    Tone::Normal,
                ),
                (
                    "  investigação e a próxima. Ctrl+R faz outra.".to_string(),
                    Tone::Normal,
                ),
                (String::new(), Tone::Normal),
                (
                    "  Nada aqui altera o banco: nenhum índice é criado, nenhum profiler é"
                        .to_string(),
                    Tone::Dim,
                ),
                (
                    "  ligado, nenhum contador é zerado. As sugestões são texto para você levar"
                        .to_string(),
                    Tone::Dim,
                ),
                ("  para uma janela de manutenção.".to_string(), Tone::Dim),
            ],
            scroll: 0,
        });
        let mut board = lock_board(&self.board);
        board.put(card);
        board.note = verdict.clone();
        board.working = false;
        board.updated = Some(Instant::now());
        drop(board);
        self.rec.report(
            self.tally(),
            match self.headline.is_empty() {
                true => "em ordem".to_string(),
                false => self.headline.clone(),
            },
        );
    }

    /// The connection itself failed. One card says so, because a board that simply stops
    /// updating looks like a board that is up to date.
    fn failed(&mut self, problem: &str) {
        self.open = None;
        let mut card = Card::new("resumo", "Resumo");
        card.height = 4;
        card.tone = Tone::Ruim;
        card.summary = Pane::Facts {
            title: "Resumo".to_string(),
            rows: vec![(String::new(), problem.to_string(), Tone::Ruim)],
        };
        card.detail = Layout::one(Pane::Text {
            title: "Resumo".to_string(),
            lines: vec![
                ("  A conexão falhou:".to_string(), Tone::Normal),
                (format!("  {problem}"), Tone::Ruim),
                (String::new(), Tone::Normal),
                (
                    "  'e' abre o formulário desta execução para corrigir a string de conexão."
                        .to_string(),
                    Tone::Dim,
                ),
            ],
            scroll: 0,
        });
        let mut board = lock_board(&self.board);
        board.put(card);
        board.note = problem.to_string();
        board.working = false;
    }

    fn tally(&self) -> String {
        format!("{} graves · {} avisos", self.graves, self.avisos)
    }
}

/// Monta o detalhe de um cartão: a tabela em cima, a prosa embaixo, e só o que existir.
///
/// Os pesos seguem o conteúdo. Uma seção que é quase toda listagem não deve ceder metade
/// da tela para três linhas de explicação, e uma que achou dez problemas não deve
/// esconder nove deles atrás de uma tabela de duas linhas.
fn detail(
    title: String,
    headers: Vec<String>,
    rows: Vec<Row>,
    lines: Vec<(String, Tone)>,
) -> Layout {
    let tabela = Pane::Table {
        title: title.clone(),
        headers,
        rows,
        selected: None,
        query: String::new(),
        note: None,
    };
    let texto = Pane::Text {
        title,
        lines,
        scroll: 0,
    };
    let (Pane::Table { rows, headers, .. }, Pane::Text { lines, .. }) = (&tabela, &texto) else {
        unreachable!("acabaram de ser construídos")
    };
    // Uma tabela sem linha nenhuma é um cabeçalho sozinho: pior que não ter tabela.
    match (headers.is_empty() || rows.is_empty(), lines.is_empty()) {
        (true, _) => Layout::one(texto),
        (false, true) => Layout::one(tabela),
        (false, false) => {
            let altura_tabela = (rows.len() as u16 + 3).clamp(4, 24);
            let altura_texto = (lines.len() as u16 + 2).clamp(3, 24);
            Layout::rows(vec![
                (altura_tabela, Layout::one(tabela)),
                (altura_texto, Layout::one(texto)),
            ])
        }
    }
}

/// A section title into a stable card key. Stable because the title is: renaming a
/// section moves its card to the end of the board once, and never again.
fn slug(title: &str) -> String {
    title
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

/// Shortens a query for a report line: whitespace collapsed, and cut with an ellipsis.
///
/// A statement arrives with the newlines and indentation an ORM gave it, and a report
/// where one entry is forty lines tall is a report nobody scrolls through.
pub fn one_line(text: &str, limit: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= limit {
        return flat;
    }
    let kept: String = flat.chars().take(limit.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// Bytes as a person says them.
pub fn bytes(value: f64) -> String {
    crate::format::human_bytes(value)
}

/// A count with thousands separated, because eight digits of table rows are unreadable
/// otherwise.
pub fn count(value: f64) -> String {
    let digits = format!("{:.0}", value.max(0.0));
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push('\u{202f}');
        }
        out.push(digit);
    }
    out
}

/// `1 execução` / `2 execuções`. The report is read by a person, and a person reading
/// "1 execuções" stops to wonder whether the number is wrong.
pub fn plural(quantity: f64, one: &str, many: &str) -> String {
    if (quantity - 1.0).abs() < f64::EPSILON {
        return format!("{} {one}", count(quantity));
    }
    format!("{} {many}", count(quantity))
}

/// Milliseconds as something with a sense of scale.
pub fn millis(value: f64) -> String {
    if value >= 60_000.0 {
        return format!("{:.1} min", value / 60_000.0);
    }
    if value >= 1000.0 {
        return format!("{:.1} s", value / 1000.0);
    }
    if value >= 10.0 {
        return format!("{value:.0} ms");
    }
    format!("{value:.1} ms")
}

/// Seconds as something a person reads without counting zeros.
pub fn duration(seconds: f64) -> String {
    if seconds <= 0.0 {
        return "agora".to_string();
    }
    if seconds < 1.0 {
        return format!("{:.0} ms", seconds * 1000.0);
    }
    if seconds < 90.0 {
        return format!("{seconds:.1}s");
    }
    if seconds < 5400.0 {
        return format!("{:.0} min", seconds / 60.0);
    }
    if seconds < 172_800.0 {
        return format!("{:.1}h", seconds / 3600.0);
    }
    format!("{:.0} dias", seconds / 86_400.0)
}
