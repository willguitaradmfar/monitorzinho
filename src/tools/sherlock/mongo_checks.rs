//! What to ask a MongoDB server, and what the answers mean.
//!
//! The shape of the investigation is the same as the Postgres one — identity, settings,
//! connections, what is slow, what is read without an index, what is taking up space —
//! but almost every answer arrives somewhere else. MongoDB keeps its statistics in one
//! enormous `serverStatus` document, its slow queries in a log or a profiler collection
//! that may not be switched on, and its index usage in an aggregation stage that has to
//! be run per collection.
//!
//! Nothing here writes. In particular the profiler is *read* and never enabled: turning
//! it on is a persistent change to a production database, and a tool that quietly did
//! that would be exactly the tool nobody should run.

use std::collections::{HashMap, HashSet};

use super::bson::{Doc, Value};
use super::{
    Depth, Plan, Report, bytes, count, duration, millis, mongo, one_line, plural, quebrar,
};
use crate::painel::Tone;

/// One live inspection of one MongoDB cluster.
///
/// Same shape as the Postgres session, and for the same reason: what anybody wants to
/// know is not what the server has done since it booted, but what it did in the last few
/// seconds. That means remembering the counters, the log lines already seen, and the
/// operations already reported.
pub struct Session {
    conn: mongo::Conn,
    target: mongo::Target,
    depth: Depth,
    /// The per-collection numbers from the last complete pass.
    collections: Vec<Collection>,
    /// Log lines already reported, so the ring buffer being read again doesn't replay
    /// the same slow query on every pass.
    seen_log: HashSet<u64>,
    /// The newest profiler entry already reported.
    last_profile: f64,
    /// Operations already announced, by id.
    running: HashSet<String>,
    /// As operações lentas já vistas, da mais recente para a mais antiga. É o histórico
    /// que faz do cartão um monitor em vez de um retrato.
    recentes: Vec<Lenta>,
    /// `serverStatus` as of the previous pass.
    last_status: Doc,
    /// A máquina por baixo: núcleos e RAM, para comparar com o cache e as conexões. Lido
    /// uma vez — não é coisa que mude entre uma investigação e a próxima.
    host: Doc,
    /// O `top` da passada anterior: tempo e contagem por coleção.
    last_top: HashMap<String, (f64, f64)>,
    last_pass: std::time::Instant,
    /// Quantas investigações esta sessão já fez — ver a sessão do Postgres.
    passadas: u64,
    profiler: i64,
}

impl Session {
    pub fn open(plan: &Plan) -> Result<Self, String> {
        let target = mongo::Target::parse(&plan.url, &plan.database)?;
        let mut conn = mongo::Conn::open(&target)?;
        let profiler = conn
            .run(&target.database, Doc::new().with("profile", -1))
            .map(|doc| doc.int("was"))
            .unwrap_or(0);
        let mut session = Self {
            conn,
            target,
            depth: plan.depth,
            collections: Vec::new(),
            seen_log: HashSet::new(),
            last_profile: 0.0,
            running: HashSet::new(),
            recentes: Vec::new(),
            last_status: Doc::new(),
            host: Doc::new(),
            last_top: HashMap::new(),
            last_pass: std::time::Instant::now(),
            passadas: 0,
            profiler,
        };
        // Everything already in the log and in the profiler is history, not something
        // that just happened: remembered, never announced.
        session.prime_log();
        session.last_profile = session.latest_profile();
        Ok(session)
    }

    /// Uma investigação inteira. A ordem das chamadas é a ordem dos cartões — ver a
    /// sessão do Postgres para o porquê.
    pub fn pass(&mut self, report: &mut Report) -> Result<(), String> {
        let window = self.last_pass.elapsed().as_secs_f64().max(0.1);
        self.last_pass = std::time::Instant::now();
        self.passadas += 1;
        // Relido a cada investigação, e não uma vez na conexão: alguém pode ter ligado o
        // profiler no intervalo — inclusive por causa do que este painel mostrou.
        self.profiler = self
            .conn
            .run(&self.target.database, Doc::new().with("profile", -1))
            .map(|doc| doc.int("was"))
            .unwrap_or(0);

        let status = ask(
            &mut self.conn,
            report,
            "serverStatus",
            "admin",
            Doc::new().with("serverStatus", 1),
        )?
        .unwrap_or_default();

        if self.host.0.is_empty() {
            self.host = self
                .conn
                .run("admin", Doc::new().with("hostInfo", 1))
                .unwrap_or_default();
        }
        self.server(report, &status)?;
        self.now(report)?;
        self.movement(report, &status, window)?;
        connections(report, &status);
        cache(report, &status);
        engine(report, &status);
        operations(report, &status);
        health(report, &status, &self.host);
        self.last_status = status;
        if report.stopping() {
            return Ok(());
        }
        databases(&mut self.conn, report, self.depth)?;
        let target = clone_target(&self.target);
        self.collections = collections(&mut self.conn, report, &target, self.depth)?;
        if self.depth >= Depth::Medio {
            let collections = std::mem::take(&mut self.collections);
            indexes(&mut self.conn, report, &target, &collections)?;
            index_shape(report, &collections);
            self.collections = collections;
            slow(&mut self.conn, report, &target)?;
            replication(&mut self.conn, report)?;
        }
        if self.depth >= Depth::Profundo {
            let collections = std::mem::take(&mut self.collections);
            storage(&mut self.conn, report, &target, &collections)?;
            self.collections = collections;
            oplog(&mut self.conn, report)?;
            // `listShards` só existe no roteador. Perguntar a um mongod avulso devolve um
            // erro que não é notícia nenhuma.
            if self.conn.hello.text("msg") == "isdbgrid" {
                sharding(&mut self.conn, report)?;
            }
            configuration(&mut self.conn, report, &target)?;
        }
        Ok(())
    }

    /// Who is on the other end.
    fn server(&mut self, report: &mut Report, status: &Doc) -> Result<(), String> {
        report.section("Servidor");
        report.field(
            "endereço",
            match self.conn.hello.text("me").as_str() {
                "" => self.target.address(),
                host => host.to_string(),
            },
        );
        report.field("base", self.target.database.clone());
        report.field(
            "usuário",
            match self.target.user.as_str() {
                "" => "(sem autenticação)",
                user => user,
            },
        );
        report.field("transporte", self.conn.encryption.clone());
        if !self.conn.discovery.is_empty() {
            report.line(self.conn.discovery.clone());
        }
        report.line(
            "todos os comandos enviados são de leitura — nenhum índice, perfil ou documento é criado ou alterado",
        );
        let target = clone_target(&self.target);
        identity(&mut self.conn, report, &target, status)?;
        Ok(())
    }

    /// What is executing this instant.
    fn now(&mut self, report: &mut Report) -> Result<(), String> {
        report.section("Agora");
        let Ok(ops) = self.conn.run("admin", Doc::new().with("currentOp", 1)) else {
            report.line("sem currentOp: este usuário não pode ver as operações do servidor");
            return Ok(());
        };
        report.table(&["há", "operação", "coleção", "plano", "comando"]);
        let mut present = HashSet::new();
        let mut running = 0;
        for op in ops.list("inprog").iter().filter_map(Value::as_doc) {
            let seconds = op.num("secs_running");
            if op.text("op") == "none" || op.text("ns").is_empty() || administrativa(op) {
                continue;
            }
            let key = format!("{}/{}", op.text("opid"), op.text("ns"));
            present.insert(key.clone());
            running += 1;
            let plan = op.text("planSummary");
            let announced = self.running.contains(&key);
            if seconds >= 1.0 {
                self.running.insert(key);
            }
            if op.flag("waitingForLock") && !announced {
                report.alerta(
                    format!(
                        "{} há {} em {} esperando lock",
                        op.text("op"),
                        duration(seconds),
                        op.text("ns")
                    ),
                    "outra operação está segurando a coleção",
                );
                continue;
            }
            if seconds >= 5.0 && !announced {
                let fix = if plan.contains("COLLSCAN") {
                    suggestion_from_doc(op.doc("command.filter"), op.doc("command.sort"))
                        .unwrap_or_else(|| {
                            "varredura de coleção: o filtro desta operação não tem índice"
                                .to_string()
                        })
                } else {
                    String::new()
                };
                report.alerta(
                    format!(
                        "{} há {} em {} ({}): {}",
                        op.text("op"),
                        duration(seconds),
                        op.text("ns"),
                        if plan.is_empty() {
                            "sem plano".to_string()
                        } else {
                            plan.clone()
                        },
                        one_line(&op.text("command"), 100)
                    ),
                    fix,
                );
                continue;
            }
            report.cells_deep(
                vec![
                    duration(seconds),
                    op.text("op"),
                    op.text("ns"),
                    plan.clone(),
                    one_line(&op.text("command"), 160),
                ],
                if seconds >= 1.0 {
                    Tone::Aviso
                } else {
                    Tone::Normal
                },
                vec![
                    ("operação", op.text("op")),
                    ("coleção", op.text("ns")),
                    ("rodando há", duration(seconds)),
                    (
                        "plano",
                        match plan.is_empty() {
                            true => "não registrado".to_string(),
                            false => plan.clone(),
                        },
                    ),
                    ("cliente", op.text("client")),
                    ("descrição", op.text("desc")),
                    ("opid", op.text("opid")),
                    (
                        "esperando lock",
                        match op.flag("waitingForLock") {
                            true => "sim".to_string(),
                            false => String::new(),
                        },
                    ),
                    ("cedeu a vez", count(op.num("numYields"))),
                ],
                quebrar(&op.text("command"), 150),
            );
            report.destacar();
        }
        if running == 0 {
            report.ok("nenhuma operação em andamento neste instante");
        }
        self.running.retain(|key| present.contains(key));
        Ok(())
    }

