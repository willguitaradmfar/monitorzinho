//! What to ask a Postgres server, and what the answers mean.
//!
//! The queries are all `SELECT`s against catalogues and statistics views, chosen so that
//! each one is a question a DBA would actually ask on being handed a server they have
//! never seen: is it configured like a real server or like a laptop, who is connected,
//! what is slow, what is being read without an index, what is being written without
//! being vacuumed, and what is about to run out.
//!
//! Two things are said in the report rather than assumed. The first is that counters are
//! cumulative: "this index was never used" means never since the statistics were last
//! reset, which on a server restarted yesterday means nothing at all — so the age of the
//! counters is printed before anything is concluded from them. The second is that a
//! suggestion is a suggestion: every `CREATE INDEX` in the output is text to read, and
//! this tool has no way to run it even if it wanted to.

use std::collections::{HashMap, HashSet};

use super::{
    Depth, Plan, Report, STATEMENT_TIMEOUT, bytes, count, duration, millis, one_line, pg, plural,
};
use crate::painel::Tone;

/// How many rows of any one listing are worth reading in a terminal.
const TOP: usize = 12;
/// Below this, a query is not worth a line of its own: the card would drown in the
/// application's ordinary traffic and hide the one statement that matters.
const SLOW_MS: f64 = 100.0;

/// One live inspection of one Postgres server.
///
/// Holds the connection and everything a pass compares itself against: the counters as
/// they were last time, and which queries were already running. That memory is the whole
/// difference between a report and a monitor — `pg_stat_statements` tells you what a
/// query has cost since the server started, and what anybody actually wants to know is
/// what it cost in the last five seconds.
pub struct Session {
    conn: pg::Conn,
    depth: Depth,
    server: String,
    database: String,
    user: String,
    encryption: String,
    read_only: bool,
    guards: Vec<String>,
    version: i64,
    version_text: String,
    settings: HashMap<String, Setting>,
    /// Each table's indexed leading columns, for deciding whether a suggestion is worth
    /// making. Refreshed on the complete passes.
    indexed: HashMap<String, HashSet<String>>,
    statements_on: bool,
    /// The statement listing from the last complete pass, used to work out which query
    /// is reading a table that is being scanned.
    statements: Vec<Stmt>,
    /// Counters as of the previous pass.
    last_statements: HashMap<String, Stmt>,
    last_tables: HashMap<String, Atividade>,
    /// Queries seen running, so their end can be reported as well as their start.
    live: HashMap<String, Live>,
    /// O que já se viu rodar, do mais recente para o mais antigo. Cresce a cada
    /// investigação e sobrevive entre elas: é este histórico que faz do cartão um monitor
    /// («o que rodou, e quanto demorou») em vez de um retrato de um instante.
    recentes: Vec<Recente>,
    last_pass: std::time::Instant,
    /// Quantas investigações esta sessão já fez. O que separa «ainda não há um antes» de
    /// «houve um antes e nada aconteceu no meio» — duas frases diferentes, e usar a lista
    /// vazia para decidir entre elas erraria para sempre num banco sem tabela nenhuma.
    passadas: u64,
    reset: Reset,
}

impl Session {
    pub fn open(plan: &Plan) -> Result<Self, String> {
        let target = pg::Target::parse(&plan.url, &plan.database)?;
        let mut conn = pg::Conn::open(&target)?;
        let guards = conn.harden(STATEMENT_TIMEOUT);
        let read_only = conn.read_only();
        let version = conn
            .query("SELECT current_setting('server_version_num') AS n")
            .ok()
            .and_then(|table| table.first().map(|row| row.int("n")))
            .unwrap_or(0);
        let statements_on = conn
            .query("SELECT 1 AS ok FROM pg_extension WHERE extname = 'pg_stat_statements'")
            .map(|table| !table.is_empty())
            .unwrap_or(false);
        Ok(Self {
            version_text: conn
                .settings
                .get("server_version")
                .cloned()
                .unwrap_or_default(),
            encryption: conn.encryption.clone(),
            conn,
            depth: plan.depth,
            server: format!("{}:{}", target.host, target.port),
            database: target.database.clone(),
            user: target.user.clone(),
            read_only,
            guards,
            version,
            settings: HashMap::new(),
            indexed: HashMap::new(),
            statements_on,
            statements: Vec::new(),
            last_statements: HashMap::new(),
            last_tables: HashMap::new(),
            live: HashMap::new(),
            recentes: Vec::new(),
            last_pass: std::time::Instant::now(),
            passadas: 0,
            reset: Reset { seconds: 0.0 },
        })
    }

    /// Uma investigação inteira, do jeito que ela vira quadro.
    ///
    /// A ordem das chamadas é a ordem dos cartões, de uma vez por todas: um cartão fica na
    /// posição — e portanto na tecla — que recebeu da primeira vez. Por isso o que muda a
    /// cada leitura vem primeiro, que é também o que merece estar no `1` e no `2`.
    pub fn pass(&mut self, report: &mut Report) -> Result<(), String> {
        let window = self.last_pass.elapsed().as_secs_f64().max(0.1);
        self.last_pass = std::time::Instant::now();
        self.passadas += 1;

        // Os ajustes são lidos antes de qualquer cartão porque três deles fazem conta com
        // esses números — a ocupação das conexões contra max_connections, o derrame em
        // disco contra work_mem. Lê agora, mostra no cartão de Ajustes mais adiante.
        self.settings = load_settings(&mut self.conn);
        self.server(report)?;
        self.now(report)?;
        self.movement(report, window)?;
        connections(&mut self.conn, report, &self.settings)?;
        locks(&mut self.conn, report)?;
        self.reset = activity(&mut self.conn, report, &self.settings)?;
        if report.stopping() {
            return Ok(());
        }
        settings_card(report, &self.settings, self.depth);
        self.indexed = indexed_columns(&mut self.conn, report)?;
        indexes(&mut self.conn, report, self.depth, &self.reset)?;
        if self.depth >= Depth::Medio {
            self.statements = statements(
                &mut self.conn,
                report,
                self.version,
                &self.settings,
                self.statements_on,
            )?;
            let (reset, statements) = (&self.reset, &self.statements);
            scans(&mut self.conn, report, reset, statements, &self.indexed)?;
            vacuum(&mut self.conn, report)?;
            wraparound(&mut self.conn, report, self.depth)?;
            replication(&mut self.conn, report, self.version)?;
            checkpoints(&mut self.conn, report, self.version)?;
        }
        if self.depth >= Depth::Profundo {
            if report.stopping() {
                return Ok(());
            }
            sizes(&mut self.conn, report)?;
            foreign_keys(&mut self.conn, report)?;
            sequences(&mut self.conn, report)?;
            no_primary_key(&mut self.conn, report)?;
            extensions(&mut self.conn, report)?;
        }
        Ok(())
    }

    /// O que o banco andou fazendo, para um servidor sem `pg_stat_statements`.
    ///
    /// É o mesmo cartão com a melhor fonte que houver: sem o texto das queries, o que
    /// sobra são os contadores por tabela, que existem em qualquer Postgres com
    /// `track_counts` ligado. Diz o que foi lido, como foi lido (por índice ou varrendo) e
    /// o que foi escrito — o suficiente para saber onde olhar, que é para isso que se abre
    /// esta tela.
    fn activity_without_statements(
        &mut self,
        report: &mut Report,
        window: f64,
    ) -> Result<(), String> {
        let agora = table_counters(&mut self.conn);
        let primeira = self.passadas <= 1;
        let movimento = match primeira {
            true => Vec::new(),
            false => activity_delta(&self.last_tables, &agora),
        };
        let scanned = match primeira {
            true => Vec::new(),
            false => table_delta(&self.last_tables, &agora),
        };
        self.last_tables = agora;

        report.line("sem pg_stat_statements: sem o texto das queries, o rastro delas por tabela");
        let conselho = instalar_statements(&self.settings);
        if primeira {
            report.line(
                "primeira leitura: o que acontecer daqui em diante aparece aqui a cada Ctrl+R",
            );
            report.aviso(
                "pg_stat_statements não está instalado nesta base — é ele que traz a query em si, com o tempo de cada uma",
                conselho,
            );
            return Ok(());
        }
        if movimento.is_empty() {
            report.ok(format!(
                "nenhuma tabela foi tocada nos últimos {window:.0}s"
            ));
        }
        report.table(&["leituras", "tabela", "como", "escritas"]);
        for (nome, delta) in movimento.iter().take(20) {
            let como = match (delta.idx_scan, delta.seq_scan) {
                (indice, 0.0) if indice > 0.0 => format!("{} por índice", count(indice)),
                (0.0, seq) if seq > 0.0 => format!("{} varrendo inteira", count(seq)),
                (indice, seq) => format!("{} por índice, {} varrendo", count(indice), count(seq)),
            };
            let escrita = match delta.escritas() {
                0.0 => String::new(),
                _ => format!(
                    "+{} ~{} -{}",
                    count(delta.inseridas),
                    count(delta.atualizadas),
                    count(delta.apagadas)
                ),
            };
            report.cells_headline(
                vec![count(delta.leituras()), nome.clone(), como, escrita],
                match delta.seq_scan > 0.0 && delta.vivas > 1000.0 {
                    true => Tone::Aviso,
                    false => Tone::Normal,
                },
            );
        }
        for (name, scans, rows, per_scan) in scanned.into_iter().take(4) {
            report.alerta(
                format!(
                    "{name} foi varrida inteira {} no intervalo: {} linhas, {} por varredura",
                    plural(scans, "vez", "vezes"),
                    count(rows),
                    count(per_scan)
                ),
                "sem pg_stat_statements não dá para dizer qual query foi. O log do servidor com log_min_duration_statement diria",
            );
        }
        report.aviso(
            "pg_stat_statements não está instalado nesta base — é ele que traz a query em si, com o tempo de cada uma",
            conselho,
        );
        Ok(())
    }

    /// Who is on the other end, and the proof that this session cannot write.
    fn server(&mut self, report: &mut Report) -> Result<(), String> {
        report.section("Servidor");
        report.field("endereço", &self.server);
        report.field("base", &self.database);
        report.field("usuário", &self.user);
        report.field("versão", &self.version_text);
        report.field("transporte", &self.encryption);
        if self.read_only {
            report.ok("sessão somente leitura, confirmada pelo servidor");
        } else {
            report.aviso(
                "o servidor não confirmou o modo somente leitura (pooler no meio?)",
                "as perguntas continuam sendo só SELECT — o que falhou foi o cinto de segurança a mais, não a regra",
            );
        }
        for problem in self.guards.clone() {
            report.line(format!("não consegui ajustar: {problem}"));
        }
        identity(&mut self.conn, report)?;
        Ok(())
    }

