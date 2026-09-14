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
    quebrar,
};
use crate::painel::Tone;

/// How many rows of any one listing are worth reading in a terminal.
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
        who(&mut self.conn, report)?;
        locks(&mut self.conn, report)?;
        self.reset = activity(&mut self.conn, report, &self.settings)?;
        if report.stopping() {
            return Ok(());
        }
        settings_card(report, &self.settings, self.depth);
        self.indexed = indexed_columns(&mut self.conn, report)?;
        indexes(&mut self.conn, report, self.depth, &self.reset)?;
        table_cache(&mut self.conn, report)?;
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
            vacuum(&mut self.conn, report, &self.reset)?;
            stale_stats(&mut self.conn, report)?;
            horizon(&mut self.conn, report)?;
            wraparound(&mut self.conn, report, self.depth)?;
            replication(&mut self.conn, report, self.version)?;
            checkpoints(&mut self.conn, report, self.version)?;
            write_activity(&mut self.conn, report, self.version)?;
        }
        if self.depth >= Depth::Profundo {
            if report.stopping() {
                return Ok(());
            }
            tables(&mut self.conn, report)?;
            bloat(&mut self.conn, report)?;
            foreign_keys(&mut self.conn, report)?;
            sequences(&mut self.conn, report)?;
            no_primary_key(&mut self.conn, report)?;
            security(&mut self.conn, report)?;
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
            report.cells_deep(
                vec![
                    count(delta.leituras()),
                    nome.clone(),
                    como.clone(),
                    escrita.clone(),
                ],
                match delta.seq_scan > 0.0 && delta.vivas > 1000.0 {
                    true => Tone::Aviso,
                    false => Tone::Normal,
                },
                vec![
                    ("tabela", nome.clone()),
                    ("leituras no intervalo", count(delta.leituras())),
                    ("por índice", count(delta.idx_scan)),
                    ("varrendo a tabela inteira", count(delta.seq_scan)),
                    ("linhas lidas varrendo", count(delta.seq_tup)),
                    ("linhas lidas por índice", count(delta.idx_tup)),
                    ("inseridas", count(delta.inseridas)),
                    ("atualizadas", count(delta.atualizadas)),
                    ("apagadas", count(delta.apagadas)),
                    ("linhas vivas", count(delta.vivas)),
                ],
                Vec::new(),
            );
            report.destacar();
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
                   COALESCE(client_addr::text, 'local') AS origem, \
                   COALESCE(backend_type, '') AS tipo, \
                   EXTRACT(epoch FROM now() - backend_start) AS conectado_ha, \
                   EXTRACT(epoch FROM now() - xact_start) AS transacao_ha, \
                   COALESCE(backend_xid::text, '') AS xid, \
                   left(query, 2000) AS query \
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
            report.cells_deep(
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
                vec![
                    ("pid", row.get("pid").to_string()),
                    ("usuário", row.get("usuario").to_string()),
                    ("aplicação", row.get("app").to_string()),
                    ("origem", row.get("origem").to_string()),
                    ("tipo de processo", row.get("tipo").to_string()),
                    ("rodando há", duration(age)),
                    (
                        "transação aberta há",
                        match row.get("transacao_ha") {
                            "" => String::new(),
                            _ => duration(row.num("transacao_ha")),
                        },
                    ),
                    ("conectado há", duration(row.num("conectado_ha"))),
                    (
                        "esperando",
                        match row.get("espera") {
                            "" => "nada — está usando CPU".to_string(),
                            evento => format!("{}/{evento}", row.get("espera_tipo")),
                        },
                    ),
                    (
                        "travado por",
                        match row.get("bloqueadores") {
                            "" => String::new(),
                            pids => format!("pid(s) {pids}"),
                        },
                    ),
                    (
                        "id da transação",
                        match row.get("xid") {
                            "" => "nenhuma escrita ainda".to_string(),
                            xid => xid.to_string(),
                        },
                    ),
                    ("tabelas que lê", tables_in(row.get("query")).join(", ")),
                    ("colunas filtradas", hints(row.get("query")).join(", ")),
                ],
                quebrar(row.get("query"), 150),
            );
        }
        if running == 0 {
            report.ok("nenhuma query em execução neste instante");
        }
        // O servidor também trabalha por conta própria, e o que ele está fazendo explica
        // I/O que nenhuma query justifica.
        progress(&mut self.conn, report, self.version)?;
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
                report.cells_deep(celulas, tom, recente.fatos(), quebrar(&recente.query, 150));
                // As primeiras também no cartão da grade; o resto só na tabela cheia.
                if posicao < 6 {
                    report.destacar();
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
        report.cells_deep(
            vec![
                row.get("estado").to_string(),
                row.get("n").to_string(),
                oldest.clone(),
            ],
            Tone::Normal,
            vec![
                ("estado", row.get("estado").to_string()),
                ("quantas conexões", row.get("n").to_string()),
                (
                    "a mais antiga neste estado",
                    match oldest.is_empty() {
                        true => String::new(),
                        false => format!("há {oldest}"),
                    },
                ),
                (
                    "o que quer dizer",
                    match row.get("estado") {
                        "active" => "está executando uma query agora".to_string(),
                        "idle" => "conectada e sem fazer nada — normal num pool".to_string(),
                        e if e.starts_with("idle in transaction (aborted)") =>
                            "transação aberta que já deu erro e ninguém fechou: segura lock e horizonte do mesmo jeito".to_string(),
                        e if e.starts_with("idle in transaction") =>
                            "transação aberta sem trabalho: segura locks e trava o vacuum enquanto durar".to_string(),
                        "fastpath function call" => "executando uma função pelo caminho curto".to_string(),
                        _ => "processo interno do servidor, não é conexão de cliente".to_string(),
                    },
                ),
            ],
            Vec::new(),
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

    // O que está rodando agora é assunto do cartão «Agora», e repetir aqui seria a mesma
    // query aparecendo duas vezes na mesma tela com palavras diferentes.
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

/// Um índice como o catálogo o descreve.
struct Indice {
    nome: String,
    tabela: String,
    /// As colunas como `indkey` as guarda: números separados por espaço, o que faz de
    /// «é prefixo de» uma comparação de texto.
    colunas: String,
    definicao: String,
    /// O método de acesso — dois índices de tipos diferentes não se substituem.
    tipo: String,
    unico: bool,
    primaria: bool,
    parcial: bool,
    valido: bool,
    bytes: f64,
    usos: f64,
    lidas: f64,
    buscadas: f64,
}

/// Todo índice da base numa tabela só: tamanho, uso, e o que ele é.
///
/// Uma linha por índice, com o que se sabe dele por trás — a definição inteira, quantas
/// linhas ele já entregou, quanto ele custa em disco. É a seção em que mais se navega, e
/// por isso é a que mais carrega detalhe: a pergunta «este índice vale o que custa?» se
/// responde olhando cinco números que não cabem numa linha de tabela.
fn indexes(
    conn: &mut pg::Conn,
    report: &mut Report,
    depth: Depth,
    reset: &Reset,
) -> Result<(), String> {
    report.section("Índices");

    let sql = "SELECT n.nspname || '.' || t.relname AS tabela, c.relname AS indice, \
               i.indkey::text AS colunas, am.amname AS tipo, \
               i.indisunique AS unico, i.indisprimary AS primaria, i.indisvalid AS valido, \
               (i.indpred IS NOT NULL) AS parcial, \
               pg_relation_size(i.indexrelid) AS bytes, \
               pg_get_indexdef(i.indexrelid) AS definicao, \
               COALESCE(s.idx_scan, 0) AS usos, \
               COALESCE(s.idx_tup_read, 0) AS lidas, \
               COALESCE(s.idx_tup_fetch, 0) AS buscadas \
               FROM pg_index i \
               JOIN pg_class c ON c.oid = i.indexrelid \
               JOIN pg_class t ON t.oid = i.indrelid \
               JOIN pg_namespace n ON n.oid = t.relnamespace \
               JOIN pg_am am ON am.oid = c.relam \
               LEFT JOIN pg_stat_user_indexes s ON s.indexrelid = i.indexrelid \
               WHERE n.nspname NOT IN ('pg_catalog','information_schema','pg_toast') \
               ORDER BY pg_relation_size(i.indexrelid) DESC";
    let Some(table) = ask(conn, report, "índices", sql)? else {
        return Ok(());
    };
    let indices: Vec<Indice> = table
        .iter()
        .map(|row| Indice {
            nome: row.get("indice").to_string(),
            tabela: row.get("tabela").to_string(),
            colunas: row.get("colunas").to_string(),
            definicao: row.get("definicao").to_string(),
            tipo: row.get("tipo").to_string(),
            unico: row.flag("unico"),
            primaria: row.flag("primaria"),
            parcial: row.flag("parcial"),
            valido: row.flag("valido"),
            bytes: row.num("bytes"),
            usos: row.num("usos"),
            lidas: row.num("lidas"),
            buscadas: row.num("buscadas"),
        })
        .collect();
    if indices.is_empty() {
        report.line("nenhum índice fora dos esquemas do sistema");
        return Ok(());
    }

    // Redundância é fato de catálogo, não estatística: um índice em (a) ao lado de um em
    // (a, b) é peso morto diga o contador o que disser, porque tudo que o primeiro responde
    // o segundo responde também.
    let mut coberto: HashMap<String, String> = HashMap::new();
    for indice in &indices {
        if indice.unico
            || indice.primaria
            || indice.parcial
            || indice.colunas.split(' ').any(|c| c == "0")
        {
            continue;
        }
        let prefixo = format!("{} ", indice.colunas);
        if let Some(maior) = indices.iter().find(|outro| {
            outro.nome != indice.nome
                && outro.tabela == indice.tabela
                && outro.tipo == indice.tipo
                && !outro.parcial
                && outro.colunas.starts_with(&prefixo)
        }) {
            coberto.insert(indice.nome.clone(), maior.nome.clone());
        }
    }

    let total: f64 = indices.iter().map(|indice| indice.bytes).sum();
    report.field(
        "ao todo",
        format!(
            "{} índices ocupando {} · contadores {}",
            indices.len(),
            bytes(total),
            reset.describe()
        ),
    );
    report.table(&["tamanho", "índice", "tabela", "usos", "situação"]);
    let (mut sem_uso, mut sem_uso_bytes) = (0usize, 0.0);
    for indice in &indices {
        let mut situacao: Vec<String> = Vec::new();
        if indice.primaria {
            situacao.push("chave primária".to_string());
        } else if indice.unico {
            situacao.push("único".to_string());
        }
        if indice.parcial {
            situacao.push("parcial".to_string());
        }
        if !indice.valido {
            situacao.push("INVÁLIDO".to_string());
        }
        if let Some(maior) = coberto.get(&indice.nome) {
            situacao.push(format!("coberto por {maior}"));
        }
        if indice.usos == 0.0 && !indice.primaria && !indice.unico {
            situacao.push("nunca usado".to_string());
            sem_uso += 1;
            sem_uso_bytes += indice.bytes;
        }
        let tom = match (
            indice.valido,
            indice.usos == 0.0 && !indice.primaria && !indice.unico,
        ) {
            (false, _) => Tone::Ruim,
            (_, true) => Tone::Aviso,
            _ => match coberto.contains_key(&indice.nome) {
                true => Tone::Aviso,
                false => Tone::Normal,
            },
        };
        report.cells_deep(
            vec![
                bytes(indice.bytes),
                indice.nome.clone(),
                indice.tabela.clone(),
                count(indice.usos),
                situacao.join(" · "),
            ],
            tom,
            vec![
                ("tabela", indice.tabela.clone()),
                ("método", indice.tipo.clone()),
                ("tamanho", bytes(indice.bytes)),
                ("varreduras", count(indice.usos)),
                ("linhas entregues", count(indice.lidas)),
                (
                    "linhas buscadas na tabela",
                    format!(
                        "{} ({})",
                        count(indice.buscadas),
                        match indice.lidas {
                            0.0 => "—".to_string(),
                            lidas => format!("{:.0}% do que leu", 100.0 * indice.buscadas / lidas),
                        }
                    ),
                ),
                (
                    "por varredura",
                    match indice.usos {
                        0.0 => String::new(),
                        usos => format!("{} linhas", count(indice.lidas / usos)),
                    },
                ),
                (
                    "único",
                    (if indice.unico { "sim" } else { "não" }).to_string(),
                ),
                (
                    "chave primária",
                    (if indice.primaria { "sim" } else { "não" }).to_string(),
                ),
                (
                    "parcial",
                    (if indice.parcial { "sim" } else { "não" }).to_string(),
                ),
                (
                    "válido",
                    (if indice.valido { "sim" } else { "NÃO" }).to_string(),
                ),
                (
                    "coberto por",
                    coberto.get(&indice.nome).cloned().unwrap_or_default(),
                ),
            ],
            vec![format!("{};", indice.definicao)],
        );
    }

    for indice in indices.iter().filter(|indice| !indice.valido) {
        report.grave(
            format!(
                "índice {} (tabela {}) está inválido: ninguém o usa e toda escrita na tabela continua pagando por ele",
                indice.nome, indice.tabela
            ),
            "sobra de um CREATE INDEX CONCURRENTLY que falhou. Confira e recrie — o REINDEX/DROP é decisão sua",
        );
    }
    if sem_uso == 0 {
        report.ok("todo índice não-único desta base já foi usado ao menos uma vez");
    } else {
        let frase = format!(
            "{sem_uso} índice(s) nunca usados ocupando {} ({})",
            bytes(sem_uso_bytes),
            reset.describe()
        );
        if reset.trustworthy() {
            report.aviso(
                frase,
                "cada um deles é espaço, e mais lentidão em todo INSERT e UPDATE da tabela. Confira se não servem a um relatório mensal antes de largar",
            );
        } else {
            report.line(format!(
                "{frase} — contadores novos demais para concluir alguma coisa"
            ));
        }
    }
    if coberto.is_empty() {
        report.ok("nenhum índice é prefixo de outro — não há duplicação óbvia");
    } else {
        let desperdicio: f64 = indices
            .iter()
            .filter(|indice| coberto.contains_key(&indice.nome))
            .map(|indice| indice.bytes)
            .sum();
        report.aviso(
            format!(
                "{} índice(s) redundantes, {} no total: as colunas de cada um já são o começo de outro índice",
                coberto.len(),
                bytes(desperdicio)
            ),
            "o índice maior atende às mesmas buscas. O menor só custa escrita e espaço",
        );
    }
    if depth >= Depth::Profundo {
        let escrita: f64 = indices
            .iter()
            .filter(|i| !i.primaria)
            .map(|i| i.bytes)
            .sum();
        report.field(
            "peso da escrita",
            format!("{} fora a chave primária", bytes(escrita)),
        );
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
    /// O resto do que o `pg_stat_statements` guarda, e que não cabe numa linha de tabela:
    /// o pior caso, o desvio, as páginas sujas, o WAL. É o que aparece quando o cursor
    /// para em cima da linha.
    min: f64,
    max: f64,
    stddev: f64,
    sujas: f64,
    escritas: f64,
    wal: f64,
    id: String,
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

impl Stmt {
    /// Tudo que se sabe desta forma de query, para quando o cursor parar em cima dela.
    fn fatos(&self, share: f64) -> Vec<(&'static str, String)> {
        let por_execucao = |valor: f64| match self.calls {
            0.0 => String::new(),
            calls => count(valor / calls),
        };
        vec![
            ("identificador", self.id.clone()),
            ("execuções", count(self.calls)),
            (
                "tempo somado",
                format!("{} ({share:.1}% do banco)", millis(self.total)),
            ),
            ("média por execução", millis(self.mean)),
            (
                "melhor e pior caso",
                match self.max {
                    0.0 => String::new(),
                    _ => format!("{} … {}", millis(self.min), millis(self.max)),
                },
            ),
            (
                "desvio",
                match self.stddev {
                    0.0 => String::new(),
                    desvio => format!(
                        "{} — {}",
                        millis(desvio),
                        match desvio > self.mean {
                            true =>
                                "o tempo varia mais que a própria média: o plano muda conforme o parâmetro",
                            false => "tempo estável entre execuções",
                        }
                    ),
                },
            ),
            ("linhas devolvidas", count(self.rows)),
            ("linhas por execução", por_execucao(self.rows)),
            (
                "páginas do cache",
                format!(
                    "{} acertos, {} do disco ({:.1}% em cache)",
                    count(self.hit),
                    count(self.miss),
                    100.0 * self.hit / (self.hit + self.miss).max(1.0)
                ),
            ),
            ("lido do disco", bytes(self.miss * 8192.0)),
            (
                "páginas sujadas",
                match self.sujas {
                    0.0 => String::new(),
                    sujas => format!(
                        "{} ({} escritas pela própria query)",
                        count(sujas),
                        count(self.escritas)
                    ),
                },
            ),
            (
                "derramado em disco",
                match self.temp {
                    0.0 => String::new(),
                    temp => bytes(temp * 8192.0),
                },
            ),
            (
                "WAL gerado",
                match self.wal {
                    0.0 => String::new(),
                    wal => bytes(wal),
                },
            ),
            ("tabelas que lê", tables_in(&self.query).join(", ")),
            ("colunas filtradas", hints(&self.query).join(", ")),
        ]
    }

    /// O texto da query, quebrado para caber na tela sem perder nada.
    fn texto(&self) -> Vec<String> {
        quebrar(&self.query, 150)
    }
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
    // Os nomes das colunas de pior caso e desvio mudaram no 13 junto com os de tempo
    // total, e o WAL por statement só existe de lá para cá.
    let (min_col, max_col, stddev_col) = match version >= 130_000 {
        true => ("min_exec_time", "max_exec_time", "stddev_exec_time"),
        false => ("min_time", "max_time", "stddev_time"),
    };
    let wal_col = match version >= 130_000 {
        true => "wal_bytes::float8",
        false => "0",
    };
    let sql = format!(
        "SELECT COALESCE(queryid::text,'') AS id, calls, rows, \
         {total_column} AS total, {mean_column} AS media, \
         {min_col} AS minimo, {max_col} AS maximo, {stddev_col} AS desvio, \
         shared_blks_hit AS hit, shared_blks_read AS miss, temp_blks_written AS temp, \
         shared_blks_dirtied AS sujas, shared_blks_written AS escritas, {wal_col} AS wal, \
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
            min: row.num("minimo"),
            max: row.num("maximo"),
            stddev: row.num("desvio"),
            sujas: row.num("sujas"),
            escritas: row.num("escritas"),
            wal: row.num("wal"),
            id: row.get("id").to_string(),
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

    report.table(&[
        "% do tempo",
        "somado",
        "execuções",
        "média",
        "pior",
        "query",
    ]);
    for stmt in statements.iter() {
        let share = if grand_total > 0.0 {
            100.0 * stmt.total / grand_total
        } else {
            0.0
        };
        report.cells_deep(
            vec![
                format!("{share:.1}%"),
                millis(stmt.total),
                count(stmt.calls),
                millis(stmt.mean),
                millis(stmt.max),
                one_line(&stmt.query, 160),
            ],
            match stmt.mean {
                lento if lento >= 1000.0 => Tone::Ruim,
                lento if lento >= SLOW_MS => Tone::Aviso,
                _ => Tone::Normal,
            },
            stmt.fatos(share),
            stmt.texto(),
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
        report.cells_deep(
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
            vec![
                ("tabela", row.get("tabela").to_string()),
                ("varreduras completas", count(scans)),
                ("linhas lidas varrendo", count(read)),
                ("linhas por varredura", count(per_scan)),
                ("leituras por índice", count(row.num("idx_scan"))),
                (
                    "proporção",
                    match scans + row.num("idx_scan") {
                        0.0 => String::new(),
                        total => format!(
                            "{:.0}% das leituras são varredura completa",
                            100.0 * scans / total
                        ),
                    },
                ),
                ("linhas vivas", count(live)),
                ("tamanho", bytes(row.num("bytes"))),
                (
                    "veredito",
                    match per_scan < 5000.0 {
                        true => "tabela pequena: ler inteiro é o plano certo, e um índice aqui seria mais lento".to_string(),
                        false => "cada varredura percorre muita linha — é onde um índice mudaria o jogo".to_string(),
                    },
                ),
            ],
            Vec::new(),
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
               WHERE i.indisvalid AND n.nspname NOT IN ('pg_catalog','information_schema','pg_toast')";
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

fn vacuum(conn: &mut pg::Conn, report: &mut Report, reset: &Reset) -> Result<(), String> {
    report.section("Vacuum e estatísticas");
    // Linha morta é contada, não medida: um reinício sujo zera o contador sem limpar uma
    // linha sequer. Dizer «nenhuma linha morta» com o contador de dois minutos atrás seria
    // dar um atestado de saúde a partir de um caderno em branco.
    if !reset.trustworthy() {
        report.line(format!(
            "contadores {} — linhas mortas de antes disso não aparecem aqui",
            reset.describe()
        ));
    }
    let sql = "SELECT t.schemaname || '.' || t.relname AS tabela, \
               GREATEST(t.n_live_tup, c.reltuples::bigint - t.n_dead_tup, 0) AS n_live_tup, \
               t.n_dead_tup, \
               CASE WHEN GREATEST(t.n_live_tup, c.reltuples::bigint - t.n_dead_tup, 0) > 0 \
                    THEN 100.0 * t.n_dead_tup \
                         / GREATEST(t.n_live_tup, c.reltuples::bigint - t.n_dead_tup, 0) \
                    ELSE -1 END AS pct, \
               EXTRACT(epoch FROM now() - GREATEST(t.last_vacuum, t.last_autovacuum)) AS desde_vacuum, \
               EXTRACT(epoch FROM now() - GREATEST(t.last_analyze, t.last_autoanalyze)) AS desde_analyze, \
               (t.last_analyze IS NULL AND t.last_autoanalyze IS NULL) AS nunca_analisada, \
               pg_total_relation_size(t.relid) AS bytes \
               FROM pg_stat_user_tables t JOIN pg_class c ON c.oid = t.relid \
               ORDER BY t.n_dead_tup DESC LIMIT 20";
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
        report.cells_deep(
            vec![
                row.get("tabela").to_string(),
                count(dead),
                // Sem linhas vivas contadas não existe proporção, e inventar uma seria
                // dizer 0% justamente na tabela em que ninguém sabe.
                match pct {
                    desconhecida if desconhecida < 0.0 => "?".to_string(),
                    conhecida => format!("{conhecida:.0}%"),
                },
                // Coluna vazia é vacuum que nunca aconteceu, e «agora» seria a leitura
                // exatamente oposta da verdade.
                match row.get("desde_vacuum") {
                    "" => "nunca".to_string(),
                    _ => duration(row.num("desde_vacuum")),
                },
            ],
            match pct {
                muito if muito >= 50.0 => Tone::Ruim,
                algum if algum >= 20.0 => Tone::Aviso,
                _ => Tone::Normal,
            },
            vec![
                ("tabela", row.get("tabela").to_string()),
                ("linhas mortas", count(dead)),
                ("linhas vivas", count(row.num("n_live_tup"))),
                (
                    "proporção",
                    match pct < 0.0 {
                        true => "não dá para saber: contadores zerados e nenhum ANALYZE".to_string(),
                        false => format!("{pct:.0}% do tamanho vivo"),
                    },
                ),
                ("tamanho total", bytes(row.num("bytes"))),
                (
                    "último vacuum",
                    match row.get("desde_vacuum") {
                        "" => "nunca".to_string(),
                        _ => format!("há {}", duration(row.num("desde_vacuum"))),
                    },
                ),
                (
                    "último analyze",
                    match row.get("desde_analyze") {
                        "" => "nunca — o planejador não sabe nada desta tabela".to_string(),
                        _ => format!("há {}", duration(row.num("desde_analyze"))),
                    },
                ),
                (
                    "o que isso custa",
                    "toda varredura da tabela percorre as linhas mortas também, e cada índice guarda uma entrada apontando para elas".to_string(),
                ),
            ],
            Vec::new(),
        );
        // Dead rows are not just space: every scan of the table walks over them, and the
        // index entries pointing at them are walked too.
        if !(0.0..50.0).contains(&pct) && dead > 50_000.0 {
            flagged = true;
            report.grave(
                match pct < 0.0 {
                    true => format!(
                        "{} tem {} linhas mortas e nenhuma linha viva contada — a tabela está acumulando lixo sem ninguém medindo",
                        row.get("tabela"),
                        count(dead)
                    ),
                    false => format!(
                        "{} tem {} linhas mortas, {pct:.0}% do tamanho vivo — a tabela e seus índices estão inchados",
                        row.get("tabela"),
                        count(dead)
                    ),
                },
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
        report.cells_deep(
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
            vec![
                ("base", row.get("datname").to_string()),
                ("transações desde o congelamento", count(age)),
                ("quanto falta para o limite", count(2_000_000_000.0 - age)),
                ("percentual do limite", format!("{pct:.2}%")),
                (
                    "limite do autovacuum",
                    format!("{} (autovacuum_freeze_max_age)", count(limit)),
                ),
                (
                    "o que acontece no limite",
                    "aos 2 bilhões o servidor para de aceitar escrita em qualquer base até alguém congelar na mão, com o banco fora do ar".to_string(),
                ),
            ],
            Vec::new(),
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

/// Cada tabela da base, com tudo o que se sabe dela.
///
/// É a seção mais navegável da investigação: a linha traz o tamanho e o movimento, e o
/// detalhe traz a anatomia — colunas com tipo, quantos valores distintos cada uma tem,
/// quanto dela é nulo, quanto ocupa, mais o histórico de vacuum e a proporção entre ler
/// por índice e ler tudo. É o que um DBA pede quando alguém diz «esta tabela está
/// estranha».
fn tables(conn: &mut pg::Conn, report: &mut Report) -> Result<(), String> {
    report.section("Tabelas");
    let sql = "SELECT n.nspname || '.' || c.relname AS tabela, \
               pg_total_relation_size(c.oid) AS total, \
               pg_relation_size(c.oid) AS dados, \
               pg_indexes_size(c.oid) AS indices, \
               COALESCE(pg_total_relation_size(c.reltoastrelid), 0) AS toast, \
               c.reltuples AS linhas, c.relpages AS paginas, \
               COALESCE(t.n_live_tup, 0) AS vivas, COALESCE(t.n_dead_tup, 0) AS mortas, \
               COALESCE(t.seq_scan, 0) AS varreduras, COALESCE(t.seq_tup_read, 0) AS lidas_varrendo, \
               COALESCE(t.idx_scan, 0) AS por_indice, COALESCE(t.idx_tup_fetch, 0) AS lidas_indice, \
               COALESCE(t.n_tup_ins, 0) AS inseridas, COALESCE(t.n_tup_upd, 0) AS atualizadas, \
               COALESCE(t.n_tup_del, 0) AS apagadas, COALESCE(t.n_tup_hot_upd, 0) AS hot, \
               EXTRACT(epoch FROM now() - GREATEST(t.last_vacuum, t.last_autovacuum)) AS desde_vacuum, \
               EXTRACT(epoch FROM now() - GREATEST(t.last_analyze, t.last_autoanalyze)) AS desde_analyze, \
               (SELECT count(*) FROM pg_index i WHERE i.indrelid = c.oid) AS qtd_indices, \
               (SELECT count(*) FROM pg_attribute a \
                 WHERE a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped) AS qtd_colunas, \
               (SELECT count(*) FROM pg_trigger g WHERE g.tgrelid = c.oid AND NOT g.tgisinternal) AS gatilhos, \
               c.relpersistence AS persistencia, \
               COALESCE(c.reloptions::text, '') AS opcoes \
               FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
               LEFT JOIN pg_stat_user_tables t ON t.relid = c.oid \
               WHERE c.relkind IN ('r','p','m') \
               AND n.nspname NOT IN ('pg_catalog','information_schema','pg_toast') \
               ORDER BY pg_total_relation_size(c.oid) DESC";
    let Some(table) = ask(conn, report, "tamanho das tabelas", sql)? else {
        return Ok(());
    };
    if table.is_empty() {
        report.line("nenhuma tabela fora dos esquemas do sistema");
        return Ok(());
    }
    let colunas = columns_by_table(conn, report)?;

    let total: f64 = table.iter().map(|row| row.num("total")).sum();
    report.field(
        "ao todo",
        format!("{} tabelas ocupando {}", table.len(), bytes(total)),
    );
    report.table(&["total", "tabela", "linhas", "dados", "índices", "movimento"]);
    for row in table.iter() {
        let dados = row.num("dados");
        let indices = row.num("indices");
        let vivas = row.num("vivas").max(row.num("linhas"));
        let varreduras = row.num("varreduras");
        let por_indice = row.num("por_indice");
        let leituras = varreduras + por_indice;
        let escritas = row.num("inseridas") + row.num("atualizadas") + row.num("apagadas");
        report.cells_deep(
            vec![
                bytes(row.num("total")),
                row.get("tabela").to_string(),
                count(vivas),
                bytes(dados),
                format!("{} ({})", bytes(indices), row.get("qtd_indices")),
                format!(
                    "{} leituras · {} escritas",
                    count(leituras),
                    count(escritas)
                ),
            ],
            match (
                indices > dados && dados > 50.0 * 1024.0 * 1024.0,
                row.num("mortas") > vivas * 0.2,
            ) {
                (true, _) | (_, true) => Tone::Aviso,
                _ => Tone::Normal,
            },
            vec![
                ("tamanho total", bytes(row.num("total"))),
                ("dados", bytes(dados)),
                (
                    "índices",
                    format!("{} em {} índices", bytes(indices), row.get("qtd_indices")),
                ),
                (
                    "toast (valores grandes)",
                    match row.num("toast") {
                        0.0 => String::new(),
                        toast => bytes(toast),
                    },
                ),
                ("colunas", row.get("qtd_colunas").to_string()),
                (
                    "gatilhos",
                    match row.get("gatilhos") {
                        "0" => String::new(),
                        n => n.to_string(),
                    },
                ),
                (
                    "linhas",
                    format!(
                        "{} vivas, {} mortas",
                        count(vivas),
                        count(row.num("mortas"))
                    ),
                ),
                (
                    "como é lida",
                    match leituras {
                        0.0 => "ninguém leu desde o último reset dos contadores".to_string(),
                        _ => format!(
                            "{:.0}% por índice, {:.0}% varrendo tudo",
                            100.0 * por_indice / leituras,
                            100.0 * varreduras / leituras
                        ),
                    },
                ),
                (
                    "linhas por varredura",
                    match varreduras {
                        0.0 => String::new(),
                        v => count(row.num("lidas_varrendo") / v),
                    },
                ),
                (
                    "escritas",
                    format!(
                        "{} inseridas · {} atualizadas · {} apagadas",
                        count(row.num("inseridas")),
                        count(row.num("atualizadas")),
                        count(row.num("apagadas"))
                    ),
                ),
                (
                    "atualizações HOT",
                    match row.num("atualizadas") {
                        0.0 => String::new(),
                        upd => format!(
                            "{:.0}% (as que não tocam índice)",
                            100.0 * row.num("hot") / upd
                        ),
                    },
                ),
                (
                    "último vacuum",
                    match row.get("desde_vacuum") {
                        "" => "nunca".to_string(),
                        _ => format!("há {}", duration(row.num("desde_vacuum"))),
                    },
                ),
                (
                    "último analyze",
                    match row.get("desde_analyze") {
                        "" => "nunca — o planejador não sabe nada desta tabela".to_string(),
                        _ => format!("há {}", duration(row.num("desde_analyze"))),
                    },
                ),
                (
                    "persistência",
                    match row.get("persistencia") {
                        "u" => "UNLOGGED — não sobrevive a uma queda".to_string(),
                        "t" => "temporária".to_string(),
                        _ => String::new(),
                    },
                ),
                (
                    "opções",
                    row.get("opcoes")
                        .trim_matches(|c| c == '{' || c == '}')
                        .to_string(),
                ),
            ],
            colunas.get(row.get("tabela")).cloned().unwrap_or_default(),
        );
    }

    for row in table.iter() {
        let dados = row.num("dados");
        let indices = row.num("indices");
        if indices > dados && dados > 50.0 * 1024.0 * 1024.0 {
            report.aviso(
                format!(
                    "{} tem mais índice ({}) do que dado ({})",
                    row.get("tabela"),
                    bytes(indices),
                    bytes(dados)
                ),
                "quase sempre é índice a mais, não dado a menos: confira a coluna de usos no cartão de Índices",
            );
        }
        if row.get("persistencia") == "u" {
            report.aviso(
                format!("{} é UNLOGGED", row.get("tabela")),
                "não vai para o WAL, não é replicada, e é esvaziada depois de uma queda. Ótimo para cache, péssimo para dado que importa",
            );
        }
    }
    Ok(())
}

/// As colunas de cada tabela, já formatadas para o detalhe: tipo, quantos valores
/// distintos, quanto é nulo, quanto ocupa.
///
/// Uma consulta só para a base inteira, agrupada aqui — é o `pg_stats`, que é o que o
/// planejador usa para decidir plano, e ler isso é ver o banco com os olhos dele.
fn columns_by_table(
    conn: &mut pg::Conn,
    report: &mut Report,
) -> Result<HashMap<String, Vec<String>>, String> {
    let sql = "SELECT n.nspname || '.' || c.relname AS tabela, a.attname AS coluna, \
               format_type(a.atttypid, a.atttypmod) AS tipo, \
               a.attnotnull AS obrigatoria, \
               COALESCE(s.n_distinct, 0) AS distintos, \
               COALESCE(s.null_frac, 0) AS nulos, \
               COALESCE(s.avg_width, 0) AS largura \
               FROM pg_attribute a \
               JOIN pg_class c ON c.oid = a.attrelid \
               JOIN pg_namespace n ON n.oid = c.relnamespace \
               LEFT JOIN pg_stats s ON s.schemaname = n.nspname \
                 AND s.tablename = c.relname AND s.attname = a.attname \
               WHERE a.attnum > 0 AND NOT a.attisdropped AND c.relkind IN ('r','p','m') \
               AND n.nspname NOT IN ('pg_catalog','information_schema','pg_toast') \
               ORDER BY c.relname, a.attnum";
    let mut por_tabela: HashMap<String, Vec<String>> = HashMap::new();
    let Some(table) = ask(conn, report, "colunas", sql)? else {
        return Ok(por_tabela);
    };
    for row in table.iter() {
        let distintos = row.num("distintos");
        let linhas = por_tabela.entry(row.get("tabela").to_string()).or_default();
        if linhas.is_empty() {
            linhas.push(format!(
                "{:<28} {:<22} {:>12} {:>8} {:>8}",
                "coluna", "tipo", "distintos", "nulos", "bytes"
            ));
        }
        linhas.push(format!(
            "{:<28} {:<22} {:>12} {:>8} {:>8}{}",
            one_line(row.get("coluna"), 28),
            one_line(row.get("tipo"), 22),
            // Negativo no pg_stats é proporção: -1 quer dizer «tudo distinto», que é o
            // que uma chave natural parece.
            match distintos {
                d if d < 0.0 => format!("{:.0}% da tabela", -100.0 * d),
                d => count(d),
            },
            format!("{:.0}%", 100.0 * row.num("nulos")),
            count(row.num("largura")),
            match row.flag("obrigatoria") {
                true => "  NOT NULL",
                false => "",
            }
        ));
    }
    Ok(por_tabela)
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
               WHERE c.contype = 'f' AND n.nspname NOT IN ('pg_catalog','information_schema','pg_toast') \
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
        report.cells_deep(
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
            vec![
                ("restrição", row.get("chave").to_string()),
                ("tabela que referencia", row.get("tabela").to_string()),
                ("colunas", row.get("colunas").to_string()),
                ("tabela referenciada", row.get("referencia").to_string()),
                ("tamanho da tabela filha", bytes(row.num("bytes"))),
                (
                    "o que custa",
                    "sem índice, todo DELETE ou UPDATE da chave na tabela pai varre a filha inteira — e faz isso segurando lock".to_string(),
                ),
            ],
            vec![format!(
                "CREATE INDEX CONCURRENTLY ON {} ({});",
                row.get("tabela"),
                row.get("colunas")
            )],
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
               AND c.relkind = 'r' AND n.nspname NOT IN ('pg_catalog','information_schema','pg_toast') \
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
               WHERE c.relkind = 'r' AND n.nspname NOT IN ('pg_catalog','information_schema','pg_toast') \
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
        report.cells_deep(
            vec![row.get("tabela").to_string(), count(row.num("linhas"))],
            Tone::Aviso,
            vec![
                ("tabela", row.get("tabela").to_string()),
                ("linhas estimadas", count(row.num("linhas"))),
                (
                    "o que falta",
                    "nenhum índice desta tabela é PRIMARY KEY".to_string(),
                ),
                (
                    "o que isso impede",
                    "apagar uma linha duplicada com segurança, e replicação lógica — que simplesmente não replica tabela sem identidade".to_string(),
                ),
            ],
            Vec::new(),
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
impl Recente {
    /// O que se sabe desta forma de query no intervalo entre duas investigações.
    fn fatos(&self) -> Vec<(&'static str, String)> {
        vec![
            ("execuções no intervalo", count(self.calls)),
            ("tempo somado", millis(self.total)),
            ("média por execução", millis(self.mean)),
            (
                "derramado em disco",
                match self.temp {
                    0.0 => String::new(),
                    temp => bytes(temp * 8192.0),
                },
            ),
            (
                "visto pela última vez",
                format!("há {}", duration(self.visto.elapsed().as_secs_f64())),
            ),
            ("tabelas que lê", tables_in(&self.query).join(", ")),
            ("colunas filtradas", hints(&self.query).join(", ")),
        ]
    }
}

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
         shared_blks_read AS miss, temp_blks_written AS temp, \
         shared_blks_dirtied AS sujas, shared_blks_written AS escritas, \
         0 AS minimo, 0 AS maximo, 0 AS desvio, 0 AS wal, query \
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
                    min: 0.0,
                    max: 0.0,
                    stddev: 0.0,
                    sujas: 0.0,
                    escritas: 0.0,
                    wal: 0.0,
                    id: row.get("id").to_string(),
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
            min: now.min,
            max: now.max,
            stddev: now.stddev,
            sujas: (now.sujas - was.map(|stmt| stmt.sujas).unwrap_or(0.0)).max(0.0),
            escritas: (now.escritas - was.map(|stmt| stmt.escritas).unwrap_or(0.0)).max(0.0),
            wal: (now.wal - was.map(|stmt| stmt.wal).unwrap_or(0.0)).max(0.0),
            id: id.clone(),
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

/// Quem está segurando o horizonte do vacuum.
///
/// É a pergunta mais valiosa que se faz a um Postgres doente, e a que quase ninguém sabe
/// fazer. O vacuum só pode remover uma linha morta mais velha que a transação mais antiga
/// ainda viva em **qualquer lugar** do cluster — e «qualquer lugar» inclui quatro coisas
/// que nada têm a ver umas com as outras: uma transação aberta, um slot de replicação
/// parado, uma réplica com `hot_standby_feedback` atrasada, e uma transação preparada que
/// alguém esqueceu num commit de duas fases.
///
/// Enquanto um desses não sair da frente, a tabela incha, os índices incham, e o
/// congelamento não avança — e tratar o sintoma (rodar VACUUM de novo) não muda nada. Esta
/// é a seção que diz o nome do culpado.
fn horizon(conn: &mut pg::Conn, report: &mut Report) -> Result<(), String> {
    report.section("Horizonte do vacuum");
    let sql = "SELECT 'sessão' AS tipo, pid::text AS quem, \
               COALESCE(age(backend_xmin), 0) AS idade, \
               COALESCE(EXTRACT(epoch FROM now() - xact_start), 0) AS segundos, \
               COALESCE(state, '') || ' · ' || left(COALESCE(query, ''), 90) AS detalhe \
               FROM pg_stat_activity WHERE backend_xmin IS NOT NULL AND pid <> pg_backend_pid() \
               UNION ALL \
               SELECT 'slot de replicação', slot_name, COALESCE(age(xmin), 0), 0, \
               CASE WHEN active THEN 'ativo' ELSE 'INATIVO' END \
               FROM pg_replication_slots WHERE xmin IS NOT NULL \
               UNION ALL \
               SELECT 'slot (catálogo)', slot_name, COALESCE(age(catalog_xmin), 0), 0, \
               CASE WHEN active THEN 'ativo' ELSE 'INATIVO' END \
               FROM pg_replication_slots WHERE catalog_xmin IS NOT NULL \
               UNION ALL \
               SELECT 'transação preparada', gid, COALESCE(age(transaction), 0), \
               COALESCE(EXTRACT(epoch FROM now() - prepared), 0), owner \
               FROM pg_prepared_xacts \
               ORDER BY 3 DESC LIMIT 10";
    let Some(table) = ask(conn, report, "horizonte do vacuum", sql)? else {
        return Ok(());
    };
    if table.is_empty() {
        report.ok("ninguém segurando o horizonte: o vacuum pode limpar tudo que está morto");
        return Ok(());
    }
    report.table(&["quem", "tipo", "transações atrás", "há", "detalhe"]);
    for row in table.iter() {
        let idade = row.num("idade");
        report.cells_deep(
            vec![
                row.get("quem").to_string(),
                row.get("tipo").to_string(),
                count(idade),
                match row.num("segundos") {
                    0.0 => String::new(),
                    segundos => duration(segundos),
                },
                one_line(row.get("detalhe"), 90),
            ],
            match idade {
                muito if muito > 50_000_000.0 => Tone::Ruim,
                algum if algum > 5_000_000.0 => Tone::Aviso,
                _ => Tone::Normal,
            },
            vec![
                ("quem segura", row.get("quem").to_string()),
                ("tipo", row.get("tipo").to_string()),
                ("transações atrás", count(idade)),
                (
                    "há quanto tempo",
                    match row.num("segundos") {
                        0.0 => String::new(),
                        segundos => duration(segundos),
                    },
                ),
                (
                    "como se resolve",
                    match row.get("tipo") {
                        "transação preparada" =>
                            "COMMIT PREPARED ou ROLLBACK PREPARED, por quem cuida do cluster. Nada no banco vai encerrá-la sozinho".to_string(),
                        t if t.starts_with("slot") =>
                            "a réplica volta a consumir o slot, ou o slot é removido. Enquanto isso ele segura WAL e horizonte".to_string(),
                        _ => "a transação termina — sozinha, ou porque alguém encerrou a sessão".to_string(),
                    },
                ),
            ],
            quebrar(row.get("detalhe"), 140),
        );
    }
    // Uma transação preparada esquecida é sempre grave, por mais nova que seja e esteja
    // ela em que posição estiver na lista: ela não vai embora sozinha, ninguém a está
    // vigiando, e o horizonte dela nunca avança.
    for row in table
        .iter()
        .filter(|row| row.get("tipo") == "transação preparada")
    {
        report.grave(
            format!(
                "a transação preparada «{}» está aberta há {} e segurando o horizonte",
                row.get("quem"),
                duration(row.num("segundos"))
            ),
            "transação de duas fases órfã: nada no banco vai encerrá-la, e o vacuum não passa dela. Quem cuida do cluster decide entre COMMIT PREPARED e ROLLBACK PREPARED — e enquanto isso o congelamento também não avança",
        );
    }
    let Some(primeiro) = table.first() else {
        return Ok(());
    };
    let idade = primeiro.num("idade");
    let quem = format!("{} {}", primeiro.get("tipo"), primeiro.get("quem"));
    if idade > 50_000_000.0 {
        report.grave(
            format!("{quem} segura o horizonte há {} transações", count(idade)),
            "nenhuma linha morta mais nova que isso pode ser removida em base nenhuma do cluster. É a causa por trás de inchaço que não some e de congelamento que não avança",
        );
    } else if idade > 5_000_000.0 {
        report.aviso(
            format!("{quem} segura o horizonte há {} transações", count(idade)),
            "ainda não é urgente, mas é o mesmo mecanismo: se não sair da frente, vira inchaço",
        );
    } else if !table
        .iter()
        .any(|row| row.get("tipo") == "transação preparada")
    {
        report.ok(format!(
            "o horizonte mais antigo está {} transações atrás — normal",
            count(idade)
        ));
    }
    Ok(())
}

/// Escrita: quanto WAL este banco gera, e quem está escrevendo as páginas sujas.
///
/// A pergunta por trás é sempre a mesma — por que o disco está ocupado — e ela tem três
/// respostas possíveis que se parecem de fora: checkpoint demais, WAL demais, ou o
/// processo que atende a consulta tendo que escrever página por conta própria porque
/// ninguém escreveu por ele.
fn write_activity(conn: &mut pg::Conn, report: &mut Report, version: i64) -> Result<(), String> {
    report.section("Escrita e WAL");

    if version >= 140_000 {
        let sql = "SELECT wal_records, wal_fpi, wal_bytes::float8 AS wal_bytes, \
                   wal_buffers_full, wal_write, wal_sync, \
                   EXTRACT(epoch FROM now() - stats_reset) AS desde FROM pg_stat_wal";
        if let Some(table) = ask(conn, report, "pg_stat_wal", sql)?
            && let Some(row) = table.first()
        {
            let desde = row.num("desde").max(1.0);
            report.field(
                "WAL gerado",
                format!(
                    "{} em {} ({} por hora)",
                    bytes(row.num("wal_bytes")),
                    duration(desde),
                    bytes(row.num("wal_bytes") / desde * 3600.0)
                ),
            );
            report.field(
                "registros",
                format!(
                    "{} ({} páginas inteiras)",
                    count(row.num("wal_records")),
                    count(row.num("wal_fpi"))
                ),
            );
            // Uma página inteira vai para o WAL na primeira vez que é tocada depois de um
            // checkpoint. Muitas delas é checkpoint frequente demais, não escrita demais.
            let fpi = row.num("wal_fpi");
            let registros = row.num("wal_records").max(1.0);
            if fpi / registros > 0.1 && registros > 100_000.0 {
                report.aviso(
                    format!(
                        "{:.0}% dos registros de WAL são páginas inteiras",
                        100.0 * fpi / registros
                    ),
                    "página inteira é escrita na primeira vez que ela muda depois de um checkpoint: tanta assim quase sempre é checkpoint frequente demais. max_wal_size maior espaça os checkpoints e encolhe o WAL",
                );
            }
            if row.num("wal_buffers_full") > 0.0 {
                report.aviso(
                    format!(
                        "o buffer de WAL encheu {} vez(es), obrigando quem escrevia a esperar",
                        count(row.num("wal_buffers_full"))
                    ),
                    "wal_buffers maior (16MB costuma bastar) tira essa espera do caminho de quem faz COMMIT",
                );
            }
        }
    }

    // O `pg_stat_bgwriter` foi partido em dois no 17: o que sobrou lá é o escritor de
    // fundo, e os checkpoints mudaram de casa.
    let sql = if version >= 170_000 {
        "SELECT buffers_clean, maxwritten_clean, buffers_alloc FROM pg_stat_bgwriter"
    } else {
        "SELECT buffers_clean, maxwritten_clean, buffers_alloc, buffers_backend, \
         buffers_backend_fsync, buffers_checkpoint FROM pg_stat_bgwriter"
    };
    if let Some(table) = ask(conn, report, "pg_stat_bgwriter", sql)?
        && let Some(row) = table.first()
    {
        report.field(
            "páginas alocadas",
            format!(
                "{} · escritas pelo bgwriter {}",
                count(row.num("buffers_alloc")),
                count(row.num("buffers_clean"))
            ),
        );
        if version < 170_000 {
            let backend = row.num("buffers_backend");
            let checkpoint = row.num("buffers_checkpoint").max(1.0);
            report.field("escritas pelo backend", count(backend));
            // Quando quem escreve a página suja é o processo que está atendendo a query,
            // a latência daquela query inclui um write. É o bgwriter não dando conta.
            if backend > checkpoint {
                report.aviso(
                    "mais páginas escritas pelos processos de consulta do que pelos checkpoints",
                    "o bgwriter não está dando conta: bgwriter_lru_maxpages e bgwriter_delay decidem o ritmo dele. Enquanto isso, cada escrita dessas entra no tempo de resposta de alguém",
                );
            }
            if row.num("buffers_backend_fsync") > 0.0 {
                report.grave(
                    format!(
                        "{} fsync(s) feitos pelo processo de consulta — a fila do checkpointer transbordou",
                        count(row.num("buffers_backend_fsync"))
                    ),
                    "é o sintoma clássico de disco saturado somado a checkpoint mal espaçado",
                );
            }
        }
        if row.num("maxwritten_clean") > 0.0 {
            report.aviso(
                format!(
                    "o bgwriter parou por bater o limite de páginas {} vez(es)",
                    count(row.num("maxwritten_clean"))
                ),
                "bgwriter_lru_maxpages está pequeno para o ritmo de escrita deste banco",
            );
        }
    }
    Ok(())
}

/// O que o próprio servidor está fazendo agora sem ninguém ter pedido: vacuum, analyze,
/// criação de índice. Aparece no cartão «Agora», junto do que os clientes estão fazendo.
fn progress(conn: &mut pg::Conn, report: &mut Report, version: i64) -> Result<(), String> {
    let sql = "SELECT p.pid, a.relid::regclass::text AS tabela, p.phase, \
               p.heap_blks_scanned::float8 AS feito, p.heap_blks_total::float8 AS total \
               FROM pg_stat_progress_vacuum p \
               JOIN pg_stat_progress_vacuum a ON a.pid = p.pid";
    if let Some(table) = ask(conn, report, "vacuum em andamento", sql)? {
        for row in table.iter() {
            let total = row.num("total").max(1.0);
            report.line(format!(
                "vacuum rodando em {} — {:.0}% ({}), fase «{}»",
                row.get("tabela"),
                100.0 * row.num("feito") / total,
                row.get("pid"),
                row.get("phase")
            ));
        }
    }
    if version >= 120_000 {
        let sql = "SELECT pid, relid::regclass::text AS tabela, phase, \
                   blocks_done::float8 AS feito, blocks_total::float8 AS total \
                   FROM pg_stat_progress_create_index";
        if let Some(table) = ask(conn, report, "índices em construção", sql)? {
            for row in table.iter() {
                report.line(format!(
                    "criando índice em {} — fase «{}», {:.0}%",
                    row.get("tabela"),
                    row.get("phase"),
                    100.0 * row.num("feito") / row.num("total").max(1.0)
                ));
            }
        }
    }
    Ok(())
}

/// Contas por usuário e por aplicação, e o que está preso numa transação de duas fases.
fn who(conn: &mut pg::Conn, report: &mut Report) -> Result<(), String> {
    let sql = "SELECT COALESCE(usename,'(interno)') AS usuario, \
               COALESCE(NULLIF(application_name,''),'(sem nome)') AS app, \
               count(*) AS n, \
               count(*) FILTER (WHERE state = 'active') AS ativas, \
               MAX(EXTRACT(epoch FROM now() - backend_start)) AS mais_velha \
               FROM pg_stat_activity GROUP BY 1, 2 ORDER BY n DESC LIMIT 12";
    report.section("Quem está conectado");
    let Some(table) = ask(conn, report, "conexões por origem", sql)? else {
        return Ok(());
    };
    report.table(&["usuário e aplicação", "conexões", "ativas", "mais antiga"]);
    for row in table.iter() {
        report.cells_deep(
            vec![
                format!("{}@{}", row.get("usuario"), row.get("app")),
                row.get("n").to_string(),
                row.get("ativas").to_string(),
                duration(row.num("mais_velha")),
            ],
            Tone::Normal,
            vec![
                ("usuário", row.get("usuario").to_string()),
                ("aplicação", row.get("app").to_string()),
                ("conexões", row.get("n").to_string()),
                ("delas ativas agora", row.get("ativas").to_string()),
                ("a mais antiga", format!("aberta há {}", duration(row.num("mais_velha")))),
                (
                    "o que isso diz",
                    "conexão velha e ociosa é pool funcionando; conexão velha e sempre ativa é trabalho que não termina".to_string(),
                ),
            ],
            Vec::new(),
        );
        report.destacar();
    }
    // Uma aplicação que abre conexão sem se identificar é a que ninguém acha quando ela é
    // a que está segurando o banco.
    let anonimas: f64 = table
        .iter()
        .filter(|row| row.get("app") == "(sem nome)" && row.get("usuario") != "(interno)")
        .map(|row| row.num("n"))
        .sum();
    if anonimas > 4.0 {
        report.aviso(
            format!("{} conexões sem application_name", count(anonimas)),
            "com `application_name` na string de conexão de cada serviço, esta lista passa a dizer quem está fazendo o quê — é uma linha de configuração e resolve metade das investigações futuras",
        );
    }
    Ok(())
}

/// Estatísticas velhas: o planejador escolhe plano com o que o ANALYZE deixou, e o que o
/// ANALYZE deixou pode ser de antes da tabela dobrar de tamanho.
fn stale_stats(conn: &mut pg::Conn, report: &mut Report) -> Result<(), String> {
    let sql = "SELECT schemaname || '.' || relname AS tabela, n_live_tup, n_mod_since_analyze, \
               CASE WHEN n_live_tup > 0 THEN 100.0 * n_mod_since_analyze / n_live_tup ELSE 0 END AS pct, \
               EXTRACT(epoch FROM now() - GREATEST(last_analyze, last_autoanalyze)) AS desde, \
               CASE WHEN n_tup_upd > 0 THEN 100.0 * n_tup_hot_upd / n_tup_upd ELSE 100 END AS hot, \
               n_tup_upd \
               FROM pg_stat_user_tables \
               WHERE n_live_tup > 10000 ORDER BY pct DESC LIMIT 15";
    let Some(table) = ask(conn, report, "idade das estatísticas", sql)? else {
        return Ok(());
    };
    for row in table.iter() {
        let pct = row.num("pct");
        if pct >= 20.0 {
            report.aviso(
                format!(
                    "{} mudou {:.0}% das linhas desde o último ANALYZE ({} de {})",
                    row.get("tabela"),
                    pct,
                    count(row.num("n_mod_since_analyze")),
                    count(row.num("n_live_tup"))
                ),
                "o planejador está escolhendo plano com um retrato velho da tabela. Se o autovacuum não alcança, autovacuum_analyze_scale_factor só desta tabela resolve (ALTER TABLE ... SET)",
            );
        }
        // Um UPDATE HOT não toca os índices. Quando quase nenhum é HOT numa tabela muito
        // atualizada, ou falta espaço na página (fillfactor) ou se está atualizando
        // justamente uma coluna indexada.
        let hot = row.num("hot");
        if row.num("n_tup_upd") > 100_000.0 && hot < 50.0 {
            report.aviso(
                format!(
                    "só {hot:.0}% dos UPDATEs de {} são HOT ({} atualizações)",
                    row.get("tabela"),
                    count(row.num("n_tup_upd"))
                ),
                "todo UPDATE não-HOT reescreve também as entradas de índice da linha. Ou a página não tem folga (fillfactor 90 ou 80 nessa tabela), ou existe índice numa coluna que muda o tempo todo",
            );
        }
    }
    Ok(())
}

/// Quanto de cada tabela está sendo lido do disco, e quanto do cache.
fn table_cache(conn: &mut pg::Conn, report: &mut Report) -> Result<(), String> {
    let sql = "SELECT schemaname || '.' || relname AS tabela, \
               heap_blks_read::float8 AS lidas, heap_blks_hit::float8 AS cache, \
               COALESCE(idx_blks_read,0)::float8 AS idx_lidas, \
               COALESCE(idx_blks_hit,0)::float8 AS idx_cache \
               FROM pg_statio_user_tables \
               WHERE heap_blks_read + heap_blks_hit > 10000 \
               ORDER BY heap_blks_read DESC LIMIT 10";
    report.section("Cache por tabela");
    let Some(table) = ask(conn, report, "pg_statio_user_tables", sql)? else {
        return Ok(());
    };
    if table.is_empty() {
        report.ok("nenhuma tabela com leitura suficiente para comparar");
        return Ok(());
    }
    report.line("quanto de cada tabela sai do cache, e quanto vem do disco:");
    report.table(&["tabela", "em cache", "lido do disco", "índices em cache"]);
    for row in table.iter() {
        let lidas = row.num("lidas");
        let total = (lidas + row.num("cache")).max(1.0);
        let acerto = 100.0 * row.num("cache") / total;
        report.cells_deep(
            vec![
                row.get("tabela").to_string(),
                format!("{acerto:.1}%"),
                bytes(lidas * 8192.0),
                format!(
                    "{:.1}%",
                    100.0 * row.num("idx_cache")
                        / (row.num("idx_cache") + row.num("idx_lidas")).max(1.0)
                ),
            ],
            match acerto {
                frio if frio < 90.0 => Tone::Aviso,
                _ => Tone::Normal,
            },
            vec![
                ("tabela", row.get("tabela").to_string()),
                ("páginas servidas pelo cache", count(row.num("cache"))),
                ("páginas lidas do disco", count(lidas)),
                ("lido do disco", bytes(lidas * 8192.0)),
                ("acerto de cache", format!("{acerto:.2}%")),
                (
                    "índices",
                    format!(
                        "{} do cache, {} do disco",
                        count(row.num("idx_cache")),
                        count(row.num("idx_lidas"))
                    ),
                ),
                (
                    "o que puxa para baixo",
                    "ou o conjunto quente não cabe em shared_buffers, ou alguma varredura repetida passa o rodo no cache a cada volta".to_string(),
                ),
            ],
            Vec::new(),
        );
        report.destacar();
    }
    Ok(())
}

/// Quem lê esta base, e com que poder.
///
/// Não é uma auditoria de segurança — é a parte dela que se responde com `SELECT` e que um
/// DBA olha de qualquer jeito ao receber um servidor que não conhece: quantos superusuários
/// existem, se alguém entra sem senha, e se a conexão pode ser em claro.
fn security(conn: &mut pg::Conn, report: &mut Report) -> Result<(), String> {
    report.section("Segurança");
    let sql = "SELECT rolname, rolsuper, rolcreaterole, rolcreatedb, rolreplication, rolbypassrls, \
               (rolpassword IS NULL) AS sem_senha, rolvaliduntil::text AS ate \
               FROM pg_authid WHERE rolcanlogin ORDER BY rolsuper DESC, rolname LIMIT 40";
    let Some(table) = ask(conn, report, "papéis (pg_authid pede superusuário)", sql)? else {
        // Sem poder ler pg_authid, o que dá para saber ainda vale ser dito.
        let sql = "SELECT rolname, rolsuper, rolcreaterole, rolcreatedb, rolreplication, \
                   false AS rolbypassrls, false AS sem_senha, rolvaliduntil::text AS ate \
                   FROM pg_roles WHERE rolcanlogin ORDER BY rolsuper DESC, rolname LIMIT 40";
        if let Some(table) = ask(conn, report, "papéis", sql)? {
            report.field("papéis que entram", count(table.len() as f64));
            let supers: Vec<String> = table
                .iter()
                .filter(|row| row.flag("rolsuper"))
                .map(|row| row.get("rolname").to_string())
                .collect();
            report.field("superusuários", supers.join(", "));
        }
        return Ok(());
    };
    report.table(&["papel", "poderes", "senha", "validade"]);
    let mut supers = 0;
    let mut sem_senha: Vec<String> = Vec::new();
    for row in table.iter() {
        let mut poderes: Vec<&str> = Vec::new();
        if row.flag("rolsuper") {
            poderes.push("SUPERUSER");
            supers += 1;
        }
        if row.flag("rolcreaterole") {
            poderes.push("CREATEROLE");
        }
        if row.flag("rolcreatedb") {
            poderes.push("CREATEDB");
        }
        if row.flag("rolreplication") {
            poderes.push("REPLICATION");
        }
        if row.flag("rolbypassrls") {
            poderes.push("BYPASSRLS");
        }
        if row.flag("sem_senha") {
            sem_senha.push(row.get("rolname").to_string());
        }
        report.cells_deep(
            vec![
                row.get("rolname").to_string(),
                poderes.join(" "),
                match row.flag("sem_senha") {
                    true => "sem senha".to_string(),
                    false => "definida".to_string(),
                },
                row.get("ate").to_string(),
            ],
            match (row.flag("rolsuper"), row.flag("sem_senha")) {
                (_, true) => Tone::Ruim,
                (true, _) => Tone::Aviso,
                _ => Tone::Normal,
            },
            vec![
                ("papel", row.get("rolname").to_string()),
                (
                    "superusuário",
                    match row.flag("rolsuper") {
                        true => "sim — ignora toda permissão e todo RLS".to_string(),
                        false => "não".to_string(),
                    },
                ),
                (
                    "pode criar papéis",
                    (if row.flag("rolcreaterole") {
                        "sim"
                    } else {
                        "não"
                    })
                    .to_string(),
                ),
                (
                    "pode criar bases",
                    (if row.flag("rolcreatedb") {
                        "sim"
                    } else {
                        "não"
                    })
                    .to_string(),
                ),
                (
                    "pode replicar",
                    (if row.flag("rolreplication") {
                        "sim"
                    } else {
                        "não"
                    })
                    .to_string(),
                ),
                (
                    "ignora RLS",
                    (if row.flag("rolbypassrls") {
                        "sim"
                    } else {
                        "não"
                    })
                    .to_string(),
                ),
                (
                    "senha",
                    match row.flag("sem_senha") {
                        true => "nenhuma — entra por confiança do pg_hba".to_string(),
                        false => "definida".to_string(),
                    },
                ),
                ("validade", row.get("ate").to_string()),
            ],
            Vec::new(),
        );
    }
    if !sem_senha.is_empty() {
        report.grave(
            format!(
                "{} papel(éis) entram sem senha: {}",
                sem_senha.len(),
                sem_senha.join(", ")
            ),
            "quem alcançar a porta entra como eles. Se for confiança por pg_hba (peer, trust), confira se vale também para quem vem de fora da máquina",
        );
    }
    if supers > 3 {
        report.aviso(
            format!("{supers} superusuários nesta instância"),
            "superusuário ignora toda permissão e todo RLS. A aplicação raramente precisa de um",
        );
    }
    Ok(())
}

/// Uma estimativa de quanto de cada tabela é espaço desperdiçado.
///
/// **Estimativa, e dita como tal.** O número exato custa ler a tabela inteira
/// (`pgstattuple`), e ler a tabela inteira é exatamente o que esta ferramenta não faz num
/// servidor de produção. A conta aqui compara o tamanho real com o que as colunas
/// deveriam ocupar segundo o último ANALYZE: erra para os lados, acerta a ordem de
/// grandeza, e é o suficiente para separar «a tabela é grande» de «a tabela está inchada».
fn bloat(conn: &mut pg::Conn, report: &mut Report) -> Result<(), String> {
    let sql = "SELECT n.nspname || '.' || c.relname AS tabela, \
               pg_relation_size(c.oid)::float8 AS real_bytes, \
               c.reltuples::float8 AS linhas, \
               COALESCE(t.n_dead_tup, 0)::float8 AS mortas, \
               COALESCE(t.n_live_tup, 0)::float8 AS vivas, \
               (SELECT sum(s.avg_width + 1)::float8 FROM pg_stats s \
                 WHERE s.schemaname = n.nspname AND s.tablename = c.relname) AS largura \
               FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
               LEFT JOIN pg_stat_user_tables t ON t.relid = c.oid \
               WHERE c.relkind = 'r' AND n.nspname NOT IN ('pg_catalog','information_schema','pg_toast') \
               AND c.reltuples > 1000 AND pg_relation_size(c.oid) > 8388608 \
               ORDER BY pg_relation_size(c.oid) DESC LIMIT 25";
    let Some(table) = ask(conn, report, "estimativa de inchaço", sql)? else {
        return Ok(());
    };
    let mut inchadas: Vec<(String, f64, f64, f64)> = Vec::new();
    for row in table.iter() {
        let largura = row.num("largura");
        let linhas = row.num("linhas");
        let real = row.num("real_bytes");
        if real <= 0.0 {
            continue;
        }
        let excesso = if largura > 0.0 && linhas > 0.0 {
            // 24 bytes de cabeçalho por linha, 4 do ponteiro no início da página, e 8 KB
            // por página menos os 24 do cabeçalho dela.
            let por_pagina = ((8192.0 - 24.0) / (largura + 28.0)).floor().max(1.0);
            real - (linhas / por_pagina).ceil() * 8192.0
        } else {
            // Sem ANALYZE não há largura de coluna, e é justamente na tabela que ninguém
            // analisa que o inchaço mora. O que sobra são as linhas mortas, que dizem
            // quanto do arquivo é lixo com uma conta bem mais simples — e que também são
            // a resposta quando a tabela foi analisada há pouco e inchou depois.
            let mortas = row.num("mortas");
            // `n_live_tup` zera num reinício sujo; o `reltuples` do catálogo é velho mas
            // sobrevive, e velho é melhor que zero — com zero, toda tabela pareceria 100%
            // desperdício.
            let vivas = row.num("vivas").max(row.num("linhas") - mortas).max(0.0);
            match vivas + mortas > 0.0 {
                true => real * mortas / (vivas + mortas),
                false => 0.0,
            }
        };
        if excesso <= 0.0 {
            continue;
        }
        let pct = 100.0 * excesso / real;
        if pct >= 30.0 && excesso > 16.0 * 1024.0 * 1024.0 {
            inchadas.push((row.get("tabela").to_string(), real, excesso, pct));
        }
    }
    // Um «tudo certo» falso é pior que um «não sei»: quando a tabela nunca foi analisada
    // e os contadores estão zerados, o catálogo não tem como saber, e dizer que está tudo
    // bem seria inventar.
    // Sem ANALYZE não existe largura de coluna, e sem ela a única conta que sobra é a das
    // linhas mortas — que mede o lixo recente, não o que a tabela acumulou antes de
    // ninguém estar contando. Dizer «não inchada» aí é dar um atestado que o catálogo não
    // assinou.
    let cegas: Vec<String> = table
        .iter()
        .filter(|row| row.num("largura") <= 0.0)
        .map(|row| row.get("tabela").to_string())
        .collect();
    if inchadas.is_empty() {
        if cegas.is_empty() {
            report.ok("nenhuma tabela grande parece inchada muito além do esperado");
        } else {
            report.aviso(
                format!(
                    "não dá para estimar o inchaço de {}: sem ANALYZE e sem contadores",
                    cegas.join(", ")
                ),
                "a estimativa sai da largura das colunas (que vem do ANALYZE) ou das linhas mortas (que vêm dos contadores, e zeram num reinício sujo). Sem os dois, o tamanho do arquivo não diz nada sobre desperdício",
            );
        }
        return Ok(());
    }
    inchadas.sort_by(|a, b| b.2.total_cmp(&a.2));
    report.aviso(
        format!(
            "{} tabela(s) com espaço desperdiçado estimado em {}",
            inchadas.len(),
            bytes(inchadas.iter().map(|entrada| entrada.2).sum())
        ),
        "estimativa a partir do último ANALYZE, não medição. O número exato sai de pgstattuple (que lê a tabela inteira, então é para janela de manutenção); a correção é VACUUM FULL ou pg_repack, e os dois pedem janela",
    );
    for (tabela, real, excesso, pct) in inchadas.iter().take(10) {
        report.cells_deep(
            vec![
                tabela.clone(),
                bytes(*real),
                format!("~{} desperdiçados", bytes(*excesso)),
                format!("~{pct:.0}%"),
            ],
            Tone::Aviso,
            vec![
                ("tabela", tabela.clone()),
                ("tamanho no disco", bytes(*real)),
                ("desperdício estimado", bytes(*excesso)),
                ("proporção", format!("~{pct:.0}% do arquivo")),
                (
                    "como a conta é feita",
                    "compara o tamanho real com o que as colunas deveriam ocupar segundo o último ANALYZE, ou com a proporção de linhas mortas quando não há ANALYZE".to_string(),
                ),
                (
                    "o número exato",
                    "sai de pgstattuple, que lê a tabela inteira — por isso não é feito daqui".to_string(),
                ),
                (
                    "como se corrige",
                    "VACUUM FULL ou pg_repack, e os dois pedem janela de manutenção".to_string(),
                ),
            ],
            Vec::new(),
        );
    }
    Ok(())
}