    /// O que o cluster fez desde a investigação anterior.
    ///
    /// Três fontes, da melhor para a que sempre existe: as operações lentas que o próprio
    /// servidor registrou (log ou profiler, com o tempo de cada uma), os contadores de
    /// `serverStatus` diferenciados, e — quando o log não é legível por este usuário — o
    /// comando `top`, que diz quanto tempo cada coleção consumiu.
    fn movement(&mut self, report: &mut Report, status: &Doc, window: f64) -> Result<(), String> {
        report.section("Queries recentes");
        let novas = self.slow_since();
        for lenta in novas {
            // Pela forma da operação, e não pelo texto inteiro: a mesma query com «40 000
            // examinados → 101» e com «40 000 → 36» é a mesma query, e listar as duas é
            // gastar a tela repetindo o que já foi dito.
            match self
                .recentes
                .iter_mut()
                .find(|antiga| antiga.ns == lenta.ns && antiga.forma == lenta.forma)
            {
                Some(antiga) => *antiga = lenta,
                None => self.recentes.push(lenta),
            }
        }
        self.recentes
            .sort_by(|a, b| b.visto.cmp(&a.visto).then(b.millis.total_cmp(&a.millis)));
        self.recentes.truncate(RECENTES);

        if self.passadas <= 1 {
            report.line("primeira leitura: o que rodar daqui em diante aparece aqui a cada Ctrl+R");
        } else {
            let before = &self.last_status;
            let delta = |path: &str| (status.num(path) - before.num(path)).max(0.0);
            let reads = delta("opcounters.query") + delta("opcounters.getmore");
            let writes = delta("opcounters.insert")
                + delta("opcounters.update")
                + delta("opcounters.delete");
            let commands = delta("opcounters.command");
            let examined = delta("metrics.queryExecutor.scannedObjects");
            let returned = delta("metrics.document.returned");
            report.field(
                format!("nos últimos {window:.0}s"),
                format!(
                    "{} leituras, {} escritas, {} comandos",
                    count(reads),
                    count(writes),
                    count(commands)
                ),
            );
            if examined > 0.0 {
                report.field(
                    "documentos abertos",
                    format!("{} para devolver {}", count(examined), count(returned)),
                );
            }
            if returned > 50.0 && examined / returned > 20.0 {
                report.alerta(
                    format!(
                        "{} documentos abertos para devolver {} — {:.0} por resultado",
                        count(examined),
                        count(returned),
                        examined / returned
                    ),
                    "é varredura acontecendo. As linhas abaixo dizem em qual coleção",
                );
            }
            let sorts = delta("metrics.operation.scanAndOrder");
            if sorts > 0.0 {
                report.alerta(
                    format!(
                        "{} ordenação(ões) feita(s) em memória, sem índice",
                        count(sorts)
                    ),
                    "o índice precisa terminar pelas chaves do sort (igualdade, sort, intervalo)",
                );
            }
            let scans = delta("metrics.queryExecutor.collectionScans.total");
            if scans > 0.0 {
                report.alerta(
                    format!("{} varredura(s) de coleção inteira", count(scans)),
                    String::new(),
                );
            }
        }

        // A lista, que é o cartão: cada operação lenta com o tempo dela.
        if !self.recentes.is_empty() {
            report.table(&["duração", "coleção", "plano", "quando", "operação"]);
        }
        for (posicao, lenta) in self.recentes.iter().enumerate() {
            let quando = lenta.visto.elapsed().as_secs_f64();
            let celulas = vec![
                millis(lenta.millis),
                lenta.ns.clone(),
                lenta.plano.clone(),
                match quando {
                    recem if recem < 2.0 => "nesta leitura".to_string(),
                    antes => format!("{} atrás", duration(antes)),
                },
                one_line(&lenta.resumo, 160),
            ];
            let tom = match lenta.millis {
                lento if lento >= 1000.0 => Tone::Ruim,
                _ => Tone::Aviso,
            };
            report.cells_deep(
                celulas,
                tom,
                vec![
                    ("coleção", lenta.ns.clone()),
                    ("duração", millis(lenta.millis)),
                    (
                        "plano",
                        match lenta.plano.is_empty() {
                            true => "não registrado".to_string(),
                            false => lenta.plano.clone(),
                        },
                    ),
                    ("documentos examinados", lenta.examinados.clone()),
                    (
                        "quando",
                        format!("há {}", duration(lenta.visto.elapsed().as_secs_f64())),
                    ),
                    ("o que fazer", lenta.fix.clone()),
                ],
                quebrar(&lenta.forma, 140),
            );
            if posicao < 6 {
                report.destacar();
            }
        }
        // O que é notável entre elas vira achado, com a sugestão de índice junto.
        for lenta in self.recentes.iter().take(6) {
            if lenta.visto.elapsed().as_secs_f64() > 2.0 || lenta.fix.is_empty() {
                continue;
            }
            report.alerta(
                format!(
                    "{} em {} ({}): {}",
                    millis(lenta.millis),
                    lenta.ns,
                    match lenta.plano.is_empty() {
                        true => "sem plano".to_string(),
                        false => lenta.plano.clone(),
                    },
                    one_line(&lenta.resumo, 100)
                ),
                lenta.fix.clone(),
            );
        }
        if self.recentes.is_empty() {
            self.by_collection(report, window)?;
        }
        Ok(())
    }

    /// As operações lentas que apareceram desde a última olhada, das duas fontes que as
    /// registram.
    fn slow_since(&mut self) -> Vec<Lenta> {
        // Uma fonte ou a outra, nunca as duas: com o profiler ligado, a mesma operação
        // está nas duas e sairia duplicada na tela. O profiler é o melhor dos dois — vem
        // estruturado, com o plano e as contagens separadas — então, quando existe, é
        // dele que se lê.
        match self.profiler > 0 {
            true => self.profile_since(),
            false => self.log_since(),
        }
    }

    /// Sem log legível e sem profiler, o que sobra: quanto tempo cada coleção consumiu.
    ///
    /// `top` é o único lugar do MongoDB que mede tempo por coleção sem nada estar ligado.
    /// Não diz qual query foi — diz onde o tempo foi, que é o suficiente para saber onde
    /// olhar, e é muito mais do que um cartão dizendo «sem profiler».
    fn by_collection(&mut self, report: &mut Report, window: f64) -> Result<(), String> {
        let Ok(top) = self.conn.run("admin", Doc::new().with("top", 1)) else {
            report.line(
                "nenhuma operação lenta registrada, e este usuário não pode ler o log nem o `top` do servidor",
            );
            return Ok(());
        };
        let Some(totals) = top.doc("totals") else {
            return Ok(());
        };
        let mut linhas: Vec<(String, f64, f64)> = Vec::new();
        for (ns, valor) in &totals.0 {
            // `admin`, `config` e `local` são a papelada do servidor, e `$cmd` é o
            // lugar onde os comandos administrativos aparecem: nenhum dos três é
            // trabalho de quem usa o banco.
            if ns == "note"
                || ns.contains(".$cmd")
                || ["admin.", "config.", "local."]
                    .iter()
                    .any(|interno| ns.starts_with(interno))
            {
                continue;
            }
            let Some(doc) = valor.as_doc() else { continue };
            let antes = self.last_top.get(ns).copied().unwrap_or((0.0, 0.0));
            let agora = (doc.num("total.time"), doc.num("total.count"));
            self.last_top.insert(ns.clone(), agora);
            let tempo = (agora.0 - antes.0).max(0.0);
            let vezes = (agora.1 - antes.1).max(0.0);
            if vezes > 0.0 {
                linhas.push((ns.clone(), tempo, vezes));
            }
        }
        if self.passadas <= 1 {
            return Ok(());
        }
        linhas.sort_by(|a, b| b.1.total_cmp(&a.1));
        if linhas.is_empty() {
            report.ok(format!(
                "nenhuma coleção foi tocada nos últimos {window:.0}s"
            ));
            return Ok(());
        }
        report.line("sem operação lenta registrada — o tempo por coleção, do `top`:");
        report.table(&["tempo", "coleção", "operações", "média"]);
        for (ns, tempo, vezes) in linhas.iter().take(20) {
            // `top` mede em microssegundos.
            report.cells_deep(
                vec![
                    millis(tempo / 1000.0),
                    ns.clone(),
                    count(*vezes),
                    millis(tempo / 1000.0 / vezes.max(1.0)),
                ],
                Tone::Normal,
                vec![
                    ("coleção", ns.clone()),
                    ("tempo somado no intervalo", millis(tempo / 1000.0)),
                    ("operações", count(*vezes)),
                    ("média por operação", millis(tempo / 1000.0 / vezes.max(1.0))),
                    (
                        "de onde vem",
                        "o comando `top`, que mede tempo dentro de lock por coleção. Não diz qual query foi — diz onde o tempo ficou".to_string(),
                    ),
                ],
                Vec::new(),
            );
            report.destacar();
        }
        Ok(())
    }

    /// Remembers what the log already held, without reporting any of it.
    fn prime_log(&mut self) {
        let Ok(log) = self.conn.run("admin", Doc::new().with("getLog", "global")) else {
            return;
        };
        for value in log.list("log") {
            if let Some(text) = value.as_text() {
                self.seen_log.insert(fingerprint(text));
            }
        }
    }

    /// The slow queries the server itself logged since the last look.
    fn log_since(&mut self) -> Vec<Lenta> {
        let Ok(log) = self.conn.run("admin", Doc::new().with("getLog", "global")) else {
            return Vec::new();
        };
        let mut fresh: Vec<Lenta> = Vec::new();
        for value in log.list("log") {
            let Some(text) = value.as_text() else {
                continue;
            };
            if !self.seen_log.insert(fingerprint(text)) {
                continue;
            }
            if let Some(entry) = from_log(text) {
                fresh.push(entry);
            }
        }
        // The ring buffer holds a thousand lines and this set would otherwise grow with
        // every pass for as long as the screen stays open.
        if self.seen_log.len() > 20_000 {
            self.seen_log.clear();
            self.prime_log();
        }
        fresh
    }

    /// The newest profiler entry's timestamp, without reporting anything.
    fn latest_profile(&mut self) -> f64 {
        self.conn
            .cursor(
                &self.target.database,
                Doc::new()
                    .with("find", "system.profile")
                    .with("sort", Doc::new().with("ts", -1))
                    .with("limit", 1),
            )
            .ok()
            .and_then(|docs| docs.first().map(|doc| doc.num("ts")))
            .unwrap_or(0.0)
    }

    fn profile_since(&mut self) -> Vec<Lenta> {
        let found = self.conn.cursor(
            &self.target.database,
            Doc::new()
                .with("find", "system.profile")
                .with(
                    "filter",
                    Doc::new().with(
                        "ts",
                        Doc::new().with("$gt", Value::Time(self.last_profile as i64)),
                    ),
                )
                .with("sort", Doc::new().with("ts", -1))
                .with("limit", 10),
        );
        let Ok(entries) = found else {
            return Vec::new();
        };
        entries
            .iter()
            .take(10)
            .filter(|entry| {
                self.last_profile = self.last_profile.max(entry.num("ts"));
                !nossa(entry)
            })
            .map(from_profile)
            .collect()
    }
}

/// Uma operação que o servidor registrou como lenta, com o que ela custou.
///
/// Vem do log do próprio mongod ou da `system.profile`, que são as duas únicas fontes de
/// «esta operação demorou tanto» no MongoDB — não existe um `pg_stat_statements` aqui. As
/// duas dizem a mesma coisa em formatos diferentes, e isto é o formato comum.
struct Lenta {
    visto: std::time::Instant,
    millis: f64,
    ns: String,
    plano: String,
    /// O comando sem as contagens: é o que identifica duas execuções como a mesma query.
    forma: String,
    /// Quantos documentos foram abertos para responder — a conta que separa «devolveu
    /// pouco porque há pouco» de «leu tudo para devolver pouco».
    examinados: String,
    resumo: String,
    fix: String,
}