    /// What is executing this instant.
    fn now(&mut self, report: &mut Report) -> Result<(), String> {
        report.section("Agora");
        let sql = "SELECT pid, query_start::text AS inicio, \
                   EXTRACT(epoch FROM now() - query_start) AS duracao, \
                   COALESCE(wait_event_type,'') AS espera_tipo, COALESCE(wait_event,'') AS espera, \
                   COALESCE(usename,'') AS usuario, COALESCE(application_name,'') AS app, \
                   array_to_string(pg_blocking_pids(pid), ',') AS bloqueadores, \
                   left(query, 400) AS query \
                   FROM pg_stat_activity \
                   WHERE state = 'active' AND query_start IS NOT NULL AND pid <> pg_backend_pid() \
                   ORDER BY query_start";
        let Some(table) = ask(&mut self.conn, report, "pg_stat_activity", sql)? else {
            return Ok(());
        };
        report.table(&["há", "pid", "cliente", "esperando", "query"]);
        let mut seen: HashSet<String> = HashSet::new();
        let mut running = 0;
        for row in table.iter() {
            let key = format!("{}@{}", row.get("pid"), row.get("inicio"));
            seen.insert(key.clone());
            let age = row.num("duracao");
            let entry = self.live.entry(key).or_insert_with(|| Live {
                query: row.get("query").to_string(),
                duration: age,
                announced: false,
            });
            entry.duration = age;
            let announced = entry.announced;
            entry.announced = entry.announced || age >= 1.0;
            running += 1;

            let waiting = match row.get("espera") {
                "" => String::new(),
                event => format!(" [{}/{}]", row.get("espera_tipo"), event),
            };
            let blockers = row.get("bloqueadores");
            if !blockers.is_empty() {
                report.alerta(
                    format!(
                        "pid {} travado há {} esperando lock do(s) pid(s) {blockers}: {}",
                        row.get("pid"),
                        duration(age),
                        one_line(row.get("query"), 90)
                    ),
                    "quem segura o lock é quem precisa terminar — a vítima não tem o que fazer",
                );
                continue;
            }
            if age >= 5.0 && !announced {
                report.alerta(
                    format!(
                        "rodando há {}{waiting} — pid {}: {}",
                        duration(age),
                        row.get("pid"),
                        one_line(row.get("query"), 100)
                    ),
                    "uma query de vários segundos no meio do dia costuma ser plano ruim, não volume",
                );
                continue;
            }
            report.cells_headline(
                vec![
                    duration(age),
                    row.get("pid").to_string(),
                    format!("{}@{}", row.get("usuario"), row.get("app")),
                    row.get("espera").to_string(),
                    one_line(row.get("query"), 160),
                ],
                if age >= 1.0 {
                    Tone::Aviso
                } else {
                    Tone::Normal
                },
            );
        }
        if running == 0 {
            report.ok("nenhuma query em execução neste instante");
        }
        // What disappeared between two looks has finished, and how long it had been
        // running is the only measurement of it anybody will ever get.
        let done: Vec<String> = self
            .live
            .keys()
            .filter(|key| !seen.contains(*key))
            .cloned()
            .collect();
        for key in done {
            if let Some(entry) = self.live.remove(&key)
                && entry.announced
            {
                report.line(format!(
                    "terminou depois de ~{}: {}",
                    duration(entry.duration),
                    one_line(&entry.query, 100)
                ));
            }
        }
        Ok(())
    }

    /// O que rodou desde a investigação anterior, e quanto custou — a metade «profiler»
    /// desta ferramenta.
    ///
    /// O Postgres não guarda execuções individuais: `pg_stat_statements` guarda um total
    /// por forma de query desde que o servidor subiu. A diferença entre duas leituras é o
    /// que aconteceu entre elas, e é por isso que a sessão fica de pé entre um Ctrl+R e o
    /// seguinte — sem a leitura anterior não existe «recentemente».
    fn movement(&mut self, report: &mut Report, window: f64) -> Result<(), String> {
        report.section("Queries recentes");
        if !self.statements_on {
            // Sem a extensão não existe o texto das queries, mas existe o rastro que elas
            // deixaram: cada tabela conta quantas vezes foi lida por índice, quantas
            // varrida inteira, e quantas linhas mudaram. É menos do que a query, e é bem
            // mais do que um cartão dizendo «não instalado».
            return self.activity_without_statements(report, window);
        }
        let (total_column, _) = statement_columns(self.version);
        let now = statement_counters(&mut self.conn, total_column);
        let primeira = self.passadas <= 1;
        let recent = delta_of(&self.last_statements, &now);
        self.last_statements = now;

        let tables = table_counters(&mut self.conn);
        let scanned = match primeira {
            true => Vec::new(),
            false => table_delta(&self.last_tables, &tables),
        };
        self.last_tables = tables;

        if primeira {
            report.line(
                "primeira leitura: o que rodar daqui em diante aparece aqui, com o tempo de cada um",
            );
            report.line("Ctrl+R lê de novo e mostra o que aconteceu no intervalo");
            return Ok(());
        }

        // O histórico é por forma de query: a mesma query rodando de novo atualiza a
        // linha dela e volta para o topo, em vez de virar duas linhas iguais.
        let agora = std::time::Instant::now();
        for stmt in &recent {
            match self
                .recentes
                .iter_mut()
                .find(|antiga| antiga.query == stmt.query)
            {
                Some(antiga) => {
                    antiga.visto = agora;
                    antiga.calls += stmt.calls;
                    antiga.mean = stmt.mean;
                    antiga.total += stmt.total;
                    antiga.temp += stmt.temp;
                }
                None => self.recentes.push(Recente {
                    visto: agora,
                    calls: stmt.calls,
                    mean: stmt.mean,
                    total: stmt.total,
                    temp: stmt.temp,
                    query: stmt.query.clone(),
                }),
            }
        }
        self.recentes
            .sort_by(|a, b| b.visto.cmp(&a.visto).then(b.total.total_cmp(&a.total)));
        self.recentes.truncate(RECENTES);

        if self.recentes.is_empty() {
            report.ok(format!("nada rodou nos últimos {window:.0}s"));
        } else {
            let executions: f64 = recent.iter().map(|stmt| stmt.calls).sum();
            let spent: f64 = recent.iter().map(|stmt| stmt.total).sum();
            report.field(
                format!("nos últimos {window:.0}s"),
                match executions {
                    0.0 => "nada rodou — a lista abaixo é do que já passou".to_string(),
                    _ => format!(
                        "{} de {} formas, {} de banco",
                        plural(executions, "execução", "execuções"),
                        recent.len(),
                        millis(spent)
                    ),
                },
            );
            // O cartão é a lista: tempo médio, quantas vezes, e a query. Um monitor de
            // queries com a duração de cada uma é a pergunta que traz alguém até aqui.
            report.table(&["média", "vezes", "somado", "quando", "query"]);
            for (posicao, recente) in self.recentes.iter().enumerate() {
                let quando = recente.visto.elapsed().as_secs_f64();
                let celulas = vec![
                    millis(recente.mean),
                    format!("×{}", count(recente.calls)),
                    millis(recente.total),
                    match quando {
                        // O que rodou nesta leitura é «agora»; o resto é história, e a
                        // idade dela é o que diz se ainda interessa.
                        recem if recem < 2.0 => "nesta leitura".to_string(),
                        antes => format!("{} atrás", duration(antes)),
                    },
                    one_line(&recente.query, 160),
                ];
                let tom = match recente.mean {
                    lento if lento >= 1000.0 => Tone::Ruim,
                    lento if lento >= SLOW_MS => Tone::Aviso,
                    _ => Tone::Normal,
                };
                // As primeiras também no cartão da grade; o resto só na tabela cheia.
                if posicao < 6 {
                    report.cells_headline(celulas, tom);
                } else {
                    report.cells(celulas, tom);
                }
            }
        }

        // O que é notável no que acabou de rodar vira achado, com o que fazer junto.
        for stmt in recent.iter().take(6) {
            if stmt.mean < SLOW_MS && stmt.temp == 0.0 {
                continue;
            }
            let mut fixes: Vec<String> = Vec::new();
            if stmt.temp > 0.0 {
                fixes.push(format!(
                    "derramou {} em disco — não coube em work_mem",
                    bytes(stmt.temp * 8192.0)
                ));
            }
            if let Some(hint) = missing_columns(&stmt.query, &self.indexed) {
                fixes.push(hint);
            }
            report.alerta(
                format!(
                    "{} × {} (total {}): {}",
                    plural(stmt.calls, "execução", "execuções"),
                    millis(stmt.mean),
                    millis(stmt.total),
                    one_line(&stmt.query, 120)
                ),
                fixes.join(" · "),
            );
        }
        for (name, scans, rows, per_scan) in scanned.into_iter().take(4) {
            let short = name.rsplit('.').next().unwrap_or(&name).to_string();
            let already = self.indexed.get(&name).cloned().unwrap_or_default();
            let suggestion = columns_for(&short, &recent, &already)
                .or_else(|| columns_for(&short, &self.statements, &already))
                .unwrap_or_else(|| {
                    format!("descubra qual query lê {short} inteira — o EXPLAIN dela diz a coluna que falta")
                });
            report.alerta(
                format!(
                    "{name} foi varrida inteira {} no intervalo: {} linhas, {} por varredura",
                    plural(scans, "vez", "vezes"),
                    count(rows),
                    count(per_scan)
                ),
                suggestion,
            );
        }
        Ok(())
    }
}

/// Runs one question./// Runs one question.
///
/// `Ok(None)` is Postgres declining to answer — no such view, no such column, not your
/// table — which is an ordinary outcome when the same inspection runs against servers
/// five major versions apart and roles with wildly different grants. Only a broken
/// connection stops the investigation.
fn ask(
    conn: &mut pg::Conn,
    report: &mut Report,
    what: &str,
    sql: &str,
) -> Result<Option<pg::Table>, String> {
    match conn.query(sql) {
        Ok(table) => Ok(Some(table)),
        Err(error) if error.benign() => {
            report.line(format!("sem {what}: {error}"));
            Ok(None)
        }
        Err(error) => Err(format!("{what}: {error}")),
    }
}

/// The rest of the server card: what the connection alone doesn't say.
fn identity(conn: &mut pg::Conn, report: &mut Report) -> Result<(), String> {
    let sql = "SELECT current_database() AS base, current_user AS usuario, \
               pg_is_in_recovery() AS replica, \
               pg_size_pretty(pg_database_size(current_database())) AS tamanho, \
               EXTRACT(epoch FROM now() - pg_postmaster_start_time()) AS uptime, \
               current_setting('server_encoding') AS encoding, \
               inet_server_addr()::text AS endereco";
    let Some(table) = ask(conn, report, "identificação", sql)? else {
        return Ok(());
    };
    let Some(row) = table.first() else {
        return Ok(());
    };
    report.field("tamanho da base", row.get("tamanho"));
    report.field("codificação", row.get("encoding"));
    let uptime = row.num("uptime");
    report.field("no ar há", duration(uptime));
    if row.flag("replica") {
        report.line("este servidor é uma réplica em recuperação: só lê, e o que estiver atrasado aqui veio do primário");
    }
    // Everything below is read off counters that start at zero on a restart. Six minutes
    // of uptime and "this index is never used" are not a finding, they are a coincidence.
    if uptime < 3600.0 {
        report.line(format!(
            "o servidor subiu há {} — contadores acumulados ainda dizem pouco",
            duration(uptime)
        ));
    }
    Ok(())
}

/// One row of `pg_settings`, kept because three of its columns matter: what it is, what
/// it would be untouched, and whether anybody ever touched it.
struct Setting {
    value: String,
    unit: String,
    boot: String,
    source: String,
}

impl Setting {
    /// The setting in bytes, for the ones measured in blocks or kB.
    fn as_bytes(&self) -> f64 {
        let number: f64 = self.value.parse().unwrap_or(0.0);
        let scale = match self.unit.as_str() {
            "8kB" => 8.0 * 1024.0,
            "kB" => 1024.0,
            "MB" => 1024.0 * 1024.0,
            "GB" => 1024.0 * 1024.0 * 1024.0,
            _ => 1.0,
        };
        number * scale
    }

    fn number(&self) -> f64 {
        self.value.parse().unwrap_or(0.0)
    }

    fn untouched(&self) -> bool {
        self.value == self.boot
    }

    /// How it reads on screen: the number with its unit, and a note when it is still
    /// whatever it was compiled with.
    fn shown(&self) -> String {
        let base = match self.unit.as_str() {
            "" => self.value.clone(),
            "8kB" | "kB" | "MB" | "GB" => bytes(self.as_bytes()),
            "ms" => millis(self.number()),
            "s" => millis(self.number() * 1000.0),
            "min" => millis(self.number() * 60_000.0),
            unit => format!("{} {unit}", self.value),
        };
        if self.untouched() && self.source == "default" {
            return format!("{base}  (padrão de fábrica)");
        }
        base
    }
}

const WATCHED_SETTINGS: &str = "'max_connections','shared_buffers','effective_cache_size','work_mem',\
'maintenance_work_mem','autovacuum','fsync','full_page_writes','synchronous_commit','wal_level',\
'max_wal_size','checkpoint_timeout','checkpoint_completion_target','random_page_cost',\
'effective_io_concurrency','default_statistics_target','log_min_duration_statement','track_io_timing',\
'track_counts','statement_timeout','idle_in_transaction_session_timeout','max_parallel_workers_per_gather',\
'shared_preload_libraries','data_checksums','archive_mode','password_encryption','autovacuum_naptime',\
'autovacuum_vacuum_scale_factor','autovacuum_analyze_scale_factor','autovacuum_max_workers',\
'autovacuum_vacuum_cost_limit','autovacuum_freeze_max_age','temp_file_limit','max_locks_per_transaction',\
'deadlock_timeout','jit','huge_pages','wal_compression','ssl'";

/// Reads the settings, saying nothing. Split from the card below because the numbers are
/// needed several cards earlier than they are shown.
fn load_settings(conn: &mut pg::Conn) -> HashMap<String, Setting> {
    let sql = format!(
        "SELECT name, setting, COALESCE(unit,'') AS unit, boot_val, source \
         FROM pg_settings WHERE name IN ({WATCHED_SETTINGS})"
    );
    let mut map = HashMap::new();
    if let Ok(table) = conn.query(&sql) {
        for row in table.iter() {
            map.insert(
                row.get("name").to_string(),
                Setting {
                    value: row.get("setting").to_string(),
                    unit: row.get("unit").to_string(),
                    boot: row.get("boot_val").to_string(),
                    source: row.get("source").to_string(),
                },
            );
        }
    }
    map
}

fn settings_card(report: &mut Report, map: &HashMap<String, Setting>, depth: Depth) {
    report.section("Ajustes");
    if map.is_empty() {
        report.line("não consegui ler pg_settings com este usuário");
        return;
    }

    for name in [
        "shared_buffers",
        "effective_cache_size",
        "work_mem",
        "maintenance_work_mem",
        "max_connections",
        "max_wal_size",
        "random_page_cost",
        "default_statistics_target",
    ] {
        if let Some(setting) = map.get(name) {
            report.field(name, setting.shown());
        }
    }

    let get = |name: &str| map.get(name);
    let is = |name: &str, value: &str| get(name).is_some_and(|s| s.value == value);

    // The three that lose data or make the server blind. None of them has a legitimate
    // reason to be off on a database anyone cares about.
    if is("fsync", "off") {
        report.grave(
            "fsync = off: uma queda de energia ou um kill -9 do sistema corrompe esta base inteira",
            "ligue fsync. A única situação em que 'off' se defende é um banco descartável de carga inicial",
        );
    }
    if is("full_page_writes", "off") {
        report.grave(
            "full_page_writes = off: uma queda no meio de uma escrita deixa páginas pela metade, sem recuperação",
            "ligue full_page_writes, a menos que o armazenamento garanta escrita atômica de página",
        );
    }
    if is("autovacuum", "off") {
        report.grave(
            "autovacuum = off: as linhas mortas nunca são recolhidas e o banco caminha para o congelamento por wraparound",
            "ligue o autovacuum. Desligá-lo só faz sentido durante uma carga, e com um VACUUM manual marcado logo depois",
        );
    }
    if is("track_counts", "off") {
        report.grave(
            "track_counts = off: o autovacuum não tem como saber o que precisa limpar, e metade das estatísticas deste relatório não existe",
            "ligue track_counts",
        );
    }
    if is("data_checksums", "off") {
        report.aviso(
            "data_checksums = off: corrupção silenciosa em disco passa despercebida até virar dado errado",
            "só se resolve recriando o cluster com checksums (initdb -k) ou com pg_checksums com o banco parado",
        );
    }

    if let Some(buffers) = get("shared_buffers")
        && buffers.as_bytes() <= 128.0 * 1024.0 * 1024.0
    {
        report.aviso(
            format!(
                "shared_buffers em {} — o valor de instalação, não um valor escolhido",
                bytes(buffers.as_bytes())
            ),
            "a regra usual é 25% da RAM da máquina; exige reinício",
        );
    }
    if let Some(cost) = get("random_page_cost")
        && cost.number() >= 4.0
    {
        report.aviso(
            "random_page_cost = 4, o custo de um disco que gira: em SSD o planejador passa a evitar índices que valeriam a pena",
            "em SSD/NVMe, 1.1. Vale medir antes: é um SET por sessão, sem reinício",
        );
    }
    if let Some(io) = get("track_io_timing")
        && io.value == "off"
    {
        report.aviso(
            "track_io_timing = off: dá para ver quanto tempo uma query levou, não quanto dele foi esperando o disco",
            "ligue track_io_timing (SET global, sem reinício). O custo é medido com pg_test_timing e costuma ser desprezível",
        );
    }
    if let Some(log) = get("log_min_duration_statement")
        && log.number() < 0.0
    {
        report.aviso(
            "log_min_duration_statement = -1: nenhuma query lenta é registrada no log do servidor",
            "um valor como 1000 (1s) grava só as lentas e é o histórico que falta quando o problema já passou",
        );
    }
    if let Some(preload) = get("shared_preload_libraries")
        && !preload.value.contains("pg_stat_statements")
    {
        report.aviso(
            "pg_stat_statements não está em shared_preload_libraries: sem ele não há como saber quais queries custam o quê",
            instalar_statements(map),
        );
    }
    if let Some(max) = get("max_connections")
        && max.number() > 300.0
    {
        report.aviso(
            format!(
                "max_connections = {}: cada conexão custa memória e o servidor não fica mais rápido por aceitar mais",
                max.value
            ),
            "um pool (PgBouncer) com 2-4× o número de núcleos rende mais que centenas de conexões diretas",
        );
    }
    if let Some(timeout) = get("idle_in_transaction_session_timeout")
        && timeout.number() == 0.0
    {
        report.aviso(
            "idle_in_transaction_session_timeout = 0: uma transação esquecida aberta segura locks e trava o vacuum para sempre",
            "algo entre 60s e 300s corta o esquecimento sem atrapalhar trabalho real",
        );
    }
    if depth >= Depth::Medio
        && let Some(archive) = get("archive_mode")
        && archive.value == "off"
    {
        report.aviso(
            "archive_mode = off: sem arquivamento de WAL não existe restauração para um ponto no tempo",
            "se houver backup físico e réplica, pode ser decisão consciente — se não houver, é a lacuna mais cara da lista",
        );
    }
    if let Some(encryption) = get("password_encryption")
        && encryption.value.contains("md5")
    {
        report.aviso(
            "password_encryption = md5: senhas novas nascem com um hash aposentado",
            "scram-sha-256, e as senhas existentes migram quando forem trocadas",
        );
    }
}

fn connections(
    conn: &mut pg::Conn,
    report: &mut Report,
    settings: &HashMap<String, Setting>,
) -> Result<(), String> {
    report.section("Conexões");
    let sql = "SELECT COALESCE(state,'(processo interno)') AS estado, count(*) AS n, \
               MAX(EXTRACT(epoch FROM now() - state_change)) AS mais_antiga \
               FROM pg_stat_activity GROUP BY 1 ORDER BY n DESC";
    let Some(table) = ask(conn, report, "pg_stat_activity", sql)? else {
        return Ok(());
    };
    let total: f64 = table.iter().map(|row| row.num("n")).sum();
    let limit = settings
        .get("max_connections")
        .map(Setting::number)
        .unwrap_or(0.0);
    report.table(&["estado", "quantas", "mais antiga"]);
    for row in table.iter() {
        // A background worker has no `state_change`, so there is no "oldest" to print and
        // printing one anyway would invent a fact.
        let oldest = match row.get("mais_antiga") {
            "" => String::new(),
            _ => duration(row.num("mais_antiga")),
        };
        report.cells(
            vec![
                row.get("estado").to_string(),
                row.get("n").to_string(),
                oldest,
            ],
            Tone::Normal,
        );
    }
    if limit > 0.0 {
        let used = 100.0 * total / limit;
        report.field(
            "ocupação",
            format!("{total:.0} de {limit:.0} conexões ({used:.0}%)"),
        );
        if used >= 80.0 {
            report.grave(
                format!("{used:.0}% das conexões em uso — quando bater no teto, ninguém mais entra, inclusive você"),
                "um pool na frente do banco resolve; subir max_connections só empurra o problema e cobra memória",
            );
        }
    }

    // An open transaction sitting idle is the single most expensive thing a client can
    // do without appearing to do anything: it pins the oldest snapshot, so every VACUUM
    // in the cluster stops being able to remove anything newer than it.
    let sql = "SELECT pid, COALESCE(usename,'') AS usuario, COALESCE(application_name,'') AS app, \
               COALESCE(client_addr::text,'local') AS origem, \
               EXTRACT(epoch FROM now() - state_change) AS parada, \
               left(query, 200) AS query \
               FROM pg_stat_activity WHERE state LIKE 'idle in transaction%' \
               ORDER BY state_change LIMIT 10";
    if let Some(table) = ask(conn, report, "transações ociosas", sql)? {
        for row in table.iter() {
            let idle = row.num("parada");
            let line = format!(
                "pid {} ({}@{}) parado em transação há {}: {}",
                row.get("pid"),
                row.get("usuario"),
                row.get("app"),
                duration(idle),
                one_line(row.get("query"), 90)
            );
            if idle > 300.0 {
                report.grave(
                    line,
                    "esse cliente esqueceu um COMMIT/ROLLBACK aberto. Enquanto ele estiver ali, nenhum vacuum limpa nada mais novo que ele",
                );
            } else if idle > 60.0 {
                report.aviso(line, "transação aberta sem trabalho: procure o caminho no código que abre e não fecha");
            } else {
                report.row(line);
            }
        }
    }

    let sql = "SELECT pid, COALESCE(usename,'') AS usuario, COALESCE(application_name,'') AS app, \
               EXTRACT(epoch FROM now() - query_start) AS duracao, \
               COALESCE(wait_event_type,'') AS espera_tipo, COALESCE(wait_event,'') AS espera, \
               left(query, 300) AS query \
               FROM pg_stat_activity \
               WHERE state = 'active' AND query_start IS NOT NULL AND pid <> pg_backend_pid() \
               ORDER BY query_start LIMIT 8";
    if let Some(table) = ask(conn, report, "queries em andamento", sql)?
        && !table.is_empty()
    {
        report.line("rodando agora:");
        for row in table.iter() {
            let seconds = row.num("duracao");
            let waiting = match row.get("espera") {
                "" => String::new(),
                event => format!("  esperando {}/{}", row.get("espera_tipo"), event),
            };
            let line = format!(
                "pid {} há {}{waiting}: {}",
                row.get("pid"),
                duration(seconds),
                one_line(row.get("query"), 100)
            );
            if seconds > 60.0 {
                report.aviso(line, "uma query de mais de um minuto no meio do dia costuma ser plano ruim, não volume");
            } else {
                report.row(line);
            }
        }
    }
    Ok(())
}