impl Lenta {
    /// A operação em uma linha, para o relatório histórico.
    fn linha(&self) -> String {
        format!(
            "{} em {}{}: {}",
            millis(self.millis),
            self.ns,
            match self.plano.is_empty() {
                true => String::new(),
                false => format!(" ({})", self.plano),
            },
            one_line(&self.resumo, 110)
        )
    }
}

/// Quantas operações lentas o histórico guarda.
const RECENTES: usize = 40;

/// A forma de um comando que veio do log, onde ele chega como JSON.
///
/// O mesmo que `Doc::render_forma` faz do lado do BSON, e pela mesma razão: o log do
/// MongoDB grava o filtro inteiro, valores e tudo, e o que responde «por que está lento» é
/// a forma da query. Ver o comentário lá.
fn forma_json(command: Option<&serde_json::Value>) -> String {
    const NOMES: &[&str] = &[
        "find",
        "aggregate",
        "count",
        "distinct",
        "update",
        "insert",
        "delete",
        "collection",
        "$db",
        "ns",
    ];
    const RUIDO: &[&str] = &[
        "lsid",
        "$clusterTime",
        "$readPreference",
        "$audit",
        "$client",
        "txnNumber",
        "autocommit",
        "maxTimeMS",
        "cursor",
        "apiVersion",
        "apiStrict",
        "signature",
        "readConcern",
        "writeConcern",
        "shardVersion",
        "databaseVersion",
        "clientOperationKey",
    ];

    fn forma(valor: &serde_json::Value) -> String {
        match valor {
            serde_json::Value::String(_) => "\"…\"".to_string(),
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Bool(b) => b.to_string(),
            serde_json::Value::Null => "null".to_string(),
            serde_json::Value::Array(itens) => {
                // Um pipeline vira os nomes dos estágios: é o que diz o que a agregação
                // faz, e é a parte dela que não é dado de ninguém.
                let estagios: Vec<String> = itens
                    .iter()
                    .filter_map(|estagio| estagio.as_object()?.keys().next().cloned())
                    .collect();
                if estagios.len() == itens.len() && !estagios.is_empty() {
                    return format!("[{}]", estagios.join(", "));
                }
                // Qualquer outra lista abre do mesmo jeito que do lado BSON: dentro de um
                // `$expr` é ela que carrega a pergunta.
                let dentro: Vec<String> = itens.iter().take(4).map(forma).collect();
                match itens.len() > 4 {
                    true => format!("[{}, …+{}]", dentro.join(", "), itens.len() - 4),
                    false => format!("[{}]", dentro.join(", ")),
                }
            }
            serde_json::Value::Object(campos) => {
                // `{"$date": "…"}` e companhia são como o log escreve um tipo do BSON:
                // dizer «data» é mais legível do que mostrar o embrulho.
                if let Some((chave, _)) = campos.iter().next()
                    && campos.len() == 1
                    && chave.starts_with('$')
                    && ![
                        "$gt", "$gte", "$lt", "$lte", "$ne", "$in", "$nin", "$regex", "$exists",
                    ]
                    .contains(&chave.as_str())
                {
                    return chave.trim_start_matches('$').to_string();
                }
                let dentro: Vec<String> = campos
                    .iter()
                    .filter(|(chave, _)| !RUIDO.contains(&chave.as_str()))
                    .take(6)
                    .map(|(chave, valor)| match NOMES.contains(&chave.as_str()) {
                        true => format!("{chave}: {}", valor.as_str().unwrap_or("…")),
                        false => format!("{chave}: {}", forma(valor)),
                    })
                    .collect();
                format!("{{{}}}", dentro.join(", "))
            }
        }
    }
    command.map(forma).unwrap_or_default()
}

/// Se esta operação é a papelada do cluster em vez de trabalho de alguém.
///
/// Um `currentOp` num MongoDB parado é uma lista de conexões ociosas: o monitor do
/// conjunto de réplicas esperando uma mudança de topologia, o coletor de sessões, e as
/// perguntas desta própria ferramenta. Todas aparecem como `command` rodando há sete
/// segundos em `admin.$cmd`, e nenhuma delas é uma resposta para «o que está rodando
/// agora».
fn administrativa(op: &Doc) -> bool {
    let ns = op.text("ns");
    ns.ends_with(".$cmd")
        || ["admin.", "config.", "local."]
            .iter()
            .any(|interno| ns.starts_with(interno))
        || op
            .doc("command")
            .is_some_and(|comando| comando.keys().any(|chave| HOUSEKEEPING.contains(&chave)))
}