fn locks(conn: &mut pg::Conn, report: &mut Report) -> Result<(), String> {
    report.section("Bloqueios");
    let sql = "SELECT vitima.pid AS pid, COALESCE(vitima.usename,'') AS usuario, \
               EXTRACT(epoch FROM now() - vitima.query_start) AS espera, \
               left(vitima.query, 200) AS query, \
               culpado.pid AS culpado_pid, COALESCE(culpado.state,'') AS culpado_estado, \
               left(COALESCE(culpado.query,''), 200) AS culpado_query \
               FROM pg_stat_activity vitima \
               JOIN LATERAL unnest(pg_blocking_pids(vitima.pid)) AS bloqueio(pid) ON true \
               JOIN pg_stat_activity culpado ON culpado.pid = bloqueio.pid \
               ORDER BY vitima.query_start LIMIT 10";
    let Some(table) = ask(conn, report, "cadeia de bloqueios", sql)? else {
        return Ok(());
    };
    if table.is_empty() {
        report.ok("ninguém esperando por lock neste instante");
        return Ok(());
    }
    for row in table.iter() {
        report.grave(
            format!(
                "pid {} esperando há {} por causa do pid {} ({}): {}",
                row.get("pid"),
                duration(row.num("espera")),
                row.get("culpado_pid"),
                row.get("culpado_estado"),
                one_line(row.get("query"), 80)
            ),
            format!(
                "quem segura o lock está rodando: {}",
                one_line(row.get("culpado_query"), 100)
            ),
        );
    }
    Ok(())
}

/// How old the statistics are. Printed once and carried into every check that reads a
/// cumulative counter, because "never used" only means something next to it.
struct Reset {
    seconds: f64,
}

impl Reset {
    fn describe(&self) -> String {
        if self.seconds <= 0.0 {
            return "desde sempre".to_string();
        }
        format!("acumulado em {}", duration(self.seconds))
    }

    /// Whether the counters have been running long enough to conclude anything from an
    /// absence — a week is the shortest window in which "nobody ever used this index"
    /// survives a weekly report nobody ran yet.
    fn trustworthy(&self) -> bool {
        self.seconds >= 7.0 * 86_400.0
    }
}

fn activity(
    conn: &mut pg::Conn,
    report: &mut Report,
    settings: &HashMap<String, Setting>,
) -> Result<Reset, String> {
    report.section("Leitura e escrita");
    let sql = "SELECT numbackends, xact_commit, xact_rollback, blks_read, blks_hit, \
               tup_returned, tup_fetched, tup_inserted, tup_updated, tup_deleted, \
               temp_files, temp_bytes, deadlocks, conflicts, \
               EXTRACT(epoch FROM now() - stats_reset) AS desde \
               FROM pg_stat_database WHERE datname = current_database()";
    let Some(table) = ask(conn, report, "pg_stat_database", sql)? else {
        return Ok(Reset { seconds: 0.0 });
    };
    let Some(row) = table.first() else {
        return Ok(Reset { seconds: 0.0 });
    };
    let reset = Reset {
        seconds: row.num("desde"),
    };
    report.field("estatísticas", reset.describe());

    let hit = row.num("blks_hit");
    let read = row.num("blks_read");
    if hit + read > 0.0 {
        let ratio = 100.0 * hit / (hit + read);
        report.field(
            "acerto de cache",
            format!("{ratio:.2}%  ({} lidos do disco)", count(read)),
        );
        if ratio < 90.0 {
            report.grave(
                format!("acerto de cache em {ratio:.1}% — o banco está lendo do disco o tempo todo"),
                "ou shared_buffers é pequeno demais para o conjunto quente, ou existe uma varredura completa repetida escorraçando o cache",
            );
        } else if ratio < 98.0 {
            report.aviso(
                format!("acerto de cache em {ratio:.1}%, abaixo dos 99% de um banco bem dimensionado"),
                "compare o tamanho da base com shared_buffers e veja a lista de varreduras sequenciais mais abaixo",
            );
        } else {
            report.ok(format!("acerto de cache em {ratio:.2}%"));
        }
    }

    let commits = row.num("xact_commit");
    let rollbacks = row.num("xact_rollback");
    if commits + rollbacks > 1000.0 {
        let ratio = 100.0 * rollbacks / (commits + rollbacks);
        report.field(
            "transações",
            format!(
                "{} commits, {} rollbacks ({ratio:.1}%)",
                count(commits),
                count(rollbacks)
            ),
        );
        if ratio > 10.0 {
            report.aviso(
                format!("{ratio:.0}% das transações terminam em rollback"),
                "erro de aplicação repetido, deadlock, ou retry automático escondendo alguma coisa",
            );
        }
    }

    let temp_files = row.num("temp_files");
    if temp_files > 0.0 {
        let work_mem = settings
            .get("work_mem")
            .map(Setting::as_bytes)
            .unwrap_or(0.0);
        report.field(
            "arquivos temporários",
            format!(
                "{} arquivos, {}",
                count(temp_files),
                bytes(row.num("temp_bytes"))
            ),
        );
        report.aviso(
            format!(
                "{} derramados em disco: alguma ordenação ou hash não coube em work_mem ({})",
                bytes(row.num("temp_bytes")),
                bytes(work_mem)
            ),
            "a lista de queries mais abaixo diz quais derramam. work_mem é por operação e por conexão — subir global multiplica",
        );
    }
    let deadlocks = row.num("deadlocks");
    if deadlocks > 0.0 {
        report.aviso(
            format!("{} deadlock(s) desde o último reset das estatísticas", count(deadlocks)),
            "dois caminhos do código pegando os mesmos registros em ordens diferentes. O log do servidor traz as duas queries envolvidas",
        );
    }
    Ok(reset)
}

/// Um índice como o catálogo o descreve, para a comparação de redundância.
struct Indice {
    nome: String,
    /// As colunas como `indkey` as guarda: números separados por espaço, o que faz de
    /// «é prefixo de» uma comparação de texto.
    colunas: String,
    /// O método de acesso — dois índices de tipos diferentes não se substituem.
    tipo: String,
    unico: bool,
    parcial: bool,
    bytes: f64,
}

fn indexes(
    conn: &mut pg::Conn,
    report: &mut Report,
    depth: Depth,
    reset: &Reset,
) -> Result<(), String> {
    report.section("Índices");

    let sql = "SELECT n.nspname || '.' || c.relname AS indice, t.relname AS tabela \
               FROM pg_index i \
               JOIN pg_class c ON c.oid = i.indexrelid \
               JOIN pg_class t ON t.oid = i.indrelid \
               JOIN pg_namespace n ON n.oid = c.relnamespace \
               WHERE NOT i.indisvalid";
    if let Some(table) = ask(conn, report, "índices inválidos", sql)? {
        for row in table.iter() {
            report.grave(
                format!(
                    "índice {} (tabela {}) está inválido: ninguém o usa e toda escrita na tabela continua pagando por ele",
                    row.get("indice"),
                    row.get("tabela")
                ),
                "sobra de um CREATE INDEX CONCURRENTLY que falhou. Confira e recrie — o REINDEX/DROP é decisão sua",
            );
        }
    }

    let sql = "SELECT s.schemaname || '.' || s.relname AS tabela, s.indexrelname AS indice, \
               s.idx_scan, pg_relation_size(s.indexrelid) AS bytes \
               FROM pg_stat_user_indexes s \
               JOIN pg_index i ON i.indexrelid = s.indexrelid \
               WHERE s.idx_scan = 0 AND NOT i.indisunique AND NOT i.indisprimary \
               AND pg_relation_size(s.indexrelid) > 1048576 \
               ORDER BY pg_relation_size(s.indexrelid) DESC LIMIT 20";
    if let Some(table) = ask(conn, report, "índices sem uso", sql)? {
        if table.is_empty() {
            report.ok("todo índice não-único desta base já foi usado ao menos uma vez");
        } else {
            let total: f64 = table.iter().map(|row| row.num("bytes")).sum();
            let headline = format!(
                "{} índice(s) nunca usados ocupando {} ({})",
                table.len(),
                bytes(total),
                reset.describe()
            );
            if reset.trustworthy() {
                report.aviso(
                    headline,
                    "cada um deles é espaço, e mais lentidão em todo INSERT e UPDATE da tabela. Confira se não servem a um relatório mensal antes de largar",
                );
            } else {
                report.line(format!(
                    "{headline} — contadores novos demais para concluir alguma coisa"
                ));
            }
            report.table(&["tamanho", "índice", "tabela", "situação"]);
            for row in table.iter() {
                report.cells(
                    vec![
                        bytes(row.num("bytes")),
                        row.get("indice").to_string(),
                        row.get("tabela").to_string(),
                        "nunca usado".to_string(),
                    ],
                    Tone::Aviso,
                );
            }
        }
    }

    // Redundancy is a catalogue fact, not a statistic: an index on (a) next to an index
    // on (a, b) is dead weight whatever the counters say, because anything the first can
    // answer the second answers too.
    let sql = "SELECT n.nspname || '.' || t.relname AS tabela, c.relname AS indice, \
               i.indkey::text AS colunas, am.amname AS tipo, \
               i.indisunique AS unico, i.indisprimary AS primaria, \
               (i.indpred IS NOT NULL) AS parcial, \
               pg_relation_size(i.indexrelid) AS bytes, \
               pg_get_indexdef(i.indexrelid) AS definicao \
               FROM pg_index i \
               JOIN pg_class c ON c.oid = i.indexrelid \
               JOIN pg_class t ON t.oid = i.indrelid \
               JOIN pg_namespace n ON n.oid = t.relnamespace \
               JOIN pg_am am ON am.oid = c.relam \
               WHERE n.nspname NOT IN ('pg_catalog','information_schema') AND i.indisvalid \
               ORDER BY 1, 2";
    if let Some(table) = ask(conn, report, "definição dos índices", sql)? {
        let mut by_table: HashMap<String, Vec<Indice>> = HashMap::new();
        for row in table.iter() {
            by_table
                .entry(row.get("tabela").to_string())
                .or_default()
                .push(Indice {
                    nome: row.get("indice").to_string(),
                    colunas: row.get("colunas").to_string(),
                    tipo: row.get("tipo").to_string(),
                    unico: row.flag("unico") || row.flag("primaria"),
                    parcial: row.flag("parcial"),
                    bytes: row.num("bytes"),
                });
        }
        let mut redundant: Vec<(String, String, f64)> = Vec::new();
        for (table_name, list) in &by_table {
            for indice in list {
                // A unique index earns its place by the constraint it enforces, and an
                // expression index (column 0 in indkey) can't be compared this way.
                if indice.unico
                    || indice.parcial
                    || indice.colunas.split(' ').any(|column| column == "0")
                {
                    continue;
                }
                let prefixo = format!("{} ", indice.colunas);
                if let Some(maior) = list.iter().find(|outro| {
                    outro.nome != indice.nome
                        && outro.tipo == indice.tipo
                        && !outro.parcial
                        && outro.colunas.starts_with(&prefixo)
                }) {
                    redundant.push((
                        format!("{table_name}.{}", indice.nome),
                        maior.nome.clone(),
                        indice.bytes,
                    ));
                }
            }
        }
        redundant.sort_by(|a, b| b.2.total_cmp(&a.2));
        if redundant.is_empty() {
            report.ok("nenhum índice é prefixo de outro — não há duplicação óbvia");
        } else {
            let total: f64 = redundant.iter().map(|entry| entry.2).sum();
            report.aviso(
                format!(
                    "{} índice(s) redundantes, {} no total: as colunas de cada um já são o começo de outro índice",
                    redundant.len(),
                    bytes(total)
                ),
                "o índice maior atende às mesmas buscas. O menor só custa escrita e espaço",
            );
            report.table(&["tamanho", "índice", "tabela", "situação"]);
            for (name, covering, size) in redundant.iter().take(TOP) {
                let (tabela, indice) = name.rsplit_once('.').unwrap_or(("", name.as_str()));
                report.cells(
                    vec![
                        bytes(*size),
                        indice.to_string(),
                        tabela.to_string(),
                        format!("redundante — coberto por {covering}"),
                    ],
                    Tone::Aviso,
                );
            }
        }

        if depth >= Depth::Profundo {
            let total: f64 = table.iter().map(|row| row.num("bytes")).sum();
            report.field("espaço total em índices", bytes(total));
        }
    }
    Ok(())
}

/// One row of `pg_stat_statements`, in the two spellings the column names have had.
struct Stmt {
    calls: f64,
    total: f64,
    mean: f64,
    rows: f64,
    hit: f64,
    miss: f64,
    temp: f64,
    query: String,
}

/// O que falta para haver `pg_stat_statements`, e o que fazer a respeito.
///
/// São dois passos e eles falham de jeitos bem diferentes: a biblioteca entra no processo
/// do servidor (`shared_preload_libraries`, e isso exige reinício), e a extensão é criada
/// em cada base (`CREATE EXTENSION`, e isso não exige nada). Quase sempre só falta o
/// segundo — e mandar alguém agendar uma janela de manutenção para uma coisa que levava um
/// segundo é o tipo de conselho que faz ninguém seguir conselho nenhum.
///
/// Por isso a frase sai daqui, olhando o `shared_preload_libraries` que já foi lido, e não
/// de uma constante que diz as duas coisas sempre.
fn instalar_statements(settings: &HashMap<String, Setting>) -> String {
    let carregada = settings
        .get("shared_preload_libraries")
        .is_some_and(|setting| setting.value.contains("pg_stat_statements"));
    if carregada {
        return "a biblioteca já está carregada neste servidor: falta só `CREATE EXTENSION pg_stat_statements;` nesta base — um comando, sem reinício e sem janela de manutenção".to_string();
    }
    "dois passos: `shared_preload_libraries = 'pg_stat_statements'` no servidor (no RDS/Aurora é o parameter group, no Cloud SQL é uma flag, no Azure é um server parameter — nos três exige reinício), e depois `CREATE EXTENSION pg_stat_statements;` em cada base que você quiser enxergar".to_string()
}

/// Whether the timing columns are the 13+ names. The rename split `total_time` into
/// planning and execution, and a query written for one spelling is a syntax error
/// against the other.
fn statement_columns(version: i64) -> (&'static str, &'static str) {
    if version >= 130_000 {
        ("total_exec_time", "mean_exec_time")
    } else {
        ("total_time", "mean_time")
    }
}

fn statements(
    conn: &mut pg::Conn,
    report: &mut Report,
    version: i64,
    settings: &HashMap<String, Setting>,
    present: bool,
) -> Result<Vec<Stmt>, String> {
    report.section("Queries");
    if !present {
        report.aviso(
            "sem pg_stat_statements não há registro de quanto cada query custou",
            instalar_statements(settings),
        );
        return Ok(Vec::new());
    }

    let (total_column, mean_column) = statement_columns(version);
    let sql = format!(
        "SELECT COALESCE(queryid::text,'') AS id, calls, rows, \
         {total_column} AS total, {mean_column} AS media, \
         shared_blks_hit AS hit, shared_blks_read AS miss, temp_blks_written AS temp, \
         query FROM pg_stat_statements \
         WHERE dbid = (SELECT oid FROM pg_database WHERE datname = current_database()) \
         AND calls > 0 ORDER BY {total_column} DESC LIMIT 200"
    );
    let Some(table) = ask(conn, report, "pg_stat_statements", &sql)? else {
        return Ok(Vec::new());
    };
    let statements: Vec<Stmt> = table
        .iter()
        .filter(|row| !monitoring(row.get("query")))
        .map(|row| Stmt {
            calls: row.num("calls"),
            total: row.num("total"),
            mean: row.num("media"),
            rows: row.num("rows"),
            hit: row.num("hit"),
            miss: row.num("miss"),
            temp: row.num("temp"),
            query: row.get("query").to_string(),
        })
        .collect();
    if statements.is_empty() {
        report.line("pg_stat_statements está instalado e vazio para esta base");
        return Ok(statements);
    }

    let grand_total: f64 = statements.iter().map(|s| s.total).sum();
    let calls: f64 = statements.iter().map(|s| s.calls).sum();
    report.field(
        "registradas",
        format!(
            "{} formas de query, {}, {} de tempo somado",
            statements.len(),
            plural(calls, "execução", "execuções"),
            millis(grand_total)
        ),
    );

    report.line("as que mais consomem tempo do servidor:");
    report.table(&["% do tempo", "somado", "execuções", "média", "query"]);
    for stmt in statements.iter().take(15) {
        let share = if grand_total > 0.0 {
            100.0 * stmt.total / grand_total
        } else {
            0.0
        };
        report.cells(
            vec![
                format!("{share:.1}%"),
                millis(stmt.total),
                count(stmt.calls),
                millis(stmt.mean),
                one_line(&stmt.query, 160),
            ],
            match stmt.mean {
                lento if lento >= 1000.0 => Tone::Ruim,
                lento if lento >= SLOW_MS => Tone::Aviso,
                _ => Tone::Normal,
            },
        );
        // One statement owning a third of the server is not automatically wrong — it may
        // be the whole application — but it is always the right place to look first.
        if share >= 30.0 {
            report.aviso(
                format!("uma única query consome {share:.0}% de todo o tempo de execução do banco"),
                format!(
                    "é aqui que qualquer ganho aparece: {}",
                    one_line(&stmt.query, 120)
                ),
            );
        }
    }

    let mut by_mean: Vec<&Stmt> = statements.iter().filter(|s| s.calls >= 3.0).collect();
    by_mean.sort_by(|a, b| b.mean.total_cmp(&a.mean));
    if let Some(slowest) = by_mean.first()
        && slowest.mean >= 200.0
    {
        report.line("as mais lentas por execução:");
        for stmt in by_mean.iter().take(6) {
            if stmt.mean < 200.0 {
                break;
            }
            let line = format!(
                "média {} em {}, {} linhas por vez: {}",
                millis(stmt.mean),
                plural(stmt.calls, "execução", "execuções"),
                count(stmt.rows / stmt.calls.max(1.0)),
                one_line(&stmt.query, 110)
            );
            if stmt.mean >= 5000.0 {
                report.grave(line, index_hint(&stmt.query));
            } else if stmt.mean >= 1000.0 {
                report.aviso(line, index_hint(&stmt.query));
            } else {
                report.row(line);
            }
        }
    }

    let spilling: Vec<&Stmt> = statements.iter().filter(|s| s.temp > 0.0).collect();
    if !spilling.is_empty() {
        let work_mem = settings
            .get("work_mem")
            .map(Setting::as_bytes)
            .unwrap_or(0.0);
        report.aviso(
            format!(
                "{} query(ies) derramam em disco por não caberem em work_mem ({})",
                spilling.len(),
                bytes(work_mem)
            ),
            "ordenar ou agrupar em disco custa uma ordem de grandeza. Ou a query devolve menos linhas, ou aquela sessão sobe work_mem — nunca o servidor inteiro sem contas",
        );
        for stmt in spilling.iter().take(5) {
            report.row(format!(
                "{:>10} em temporários: {}",
                bytes(stmt.temp * 8192.0),
                one_line(&stmt.query, 110)
            ));
        }
    }

    // A query that is run constantly and always returns the same expensive aggregate is
    // the definition of something that should have been computed once.
    let candidates: Vec<&Stmt> = statements
        .iter()
        .filter(|s| s.calls >= 20.0 && s.mean >= 150.0 && aggregating(&s.query))
        .collect();
    if !candidates.is_empty() {
        report.aviso(
            format!(
                "{} agregação(ões) cara(s) repetida(s) muitas vezes — candidatas a materialização",
                candidates.len()
            ),
            "uma MATERIALIZED VIEW com REFRESH programado, ou uma tabela de resumo mantida pela aplicação, troca N execuções caras por uma",
        );
        for stmt in candidates.iter().take(5) {
            report.row(format!(
                "{} × {} = {}: {}",
                plural(stmt.calls, "execução", "execuções"),
                millis(stmt.mean),
                millis(stmt.total),
                one_line(&stmt.query, 110)
            ));
        }
    }

    let cold: Vec<&Stmt> = statements
        .iter()
        .filter(|s| s.miss > 10_000.0 && s.miss > s.hit * 0.1)
        .collect();
    if !cold.is_empty() {
        report.aviso(
            format!("{} query(ies) leem muito do disco em vez do cache", cold.len()),
            "ou tocam mais dados do que a memória do servidor comporta, ou varrem tabela inteira — a seção seguinte diz quais tabelas",
        );
        for stmt in cold.iter().take(5) {
            report.row(format!(
                "{} lidos do disco: {}",
                bytes(stmt.miss * 8192.0),
                one_line(&stmt.query, 110)
            ));
        }
    }
    Ok(statements)
}

/// Whether a statement is somebody watching the database rather than using it.
///
/// This investigation's own questions land in `pg_stat_statements` like everybody
/// else's, and a watch that reported its own footsteps as slow queries would be both
/// noise and a lie. The same filter also hides other monitoring — an exporter polling
/// the statistics views — which is the right call for the same reason: it is not the
/// application, and it is not what anyone opened this screen to look at.
fn monitoring(query: &str) -> bool {
    let lowered = query.trim_start().to_ascii_lowercase();
    lowered.starts_with("set ")
        || lowered.starts_with("show ")
        || lowered.contains("pg_catalog")
        // Pelas tabelas que a query lê, e não por um pedaço do texto dela: quem lê uma
        // tabela `pg_*` está olhando para o banco, não usando o banco. Metade das
        // perguntas desta ferramenta chama uma função `pg_` sobre uma tabela do usuário,
        // e essas não são monitoramento de ninguém.
        || {
            let tabelas = tables_in(query);
            // Quem lê uma tabela `pg_*` está olhando para o banco, não usando o banco. E
            // quem não lê tabela nenhuma e ainda assim fala de `pg_` está perguntando ao
            // servidor sobre ele mesmo — que é literalmente o que esta ferramenta faz o
            // tempo todo, e não é o que quem abriu esta tela quer ver listado.
            tabelas.iter().any(|nome| nome.starts_with("pg_"))
                || (tabelas.is_empty()
                    && ["pg_", "current_setting", "current_database", "current_user"]
                        .iter()
                        .any(|marca| lowered.contains(marca)))
        }
}

/// Whether a statement is the kind whose answer could have been stored instead of
/// recomputed.
fn aggregating(query: &str) -> bool {
    let lowered = query.to_ascii_lowercase();
    ["group by", "count(", "sum(", "avg(", "distinct", "having"]
        .iter()
        .any(|needle| lowered.contains(needle))
}