/// A cheap identity for a log line, so the same line isn't reported twice as the ring
/// buffer is read again.
fn fingerprint(text: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// A second handle on the same target, so a `&mut self` method can hand the target to a
/// free function that also needs the connection.
fn clone_target(target: &mongo::Target) -> mongo::Target {
    mongo::Target {
        hosts: target.hosts.clone(),
        user: target.user.clone(),
        password: String::new(),
        database: target.database.clone(),
        auth_source: target.auth_source.clone(),
        tls: target.tls,
        srv: target.srv,
        timeout: target.timeout,
    }
}

/// Runs one command, treating a refusal as an absence rather than a failure./// Runs one command, treating a refusal as an absence rather than a failure.
///
/// Managed MongoDB takes most administrative commands away from the user, and a report
/// that stopped at the first `Unauthorized` would never get past the first section on
/// Atlas. What it can't ask, it says it can't ask.
fn ask(
    conn: &mut mongo::Conn,
    report: &mut Report,
    what: &str,
    database: &str,
    command: Doc,
) -> Result<Option<Doc>, String> {
    match conn.run(database, command) {
        Ok(doc) => Ok(Some(doc)),
        Err(error) if error.benign() => {
            report.line(format!("sem {what}: {error}"));
            Ok(None)
        }
        Err(error) => Err(format!("{what}: {error}")),
    }
}

/// The same, for a command that answers with a cursor.
fn ask_many(
    conn: &mut mongo::Conn,
    report: &mut Report,
    what: &str,
    database: &str,
    command: Doc,
) -> Result<Vec<Doc>, String> {
    match conn.cursor(database, command) {
        Ok(docs) => Ok(docs),
        Err(error) if error.benign() => {
            report.line(format!("sem {what}: {error}"));
            Ok(Vec::new())
        }
        Err(error) => Err(format!("{what}: {error}")),
    }
}

fn identity(
    conn: &mut mongo::Conn,
    report: &mut Report,
    target: &mongo::Target,
    status: &Doc,
) -> Result<(), String> {
    let build = ask(
        conn,
        report,
        "buildInfo",
        "admin",
        Doc::new().with("buildInfo", 1),
    )?
    .unwrap_or_default();
    let version = build.text("version");
    report.field("versão", &version);
    report.field("processo", status.text("process"));
    report.field("host", status.text("host"));
    report.field("motor de armazenamento", status.text("storageEngine.name"));
    report.field("no ar há", duration(status.num("uptime")));
    let hello = &conn.hello;
    if let Some(set) = hello.path("setName") {
        report.field(
            "conjunto de réplicas",
            format!(
                "{} — este nó é {}",
                set.render(),
                if hello.flag("isWritablePrimary") || hello.flag("ismaster") {
                    "primário"
                } else {
                    "secundário"
                }
            ),
        );
    }
    if conn.hello.text("msg") == "isdbgrid" {
        report.line(
            "este endereço é um mongos: o que segue é a visão do roteador, não a de um shard",
        );
    }

    // A major version out of support gets no security fixes, and every performance
    // question asked about it has an answer that begins with "upgrade".
    if let Some(major) = version
        .split('.')
        .next()
        .and_then(|n| n.parse::<i64>().ok())
        && major > 0
        && major < 5
    {
        report.aviso(
            format!("MongoDB {version} está fora do suporte do fabricante"),
            "sem correções de segurança, e várias das estatísticas usadas aqui só existem da 5.0 em diante",
        );
    }
    if build.flag("debug") {
        report.grave(
            "este servidor é uma compilação de depuração",
            "compilações debug rodam ordens de grandeza mais devagar. Não é para produção",
        );
    }
    if status.text("storageEngine.name") != "wiredTiger"
        && !status.text("storageEngine.name").is_empty()
    {
        report.aviso(
            format!("motor de armazenamento {}", status.text("storageEngine.name")),
            "o motor moderno é o wiredTiger — tudo neste relatório sobre cache e compressão assume ele",
        );
    }

    // Who we are decides which of the questions below can even be asked.
    if let Some(who) = ask(
        conn,
        report,
        "connectionStatus",
        "admin",
        Doc::new().with("connectionStatus", 1),
    )? {
        let users: Vec<String> = who
            .list("authInfo.authenticatedUsers")
            .iter()
            .filter_map(|value| value.as_doc())
            .map(|doc| format!("{}@{}", doc.text("user"), doc.text("db")))
            .collect();
        let roles: Vec<String> = who
            .list("authInfo.authenticatedUserRoles")
            .iter()
            .filter_map(|value| value.as_doc())
            .map(|doc| doc.text("role"))
            .collect();
        if users.is_empty() && target.user.is_empty() {
            report.grave(
                "o servidor aceitou uma conexão sem usuário nenhum: este MongoDB está sem autenticação",
                "qualquer um que alcance a porta lê e escreve tudo. É a configuração por trás da maior parte dos vazamentos de MongoDB",
            );
        } else {
            report.field("autenticado como", users.join(", "));
            report.field("papéis", unique(roles).join(", "));
        }
    }
    Ok(())
}

fn connections(report: &mut Report, status: &Doc) {
    report.section("Conexões");
    let current = status.num("connections.current");
    let available = status.num("connections.available");
    let created = status.num("connections.totalCreated");
    if current + available == 0.0 {
        return;
    }
    let used = 100.0 * current / (current + available);
    report.field(
        "abertas",
        format!(
            "{} de {} ({used:.0}%)",
            count(current),
            count(current + available)
        ),
    );
    report.field("criadas desde o início", count(created));
    if used > 80.0 {
        report.grave(
            format!("{used:.0}% do limite de conexões em uso"),
            "quando encher, novas conexões são recusadas. Quase sempre é pool da aplicação mal dimensionado, não falta de servidor",
        );
    }
    // Connections created far in excess of connections held means clients are opening
    // one per operation instead of keeping a pool.
    let uptime = status.num("uptime").max(1.0);
    let rate = created / uptime;
    if rate > 5.0 && created > 1000.0 {
        report.aviso(
            format!("{rate:.1} conexões novas por segundo desde que o servidor subiu"),
            "handshake e autenticação a cada operação custam caro: é sinal de cliente sem pool ou com pool morrendo a cada requisição",
        );
    }
    let queue_readers = status.num("globalLock.currentQueue.readers");
    let queue_writers = status.num("globalLock.currentQueue.writers");
    if queue_readers + queue_writers > 0.0 {
        report.field(
            "fila no lock global",
            format!("{queue_readers:.0} leituras, {queue_writers:.0} escritas"),
        );
    }
}

fn cache(report: &mut Report, status: &Doc) {
    let Some(wt) = status.doc("wiredTiger") else {
        return;
    };
    report.section("Cache");
    let cache = wt.doc("cache").cloned().unwrap_or_default();
    let used = cache.num("bytes currently in the cache");
    let max = cache.num("maximum bytes configured");
    let dirty = cache.num("tracked dirty bytes in the cache");
    if max > 0.0 {
        report.field(
            "cache do wiredTiger",
            format!(
                "{} de {} ({:.0}%)",
                bytes(used),
                bytes(max),
                100.0 * used / max
            ),
        );
        report.field("páginas sujas", bytes(dirty));
        if used / max > 0.95 {
            report.aviso(
                format!("cache em {:.0}% da capacidade", 100.0 * used / max),
                "acima de 95% o servidor passa a despejar páginas para conseguir ler as próximas. Ou o conjunto quente cresceu, ou alguma consulta varre coleção inteira e passa o rodo no cache",
            );
        }
    }
    // Eviction done by the threads serving queries, rather than by the background
    // eviction workers, is the cache saying it cannot keep up.
    let by_application = cache.num("pages evicted by application threads");
    if by_application > 0.0 {
        report.grave(
            format!(
                "{} páginas foram despejadas do cache pelas próprias threads de consulta",
                count(by_application)
            ),
            "isso é latência aparecendo direto na resposta ao cliente. Mais RAM para o cache, ou menos dados sendo tocados por consulta",
        );
    }

    // Tickets: how many operations may be inside the storage engine at once. Running out
    // is a queue in the one place a queue is invisible from the application.
    for kind in ["read", "write"] {
        let out = wt.num(&format!("concurrentTransactions.{kind}.out"));
        let available = wt.num(&format!("concurrentTransactions.{kind}.available"));
        if out + available == 0.0 {
            continue;
        }
        report.field(
            format!("tickets de {kind}"),
            format!("{out:.0} em uso, {available:.0} livres"),
        );
        if available == 0.0 {
            report.grave(
                format!("acabaram os tickets de {kind} do wiredTiger"),
                "toda operação nova entra numa fila invisível para a aplicação. É saturação de disco ou consultas lentas demais segurando o ticket",
            );
        }
    }
}

fn operations(report: &mut Report, status: &Doc) {
    report.section("Operações");
    let ops = status.doc("opcounters").cloned().unwrap_or_default();
    let uptime = status.num("uptime").max(1.0);
    let summary: Vec<String> = ["query", "insert", "update", "delete", "getmore", "command"]
        .iter()
        .map(|name| format!("{name} {}", count(ops.num(name))))
        .collect();
    report.field("desde o início", summary.join(", "));
    report.field(
        "por segundo",
        format!(
            "{:.1} leituras, {:.1} escritas",
            (ops.num("query") + ops.num("getmore")) / uptime,
            (ops.num("insert") + ops.num("update") + ops.num("delete")) / uptime
        ),
    );

    // The ratio that finds missing indexes without looking at a single query: how many
    // documents the server had to open to produce the documents it returned.
    let examined = status.num("metrics.queryExecutor.scannedObjects");
    let keys = status.num("metrics.queryExecutor.scanned");
    let returned = status.num("metrics.document.returned");
    if returned > 1000.0 {
        let ratio = examined / returned;
        report.field(
            "documentos examinados por devolvido",
            format!(
                "{ratio:.1}  ({} examinados, {} devolvidos)",
                count(examined),
                count(returned)
            ),
        );
        if ratio > 100.0 {
            report.grave(
                format!("o servidor abre {ratio:.0} documentos para cada um que devolve"),
                "isso é varredura de coleção em escala. As seções de índices e de queries lentas abaixo dizem onde",
            );
        } else if ratio > 10.0 {
            report.aviso(
                format!("{ratio:.1} documentos examinados para cada um devolvido"),
                "o ideal é perto de 1. Acima de 10 costuma haver filtro sem índice ou índice com seletividade ruim",
            );
        } else {
            report.ok(format!(
                "{ratio:.1} documentos examinados por devolvido — os índices estão sendo usados"
            ));
        }
        if keys > 0.0 {
            report.field("chaves de índice examinadas", count(keys));
        }
    }

    // A sort the server had to do in memory because no index provided the order.
    let sorts = status.num("metrics.operation.scanAndOrder");
    if sorts > 0.0 {
        report.aviso(
            format!("{} ordenações feitas em memória por falta de índice", count(sorts)),
            "um sort sem índice carrega tudo na memória e estoura em 32 MB. O índice tem que terminar pelas chaves do sort (regra E-S-R: igualdade, sort, intervalo)",
        );
    }
    let scans = status.num("metrics.queryExecutor.collectionScans.total");
    if scans > 0.0 {
        report.field("varreduras de coleção", count(scans));
    }
    let timed_out = status.num("metrics.cursor.timedOut");
    if timed_out > 100.0 {
        report.aviso(
            format!(
                "{} cursores expiraram sem serem lidos até o fim",
                count(timed_out)
            ),
            "a aplicação abre cursor e abandona. Cada um segura recursos no servidor até expirar",
        );
    }
    let asserts = status.num("asserts.warning") + status.num("asserts.msg");
    if asserts > 0.0 {
        report.aviso(
            format!(
                "{} avisos internos registrados pelo servidor",
                count(asserts)
            ),
            "o log do servidor diz quais. Costuma preceder problema maior",
        );
    }
}

fn databases(conn: &mut mongo::Conn, report: &mut Report, depth: Depth) -> Result<(), String> {
    report.section("Bases");
    let Some(list) = ask(
        conn,
        report,
        "listDatabases",
        "admin",
        Doc::new().with("listDatabases", 1),
    )?
    else {
        return Ok(());
    };
    let mut rows: Vec<(String, f64)> = list
        .list("databases")
        .iter()
        .filter_map(|value| value.as_doc())
        .map(|doc| (doc.text("name"), doc.num("sizeOnDisk")))
        .collect();
    rows.sort_by(|a, b| b.1.total_cmp(&a.1));
    report.table(&["em disco", "base", "coleções", "objetos", "índices"]);
    for (name, size) in rows.iter().take(if depth >= Depth::Medio { 20 } else { 8 }) {
        // Uma consulta por base, e só a partir do nível médio: num cluster com cinquenta
        // bases isso são cinquenta idas, e no nível raso o que se quer é o tamanho.
        let stats = match depth >= Depth::Medio {
            true => conn
                .run(
                    name,
                    Doc::new().with("dbStats", 1).with("maxTimeMS", 10_000),
                )
                .unwrap_or_default(),
            false => Doc::new(),
        };
        report.cells_deep(
            vec![
                bytes(*size),
                name.clone(),
                stats.text("collections"),
                count(stats.num("objects")),
                bytes(stats.num("indexSize")),
            ],
            Tone::Normal,
            vec![
                ("em disco", bytes(*size)),
                ("coleções", stats.text("collections")),
                ("visões", stats.text("views")),
                ("documentos", count(stats.num("objects"))),
                ("dados", bytes(stats.num("dataSize"))),
                ("documento médio", bytes(stats.num("avgObjSize"))),
                (
                    "índices",
                    format!(
                        "{} ocupando {}",
                        stats.text("indexes"),
                        bytes(stats.num("indexSize"))
                    ),
                ),
                (
                    "espaço livre dentro dos arquivos",
                    match stats.num("freeStorageSize") {
                        0.0 => String::new(),
                        livre => bytes(livre),
                    },
                ),
            ],
            Vec::new(),
        );
    }
    report.field("total em disco", bytes(list.num("totalSize")));
    Ok(())
}

/// One collection and the numbers that describe it, gathered once and reused by every
/// check below so the cluster is asked once per collection rather than once per check.
struct Collection {
    name: String,
    documents: f64,
    size: f64,
    storage: f64,
    reusable: f64,
    indexes: f64,
    index_bytes: f64,
    average: f64,
    /// O tamanho de cada índice, que é o que diz qual deles está custando o espaço.
    index_sizes: Vec<(String, f64)>,
    capped: bool,
    /// Regra de validação declarada na coleção — ou nada, que é o caso quase sempre.
    validador: bool,
    /// Quantas linhas cabem no cache: `$collStats` conta as páginas que o WiredTiger tem
    /// desta coleção em memória.
    em_cache: f64,
}

fn collections(
    conn: &mut mongo::Conn,
    report: &mut Report,
    target: &mongo::Target,
    depth: Depth,
) -> Result<Vec<Collection>, String> {
    report.section("Coleções");
    let listing = ask_many(
        conn,
        report,
        "listCollections",
        &target.database,
        Doc::new()
            .with("listCollections", 1)
            .with("nameOnly", true)
            .with("authorizedCollections", true)
            .with("cursor", Doc::new()),
    )?;
    let names: Vec<String> = listing
        .iter()
        .filter(|doc| doc.text("type") != "view")
        .map(|doc| doc.text("name"))
        .filter(|name| !name.starts_with("system."))
        .collect();
    // Quem declarou regra de validação de esquema. É a única coisa que o MongoDB tem de
    // parecido com um `NOT NULL`, e saber que não existe nenhuma é metade de um
    // diagnóstico de modelagem.
    let com_regra: std::collections::HashSet<String> = listing
        .iter()
        .filter(|doc| doc.doc("options.validator").is_some())
        .map(|doc| doc.text("name"))
        .collect();
    report.field(
        "na base",
        format!("{} coleção(ões) em {}", names.len(), target.database),
    );
    if names.is_empty() {
        return Ok(Vec::new());
    }

    // `$collStats` is one aggregation per collection and it reads metadata only — but a
    // cluster with a thousand collections is a thousand round trips, so the shallow
    // level doesn't pay for it.
    if depth < Depth::Medio {
        report.line("nível raso: tamanhos por coleção não são medidos (suba para médio)");
        return Ok(names
            .into_iter()
            .map(|name| Collection {
                name,
                documents: 0.0,
                size: 0.0,
                storage: 0.0,
                reusable: 0.0,
                indexes: 0.0,
                index_bytes: 0.0,
                average: 0.0,
                index_sizes: Vec::new(),
                capped: false,
                validador: false,
                em_cache: 0.0,
            })
            .collect());
    }

    let mut out = Vec::new();
    for name in names {
        if report.stopping() {
            break;
        }
        let name_para_regra = name.clone();
        let stats = conn.cursor(
            &target.database,
            Doc::new()
                .with("aggregate", name.clone())
                .with(
                    "pipeline",
                    vec![Value::Doc(Doc::new().with(
                        "$collStats",
                        Doc::new().with("storageStats", Doc::new()),
                    ))],
                )
                .with("cursor", Doc::new()),
        );
        let Ok(stats) = stats else { continue };
        let Some(storage) = stats.first().and_then(|doc| doc.doc("storageStats")) else {
            continue;
        };
        out.push(Collection {
            name,
            documents: storage.num("count"),
            size: storage.num("size"),
            storage: storage.num("storageSize"),
            reusable: storage.num("wiredTiger.block-manager.file bytes available for reuse"),
            indexes: storage.num("nindexes"),
            index_bytes: storage.num("totalIndexSize"),
            average: storage.num("avgObjSize"),
            index_sizes: storage
                .doc("indexSizes")
                .map(|doc| {
                    doc.0
                        .iter()
                        .map(|(nome, valor)| (nome.clone(), valor.as_num().unwrap_or(0.0)))
                        .collect()
                })
                .unwrap_or_default(),
            capped: storage.flag("capped"),
            validador: com_regra.contains(&name_para_regra),
            em_cache: storage.num("wiredTiger.cache.bytes currently in the cache"),
        });
    }
    out.sort_by(|a, b| (b.storage + b.index_bytes).total_cmp(&(a.storage + a.index_bytes)));
    report.table(&[
        "em disco",
        "coleção",
        "documentos",
        "dados",
        "índices",
        "tam. índices",
        "doc. médio",
    ]);
    for collection in out.iter() {
        let celulas = vec![
            bytes(collection.storage + collection.index_bytes),
            collection.name.clone(),
            count(collection.documents),
            bytes(collection.size),
            count(collection.indexes),
            bytes(collection.index_bytes),
            bytes(collection.average),
        ];
        report.cells_deep(
            celulas,
            Tone::Normal,
            vec![
                ("documentos", count(collection.documents)),
                ("dados (descomprimidos)", bytes(collection.size)),
                (
                    "em disco",
                    format!(
                        "{} ({})",
                        bytes(collection.storage),
                        match collection.size {
                            0.0 => "—".to_string(),
                            tamanho => format!(
                                "compressão de {:.1}×",
                                tamanho / collection.storage.max(1.0)
                            ),
                        }
                    ),
                ),
                ("documento médio", bytes(collection.average)),
                (
                    "índices",
                    format!("{} ocupando {}", collection.indexes, bytes(collection.index_bytes)),
                ),
                (
                    "espaço livre dentro do arquivo",
                    match collection.reusable {
                        0.0 => String::new(),
                        livre => format!(
                            "{} ({:.0}% do arquivo) — volta a ser usado pela coleção, mas não pelo disco",
                            bytes(livre),
                            100.0 * livre / collection.storage.max(1.0)
                        ),
                    },
                ),
                (
                    "no cache agora",
                    match collection.em_cache {
                        0.0 => String::new(),
                        cache => bytes(cache),
                    },
                ),
                (
                    "regra de validação",
                    match collection.validador {
                        true => "declarada".to_string(),
                        false => "nenhuma — qualquer documento entra".to_string(),
                    },
                ),
                (
                    "limitada (capped)",
                    match collection.capped {
                        true => "sim — escreve em círculo e descarta o mais antigo".to_string(),
                        false => String::new(),
                    },
                ),
            ],
            collection
                .index_sizes
                .iter()
                .map(|(nome, tamanho)| format!("{:<40} {}", nome, bytes(*tamanho)))
                .collect(),
        );
        report.destacar();
    }
    Ok(out)
}

/// Todo índice desta base numa tabela só: o que ele indexa, quantas vezes foi usado, e o
/// que isso quer dizer.
///
/// Uma linha por índice, e não uma lista de nunca usados ao lado de outra lista de
/// declarados: é a mesma pergunta («este índice vale o que custa?») e ela se responde
/// olhando as duas colunas juntas.
fn indexes(
    conn: &mut mongo::Conn,
    report: &mut Report,
    target: &mongo::Target,
    collections: &[Collection],
) -> Result<(), String> {
    report.section("Índices");
    report.table(&["coleção", "índice", "chaves", "usos", "situação"]);
    let mut sem_uso = 0;
    let mut sem_indice: Vec<&Collection> = Vec::new();
    let mut pesados: Vec<&Collection> = Vec::new();

    for collection in collections {
        if report.stopping() {
            break;
        }
        // Uma coleção com nada além do `_id` só pode ser buscada por `_id`. Todo outro
        // filtro lê ela inteira.
        if collection.indexes <= 1.0 && collection.documents > 10_000.0 {
            sem_indice.push(collection);
        }
        if collection.index_bytes > collection.size && collection.size > 8.0 * 1024.0 * 1024.0 {
            pesados.push(collection);
        }

        // Quantas vezes cada índice foi usado desde que o servidor subiu.
        let mut usos: HashMap<String, f64> = HashMap::new();
        if let Ok(stats) = conn.cursor(
            &target.database,
            Doc::new()
                .with("aggregate", collection.name.clone())
                .with(
                    "pipeline",
                    vec![Value::Doc(Doc::new().with("$indexStats", Doc::new()))],
                )
                .with("cursor", Doc::new()),
        ) {
            for index in stats {
                usos.insert(index.text("name"), index.num("accesses.ops"));
            }
        }

        let Ok(list) = conn.cursor(
            &target.database,
            Doc::new()
                .with("listIndexes", collection.name.clone())
                .with("cursor", Doc::new()),
        ) else {
            continue;
        };
        for index in list {
            let nome = index.text("name");
            let chaves: Vec<String> = index
                .doc("key")
                .map(|doc| {
                    doc.0
                        .iter()
                        .map(|(chave, valor)| format!("{chave}: {}", valor.render()))
                        .collect()
                })
                .unwrap_or_default();
            let ops = usos.get(&nome).copied().unwrap_or(-1.0);
            let mut situacao: Vec<String> = Vec::new();
            if index.flag("unique") {
                situacao.push("único".to_string());
            }
            if index.path("expireAfterSeconds").is_some() {
                situacao.push(format!(
                    "TTL, apaga depois de {}",
                    duration(index.num("expireAfterSeconds"))
                ));
            }
            if index.flag("sparse") {
                situacao.push("esparso".to_string());
            }
            if nome != "_id_" && ops == 0.0 {
                situacao.push("nunca usado".to_string());
                sem_uso += 1;
            }
            let tamanho = collection
                .index_sizes
                .iter()
                .find(|(chave, _)| *chave == nome)
                .map(|(_, tamanho)| *tamanho)
                .unwrap_or(0.0);
            report.cells_deep(
                vec![
                    collection.name.clone(),
                    nome.clone(),
                    format!("{{ {} }}", chaves.join(", ")),
                    match ops {
                        desconhecido if desconhecido < 0.0 => "—".to_string(),
                        usado => count(usado),
                    },
                    situacao.join(" · "),
                ],
                match ops == 0.0 {
                    true => Tone::Aviso,
                    false => Tone::Normal,
                },
                vec![
                    ("coleção", collection.name.clone()),
                    ("chaves", format!("{{ {} }}", chaves.join(", "))),
                    (
                        "tamanho",
                        match tamanho {
                            0.0 => String::new(),
                            t => format!(
                                "{} ({:.0}% dos índices da coleção)",
                                bytes(t),
                                100.0 * t / collection.index_bytes.max(1.0)
                            ),
                        },
                    ),
                    (
                        "usos desde que o servidor subiu",
                        match ops {
                            desconhecido if desconhecido < 0.0 => {
                                "não deu para ver ($indexStats recusado)".to_string()
                            }
                            usado => count(usado),
                        },
                    ),
                    (
                        "único",
                        (if index.flag("unique") { "sim" } else { "não" }).to_string(),
                    ),
                    (
                        "esparso",
                        (if index.flag("sparse") { "sim" } else { "" }).to_string(),
                    ),
                    (
                        "TTL",
                        match index.path("expireAfterSeconds") {
                            Some(_) => format!(
                                "apaga depois de {}",
                                duration(index.num("expireAfterSeconds"))
                            ),
                            None => String::new(),
                        },
                    ),
                    (
                        "parcial",
                        match index.doc("partialFilterExpression") {
                            Some(filtro) => filtro.render_forma(),
                            None => String::new(),
                        },
                    ),
                    ("versão", index.text("v")),
                ],
                Vec::new(),
            );
        }
    }

    if sem_uso == 0 {
        report.ok("todo índice desta base já foi usado desde que o servidor subiu");
    } else {
        report.aviso(
            format!("{sem_uso} índice(s) nunca usados desde que o servidor subiu"),
            "cada índice é escrito em todo insert e update da coleção. Confira se não servem a um relatório raro antes de largar — e lembre que o contador zera quando o mongod reinicia",
        );
    }
    for collection in sem_indice {
        report.aviso(
            format!(
                "{} tem {} documentos e nenhum índice além de _id",
                collection.name,
                count(collection.documents)
            ),
            "qualquer filtro diferente de _id lê a coleção inteira. Veja pelo que a aplicação busca aqui e indexe",
        );
    }
    for collection in pesados {
        report.aviso(
            format!(
                "{} tem mais índice ({}) do que dado ({})",
                collection.name,
                bytes(collection.index_bytes),
                bytes(collection.size)
            ),
            "quase sempre é índice sobrando: veja a coluna de usos acima",
        );
    }
    Ok(())
}

/// Sem repetir, preservando a ordem em que apareceram.
fn unique(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .filter(|value| seen.insert(value.clone()))
        .collect()
}

/// As operações lentas que o servidor já tinha registrado antes de a gente chegar.
///
/// Duas fontes para a mesma pergunta, e as duas entram na mesma tabela: a `system.profile`
/// (quando alguém já ligou o profiler) e o log do próprio servidor, que grava as lentas
/// por conta própria em qualquer instalação. Uma linha por **forma** de operação — o
/// profiler grava cada execução, e cinco linhas iguais não dizem mais do que uma.
fn slow(conn: &mut mongo::Conn, report: &mut Report, target: &mongo::Target) -> Result<(), String> {
    report.section("Queries lentas");
    // `profile: -1` pergunta em que nível o profiler está. Qualquer outro valor o
    // *mudaria*, e mudar coisa em banco de produção é o que esta ferramenta não faz.
    let level = ask(
        conn,
        report,
        "estado do profiler",
        &target.database,
        Doc::new().with("profile", -1),
    )?
    .unwrap_or_default();
    let threshold = level.num("slowms");
    report.field(
        "profiler",
        match level.int("was") {
            0 => format!("desligado (limite de query lenta: {threshold:.0} ms)"),
            1 => format!("gravando as acima de {threshold:.0} ms"),
            2 => "gravando TODAS as operações".to_string(),
            _ => "estado desconhecido".to_string(),
        },
    );
    if level.int("was") == 2 {
        report.aviso(
            "o profiler está no nível 2, gravando toda operação do banco",
            "em produção isso custa caro e enche a system.profile. O nível 1 grava só o que interessa",
        );
    }

    let mut achadas: Vec<Lenta> = Vec::new();
    let mut vistas: HashSet<String> = HashSet::new();
    if level.int("was") > 0 {
        let profiled = ask_many(
            conn,
            report,
            "system.profile",
            &target.database,
            Doc::new()
                .with("find", "system.profile")
                .with(
                    "filter",
                    Doc::new().with("millis", Doc::new().with("$gte", 20)),
                )
                .with("sort", Doc::new().with("ts", -1))
                .with("limit", 200),
        )?;
        for entry in profiled.iter().filter(|entry| !nossa(entry)) {
            let lenta = from_profile(entry);
            if vistas.insert(format!("{}{}", lenta.ns, lenta.forma)) {
                achadas.push(lenta);
            }
        }
    } else {
        report.line(
            "com o profiler desligado, o que sobra é o log do servidor — que já registra as lentas por conta própria",
        );
    }

    // O log só entra quando o profiler não está ligado: com os dois, a mesma operação
    // aparece duas vezes escrita de dois jeitos, e nenhuma deduplicação junta as duas sem
    // mentir sobre qual é qual.
    if level.int("was") == 0
        && let Some(log) = ask(
            conn,
            report,
            "log do servidor",
            "admin",
            Doc::new().with("getLog", "global"),
        )?
    {
        for value in log.list("log").iter().rev() {
            let Some(text) = value.as_text() else {
                continue;
            };
            let Some(lenta) = from_log(text) else {
                continue;
            };
            if vistas.insert(format!("{}{}", lenta.ns, lenta.forma)) {
                achadas.push(lenta);
            }
        }
    }

    if achadas.is_empty() {
        report.ok("nenhuma operação lenta registrada");
        return Ok(());
    }
    achadas.sort_by(|a, b| b.millis.total_cmp(&a.millis));
    report.table(&["duração", "coleção", "plano", "examinados", "operação"]);
    for lenta in &achadas {
        report.cells_deep(
            vec![
                millis(lenta.millis),
                lenta.ns.clone(),
                lenta.plano.clone(),
                lenta.examinados.clone(),
                one_line(&lenta.forma, 140),
            ],
            match lenta.millis >= 1000.0 {
                true => Tone::Ruim,
                false => Tone::Aviso,
            },
            vec![
                ("coleção", lenta.ns.clone()),
                ("duração", millis(lenta.millis)),
                (
                    "plano",
                    match lenta.plano.is_empty() {
                        true => "não registrado".to_string(),
                        false => lenta.plano.clone(),
                    },
                ),
                ("documentos examinados", lenta.examinados.clone()),
                ("o que fazer", lenta.fix.clone()),
            ],
            quebrar(&lenta.forma, 140),
        );
    }
    // A sugestão é o que ninguém copia de uma tabela: vai embaixo, e só para as que têm
    // uma para dar.
    for lenta in achadas.iter().filter(|lenta| !lenta.fix.is_empty()).take(6) {
        report.aviso(lenta.linha(), lenta.fix.clone());
    }
    Ok(())
}

/// One `system.profile` entry as a line plus what to do about it.
/// Se esta entrada é a própria ferramenta aparecendo no espelho.
///
/// O profiler grava tudo que passa do limite, e as perguntas desta investigação passam
/// como quaisquer outras. Mostrar o `$collStats` desta tela como «query lenta encontrada»
/// seria o relatório reportando a si mesmo.
fn nossa(entry: &Doc) -> bool {
    let ns = entry.text("ns");
    if ns.contains(".$cmd")
        || ns.contains(".system.")
        || ["admin.", "config.", "local."]
            .iter()
            .any(|interno| ns.starts_with(interno))
    {
        return true;
    }
    let Some(comando) = entry.doc("command") else {
        return false;
    };
    if comando.keys().any(|chave| HOUSEKEEPING.contains(&chave)) {
        return true;
    }
    // Um `aggregate` pode ser trabalho de verdade; o que denuncia os nossos é o estágio.
    comando.list("pipeline").iter().any(|estagio| {
        estagio.as_doc().is_some_and(|doc| {
            doc.keys()
                .any(|chave| ["$collStats", "$indexStats", "$currentOp"].contains(&chave))
        })
    })
}

fn from_profile(entry: &Doc) -> Lenta {
    let examined = entry.num("docsExamined");
    let returned = entry.num("nreturned");
    let plano = entry.text("planSummary");
    let esforco = match examined + returned > 0.0 {
        true => format!("{} examinados → {}", count(examined), count(returned)),
        false => String::new(),
    };
    let (do_pipeline, ordem_do_pipeline) = stages_of(entry.doc("command"));
    let filter = entry
        .doc("command.filter")
        .or_else(|| entry.doc("query.filter"))
        .or_else(|| entry.doc("command.q"))
        .or(do_pipeline.as_ref());
    let sort = entry
        .doc("command.sort")
        .or_else(|| entry.doc("query.sort"))
        .or(ordem_do_pipeline.as_ref());
    let mut fix = suggestion_from_doc(filter, sort).unwrap_or_default();
    if plano.contains("COLLSCAN") {
        fix = format!("varreu a coleção inteira. {fix}");
    }
    let comando = entry
        .doc("command")
        .map(|doc| doc.render_forma())
        .unwrap_or_default();
    Lenta {
        visto: std::time::Instant::now(),
        millis: entry.num("millis"),
        ns: entry.text("ns"),
        plano,
        forma: comando.clone(),
        examinados: count(examined),
        resumo: format!("{} {esforco} {}", entry.text("op"), one_line(&comando, 100)),
        fix: fix.trim().to_string(),
    }
}

/// Commands that are administration rather than application traffic — including every
/// command this tool sends. A cold server logs its own `listDatabases` as a slow query,
/// and reporting the investigation's own footsteps back to the investigator is both
/// noise and a lie about what the application is doing.
const HOUSEKEEPING: &[&str] = &[
    "createIndexes",
    "dropIndexes",
    "create",
    "drop",
    "hello",
    "isMaster",
    "ismaster",
    "buildInfo",
    "serverStatus",
    "hostInfo",
    "listDatabases",
    "listCollections",
    "listIndexes",
    "dbStats",
    "collStats",
    "connectionStatus",
    "replSetGetStatus",
    "currentOp",
    "getLog",
    "getCmdLineOpts",
    "getParameter",
    "profile",
    "saslStart",
    "saslContinue",
    "killCursors",
    "ping",
    "endSessions",
    "isdbgrid",
];

/// A "Slow query" line out of the server log, in the JSON the server has written since
/// 4.4. Returns nothing for every other line, which is almost all of them.
fn from_log(text: &str) -> Option<Lenta> {
    let parsed: serde_json::Value = serde_json::from_str(text).ok()?;
    if parsed.get("msg")?.as_str()? != "Slow query" {
        return None;
    }
    let attr = parsed.get("attr")?;
    let namespace = attr.get("ns").and_then(|v| v.as_str()).unwrap_or("?");
    // `config`, `local` e `admin` são a papelada do próprio servidor: a query do coletor
    // de sessões contra `config.transactions` é lenta em todo MongoDB parado, e sugerir um
    // índice nela seria dar conselho sobre as tripas do MongoDB.
    if ["config.", "local.", "admin."]
        .iter()
        .any(|internal| namespace.starts_with(internal))
    {
        return None;
    }
    let duration = attr
        .get("durationMillis")
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.0);
    let plano = attr
        .get("planSummary")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let examined = attr
        .get("docsExamined")
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.0);
    let returned = attr
        .get("nreturned")
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.0);
    let command = attr.get("command");
    if let Some(object) = command.and_then(|c| c.as_object())
        && object
            .keys()
            .any(|key| HOUSEKEEPING.contains(&key.as_str()))
    {
        return None;
    }
    // Uma escrita não tem plano e não examina nada; imprimir zeros para ela se leria como
    // achado em vez de como ausência.
    let esforco = match examined + returned > 0.0 {
        true => format!("{} examinados → {}", count(examined), count(returned)),
        false => String::new(),
    };
    // Numa agregação, o índice que faltou é o do primeiro `$match` e do primeiro `$sort`:
    // depois de um `$group` o filtro é sobre outra coisa e nenhum índice alcança.
    let estagio = |nome: &str| -> Option<&serde_json::Value> {
        let pipeline = command.and_then(|c| c.get("pipeline"))?.as_array()?;
        for estagio in pipeline {
            let objeto = estagio.as_object()?;
            let (primeiro, valor) = objeto.iter().next()?;
            if primeiro == nome {
                return Some(valor);
            }
            if ["$group", "$bucket", "$unwind"].contains(&primeiro.as_str()) {
                return None;
            }
        }
        None
    };
    let mut fix = suggestion_from_json(
        command
            .and_then(|c| c.get("filter"))
            .or_else(|| estagio("$match")),
        command
            .and_then(|c| c.get("sort"))
            .or_else(|| estagio("$sort")),
    )
    .unwrap_or_default();
    if plano.contains("COLLSCAN") {
        fix = format!("varreu a coleção inteira. {fix}");
    }
    let comando = forma_json(command);
    Some(Lenta {
        visto: std::time::Instant::now(),
        millis: duration,
        ns: namespace.to_string(),
        plano,
        forma: comando.clone(),
        examinados: count(examined),
        resumo: format!("{esforco} {comando}"),
        fix: fix.trim().to_string(),
    })
}