fn scans(
    conn: &mut pg::Conn,
    report: &mut Report,
    reset: &Reset,
    statements: &[Stmt],
    indexed: &HashMap<String, HashSet<String>>,
) -> Result<(), String> {
    report.section("Varreduras sequenciais");
    let sql = "SELECT schemaname || '.' || relname AS tabela, relname AS nome, \
               seq_scan, seq_tup_read, COALESCE(idx_scan,0) AS idx_scan, \
               n_live_tup, pg_relation_size(relid) AS bytes \
               FROM pg_stat_user_tables WHERE seq_scan > 0 \
               ORDER BY seq_tup_read DESC LIMIT 20";
    let Some(table) = ask(conn, report, "pg_stat_user_tables", sql)? else {
        return Ok(());
    };
    if table.is_empty() {
        report.ok("nenhuma varredura sequencial registrada");
        return Ok(());
    }
    report.line(format!("leitura de tabela inteira, {}:", reset.describe()));
    report.table(&[
        "tabela",
        "varreduras",
        "linhas lidas",
        "por varredura",
        "linhas",
    ]);
    let mut found = false;
    for row in table.iter() {
        let scans = row.num("seq_scan");
        let read = row.num("seq_tup_read");
        let live = row.num("n_live_tup");
        let per_scan = if scans > 0.0 { read / scans } else { 0.0 };
        report.cells(
            vec![
                row.get("tabela").to_string(),
                count(scans),
                count(read),
                count(per_scan),
                count(live),
            ],
            match per_scan >= 5000.0 && scans >= 10.0 {
                true => Tone::Aviso,
                false => Tone::Normal,
            },
        );
        // A small table is *supposed* to be read whole: the planner is right and an index
        // there would be slower. The problem starts when each scan walks thousands of rows.
        if per_scan < 5000.0 || scans < 10.0 {
            continue;
        }
        found = true;
        let name = row.get("nome");
        let already: HashSet<String> = indexed.get(row.get("tabela")).cloned().unwrap_or_default();
        let suggestion = columns_for(name, statements, &already);
        report.aviso(
            format!(
                "{} é varrida inteira {} vezes ({} linhas por varredura)",
                row.get("tabela"),
                count(scans),
                count(per_scan)
            ),
            suggestion.unwrap_or_else(|| {
                format!(
                    "veja quais queries filtram {name} e indexe a coluna do WHERE. Com pg_stat_statements ativo esta linha vem preenchida"
                )
            }),
        );
    }
    if !found {
        report.ok(
            "as varreduras registradas são de tabelas pequenas — ler inteiro ali é o plano certo",
        );
    }
    Ok(())
}

/// Each table's indexed leading columns. The leading column is what decides whether an
/// index can serve a filter at all, so it's the right unit for "is this already covered".
fn indexed_columns(
    conn: &mut pg::Conn,
    report: &mut Report,
) -> Result<HashMap<String, HashSet<String>>, String> {
    let sql = "SELECT n.nspname || '.' || t.relname AS tabela, a.attname AS coluna \
               FROM pg_index i \
               JOIN pg_class t ON t.oid = i.indrelid \
               JOIN pg_namespace n ON n.oid = t.relnamespace \
               JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = i.indkey[0] \
               WHERE i.indisvalid AND n.nspname NOT IN ('pg_catalog','information_schema')";
    let mut map: HashMap<String, HashSet<String>> = HashMap::new();
    if let Some(table) = ask(conn, report, "colunas indexadas", sql)? {
        for row in table.iter() {
            map.entry(row.get("tabela").to_string())
                .or_default()
                .insert(row.get("coluna").to_string());
        }
    }
    Ok(map)
}

/// The columns a statement filters or orders by, guessed from its text.
///
/// This is pattern matching on SQL, not parsing it, and it is offered as a guess in the
/// report rather than as a conclusion. It earns its place anyway: the alternative is
/// telling someone "this table is scanned a lot, go and find out why", which is the part
/// they were already doing.
fn hints(query: &str) -> Vec<String> {
    use regex::Regex;
    use std::sync::LazyLock;
    static COMPARISON: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r#"(?i)\b([a-z_][a-z0-9_]*)\s*(?:=|<|>|<=|>=|<>|!=|\bin\b|\blike\b|\bbetween\b)"#,
        )
        .expect("regex constante")
    });
    static ORDERING: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\border\s+by\s+([a-z_][a-z0-9_.]*)").expect("regex constante")
    });
    // Words that match the shape of a column and are not one.
    const NOISE: &[&str] = &[
        "select",
        "where",
        "and",
        "or",
        "not",
        "from",
        "join",
        "on",
        "set",
        "values",
        "limit",
        "offset",
        "case",
        "when",
        "then",
        "else",
        "end",
        "null",
        "true",
        "false",
        "count",
        "sum",
        "min",
        "max",
        "avg",
        "coalesce",
        "cast",
        "as",
        "by",
        "group",
        "order",
        "having",
        "inner",
        "left",
        "right",
        "outer",
        "union",
        "all",
        "distinct",
        "exists",
        "any",
        "with",
        "returning",
    ];
    let mut out: Vec<String> = Vec::new();
    let mut push = |name: &str| {
        let name = name.rsplit('.').next().unwrap_or(name).to_ascii_lowercase();
        if NOISE.contains(&name.as_str()) || name.len() < 2 || out.contains(&name) {
            return;
        }
        out.push(name);
    };
    for capture in COMPARISON.captures_iter(query) {
        push(&capture[1]);
    }
    for capture in ORDERING.captures_iter(query) {
        push(&capture[1]);
    }
    out
}

/// Which tables a statement reads, by name.
fn tables_in(query: &str) -> Vec<String> {
    use regex::Regex;
    use std::sync::LazyLock;
    static SOURCE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?i)\b(?:from|join|update|into)\s+"?([a-z_][a-z0-9_]*)"?\s*(\()?"#)
            .expect("regex constante")
    });
    SOURCE
        .captures_iter(query)
        // O que vem seguido de parêntese é função, não tabela: `FROM generate_series(…)`,
        // `FROM unnest(…)` e — o que engana de verdade — o `FROM` de dentro de
        // `EXTRACT(epoch FROM now())`, que faria «now» passar por nome de tabela.
        .filter(|capture| capture.get(2).is_none())
        .map(|capture| capture[1].to_ascii_lowercase())
        .collect()
}

/// An index suggestion for one table, taken from the statements that actually read it.
fn columns_for(table: &str, statements: &[Stmt], already: &HashSet<String>) -> Option<String> {
    let table = table.to_ascii_lowercase();
    let mut counted: HashMap<String, usize> = HashMap::new();
    for stmt in statements {
        if !tables_in(&stmt.query).contains(&table) {
            continue;
        }
        for column in hints(&stmt.query) {
            if already.contains(&column) {
                continue;
            }
            *counted.entry(column).or_default() += 1;
        }
    }
    let mut ranked: Vec<(String, usize)> = counted.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let best: Vec<String> = ranked.into_iter().take(3).map(|(name, _)| name).collect();
    if best.is_empty() {
        return None;
    }
    Some(format!(
        "as queries que leem esta tabela filtram por {} — nenhuma delas começa um índice hoje. Candidato: CREATE INDEX CONCURRENTLY ON {table} ({});  confirme com EXPLAIN (ANALYZE, BUFFERS) antes",
        best.join(", "),
        best[0]
    ))
}

/// The same suggestion for a single statement, when there is no table to hang it on.
fn index_hint(query: &str) -> String {
    let columns = hints(query);
    if columns.is_empty() {
        return "rode EXPLAIN (ANALYZE, BUFFERS) nesta query: o nó mais caro do plano é onde está a resposta".to_string();
    }
    format!(
        "filtra por {} — confira com EXPLAIN (ANALYZE, BUFFERS) se alguma dessas colunas está sem índice",
        columns.join(", ")
    )
}

fn vacuum(conn: &mut pg::Conn, report: &mut Report) -> Result<(), String> {
    report.section("Vacuum e estatísticas");
    let sql = "SELECT schemaname || '.' || relname AS tabela, n_live_tup, n_dead_tup, \
               CASE WHEN n_live_tup > 0 THEN 100.0 * n_dead_tup / n_live_tup ELSE 0 END AS pct, \
               EXTRACT(epoch FROM now() - GREATEST(last_vacuum, last_autovacuum)) AS desde_vacuum, \
               EXTRACT(epoch FROM now() - GREATEST(last_analyze, last_autoanalyze)) AS desde_analyze, \
               (last_analyze IS NULL AND last_autoanalyze IS NULL) AS nunca_analisada, \
               pg_total_relation_size(relid) AS bytes \
               FROM pg_stat_user_tables ORDER BY n_dead_tup DESC LIMIT 20";
    let Some(table) = ask(conn, report, "estatísticas de vacuum", sql)? else {
        return Ok(());
    };
    let dead: f64 = table.iter().map(|row| row.num("n_dead_tup")).sum();
    if dead == 0.0 {
        report.ok("nenhuma linha morta acumulada nas maiores tabelas");
    }
    let mut flagged = false;
    report.table(&["tabela", "linhas mortas", "% das vivas", "último vacuum"]);
    for row in table.iter() {
        let dead = row.num("n_dead_tup");
        let pct = row.num("pct");
        if dead < 1000.0 {
            continue;
        }
        report.cells(
            vec![
                row.get("tabela").to_string(),
                count(dead),
                format!("{pct:.0}%"),
                duration(row.num("desde_vacuum")),
            ],
            match pct {
                muito if muito >= 50.0 => Tone::Ruim,
                algum if algum >= 20.0 => Tone::Aviso,
                _ => Tone::Normal,
            },
        );
        // Dead rows are not just space: every scan of the table walks over them, and the
        // index entries pointing at them are walked too.
        if pct >= 50.0 && dead > 50_000.0 {
            flagged = true;
            report.grave(
                format!(
                    "{} tem {} linhas mortas, {pct:.0}% do tamanho vivo — a tabela e seus índices estão inchados",
                    row.get("tabela"),
                    count(dead)
                ),
                "o autovacuum não está dando conta dela. Ajuste autovacuum_vacuum_scale_factor só para esta tabela (ALTER TABLE ... SET) em vez de mexer no servidor inteiro",
            );
        } else if pct >= 20.0 && dead > 10_000.0 {
            flagged = true;
            report.aviso(
                format!(
                    "{} com {pct:.0}% de linhas mortas ({})",
                    row.get("tabela"),
                    count(dead)
                ),
                "vale acompanhar: se não cair sozinho, o autovacuum desta tabela está chegando tarde demais",
            );
        }
    }
    if dead > 0.0 && !flagged {
        report.ok("há linhas mortas, mas em proporção normal para tabelas ativas");
    }

    for row in table.iter() {
        if row.flag("nunca_analisada") && row.num("n_live_tup") > 10_000.0 {
            report.aviso(
                format!(
                    "{} nunca passou por ANALYZE e tem {} linhas — o planejador está escolhendo planos no escuro",
                    row.get("tabela"),
                    count(row.num("n_live_tup"))
                ),
                "um ANALYZE nessa tabela é barato e costuma mudar plano ruim em plano bom na hora (é escrita de estatística, então não é feito daqui)",
            );
        }
    }
    Ok(())
}