/// The index a filter and a sort are asking for, by the rule that decides the order of
/// its keys: equality first, then the sort, then ranges. Getting that order wrong is the
/// most common way a present index still fails to help.
fn compose(equality: Vec<String>, sort: Vec<String>, range: Vec<String>) -> Option<String> {
    let mut keys: Vec<String> = Vec::new();
    for group in [equality, sort, range] {
        for key in group {
            if !keys.contains(&key) && !key.starts_with('$') {
                keys.push(key);
            }
        }
    }
    if keys.is_empty() {
        return None;
    }
    let spec: Vec<String> = keys.iter().take(4).map(|key| format!("{key}: 1")).collect();
    Some(format!(
        "índice candidato (igualdade, ordenação, intervalo): createIndex({{ {} }}) — confira com explain(\"executionStats\") antes",
        spec.join(", ")
    ))
}

/// Whether an operator restricts to one value or to a range, which is what decides
/// where its field belongs in the index.
fn ranged(operator: &str) -> bool {
    matches!(
        operator,
        "$gt" | "$gte" | "$lt" | "$lte" | "$ne" | "$nin" | "$regex" | "$exists" | "$not"
    )
}

/// O `$match` e o `$sort` de um pipeline de agregação, que é onde mora o índice que ela
/// queria ter. Só os primeiros: depois de um `$group` a ordem e o filtro já são sobre
/// outra coisa, e um índice não alcança mais.
fn stages_of(comando: Option<&Doc>) -> (Option<Doc>, Option<Doc>) {
    let Some(pipeline) = comando.map(|doc| doc.list("pipeline")) else {
        return (None, None);
    };
    let (mut filtro, mut ordem) = (None, None);
    for estagio in pipeline.iter().filter_map(Value::as_doc) {
        match estagio.0.first().map(|(nome, _)| nome.as_str()) {
            Some("$match") if filtro.is_none() => filtro = estagio.doc("$match").cloned(),
            Some("$sort") if ordem.is_none() => ordem = estagio.doc("$sort").cloned(),
            // Depois de agrupar, o que se filtra e ordena são os resultados, e nenhum
            // índice da coleção serve para isso.
            Some("$group") | Some("$bucket") | Some("$unwind") => break,
            _ => {}
        }
    }
    (filtro, ordem)
}

fn suggestion_from_doc(filter: Option<&Doc>, sort: Option<&Doc>) -> Option<String> {
    let (mut equality, mut range) = (Vec::new(), Vec::new());
    if let Some(filter) = filter {
        for (key, value) in &filter.0 {
            if key == "$and" {
                for item in value.as_list().unwrap_or(&[]) {
                    if let Some(doc) = item.as_doc() {
                        for (key, value) in &doc.0 {
                            classify(key, value.as_doc(), &mut equality, &mut range);
                        }
                    }
                }
                continue;
            }
            classify(key, value.as_doc(), &mut equality, &mut range);
        }
    }
    let sorted = sort
        .map(|doc| doc.keys().map(str::to_string).collect())
        .unwrap_or_default();
    compose(equality, sorted, range)
}

fn classify(
    key: &str,
    operators: Option<&Doc>,
    equality: &mut Vec<String>,
    range: &mut Vec<String>,
) {
    if key.starts_with('$') {
        return;
    }
    let is_range = operators.is_some_and(|doc| doc.keys().any(ranged));
    if is_range {
        range.push(key.to_string());
    } else {
        equality.push(key.to_string());
    }
}

fn suggestion_from_json(
    filter: Option<&serde_json::Value>,
    sort: Option<&serde_json::Value>,
) -> Option<String> {
    let (mut equality, mut range) = (Vec::new(), Vec::new());
    if let Some(object) = filter.and_then(serde_json::Value::as_object) {
        for (key, value) in object {
            if key == "$and" {
                for item in value.as_array().unwrap_or(&Vec::new()) {
                    for (key, value) in item.as_object().into_iter().flatten() {
                        classify_json(key, value, &mut equality, &mut range);
                    }
                }
                continue;
            }
            classify_json(key, value, &mut equality, &mut range);
        }
    }
    let sorted: Vec<String> = sort
        .and_then(serde_json::Value::as_object)
        .map(|object| object.keys().cloned().collect())
        .unwrap_or_default();
    compose(equality, sorted, range)
}