fn wraparound(conn: &mut pg::Conn, report: &mut Report, depth: Depth) -> Result<(), String> {
    report.section("Congelamento (wraparound)");
    let sql = "SELECT datname, age(datfrozenxid) AS idade FROM pg_database \
               WHERE datallowconn ORDER BY age(datfrozenxid) DESC LIMIT 5";
    let Some(table) = ask(conn, report, "idade das transações", sql)? else {
        return Ok(());
    };
    let limit = conn
        .query(
            "SELECT setting::float AS n FROM pg_settings WHERE name = 'autovacuum_freeze_max_age'",
        )
        .ok()
        .and_then(|table| table.first().map(|row| row.num("n")))
        .unwrap_or(200_000_000.0);
    report.table(&["base", "transações desde o congelamento", "% do limite"]);
    for row in table.iter() {
        let age = row.num("idade");
        let pct = 100.0 * age / 2_000_000_000.0;
        report.cells(
            vec![
                row.get("datname").to_string(),
                count(age),
                format!("{pct:.1}%"),
            ],
            match pct {
                perto if perto >= 60.0 => Tone::Ruim,
                algum if algum >= 10.0 => Tone::Aviso,
                _ => Tone::Normal,
            },
        );
        if age > 1_200_000_000.0 {
            report.grave(
                format!(
                    "{} está a {} transações do congelamento forçado — quando chegar, o banco para de aceitar escrita",
                    row.get("datname"),
                    count(2_000_000_000.0 - age)
                ),
                "é o incidente que derruba o banco sem aviso. VACUUM (FREEZE) nas tabelas mais velhas, e descubra o que impede o autovacuum de avançar (transação antiga? slot de replicação parado?)",
            );
        } else if age > limit * 1.5 {
            report.aviso(
                format!(
                    "{} acumulou {} transações, acima de autovacuum_freeze_max_age",
                    row.get("datname"),
                    count(age)
                ),
                "o autovacuum já deveria ter congelado. Normalmente é uma transação antiga ou um slot de replicação inativo segurando o horizonte",
            );
        }
    }
    if depth >= Depth::Profundo {
        let sql = "SELECT n.nspname || '.' || c.relname AS tabela, age(c.relfrozenxid) AS idade \
                   FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                   WHERE c.relkind IN ('r','m','t') AND n.nspname NOT IN ('pg_catalog','information_schema') \
                   ORDER BY age(c.relfrozenxid) DESC LIMIT 5";
        if let Some(table) = ask(conn, report, "idade por tabela", sql)? {
            report.line("tabelas mais atrasadas no congelamento:");
            for row in table.iter() {
                report.row(format!(
                    "{:<40} {}",
                    row.get("tabela"),
                    count(row.num("idade"))
                ));
            }
        }
    }
    Ok(())
}

fn replication(conn: &mut pg::Conn, report: &mut Report, version: i64) -> Result<(), String> {
    if version < 100_000 {
        return Ok(());
    }
    report.section("Replicação");
    let sql = "SELECT slot_name, COALESCE(slot_type,'') AS tipo, active, \
               COALESCE(pg_wal_lsn_diff(pg_current_wal_lsn(), restart_lsn), 0) AS retido \
               FROM pg_replication_slots";
    if let Some(table) = ask(conn, report, "slots de replicação", sql)? {
        for row in table.iter() {
            let held = row.num("retido");
            report.row(format!(
                "slot {:<30} {}  segurando {}",
                row.get("slot_name"),
                if row.flag("active") {
                    "ativo"
                } else {
                    "INATIVO"
                },
                bytes(held)
            ));
            // An abandoned slot is a disk-full incident on a timer: the server keeps
            // every WAL segment the slot has not consumed, forever.
            if !row.flag("active") {
                report.grave(
                    format!(
                        "slot {} está inativo e segurando {} de WAL",
                        row.get("slot_name"),
                        bytes(held)
                    ),
                    "enquanto ninguém consumir esse slot, o WAL não é reciclado e o disco enche. Se a réplica/consumidor não volta, o slot precisa ser removido por quem cuida do cluster",
                );
            }
        }
    }

    let sql = "SELECT COALESCE(application_name,'') AS nome, COALESCE(client_addr::text,'') AS origem, \
               state, COALESCE(sync_state,'') AS sincronia, \
               COALESCE(EXTRACT(epoch FROM replay_lag), 0) AS atraso, \
               COALESCE(pg_wal_lsn_diff(pg_current_wal_lsn(), replay_lsn), 0) AS bytes_atras \
               FROM pg_stat_replication";
    if let Some(table) = ask(conn, report, "réplicas conectadas", sql)? {
        if table.is_empty() {
            report.line("nenhuma réplica conectada a este servidor");
        }
        for row in table.iter() {
            report.row(format!(
                "{} ({}) {} / {} — atraso {} ({} atrás)",
                row.get("nome"),
                row.get("origem"),
                row.get("state"),
                row.get("sincronia"),
                duration(row.num("atraso")),
                bytes(row.num("bytes_atras"))
            ));
            if row.num("atraso") > 60.0 {
                report.aviso(
                    format!(
                        "réplica {} atrasada {}",
                        row.get("nome"),
                        duration(row.num("atraso"))
                    ),
                    "quem lê dessa réplica está lendo o passado. Costuma ser disco ou rede da réplica, ou um conflito de recuperação",
                );
            }
        }
    }
    Ok(())
}

fn checkpoints(conn: &mut pg::Conn, report: &mut Report, version: i64) -> Result<(), String> {
    report.section("Checkpoints");
    // Postgres 17 moved the counters out of pg_stat_bgwriter into their own view and
    // renamed them on the way.
    let sql = if version >= 170_000 {
        "SELECT num_timed AS agendados, num_requested AS forcados, \
         EXTRACT(epoch FROM now() - stats_reset) AS desde FROM pg_stat_checkpointer"
    } else {
        "SELECT checkpoints_timed AS agendados, checkpoints_req AS forcados, \
         EXTRACT(epoch FROM now() - stats_reset) AS desde FROM pg_stat_bgwriter"
    };
    let Some(table) = ask(conn, report, "contadores de checkpoint", sql)? else {
        return Ok(());
    };
    let Some(row) = table.first() else {
        return Ok(());
    };
    let scheduled = row.num("agendados");
    let forced = row.num("forcados");
    report.field(
        "checkpoints",
        format!(
            "{} no horário, {} forçados ({})",
            count(scheduled),
            count(forced),
            duration(row.num("desde"))
        ),
    );
    // A forced checkpoint means the WAL filled up before the timer went off: the server
    // is flushing on the writer's schedule instead of its own.
    if forced > 0.0 && forced > scheduled * 0.3 {
        report.aviso(
            format!(
                "{:.0}% dos checkpoints são forçados por volume de WAL, não pelo tempo",
                100.0 * forced / (forced + scheduled).max(1.0)
            ),
            "max_wal_size pequeno para a carga de escrita: subir espalha a escrita e tira os picos de I/O",
        );
    }
    Ok(())
}

fn sizes(conn: &mut pg::Conn, report: &mut Report) -> Result<(), String> {
    report.section("Volumes");
    let sql = "SELECT n.nspname || '.' || c.relname AS tabela, \
               pg_total_relation_size(c.oid) AS total, \
               pg_relation_size(c.oid) AS dados, \
               pg_indexes_size(c.oid) AS indices, \
               COALESCE(pg_total_relation_size(c.reltoastrelid), 0) AS toast, \
               c.reltuples AS linhas \
               FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
               WHERE c.relkind IN ('r','p','m') \
               AND n.nspname NOT IN ('pg_catalog','information_schema','pg_toast') \
               ORDER BY pg_total_relation_size(c.oid) DESC LIMIT 20";
    let Some(table) = ask(conn, report, "tamanho das tabelas", sql)? else {
        return Ok(());
    };
    report.table(&["total", "tabela", "dados", "índices", "toast", "linhas"]);
    for row in table.iter() {
        report.cells_headline(
            vec![
                bytes(row.num("total")),
                row.get("tabela").to_string(),
                bytes(row.num("dados")),
                bytes(row.num("indices")),
                match row.num("toast") {
                    toast if toast > 0.0 => bytes(toast),
                    _ => String::new(),
                },
                count(row.num("linhas")),
            ],
            Tone::Normal,
        );
    }
    for row in table.iter() {
        let data = row.num("dados");
        let indexes = row.num("indices");
        if indexes > data && data > 50.0 * 1024.0 * 1024.0 {
            report.aviso(
                format!(
                    "{} tem mais índice ({}) do que dado ({})",
                    row.get("tabela"),
                    bytes(indexes),
                    bytes(data)
                ),
                "quase sempre é índice a mais, não dado a menos: confira a lista de índices sem uso e de redundantes acima",
            );
        }
    }
    Ok(())
}

fn foreign_keys(conn: &mut pg::Conn, report: &mut Report) -> Result<(), String> {
    report.section("Chaves estrangeiras sem índice");
    let sql = "SELECT c.conrelid::regclass::text AS tabela, c.conname AS chave, \
               c.confrelid::regclass::text AS referencia, \
               (SELECT string_agg(a.attname, ', ') FROM pg_attribute a \
                WHERE a.attrelid = c.conrelid AND a.attnum = ANY(c.conkey)) AS colunas, \
               pg_total_relation_size(c.conrelid) AS bytes \
               FROM pg_constraint c \
               JOIN pg_class t ON t.oid = c.conrelid \
               JOIN pg_namespace n ON n.oid = t.relnamespace \
               WHERE c.contype = 'f' AND n.nspname NOT IN ('pg_catalog','information_schema') \
               AND NOT EXISTS (SELECT 1 FROM pg_index i WHERE i.indrelid = c.conrelid \
                               AND i.indisvalid AND i.indkey[0] = c.conkey[1]) \
               ORDER BY pg_total_relation_size(c.conrelid) DESC LIMIT 20";
    let Some(table) = ask(conn, report, "chaves estrangeiras", sql)? else {
        return Ok(());
    };
    if table.is_empty() {
        report.ok("toda chave estrangeira tem índice começando pela sua coluna");
        return Ok(());
    }
    report.aviso(
        format!(
            "{} chave(s) estrangeira(s) sem índice do lado que referencia",
            table.len()
        ),
        "sem esse índice, cada DELETE ou UPDATE da chave na tabela pai varre a tabela filha inteira — e o faz segurando lock",
    );
    report.table(&[
        "tabela",
        "colunas",
        "referencia",
        "tamanho",
        "o índice que falta",
    ]);
    for row in table.iter() {
        report.cells(
            vec![
                row.get("tabela").to_string(),
                row.get("colunas").to_string(),
                row.get("referencia").to_string(),
                bytes(row.num("bytes")),
                format!(
                    "CREATE INDEX CONCURRENTLY ON {} ({});",
                    row.get("tabela"),
                    row.get("colunas")
                ),
            ],
            Tone::Aviso,
        );
    }
    Ok(())
}