fn classify_json(
    key: &str,
    value: &serde_json::Value,
    equality: &mut Vec<String>,
    range: &mut Vec<String>,
) {
    if key.starts_with('$') {
        return;
    }
    let is_range = value
        .as_object()
        .is_some_and(|object| object.keys().any(|operator| ranged(operator)));
    if is_range {
        range.push(key.to_string());
    } else {
        equality.push(key.to_string());
    }
}

fn replication(conn: &mut mongo::Conn, report: &mut Report) -> Result<(), String> {
    // A seção abre **antes** da pergunta: uma recusa é resposta desta seção, e sem abrir
    // ela primeiro a frase cairia dentro do cartão anterior, falando de outro assunto.
    report.section("Replicação");
    let Some(status) = ask(
        conn,
        report,
        "replSetGetStatus",
        "admin",
        Doc::new().with("replSetGetStatus", 1),
    )?
    else {
        report.line("este servidor não faz parte de um conjunto de réplicas");
        report.aviso(
            "sem réplica: este servidor é o único lugar onde estes dados existem",
            "uma máquina só é um ponto único de falha, e restaurar backup leva o tempo que leva. Um conjunto de réplicas com três membros troca isso por uma eleição de dez segundos",
        );
        return Ok(());
    };
    let members: Vec<&Doc> = status
        .list("members")
        .iter()
        .filter_map(Value::as_doc)
        .collect();
    let primary = members
        .iter()
        .find(|member| member.text("stateStr") == "PRIMARY")
        .map(|member| member.num("optimeDate"))
        .unwrap_or(0.0);
    report.table(&["membro", "estado", "saúde", "atraso"]);
    for member in &members {
        let lag = (primary - member.num("optimeDate")) / 1000.0;
        report.cells_deep(
            vec![
                member.text("name"),
                member.text("stateStr"),
                match member.num("health") {
                    viva if viva > 0.0 => "ok".to_string(),
                    _ => "inacessível".to_string(),
                },
                if member.text("stateStr") == "PRIMARY" {
                    "—".to_string()
                } else {
                    duration(lag)
                },
            ],
            match (member.num("health"), lag) {
                (0.0, _) => Tone::Ruim,
                (_, atraso) if atraso > 60.0 => Tone::Aviso,
                _ => Tone::Normal,
            },
            vec![
                ("membro", member.text("name")),
                ("estado", member.text("stateStr")),
                ("saúde", member.text("health")),
                (
                    "atraso para o primário",
                    match member.text("stateStr").as_str() {
                        "PRIMARY" => "é o primário".to_string(),
                        _ => duration(lag),
                    },
                ),
                ("último optime", member.text("optimeDate")),
                ("último heartbeat", member.text("lastHeartbeatRecv")),
                ("ping", format!("{} ms", member.text("pingMs"))),
                ("no ar há", duration(member.num("uptime"))),
                ("erro no heartbeat", member.text("lastHeartbeatMessage")),
            ],
            Vec::new(),
        );
        if member.num("health") == 0.0 {
            report.grave(
                format!("o membro {} está inacessível", member.text("name")),
                "um conjunto sem maioria viva perde o primário e o banco inteiro para de aceitar escrita",
            );
        } else if lag > 60.0 && member.text("stateStr") == "SECONDARY" {
            report.aviso(
                format!("{} está {} atrás do primário", member.text("name"), duration(lag)),
                "quem lê desse secundário lê o passado. Costuma ser disco da réplica ou rede entre elas",
            );
        }
    }
    Ok(())
}

fn storage(
    conn: &mut mongo::Conn,
    report: &mut Report,
    target: &mongo::Target,
    collections: &[Collection],
) -> Result<(), String> {
    report.section("Volumes");
    if let Some(stats) = ask(
        conn,
        report,
        "dbStats",
        &target.database,
        Doc::new().with("dbStats", 1).with("maxTimeMS", 15_000),
    )? {
        report.field("documentos", count(stats.num("objects")));
        report.field("dados", bytes(stats.num("dataSize")));
        report.field("em disco", bytes(stats.num("storageSize")));
        report.field("índices", bytes(stats.num("indexSize")));
        let total = stats.num("fsTotalSize");
        if total > 0.0 {
            let used = stats.num("fsUsedSize");
            report.field(
                "disco do servidor",
                format!(
                    "{} de {} ({:.0}%)",
                    bytes(used),
                    bytes(total),
                    100.0 * used / total
                ),
            );
            if used / total > 0.85 {
                report.grave(
                    format!("o disco do servidor está {:.0}% cheio", 100.0 * used / total),
                    "MongoDB para de aceitar escrita quando o disco enche, e compactar precisa de espaço livre para rodar",
                );
            }
        }
    }

    // Space the storage engine has freed inside the files and will reuse, but has not
    // given back to the filesystem. Normal in small amounts; a third of the collection
    // means a lot of deleting happened.
    for collection in collections {
        if collection.storage < 8.0 * 1024.0 * 1024.0 || collection.reusable == 0.0 {
            continue;
        }
        let share = collection.reusable / collection.storage;
        if share > 0.3 {
            report.aviso(
                format!(
                    "{} ocupa {} em disco, dos quais {} ({:.0}%) são espaço livre dentro do arquivo",
                    collection.name,
                    bytes(collection.storage),
                    bytes(collection.reusable),
                    100.0 * share
                ),
                "o espaço volta a ser usado pela própria coleção, mas não volta para o disco. Devolver exige compact, que bloqueia — decisão de janela de manutenção",
            );
        }
    }
    // A document that is routinely huge is usually an array nobody bounded.
    for collection in collections {
        if collection.average > 256.0 * 1024.0 {
            report.aviso(
                format!(
                    "documentos de {} têm em média {}",
                    collection.name,
                    bytes(collection.average)
                ),
                "documentos grandes assim quase sempre são array que cresce sem limite. O teto rígido é 16 MB, e o custo aparece muito antes",
            );
        }
    }
    Ok(())
}

fn oplog(conn: &mut mongo::Conn, report: &mut Report) -> Result<(), String> {
    let first = conn.cursor(
        "local",
        Doc::new()
            .with("find", "oplog.rs")
            .with("sort", Doc::new().with("$natural", 1))
            .with("limit", 1),
    );
    let last = conn.cursor(
        "local",
        Doc::new()
            .with("find", "oplog.rs")
            .with("sort", Doc::new().with("$natural", -1))
            .with("limit", 1),
    );
    let (Ok(first), Ok(last)) = (first, last) else {
        return Ok(());
    };
    let (Some(first), Some(last)) = (first.first(), last.first()) else {
        return Ok(());
    };
    let window = last.num("ts") - first.num("ts");
    if window <= 0.0 {
        return Ok(());
    }
    report.field("janela do oplog", duration(window));
    // The oplog window is how long a secondary may be down before it can no longer catch
    // up and has to be rebuilt from scratch.
    if window < 24.0 * 3600.0 {
        report.aviso(
            format!("o oplog cobre só {}", duration(window)),
            "uma réplica parada mais que isso não consegue mais alcançar e precisa de ressincronização completa. É também a janela de qualquer recuperação para um ponto no tempo",
        );
    }
    Ok(())
}