fn sequences(conn: &mut pg::Conn, report: &mut Report) -> Result<(), String> {
    report.section("Sequências");
    let sql = "SELECT schemaname || '.' || sequencename AS seq, \
               COALESCE(last_value, 0) AS valor, max_value, \
               CASE WHEN max_value > 0 THEN 100.0 * COALESCE(last_value,0) / max_value ELSE 0 END AS pct \
               FROM pg_sequences WHERE COALESCE(last_value,0) > 0 \
               ORDER BY pct DESC LIMIT 10";
    let Some(table) = ask(conn, report, "pg_sequences", sql)? else {
        return Ok(());
    };
    if table.is_empty() {
        report.ok("nenhuma sequência em uso nesta base");
    }
    let mut values: HashMap<String, f64> = HashMap::new();
    for row in table.iter() {
        values.insert(row.get("seq").to_string(), row.num("valor"));
        if row.num("pct") > 70.0 {
            report.grave(
                format!(
                    "sequência {} já usou {:.0}% do seu limite",
                    row.get("seq"),
                    row.num("pct")
                ),
                "quando estourar, todo INSERT que depende dela falha. A correção é trocar o tipo da coluna, e isso é uma migração planejada",
            );
        }
    }

    // The classic outage: the sequence has room to spare, and the column it feeds is a
    // 32-bit integer that does not.
    let sql = "SELECT n.nspname || '.' || c.relname AS tabela, a.attname AS coluna, \
               pg_get_serial_sequence(n.nspname || '.' || c.relname, a.attname) AS seq \
               FROM pg_attribute a \
               JOIN pg_class c ON c.oid = a.attrelid \
               JOIN pg_namespace n ON n.oid = c.relnamespace \
               WHERE a.atttypid = 'int4'::regtype AND a.attnum > 0 AND NOT a.attisdropped \
               AND c.relkind = 'r' AND n.nspname NOT IN ('pg_catalog','information_schema') \
               AND pg_get_serial_sequence(n.nspname || '.' || c.relname, a.attname) IS NOT NULL";
    if let Some(table) = ask(conn, report, "colunas serial de 32 bits", sql)? {
        for row in table.iter() {
            let current = values.get(row.get("seq")).copied().unwrap_or(0.0);
            let pct = 100.0 * current / 2_147_483_647.0;
            if pct < 50.0 {
                continue;
            }
            let line = format!(
                "{}.{} é integer (limite 2.147.483.647) e a sequência já está em {} — {pct:.0}%",
                row.get("tabela"),
                row.get("coluna"),
                count(current)
            );
            let fix = "migrar a coluna para bigint antes de chegar lá. Depois que estoura, cada INSERT falha e a migração tem que ser feita com o sistema fora do ar";
            if pct > 80.0 {
                report.grave(line, fix);
            } else {
                report.aviso(line, fix);
            }
        }
    }
    Ok(())
}

fn no_primary_key(conn: &mut pg::Conn, report: &mut Report) -> Result<(), String> {
    report.section("Modelagem");
    let sql = "SELECT n.nspname || '.' || c.relname AS tabela, c.reltuples::bigint AS linhas \
               FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
               WHERE c.relkind = 'r' AND n.nspname NOT IN ('pg_catalog','information_schema') \
               AND NOT EXISTS (SELECT 1 FROM pg_index i WHERE i.indrelid = c.oid AND i.indisprimary) \
               AND c.reltuples > 1000 ORDER BY c.reltuples DESC LIMIT 10";
    let Some(table) = ask(conn, report, "tabelas sem chave primária", sql)? else {
        return Ok(());
    };
    if table.is_empty() {
        report.ok("toda tabela com volume relevante tem chave primária");
        return Ok(());
    }
    report.aviso(
        format!("{} tabela(s) grandes sem chave primária", table.len()),
        "sem chave primária não há como apagar uma linha duplicada com segurança, e a replicação lógica simplesmente não replica a tabela",
    );
    report.table(&["tabela", "linhas (estimadas)"]);
    for row in table.iter() {
        report.cells(
            vec![row.get("tabela").to_string(), count(row.num("linhas"))],
            Tone::Aviso,
        );
    }
    Ok(())
}

fn extensions(conn: &mut pg::Conn, report: &mut Report) -> Result<(), String> {
    report.section("Extensões");
    let sql = "SELECT extname || ' ' || extversion AS ext FROM pg_extension ORDER BY extname";
    if let Some(table) = ask(conn, report, "extensões", sql)? {
        let list: Vec<String> = table.iter().map(|row| row.get("ext").to_string()).collect();
        report.field("instaladas", list.join(", "));
    }
    Ok(())
}

/// Uma forma de query que já se viu rodar, e o que ela custou da última vez.
struct Recente {
    visto: std::time::Instant,
    /// Quantas execuções desde que esta ferramenta começou a olhar.
    calls: f64,
    /// Média da última janela observada, em milissegundos.
    mean: f64,
    total: f64,
    temp: f64,
    query: String,
}

/// Quantas formas de query o histórico guarda. Mais que isto não cabe na tela cheia de um
/// cartão sem virar um log, que é justamente o que este painel não é.
const RECENTES: usize = 40;

/// A query seen running, so its end can be reported as well as its start.
struct Live {
    query: String,
    duration: f64,
    announced: bool,
}

/// A snapshot of `pg_stat_statements`, keyed by statement.
fn statement_counters(conn: &mut pg::Conn, total_column: &str) -> HashMap<String, Stmt> {
    let sql = format!(
        "SELECT COALESCE(queryid::text, md5(query)) AS id, calls, rows, \
         {total_column} AS total, 0 AS media, shared_blks_hit AS hit, \
         shared_blks_read AS miss, temp_blks_written AS temp, query \
         FROM pg_stat_statements \
         WHERE dbid = (SELECT oid FROM pg_database WHERE datname = current_database())"
    );
    let mut map = HashMap::new();
    if let Ok(table) = conn.query(&sql) {
        for row in table.iter() {
            if monitoring(row.get("query")) {
                continue;
            }
            map.insert(
                row.get("id").to_string(),
                Stmt {
                    calls: row.num("calls"),
                    total: row.num("total"),
                    mean: 0.0,
                    rows: row.num("rows"),
                    hit: row.num("hit"),
                    miss: row.num("miss"),
                    temp: row.num("temp"),
                    query: row.get("query").to_string(),
                },
            );
        }
    }
    map
}

/// What ran between two snapshots, and what it cost, heaviest first.
fn delta_of(before: &HashMap<String, Stmt>, after: &HashMap<String, Stmt>) -> Vec<Stmt> {
    let mut recent: Vec<Stmt> = Vec::new();
    for (id, now) in after {
        let was = before.get(id);
        let calls = now.calls - was.map(|stmt| stmt.calls).unwrap_or(0.0);
        if calls <= 0.0 {
            continue;
        }
        let total = (now.total - was.map(|stmt| stmt.total).unwrap_or(0.0)).max(0.0);
        recent.push(Stmt {
            calls,
            total,
            mean: total / calls,
            rows: (now.rows - was.map(|stmt| stmt.rows).unwrap_or(0.0)).max(0.0),
            hit: (now.hit - was.map(|stmt| stmt.hit).unwrap_or(0.0)).max(0.0),
            miss: (now.miss - was.map(|stmt| stmt.miss).unwrap_or(0.0)).max(0.0),
            temp: (now.temp - was.map(|stmt| stmt.temp).unwrap_or(0.0)).max(0.0),
            query: now.query.clone(),
        });
    }
    recent.sort_by(|a, b| b.total.total_cmp(&a.total));
    recent
}

/// O que uma tabela registrou desde que o servidor subiu. A diferença entre duas leituras
/// é o que aconteceu no intervalo — e é a única fonte de «o que este banco andou fazendo»
/// que existe em **todo** servidor, com ou sem extensão nenhuma instalada.
#[derive(Clone, Copy, Default)]
struct Atividade {
    seq_scan: f64,
    seq_tup: f64,
    idx_scan: f64,
    idx_tup: f64,
    inseridas: f64,
    atualizadas: f64,
    apagadas: f64,
    vivas: f64,
}

impl Atividade {
    fn menos(&self, antes: &Atividade) -> Atividade {
        Atividade {
            seq_scan: (self.seq_scan - antes.seq_scan).max(0.0),
            seq_tup: (self.seq_tup - antes.seq_tup).max(0.0),
            idx_scan: (self.idx_scan - antes.idx_scan).max(0.0),
            idx_tup: (self.idx_tup - antes.idx_tup).max(0.0),
            inseridas: (self.inseridas - antes.inseridas).max(0.0),
            atualizadas: (self.atualizadas - antes.atualizadas).max(0.0),
            apagadas: (self.apagadas - antes.apagadas).max(0.0),
            vivas: self.vivas,
        }
    }

    fn leituras(&self) -> f64 {
        self.seq_scan + self.idx_scan
    }

    fn escritas(&self) -> f64 {
        self.inseridas + self.atualizadas + self.apagadas
    }

    fn houve_algo(&self) -> bool {
        self.leituras() + self.escritas() > 0.0
    }
}

fn table_counters(conn: &mut pg::Conn) -> HashMap<String, Atividade> {
    let sql = "SELECT schemaname || '.' || relname AS tabela, seq_scan, seq_tup_read, \
               COALESCE(idx_scan,0) AS idx_scan, COALESCE(idx_tup_fetch,0) AS idx_tup, \
               n_tup_ins, n_tup_upd, n_tup_del, n_live_tup \
               FROM pg_stat_user_tables";
    let mut map = HashMap::new();
    if let Ok(table) = conn.query(sql) {
        for row in table.iter() {
            map.insert(
                row.get("tabela").to_string(),
                Atividade {
                    seq_scan: row.num("seq_scan"),
                    seq_tup: row.num("seq_tup_read"),
                    idx_scan: row.num("idx_scan"),
                    idx_tup: row.num("idx_tup"),
                    inseridas: row.num("n_tup_ins"),
                    atualizadas: row.num("n_tup_upd"),
                    apagadas: row.num("n_tup_del"),
                    vivas: row.num("n_live_tup"),
                },
            );
        }
    }
    map
}

/// O que cada tabela fez no intervalo, da mais movimentada para a menos.
fn activity_delta(
    before: &HashMap<String, Atividade>,
    after: &HashMap<String, Atividade>,
) -> Vec<(String, Atividade)> {
    let mut linhas: Vec<(String, Atividade)> = after
        .iter()
        .map(|(nome, agora)| {
            (
                nome.clone(),
                agora.menos(&before.get(nome).copied().unwrap_or_default()),
            )
        })
        .filter(|(_, delta)| delta.houve_algo())
        .collect();
    linhas.sort_by(|a, b| {
        (b.1.leituras() + b.1.escritas()).total_cmp(&(a.1.leituras() + a.1.escritas()))
    });
    linhas
}

/// Which tables were read end to end between two snapshots: name, scans, rows, and rows
/// per scan. Small tables are left out — reading one whole is the right plan, and saying
/// so every three seconds would be noise standing where a finding should be.
fn table_delta(
    before: &HashMap<String, Atividade>,
    after: &HashMap<String, Atividade>,
) -> Vec<(String, f64, f64, f64)> {
    let mut hits: Vec<(String, f64, f64, f64)> = Vec::new();
    for (name, agora) in after {
        let antes = before.get(name).copied().unwrap_or_default();
        let scanned = (agora.seq_scan - antes.seq_scan).max(0.0);
        let rows = (agora.seq_tup - antes.seq_tup).max(0.0);
        if scanned <= 0.0 || agora.vivas < 1000.0 {
            continue;
        }
        let per_scan = rows / scanned;
        if per_scan < 1000.0 {
            continue;
        }
        hits.push((name.clone(), scanned, rows, per_scan));
    }
    hits.sort_by(|a, b| b.2.total_cmp(&a.2));
    hits
}

/// The columns a statement filters by that no index starts with — the suggestion, when
/// there is one to make.
fn missing_columns(query: &str, indexed: &HashMap<String, HashSet<String>>) -> Option<String> {
    let mut wanted: Vec<String> = Vec::new();
    for table in tables_in(query) {
        let known = indexed
            .iter()
            .find(|(name, _)| name.rsplit('.').next() == Some(table.as_str()))
            .map(|(_, columns)| columns.clone())
            .unwrap_or_default();
        for column in hints(query) {
            if !known.contains(&column) && !wanted.contains(&column) {
                wanted.push(column);
            }
        }
    }
    if wanted.is_empty() {
        return None;
    }
    Some(format!(
        "filtra por {} e nenhuma dessas colunas começa um índice: candidata a CREATE INDEX CONCURRENTLY",
        wanted.join(", ")
    ))
}