fn configuration(
    conn: &mut mongo::Conn,
    report: &mut Report,
    target: &mongo::Target,
) -> Result<(), String> {
    let Some(options) = ask(
        conn,
        report,
        "getCmdLineOpts",
        "admin",
        Doc::new().with("getCmdLineOpts", 1),
    )?
    else {
        return Ok(());
    };
    report.section("Configuração");
    let parsed = options.doc("parsed").cloned().unwrap_or_default();
    let authorization = parsed.text("security.authorization");
    report.field(
        "autenticação",
        match authorization.as_str() {
            "" => "não declarada",
            value => value,
        },
    );
    report.field("tls", parsed.text("net.tls.mode"));
    report.field("bindIp", parsed.text("net.bindIp"));
    let cache = parsed.num("storage.wiredTiger.engineConfig.cacheSizeGB");
    if cache > 0.0 {
        report.field("cache configurado", format!("{cache} GB"));
    }
    if authorization != "enabled" && target.user.is_empty() {
        report.grave(
            "o servidor está sem autorização ligada",
            "security.authorization: enabled, e usuários criados antes de expor a porta",
        );
    }
    if parsed.text("net.bindIp") == "0.0.0.0" && authorization != "enabled" {
        report.grave(
            "escutando em 0.0.0.0 sem autenticação",
            "esta é literalmente a configuração dos vazamentos de MongoDB que viram notícia",
        );
    }
    Ok(())
}

/// Latência, memória, cursores e conflitos: o que o `serverStatus` responde sobre a saúde
/// do processo, e que nenhuma query isolada revela.
///
/// A latência média por operação é a métrica que um cliente sente e que quase nenhum
/// painel mostra: `opLatencies` guarda o tempo somado e a contagem, e a divisão dos dois é
/// a resposta para «o banco está lento?» antes de qualquer investigação de query.
fn health(report: &mut Report, status: &Doc, host: &Doc) {
    report.section("Saúde do servidor");

    for (nome, caminho) in [
        ("leituras", "opLatencies.reads"),
        ("escritas", "opLatencies.writes"),
        ("comandos", "opLatencies.commands"),
    ] {
        let total = status.num(&format!("{caminho}.latency"));
        let ops = status.num(&format!("{caminho}.ops"));
        if ops <= 0.0 {
            continue;
        }
        // O MongoDB conta em microssegundos.
        let media = total / ops / 1000.0;
        report.field(
            format!("latência média — {nome}"),
            format!(
                "{} em {}",
                millis(media),
                plural(ops, "operação", "operações")
            ),
        );
        if media > 100.0 {
            report.aviso(
                format!("{nome} levam {} em média", millis(media)),
                "média alta assim raramente é uma query só: costuma ser disco saturado, cache pequeno demais, ou fila de tickets",
            );
        }
    }

    let residente = status.num("mem.resident") * 1024.0 * 1024.0;
    let ram = host.num("system.memSizeMB") * 1024.0 * 1024.0;
    if residente > 0.0 {
        report.field(
            "memória do processo",
            match ram > 0.0 {
                true => format!(
                    "{} de {} da máquina ({:.0}%)",
                    bytes(residente),
                    bytes(ram),
                    100.0 * residente / ram
                ),
                false => bytes(residente),
            },
        );
    }
    if host.num("system.numCores") > 0.0 {
        report.field(
            "máquina",
            format!(
                "{} núcleos, {}",
                count(host.num("system.numCores")),
                host.text("os.name")
            ),
        );
    }

    let abertos = status.num("metrics.cursor.open.total");
    let sem_prazo = status.num("metrics.cursor.open.noTimeout");
    if abertos > 0.0 || sem_prazo > 0.0 {
        report.field(
            "cursores abertos",
            format!("{} ({} sem prazo)", count(abertos), count(sem_prazo)),
        );
    }
    if sem_prazo > 0.0 {
        report.aviso(
            format!("{} cursor(es) abertos com noTimeout", count(sem_prazo)),
            "um cursor sem prazo que o cliente esqueceu fica segurando recursos até o servidor reiniciar. Quase sempre é driver configurado com noCursorTimeout sem necessidade",
        );
    }

    // Conflito de escrita é o WiredTiger mandando a operação tentar de novo porque outra
    // mexeu no mesmo documento. Um punhado é normal; muitos são duas partes do sistema
    // brigando pela mesma linha.
    let conflitos = status.num("metrics.operation.writeConflicts");
    let escritas = status.num("opcounters.update") + status.num("opcounters.delete");
    if conflitos > 0.0 {
        report.field("conflitos de escrita", count(conflitos));
        if escritas > 1000.0 && conflitos / escritas > 0.01 {
            report.aviso(
                format!(
                    "{:.1}% das escritas foram refeitas por conflito",
                    100.0 * conflitos / escritas
                ),
                "duas partes da aplicação disputam os mesmos documentos. Cada conflito é a operação inteira repetida — e repetida sem ninguém ver",
            );
        }
    }

    let abortadas = status.num("transactions.totalAborted");
    let commitadas = status.num("transactions.totalCommitted");
    if abortadas + commitadas > 0.0 {
        report.field(
            "transações multi-documento",
            format!(
                "{} confirmadas, {} abortadas",
                count(commitadas),
                count(abortadas)
            ),
        );
        if abortadas > commitadas * 0.1 && abortadas > 10.0 {
            report.aviso(
                format!(
                    "{:.0}% das transações são abortadas",
                    100.0 * abortadas / (abortadas + commitadas)
                ),
                "transação abortada é trabalho jogado fora, e no MongoDB costuma ser transação longa demais (o limite padrão é 60s) ou conflito de escrita",
            );
        }
    }

    let ttl = status.num("metrics.ttl.deletedDocuments");
    if ttl > 0.0 {
        report.field(
            "documentos apagados por TTL",
            format!(
                "{} em {} passagens",
                count(ttl),
                count(status.num("metrics.ttl.passes"))
            ),
        );
    }

    let requisicoes = status.num("network.numRequests");
    if requisicoes > 0.0 {
        report.field(
            "rede",
            format!(
                "{} entraram, {} saíram, {} por requisição",
                bytes(status.num("network.bytesIn")),
                bytes(status.num("network.bytesOut")),
                bytes(status.num("network.bytesOut") / requisicoes)
            ),
        );
        // Muitos bytes por requisição é quase sempre projeção faltando: a aplicação pede o
        // documento inteiro e usa três campos dele.
        if status.num("network.bytesOut") / requisicoes > 256.0 * 1024.0 {
            report.aviso(
                format!(
                    "cada requisição devolve {} em média",
                    bytes(status.num("network.bytesOut") / requisicoes)
                ),
                "devolver documento inteiro quando a tela usa três campos custa rede, cache e serialização. Uma projeção no find corta isso sem mudar mais nada",
            );
        }
    }
}

/// O que o WiredTiger diz sobre o próprio trabalho: sujeira no cache, despejo, checkpoint
/// e journal. É onde uma latência que não aparece em query nenhuma costuma estar.
fn engine(report: &mut Report, status: &Doc) {
    let Some(wt) = status.doc("wiredTiger") else {
        return;
    };
    let cache = wt.doc("cache").cloned().unwrap_or_default();
    let max = cache.num("maximum bytes configured");
    let sujas = cache.num("tracked dirty bytes in the cache");
    if max > 0.0 && sujas > 0.0 {
        let pct = 100.0 * sujas / max;
        // Acima de 20% o WiredTiger passa a frear as escritas para conseguir acompanhar o
        // checkpoint, e a aplicação sente isso como latência sem causa aparente.
        if pct > 20.0 {
            report.grave(
                format!("{pct:.0}% do cache está sujo (páginas por gravar)"),
                "acima de 20% o próprio motor começa a segurar as escritas para o checkpoint alcançar. É disco não dando conta do ritmo de escrita",
            );
        }
    }

    let checkpoint = wt.num("transaction.transaction checkpoint most recent time (msecs)");
    if checkpoint > 0.0 {
        report.field("último checkpoint levou", millis(checkpoint));
        if checkpoint > 60_000.0 {
            report.aviso(
                format!("o último checkpoint levou {}", millis(checkpoint)),
                "o intervalo padrão entre checkpoints é 60s: um que demora mais que isso nunca termina antes do próximo começar",
            );
        }
    }

    let sync_tempo = wt.num("log.log sync time duration (usecs)");
    let sync_ops = wt.num("log.log sync operations");
    if sync_ops > 0.0 {
        let media = sync_tempo / sync_ops / 1000.0;
        report.field("sincronização do journal", millis(media));
        if media > 30.0 {
            report.aviso(
                format!("cada sincronização do journal leva {}", millis(media)),
                "é o disco respondendo devagar no caminho do commit: todo write com journal espera por isso",
            );
        }
    }

    let faltas = status.num("extra_info.page_faults");
    if faltas > 0.0 {
        report.field("faltas de página", count(faltas));
    }
}

/// Um cluster fragmentado: shards, balanceador, e os pedaços que não cabem mais.
fn sharding(conn: &mut mongo::Conn, report: &mut Report) -> Result<(), String> {
    let Some(shards) = ask(
        conn,
        report,
        "listShards",
        "admin",
        Doc::new().with("listShards", 1),
    )?
    else {
        return Ok(());
    };
    report.section("Sharding");
    report.table(&["shard", "endereço", "estado"]);
    for shard in shards.list("shards").iter().filter_map(Value::as_doc) {
        report.cells_deep(
            vec![
                shard.text("_id"),
                one_line(&shard.text("host"), 90),
                match shard.text("state").as_str() {
                    "1" => "ativo".to_string(),
                    outro => outro.to_string(),
                },
            ],
            Tone::Normal,
            vec![
                ("shard", shard.text("_id")),
                ("estado", shard.text("state")),
                ("etiquetas", shard.text("tags")),
            ],
            quebrar(&shard.text("host"), 120),
        );
    }
    if let Some(balanceador) = ask(
        conn,
        report,
        "balancerStatus",
        "admin",
        Doc::new().with("balancerStatus", 1),
    )? {
        report.field(
            "balanceador",
            match balanceador.flag("mode") || balanceador.text("mode") == "full" {
                true => "ligado".to_string(),
                false => format!("modo {}", balanceador.text("mode")),
            },
        );
        if balanceador.text("mode") == "off" {
            report.aviso(
                "o balanceador está desligado",
                "sem ele os pedaços param onde estiverem, e um shard cresce enquanto os outros ficam vazios. Desligado por uma janela é normal; desligado e esquecido é o começo de um shard cheio",
            );
        }
    }
    Ok(())
}

/// Índices demais numa coleção.
fn index_shape(report: &mut Report, collections: &[Collection]) {
    for collection in collections {
        // Cada índice é escrito em todo insert e em todo update que toca a chave dele. O
        // limite rígido do MongoDB é 64, e muito antes disso a escrita já está pagando
        // caro por índices que ninguém pediu.
        if collection.indexes >= 8.0 {
            report.aviso(
                format!(
                    "{} tem {} índices",
                    collection.name,
                    count(collection.indexes)
                ),
                "todo insert e todo update escrevem em cada um deles. A coluna de usos na tabela acima costuma explicar metade",
            );
        }
    }
}
