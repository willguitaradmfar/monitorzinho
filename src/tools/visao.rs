//! Visão: uma pergunta guardada, respondida em tabela.
//!
//! O Sherlock faz as perguntas dele; esta ferramenta faz **a sua**. Um SQL num Postgres,
//! num MySQL ou num SQLite, um comando num MongoDB, ou o array JSON que uma API devolve
//! — cadastrado uma vez, executado quando a tela abre pela primeira vez na sessão, e
//! depois só quando alguém pede outra vez com Ctrl+R. O que está na tela é o retrato da
//! última execução, e o cabeçalho diz de quando ele é.
//!
//! **Só leitura, em cada fonte do jeito dela.** No Postgres a query roda dentro de um
//! `BEGIN READ ONLY`; no MySQL a sessão é posta em `TRANSACTION READ ONLY` antes de
//! qualquer coisa; o SQLite é aberto com a flag de leitura e `query_only` ligado; no
//! MongoDB só passa um comando de uma lista fixa de leitores, e um `aggregate` com
//! `$out` ou `$merge` é recusado antes de sair daqui. Uma visão apontada para produção
//! ganha o lugar dela sendo impossível de culpar — a mesma regra do Sherlock, pelo mesmo
//! motivo.
//!
//! **Um banco fechado atrás de uma máquina de salto** é o caso comum em produção, e é
//! por isso que as três fontes de banco aceitam um túnel SSH opcional: o `ssh` desta
//! máquina abre um `-L` para o banco, a consulta passa por ele e o túnel é fechado em
//! seguida. Só por chave — o túnel não pode perguntar nada, porque não há onde
//! responder.
//!
//! **A fonte HTTP tem a sua própria pergunta.** Um SQL já diz quais colunas quer; um
//! JSON não. Então há dois campos: o *caminho* até o array dentro da resposta e as
//! *colunas* de cada item, os dois no jeito do `jq` — `.data.items`, `.address.city as
//! cidade`. É o pedaço do `jq` que chega a uma lista e escolhe campos dela, escrito à
//! mão aqui embaixo, e nada além disso.
//!
//! O resultado fica **só em memória**: fechar o programa esquece, e a próxima abertura
//! executa de novo. Um retrato guardado em disco envelhece sem avisar; um que se refaz ao
//! abrir diz sempre de quando é.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value as Json;

use crate::painel::{Layout, Pane, Row, Tone};

use super::sherlock::{self, bson, mongo, pg};
use super::{
    Board, Card, EventKind, Execution, ParamSpec, Recorder, Rotulos, Tool, lock_board, sshfwd,
};

/// As fontes, pelo nome que aparece na tela. Nunca mude um destes textos sem migrar o que
/// está gravado: é o valor do parâmetro, e uma execução salva volta procurando
/// exatamente esta palavra.
const FONTES: &[&str] = &["PostgreSQL", "MySQL", "MongoDB", "SQLite", "HTTP JSON"];
const METODOS: &[&str] = &["GET", "POST"];
const AUTENTICACOES: &[&str] = &["nenhuma", "Bearer", "Basic"];
const SIM_NAO: &[&str] = &["não", "sim"];
const TLS_OPCOES: &[&str] = &["não", "sim", "sim, sem validar certificado"];
const SIM: &[&str] = &["sim"];
const POST: &[&str] = &["POST"];
const COM_CREDENCIAL: &[&str] = &["Bearer", "Basic"];

/// Quantas linhas a tela mostra. A query deveria ter o seu próprio `LIMIT`; isto é a rede
/// embaixo de quem esqueceu, e a nota do rodapé diz quando ela segurou alguma coisa.
pub const LINHAS_MAX: usize = 1000;
/// Quanto cada célula carrega. Um documento inteiro numa célula não cabe em tabela
/// nenhuma; o que cabe é o começo dele, e o detalhe da linha mostra o resto.
const CELULA_MAX: usize = 200;
/// O teto de tempo de uma consulta, e o do soquete. O mesmo do Sherlock, pelo mesmo
/// motivo: uma visão que trava num servidor ocupado tem que desistir, não esperar.
const TEMPO: Duration = Duration::from_secs(15);
/// Quanto esperar o túnel SSH subir — autenticação e tudo — antes de desistir dele.
const TUNEL_ESPERA: Duration = Duration::from_secs(20);

const ROTULOS: Rotulos = Rotulos {
    andamento: "executando…",
    feito: "executado",
    pedir: "executa de novo",
    pedir_inf: "executar de novo",
    nunca: "ainda não executou",
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Fonte {
    Postgres,
    Mysql,
    Mongo,
    Sqlite,
    Http,
}

impl Fonte {
    fn from(texto: &str) -> Self {
        match texto {
            "MySQL" => Fonte::Mysql,
            "MongoDB" => Fonte::Mongo,
            "SQLite" => Fonte::Sqlite,
            "HTTP JSON" => Fonte::Http,
            _ => Fonte::Postgres,
        }
    }

    /// As fontes que são um servidor de banco — as que têm máquina, porta, usuário,
    /// senha e base, e as que podem passar por um túnel.
    const BANCOS: &'static [&'static str] = &[FONTES[0], FONTES[1], FONTES[2]];
    /// As que falam TLS por conta própria. O Postgres negocia sozinho (`sslmode=prefer`)
    /// e não pergunta.
    const COM_TLS: &'static [&'static str] = &[FONTES[1], FONTES[2]];
    /// As que recebem uma consulta no campo de consulta.
    const COM_CONSULTA: &'static [&'static str] = &[FONTES[0], FONTES[1], FONTES[2], FONTES[3]];
    const SQLITE: &'static [&'static str] = &[FONTES[3]];
    const HTTP: &'static [&'static str] = &[FONTES[4]];

    fn porta_padrao(self) -> u16 {
        match self {
            Fonte::Postgres => 5432,
            Fonte::Mysql => 3306,
            Fonte::Mongo => 27017,
            Fonte::Sqlite | Fonte::Http => 0,
        }
    }

    /// O prefixo da linha na lista.
    fn nome(self) -> &'static str {
        match self {
            Fonte::Postgres => "pg",
            Fonte::Mysql => "mysql",
            Fonte::Mongo => "mongo",
            Fonte::Sqlite => "sqlite",
            Fonte::Http => "http",
        }
    }
}

/// O que fazer com TLS nas fontes que perguntam.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tls {
    Nao,
    Sim,
    /// Cifra, mas aceita qualquer certificado: o próprio, o vencido, o de outro nome.
    SemValidar,
}

impl Tls {
    fn from(texto: &str) -> Self {
        match texto {
            "sim" => Tls::Sim,
            t if t.starts_with("sim") => Tls::SemValidar,
            _ => Tls::Nao,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Auth {
    Nenhuma,
    Bearer,
    Basic,
}

impl Auth {
    fn from(texto: &str) -> Self {
        match texto {
            "Bearer" => Auth::Bearer,
            "Basic" => Auth::Basic,
            _ => Auth::Nenhuma,
        }
    }
}

pub struct VisaoTool;

impl Tool for VisaoTool {
    fn id(&self) -> &'static str {
        "visao"
    }

    fn name(&self) -> &'static str {
        "Visão"
    }

    fn description(&self) -> &'static str {
        "Uma pergunta sua, respondida em tabela: um SQL num Postgres, MySQL ou SQLite, um comando num MongoDB, ou o array JSON de uma API — só leitura, executada ao abrir e a cada Ctrl+R"
    }

    fn params(&self) -> Vec<ParamSpec> {
        vec![
            ParamSpec::choice(
                "fonte",
                "Fonte",
                FONTES,
                "De onde vem a lista. Decide os campos abaixo e o que a consulta quer dizer",
            ),
            ParamSpec::text(
                "host",
                "Máquina",
                "localhost",
                "IP ou nome do servidor de banco. Com o túnel SSH ligado, é a máquina como a máquina de salto a enxerga — «localhost» quando o banco está nela mesma",
            )
            .only_when("fonte", Fonte::BANCOS),
            ParamSpec::text(
                "porta",
                "Porta",
                "",
                "Vazio usa a padrão da fonte: 5432, 3306 ou 27017",
            )
            .only_when("fonte", Fonte::BANCOS),
            ParamSpec::text(
                "usuario",
                "Usuário",
                "",
                "Vazio no Postgres é «postgres»; no MongoDB, conecta sem autenticar",
            )
            .only_when("fonte", Fonte::BANCOS),
            ParamSpec::text(
                "senha",
                "Senha",
                "",
                "Fica gravada junto com a execução, como no Sherlock. Nunca aparece na lista nem na tela",
            )
            .only_when("fonte", Fonte::BANCOS),
            ParamSpec::text(
                "base",
                "Base",
                "",
                "A base onde a consulta roda. Vazio no Postgres é a base com o nome do usuário; no MongoDB é «admin»",
            )
            .only_when("fonte", Fonte::BANCOS),
            ParamSpec::choice(
                "tls",
                "TLS",
                TLS_OPCOES,
                "No MySQL, «sim» verifica o certificado e «sem validar» aceita qualquer um — o caso do certificado próprio, e do túnel, em que o servidor se apresenta como 127.0.0.1. No MongoDB o cliente cifra sem verificar nas duas opções. O Postgres não pergunta: negocia TLS se o servidor oferecer",
            )
            .only_when("fonte", Fonte::COM_TLS),
            ParamSpec::choice(
                "ssh",
                "Túnel SSH",
                SIM_NAO,
                "«sim» chega ao banco por uma máquina de salto, como o pgAdmin: o ssh daqui abre um túnel até ela e o banco é alcançado por dentro dele. A máquina e a porta acima passam a ser as que a máquina de salto enxerga",
            )
            .only_when("fonte", Fonte::BANCOS),
            ParamSpec::text(
                "ssh_host",
                "Máquina de salto",
                "",
                "IP, nome, «usuario@host», ou um apelido do ~/.ssh/config — e aí vale tudo que estiver escrito lá, ProxyJump inclusive",
            )
            .suggesting(sshfwd::host_suggestions())
            .only_when("fonte", Fonte::BANCOS)
            .only_when("ssh", SIM),
            ParamSpec::text(
                "ssh_usuario",
                "Usuário SSH",
                "",
                "Vazio deixa o ssh decidir: o que o ~/.ssh/config disser para essa máquina, ou o seu login",
            )
            .suggesting(sshfwd::user_suggestions())
            .only_when("fonte", Fonte::BANCOS)
            .only_when("ssh", SIM),
            ParamSpec::text(
                "ssh_porta",
                "Porta do SSH",
                "",
                "Vazio é 22, ou o que o ~/.ssh/config disser",
            )
            .only_when("fonte", Fonte::BANCOS)
            .only_when("ssh", SIM),
            ParamSpec::text(
                "ssh_chave",
                "Chave privada",
                "",
                "Vazio usa o agente e as chaves padrão do ~/.ssh, que é o caminho normal. Só chave: o túnel nunca pergunta nada, então um salto que exige senha não serve aqui",
            )
            .suggesting(sshfwd::key_suggestions())
            .only_when("fonte", Fonte::BANCOS)
            .only_when("ssh", SIM),
            ParamSpec::text(
                "arquivo",
                "Arquivo",
                "",
                "O caminho do banco SQLite. Aberto só para leitura",
            )
            .only_when("fonte", Fonte::SQLITE),
            ParamSpec::text(
                "consulta",
                "Consulta",
                "",
                "SQL, no Postgres, MySQL e SQLite — o AS do SQL é o nome da coluna. No MongoDB, um comando em JSON: {\"find\": \"pedidos\", \"filter\": {\"status\": \"preso\"}, \"limit\": 50} ou {\"aggregate\": \"pedidos\", \"pipeline\": [...]}. Só leitura: um UPDATE falha",
            )
            .only_when("fonte", Fonte::COM_CONSULTA),
            ParamSpec::text(
                "url",
                "URL",
                "",
                "http:// ou https://, com a query string se precisar",
            )
            .only_when("fonte", Fonte::HTTP),
            ParamSpec::choice("metodo", "Método", METODOS, "GET pede; POST manda o corpo abaixo")
                .only_when("fonte", Fonte::HTTP),
            ParamSpec::text(
                "corpo",
                "Corpo",
                "",
                "O JSON enviado no POST, como application/json. Vazio manda um POST sem corpo",
            )
            .only_when("fonte", Fonte::HTTP)
            .only_when("metodo", POST),
            ParamSpec::choice(
                "auth",
                "Autenticação",
                AUTENTICACOES,
                "Como a API reconhece você. Vira o cabeçalho Authorization",
            )
            .only_when("fonte", Fonte::HTTP),
            ParamSpec::text(
                "credencial",
                "Credencial",
                "",
                "Bearer: o token. Basic: usuario:senha. Fica gravada junto com a execução",
            )
            .only_when("fonte", Fonte::HTTP)
            .only_when("auth", COM_CREDENCIAL),
            ParamSpec::text(
                "caminho",
                "Caminho",
                "",
                "Onde está o array dentro da resposta, no jeito do jq: .data.items, .results[0].rows. Vazio é a raiz — e, se a raiz é um objeto, o primeiro campo dele que é um array",
            )
            .only_when("fonte", Fonte::HTTP),
            ParamSpec::text(
                "colunas",
                "Colunas",
                "",
                "Opcional, separadas por vírgula, cada uma um caminho dentro do item: .id, .name as nome, .address.city as cidade, .tags[0] as tag. Vazio mostra todas as chaves de primeiro nível",
            )
            .only_when("fonte", Fonte::HTTP),
        ]
    }

    fn summarize(&self, params: &HashMap<&'static str, String>) -> String {
        match Plano::from(params) {
            Ok(plano) => plano.resumo(),
            Err(_) => "configuração inválida".to_string(),
        }
    }

    /// Sempre: uma visão é uma consulta num sistema de alguém, e a única coisa que ela
    /// nunca pode fazer é rodar porque o programa abriu.
    fn on_demand(&self, _params: &HashMap<&'static str, String>) -> bool {
        true
    }

    fn off_note(&self, _params: &HashMap<&'static str, String>) -> Option<String> {
        Some("desligada: não executa, nem quando a tela dela é aberta".to_string())
    }

    fn start(&self, id: u64, params: &HashMap<&'static str, String>) -> Result<Execution, String> {
        let plano = Plano::from(params)?;
        let (execution, recorder) = Execution::new(id, self.name(), plano.resumo());
        recorder.record(
            0,
            EventKind::Note(
                "pronta. Executa quando a tela abrir pela primeira vez, e depois a cada Ctrl+R"
                    .to_string(),
            ),
        );
        let board = Board {
            cards: vec![cartao_inicial(plano.titulo())],
            note: "nada executado ainda".to_string(),
            direto: true,
            rotulos: ROTULOS,
            ..Board::default()
        };
        Ok(execution
            .on_demand()
            .with_board(Arc::new(Mutex::new(board))))
    }

    /// Abrir executa **uma vez na vida da sessão**: na primeira abertura. Nas seguintes a
    /// tela mostra o retrato da última execução, e o Ctrl+R é quem renova.
    fn open(&self, execution: &Execution, params: &HashMap<&'static str, String>) {
        let Some(board) = execution.board() else {
            return;
        };
        let nunca = {
            let quadro = lock_board(board);
            quadro.updated.is_none() && !quadro.working
        };
        if nunca {
            disparar(execution, params);
        }
    }

    /// Ctrl+R, ou 'r' na lista: executa de novo, agora.
    fn rerun(&self, execution: &Execution, params: &HashMap<&'static str, String>) {
        disparar(execution, params);
    }

    fn columns(&self, execution: &Execution) -> (String, String) {
        execution.outcome()
    }
}

/// As execuções consultando agora. Um `Tool` é um objeto só, sem lugar para guardar nada
/// por execução, e isto é o que tem que ser guardado: a garantia de que dois Ctrl+R
/// seguidos não abrem duas conexões para a mesma pergunta.
static EM_CURSO: LazyLock<Mutex<HashSet<u64>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

fn em_curso() -> MutexGuard<'static, HashSet<u64>> {
    EM_CURSO
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Uma execução da consulta, na sua própria thread. Não faz nada se a execução está
/// desligada, se a configuração não presta, ou se já há uma consulta em curso.
fn disparar(execution: &Execution, params: &HashMap<&'static str, String>) {
    let Ok(plano) = Plano::from(params) else {
        return;
    };
    let (Some(board), false) = (execution.board().cloned(), execution.is_off()) else {
        return;
    };
    if !em_curso().insert(execution.id) {
        return;
    }
    let id = execution.id;
    let rec = execution.recorder();
    let finished = execution.finish_flag();
    finished.store(false, Ordering::Relaxed);
    thread::spawn(move || {
        executar(&plano, &rec, &board);
        rec.ran();
        finished.store(true, Ordering::Relaxed);
        em_curso().remove(&id);
    });
}

/// Consulta, e põe o que veio — ou o que deu errado — no cartão.
fn executar(plano: &Plano, rec: &Recorder, board: &Arc<Mutex<Board>>) {
    lock_board(board).working = true;
    rec.record(
        0,
        EventKind::Note(format!("executando: {}", plano.titulo())),
    );
    let inicio = Instant::now();
    let resultado = plano.consultar();
    let levou = inicio.elapsed().as_millis();

    let mut quadro = lock_board(board);
    quadro.working = false;
    quadro.updated = Some(Instant::now());
    match resultado {
        Ok(res) => {
            for aviso in &res.avisos {
                rec.record(0, EventKind::Note(aviso.clone()));
            }
            let n = res.linhas.len();
            let corte = match res.truncado {
                true => format!(" — mostrando as primeiras {LINHAS_MAX}"),
                false => String::new(),
            };
            let nota = format!("{n} linhas{corte} · {levou} ms");
            quadro.put(cartao_de(&res, plano.titulo(), &nota));
            quadro.note = nota.clone();
            rec.report(format!("{n} linhas"), format!("{levou} ms"));
            rec.record(0, EventKind::Note(format!("executada: {nota}")));
        }
        Err(erro) => {
            quadro.put(cartao_erro(plano.titulo(), &erro));
            quadro.note = format!("falhou: {erro}");
            rec.report("falhou", erro.clone());
            rec.record(0, EventKind::Error(erro));
        }
    }
}

/// O cartão antes da primeira execução: diz o que vai acontecer, em vez de ficar em
/// branco.
fn cartao_inicial(titulo: String) -> Card {
    let mut card = Card::new("resultado", titulo.clone());
    let vazio = || Pane::Empty {
        title: titulo.clone(),
        note: "nada executado ainda — executa ao abrir, e a cada Ctrl+R".to_string(),
    };
    card.summary = vazio();
    card.detail = Layout::one(vazio());
    card
}

/// O cartão com a tabela. Cada linha tem o detalhe dela — os pares coluna/valor —, que é
/// o que salva uma tabela larga demais para caber: a seta anda pelas linhas e a de baixo
/// aparece inteira, com os valores que a coluna cortou.
fn cartao_de(res: &Resultado, titulo: String, nota: &str) -> Card {
    let mut card = Card::new("resultado", titulo.clone());
    if res.colunas.is_empty() {
        let vazio = || Pane::Empty {
            title: titulo.clone(),
            note: "a consulta não devolveu colunas".to_string(),
        };
        card.summary = vazio();
        card.detail = Layout::one(vazio());
        return card;
    }
    let tabela = || Pane::Table {
        title: titulo.clone(),
        headers: res.colunas.clone(),
        rows: res
            .linhas
            .iter()
            .map(|linha| Row::new(linha.clone()))
            .collect(),
        selected: None,
        query: String::new(),
        note: Some((nota.to_string(), Tone::Dim)),
    };
    card.summary = tabela();
    card.detail = Layout::one(tabela());
    card.rows_detail = res
        .linhas
        .iter()
        .enumerate()
        .map(|(i, linha)| {
            Layout::one(Pane::Facts {
                title: format!("linha {}", i + 1),
                rows: res
                    .colunas
                    .iter()
                    .zip(linha)
                    .map(|(coluna, valor)| (coluna.clone(), valor.clone(), Tone::Normal))
                    .collect(),
            })
        })
        .collect();
    card.height = (res.linhas.len() as u16).saturating_add(3).min(12);
    card.select(0);
    card
}

fn cartao_erro(titulo: String, erro: &str) -> Card {
    let mut card = Card::new("resultado", titulo.clone());
    let texto = || Pane::Text {
        title: titulo.clone(),
        lines: vec![
            (erro.to_string(), Tone::Ruim),
            (String::new(), Tone::Normal),
            (
                "Ctrl+R tenta de novo · 'e' na lista corrige a configuração".to_string(),
                Tone::Dim,
            ),
        ],
        scroll: 0,
    };
    card.summary = texto();
    card.detail = Layout::one(texto());
    card.tone = Tone::Ruim;
    card
}

// --- o plano -------------------------------------------------------------------------

/// Tudo o que o formulário quer dizer, já validado. O que pode falhar previsivelmente —
/// uma porta que não é número, um JSON que não fecha, um caminho que não se lê — falha
/// aqui, com o formulário ainda na tela.
#[derive(Clone, Debug)]
pub struct Plano {
    fonte: Fonte,
    host: String,
    porta: u16,
    usuario: String,
    senha: String,
    base: String,
    tls: Tls,
    salto: Option<Salto>,
    arquivo: String,
    consulta: String,
    url: String,
    metodo: String,
    corpo: String,
    auth: Auth,
    credencial: String,
    caminho_texto: String,
    caminho: Vec<Passo>,
    colunas: Vec<Coluna>,
}

impl Plano {
    fn from(params: &HashMap<&'static str, String>) -> Result<Self, String> {
        let get = |key: &str| {
            params
                .get(key)
                .map(String::as_str)
                .unwrap_or("")
                .trim()
                .to_string()
        };
        let fonte = Fonte::from(&get("fonte"));
        let mut plano = Plano {
            fonte,
            host: String::new(),
            porta: 0,
            usuario: String::new(),
            senha: String::new(),
            base: String::new(),
            tls: Tls::Nao,
            salto: None,
            arquivo: String::new(),
            consulta: get("consulta"),
            url: String::new(),
            metodo: "GET".to_string(),
            corpo: String::new(),
            auth: Auth::Nenhuma,
            credencial: String::new(),
            caminho_texto: String::new(),
            caminho: Vec::new(),
            colunas: Vec::new(),
        };
        match fonte {
            Fonte::Postgres | Fonte::Mysql | Fonte::Mongo => {
                plano.host = get("host");
                if plano.host.is_empty() {
                    return Err("informe a máquina do banco".to_string());
                }
                plano.porta = match get("porta").as_str() {
                    "" => fonte.porta_padrao(),
                    texto => match texto.parse::<u16>() {
                        Ok(0) | Err(_) => return Err(format!("porta inválida: «{texto}»")),
                        Ok(porta) => porta,
                    },
                };
                plano.usuario = get("usuario");
                plano.senha = get("senha");
                plano.base = get("base");
                plano.tls = Tls::from(&get("tls"));
                if get("ssh") == "sim" {
                    plano.salto = Some(Salto::from(&get)?);
                }
                if plano.consulta.is_empty() {
                    return Err("informe a consulta".to_string());
                }
                if fonte == Fonte::Mongo {
                    comando_mongo(&plano.consulta)?;
                }
            }
            Fonte::Sqlite => {
                let arquivo = get("arquivo");
                if arquivo.is_empty() {
                    return Err("informe o arquivo do banco".to_string());
                }
                let caminho = sshfwd::expand(&arquivo);
                if !caminho.is_file() {
                    return Err(format!("não há arquivo em {}", caminho.display()));
                }
                plano.arquivo = caminho.display().to_string();
                if plano.consulta.is_empty() {
                    return Err("informe a consulta".to_string());
                }
            }
            Fonte::Http => {
                plano.url = get("url");
                if !(plano.url.starts_with("http://") || plano.url.starts_with("https://")) {
                    return Err("a URL precisa começar com http:// ou https://".to_string());
                }
                plano.metodo = match get("metodo").as_str() {
                    "POST" => "POST".to_string(),
                    _ => "GET".to_string(),
                };
                if plano.metodo == "POST" {
                    plano.corpo = get("corpo");
                    if !plano.corpo.is_empty()
                        && let Err(e) = serde_json::from_str::<Json>(&plano.corpo)
                    {
                        return Err(format!("o corpo não é um JSON válido: {e}"));
                    }
                }
                plano.auth = Auth::from(&get("auth"));
                plano.credencial = get("credencial");
                match plano.auth {
                    Auth::Nenhuma => {}
                    Auth::Bearer if plano.credencial.is_empty() => {
                        return Err("informe o token do Bearer".to_string());
                    }
                    Auth::Basic if !plano.credencial.contains(':') => {
                        return Err("a credencial Basic é usuario:senha".to_string());
                    }
                    _ => {}
                }
                plano.caminho_texto = get("caminho");
                plano.caminho = parse_caminho(&plano.caminho_texto)?;
                plano.colunas = parse_colunas(&get("colunas"))?;
            }
        }
        Ok(plano)
    }

    /// A linha na lista: fonte, quem e onde — nunca a senha —, e o começo da pergunta.
    fn resumo(&self) -> String {
        match self.fonte {
            Fonte::Postgres | Fonte::Mysql | Fonte::Mongo => {
                let quem = match self.usuario.is_empty() {
                    true => String::new(),
                    false => format!("{}@", self.usuario),
                };
                let base = match self.base.is_empty() {
                    true => String::new(),
                    false => format!("/{}", self.base),
                };
                let salto = match &self.salto {
                    Some(salto) => format!(" via ssh {}", salto.resumo()),
                    None => String::new(),
                };
                format!(
                    "{} {quem}{}:{}{base}{salto} · {}",
                    self.fonte.nome(),
                    self.host,
                    self.porta,
                    clip(&self.consulta, 48)
                )
            }
            Fonte::Sqlite => format!("sqlite {} · {}", self.arquivo, clip(&self.consulta, 48)),
            Fonte::Http => format!(
                "{} {} · {}",
                self.metodo,
                self.url,
                match self.caminho_texto.is_empty() {
                    true => ".",
                    false => self.caminho_texto.as_str(),
                }
            ),
        }
    }

    /// O título do cartão: a pergunta.
    fn titulo(&self) -> String {
        match self.fonte {
            Fonte::Http => clip(&format!("{} {}", self.metodo, self.url), 90),
            _ => clip(&self.consulta, 90),
        }
    }

    /// Onde o banco é alcançado: direto, ou pela ponta de cá de um túnel que fica de pé
    /// enquanto o valor devolvido viver.
    fn alcance(&self) -> Result<(String, u16, Option<Tunel>), String> {
        match &self.salto {
            Some(salto) => {
                let tunel = Tunel::abrir(salto, &self.host, self.porta)?;
                let porta = tunel.porta_local;
                Ok(("127.0.0.1".to_string(), porta, Some(tunel)))
            }
            None => Ok((self.host.clone(), self.porta, None)),
        }
    }

    fn consultar(&self) -> Result<Resultado, String> {
        match self.fonte {
            Fonte::Postgres => {
                let (host, porta, _tunel) = self.alcance()?;
                self.consultar_pg(host, porta)
            }
            Fonte::Mysql => {
                let (host, porta, tunel) = self.alcance()?;
                self.consultar_mysql(host, porta, tunel.is_some())
            }
            Fonte::Mongo => {
                let (host, porta, _tunel) = self.alcance()?;
                self.consultar_mongo(host, porta)
            }
            Fonte::Sqlite => self.consultar_sqlite(),
            Fonte::Http => self.consultar_http(),
        }
    }

    fn consultar_pg(&self, host: String, porta: u16) -> Result<Resultado, String> {
        let user = match self.usuario.is_empty() {
            true => "postgres".to_string(),
            false => self.usuario.clone(),
        };
        let target = pg::Target {
            host,
            port: porta,
            database: match self.base.is_empty() {
                true => user.clone(),
                false => self.base.clone(),
            },
            user,
            password: self.senha.clone(),
            ssl: sherlock::Ssl::Prefer,
            timeout: TEMPO,
        };
        let mut conn = pg::Conn::open(&target)?;
        let avisos = conn.harden(TEMPO);
        // A transação somente leitura em volta da query, e não só a característica da
        // sessão: um pooler em modo transação esquece o `SET SESSION` entre uma ida e
        // outra, e o `BEGIN READ ONLY` vai na mesma mensagem que a query.
        let sql = format!(
            "BEGIN READ ONLY; {}; COMMIT",
            self.consulta.trim().trim_end_matches(';')
        );
        let tabela = conn.query(&sql).map_err(|e| e.to_string())?;
        let mut linhas: Vec<Vec<String>> = tabela
            .rows
            .into_iter()
            .map(|linha| linha.into_iter().map(Option::unwrap_or_default).collect())
            .collect();
        let truncado = linhas.len() > LINHAS_MAX;
        linhas.truncate(LINHAS_MAX);
        Ok(Resultado {
            colunas: tabela.columns,
            linhas,
            truncado,
            avisos,
        })
    }

    fn consultar_mysql(
        &self,
        host: String,
        porta: u16,
        por_tunel: bool,
    ) -> Result<Resultado, String> {
        use mysql::prelude::Queryable;

        let mut opts = mysql::OptsBuilder::new()
            .ip_or_hostname(Some(host))
            .tcp_port(porta)
            .prefer_socket(false)
            .tcp_connect_timeout(Some(Duration::from_secs(10)))
            .read_timeout(Some(TEMPO * 2))
            .write_timeout(Some(Duration::from_secs(10)));
        if !self.usuario.is_empty() {
            opts = opts.user(Some(self.usuario.clone()));
        }
        if !self.senha.is_empty() {
            opts = opts.pass(Some(self.senha.clone()));
        }
        if !self.base.is_empty() {
            opts = opts.db_name(Some(self.base.clone()));
        }
        if self.tls != Tls::Nao {
            let mut ssl = mysql::SslOpts::default();
            // Pelo túnel o servidor se apresenta como 127.0.0.1, e nenhum certificado
            // diz isso. O certificado continua sendo verificado — só o nome não.
            if por_tunel {
                ssl = ssl.with_danger_skip_domain_validation(true);
            }
            if self.tls == Tls::SemValidar {
                ssl = ssl.with_danger_accept_invalid_certs(true);
            }
            opts = opts.ssl_opts(ssl);
        }
        let mut conn = mysql::Conn::new(opts).map_err(|e| format!("não conectou: {e}"))?;
        conn.query_drop("SET SESSION TRANSACTION READ ONLY")
            .map_err(|e| format!("o servidor não aceitou uma sessão somente leitura: {e}"))?;
        let mut avisos = Vec::new();
        if let Err(e) = conn.query_drop(format!(
            "SET SESSION max_execution_time = {}",
            TEMPO.as_millis()
        )) {
            avisos.push(format!("sem teto de tempo no servidor: {e}"));
        }
        let mut resultado = conn
            .query_iter(self.consulta.as_str())
            .map_err(|e| e.to_string())?;
        let colunas: Vec<String> = resultado
            .columns()
            .as_ref()
            .iter()
            .map(|coluna| coluna.name_str().into_owned())
            .collect();
        let mut linhas = Vec::new();
        let mut truncado = false;
        for linha in resultado.by_ref() {
            let linha = linha.map_err(|e| e.to_string())?;
            if linhas.len() == LINHAS_MAX {
                truncado = true;
                break;
            }
            linhas.push(
                (0..linha.len())
                    .map(|i| linha.as_ref(i).map(texto_mysql).unwrap_or_default())
                    .collect(),
            );
        }
        drop(resultado);
        Ok(Resultado {
            colunas,
            linhas,
            truncado,
            avisos,
        })
    }

    fn consultar_mongo(&self, host: String, porta: u16) -> Result<Resultado, String> {
        let (nome, comando) = comando_mongo(&self.consulta)?;
        let base = match self.base.is_empty() {
            true => "admin".to_string(),
            false => self.base.clone(),
        };
        let target = mongo::Target {
            hosts: vec![(host, porta)],
            user: self.usuario.clone(),
            password: self.senha.clone(),
            // Onde a credencial mora: `admin` para um usuário criado lá, que é o caso
            // comum. Sem usuário, não há credencial para procurar.
            auth_source: match self.usuario.is_empty() {
                true => base.clone(),
                false => "admin".to_string(),
            },
            database: base.clone(),
            tls: self.tls != Tls::Nao,
            srv: false,
            timeout: TEMPO,
        };
        let mut conn = mongo::Conn::open(&target)?;
        if COM_CURSOR.contains(&nome.as_str()) {
            let docs = conn.cursor(&base, comando).map_err(|e| e.to_string())?;
            Ok(tabela_de_docs(&docs))
        } else {
            let resposta = conn.run(&base, comando).map_err(|e| e.to_string())?;
            Ok(tabela_campo_valor(&resposta))
        }
    }

    fn consultar_sqlite(&self) -> Result<Resultado, String> {
        use rusqlite::types::ValueRef;
        use rusqlite::{Connection, OpenFlags};

        let conn = Connection::open_with_flags(
            &self.arquivo,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| format!("não abriu {}: {e}", self.arquivo))?;
        let _ = conn.busy_timeout(Duration::from_secs(5));
        conn.execute_batch("PRAGMA query_only = 1")
            .map_err(|e| e.to_string())?;
        let mut stmt = conn.prepare(&self.consulta).map_err(|e| e.to_string())?;
        let colunas: Vec<String> = stmt.column_names().iter().map(|c| c.to_string()).collect();
        let largura = colunas.len();
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        let mut linhas = Vec::new();
        let mut truncado = false;
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            if linhas.len() == LINHAS_MAX {
                truncado = true;
                break;
            }
            linhas.push(
                (0..largura)
                    .map(|i| match row.get_ref(i) {
                        Ok(ValueRef::Integer(n)) => n.to_string(),
                        Ok(ValueRef::Real(f)) => f.to_string(),
                        Ok(ValueRef::Text(t)) => clip(&String::from_utf8_lossy(t), CELULA_MAX),
                        Ok(ValueRef::Blob(b)) => format!("{} bytes", b.len()),
                        Ok(ValueRef::Null) | Err(_) => String::new(),
                    })
                    .collect(),
            );
        }
        Ok(Resultado {
            colunas,
            linhas,
            truncado,
            avisos: Vec::new(),
        })
    }

    fn consultar_http(&self) -> Result<Resultado, String> {
        use crate::invest::feed;

        let mut pedido = feed::Pedido::get();
        pedido.metodo = self.metodo.clone();
        match self.auth {
            Auth::Nenhuma => {}
            Auth::Bearer => pedido.cabecalhos.push((
                "Authorization".to_string(),
                format!("Bearer {}", self.credencial),
            )),
            Auth::Basic => pedido.cabecalhos.push((
                "Authorization".to_string(),
                format!(
                    "Basic {}",
                    sherlock::scram::b64_encode(self.credencial.as_bytes())
                ),
            )),
        }
        if self.metodo == "POST" && !self.corpo.is_empty() {
            pedido.corpo = Some((
                "application/json".to_string(),
                self.corpo.as_bytes().to_vec(),
            ));
        }
        let resposta = feed::pedir(&self.url, &pedido).map_err(|e| e.frase())?;
        let json: Json = serde_json::from_slice(&resposta.body)
            .map_err(|e| format!("a resposta não é JSON: {e}"))?;
        tabela_de_json(&json, &self.caminho_texto, &self.caminho, &self.colunas)
    }
}

/// O que uma consulta devolveu, já em texto: cabeçalhos e linhas, se o teto cortou, e o
/// que vale dizer no log sem ser erro.
#[derive(Debug)]
pub struct Resultado {
    pub colunas: Vec<String>,
    pub linhas: Vec<Vec<String>>,
    pub truncado: bool,
    pub avisos: Vec<String>,
}

/// Até `n` caracteres, com a reticência avisando que havia mais.
fn clip(texto: &str, n: usize) -> String {
    let texto: String = texto.split_whitespace().collect::<Vec<_>>().join(" ");
    match texto.chars().count() > n {
        true => format!(
            "{}…",
            texto.chars().take(n.saturating_sub(1)).collect::<String>()
        ),
        false => texto,
    }
}

// --- o túnel SSH ---------------------------------------------------------------------

/// A máquina de salto: por onde o banco é alcançado quando não é alcançável daqui.
#[derive(Clone, Debug)]
struct Salto {
    ssh: PathBuf,
    host: String,
    usuario: String,
    porta: String,
    chave: Option<PathBuf>,
}

impl Salto {
    fn from(get: &dyn Fn(&str) -> String) -> Result<Self, String> {
        let mut host = get("ssh_host");
        let mut usuario = get("ssh_usuario");
        if host.is_empty() {
            return Err("informe a máquina de salto".to_string());
        }
        // «usuario@host» no campo da máquina é como todo mundo escreve, e vale aqui.
        if let Some((quem, onde)) = host.split_once('@')
            && usuario.is_empty()
        {
            usuario = quem.to_string();
            host = onde.to_string();
        }
        let porta = get("ssh_porta");
        if !porta.is_empty() && !porta.parse::<u16>().is_ok_and(|p| p > 0) {
            return Err(format!("porta do SSH inválida: «{porta}»"));
        }
        let chave = match get("ssh_chave").as_str() {
            "" => None,
            caminho => {
                let caminho = sshfwd::expand(caminho);
                if !caminho.is_file() {
                    return Err(format!("não há chave em {}", caminho.display()));
                }
                Some(caminho)
            }
        };
        let ssh = sshfwd::ssh_binary().ok_or("não há ssh nesta máquina")?;
        Ok(Self {
            ssh,
            host,
            usuario,
            porta,
            chave,
        })
    }

    fn resumo(&self) -> String {
        match self.usuario.is_empty() {
            true => self.host.clone(),
            false => format!("{}@{}", self.usuario, self.host),
        }
    }

    /// A linha do `ssh`: um túnel e nada mais, sem pergunta, que falha alto quando o
    /// encaminhamento não sobe.
    fn argv(&self, porta_local: u16, destino: &str, porta: u16) -> Vec<String> {
        let mut args: Vec<String> = vec![
            "-N".to_string(),
            "-o".to_string(),
            "BatchMode=yes".to_string(),
            "-o".to_string(),
            "ExitOnForwardFailure=yes".to_string(),
            "-o".to_string(),
            "ConnectTimeout=10".to_string(),
            "-o".to_string(),
            "StrictHostKeyChecking=accept-new".to_string(),
        ];
        if let Some(chave) = &self.chave {
            args.push("-i".to_string());
            args.push(chave.display().to_string());
            args.push("-o".to_string());
            args.push("IdentitiesOnly=yes".to_string());
        }
        if !self.porta.is_empty() {
            args.push("-p".to_string());
            args.push(self.porta.clone());
        }
        args.push("-L".to_string());
        args.push(format!("127.0.0.1:{porta_local}:{destino}:{porta}"));
        if !self.usuario.is_empty() {
            args.push("-l".to_string());
            args.push(self.usuario.clone());
        }
        args.push(self.host.clone());
        args
    }
}

/// Um `ssh -N -L` de pé. Cai junto com o valor — e, se o programa morrer antes, o kernel
/// derruba o `ssh` por conta própria (ver `sshfwd::morrer_com_o_pai`).
struct Tunel {
    filho: Child,
    porta_local: u16,
}

impl Tunel {
    fn abrir(salto: &Salto, destino: &str, porta: u16) -> Result<Self, String> {
        let porta_local = porta_livre()?;
        let mut comando = Command::new(&salto.ssh);
        comando
            .args(salto.argv(porta_local, destino, porta))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        sshfwd::morrer_com_o_pai(&mut comando);
        let mut filho = comando
            .spawn()
            .map_err(|e| format!("não consegui executar o ssh: {e}"))?;

        // O `ssh` só abre a porta local depois de autenticado: a porta aceitar conexão é
        // a prova de que o túnel existe. Até lá, ou ele ainda está negociando, ou já
        // morreu — e aí o que ele disse no stderr é a resposta.
        let inicio = Instant::now();
        loop {
            if let Ok(Some(status)) = filho.try_wait() {
                let mut erro = String::new();
                if let Some(mut stderr) = filho.stderr.take() {
                    let _ = stderr.read_to_string(&mut erro);
                }
                let motivo = erro
                    .lines()
                    .map(str::trim)
                    .rfind(|l| !l.is_empty())
                    .unwrap_or("sem explicação")
                    .to_string();
                return Err(format!("o túnel SSH não subiu ({status}): {motivo}"));
            }
            let endereco = std::net::SocketAddr::from(([127, 0, 0, 1], porta_local));
            if TcpStream::connect_timeout(&endereco, Duration::from_millis(300)).is_ok() {
                return Ok(Self { filho, porta_local });
            }
            if inicio.elapsed() > TUNEL_ESPERA {
                sshfwd::terminate(&mut filho);
                return Err(format!(
                    "o túnel SSH não subiu em {} s",
                    TUNEL_ESPERA.as_secs()
                ));
            }
            thread::sleep(Duration::from_millis(150));
        }
    }
}

impl Drop for Tunel {
    fn drop(&mut self) {
        sshfwd::terminate(&mut self.filho);
    }
}

/// Uma porta que ninguém está usando agora, perguntada ao sistema. O `ssh` a toma logo
/// em seguida; a janela entre uma coisa e outra existe e é pequena o bastante.
fn porta_livre() -> Result<u16, String> {
    let ouvinte = TcpListener::bind("127.0.0.1:0")
        .map_err(|e| format!("não consegui reservar uma porta local: {e}"))?;
    ouvinte
        .local_addr()
        .map(|a| a.port())
        .map_err(|e| e.to_string())
}

// --- MongoDB -------------------------------------------------------------------------

/// Os comandos que uma visão pode mandar: os que só leem. Qualquer outro é recusado no
/// formulário, antes de existir uma conexão para mandá-lo.
const LEITORES: &[&str] = &[
    "find",
    "aggregate",
    "count",
    "distinct",
    "listCollections",
    "listIndexes",
    "listDatabases",
    "collStats",
    "dbStats",
    "dataSize",
    "serverStatus",
    "hostInfo",
    "buildInfo",
    "connectionStatus",
    "currentOp",
    "getParameter",
    "getCmdLineOpts",
    "getLog",
    "top",
    "ping",
    "hello",
    "isMaster",
    "replSetGetStatus",
    "usersInfo",
    "rolesInfo",
];

/// Os que respondem com cursor, e por isso viram uma tabela de documentos.
const COM_CURSOR: &[&str] = &["find", "aggregate", "listCollections", "listIndexes"];

/// Os campos de toda resposta que não dizem nada sobre a pergunta.
const RUIDO: &[&str] = &["ok", "$clusterTime", "operationTime"];

/// A consulta como documento de comando: o nome (a primeira chave) e o documento inteiro.
/// Um `aggregate` sai daqui com o `cursor` que o servidor exige e sem a chance de
/// escrever em lugar nenhum.
fn comando_mongo(texto: &str) -> Result<(String, bson::Doc), String> {
    let json: Json = serde_json::from_str(texto)
        .map_err(|e| format!("a consulta do MongoDB não é um JSON válido: {e}"))?;
    let Json::Object(mapa) = &json else {
        return Err(
            "a consulta do MongoDB é um objeto JSON, como {\"find\": \"colecao\"}".to_string(),
        );
    };
    let Some(nome) = mapa.keys().next().cloned() else {
        return Err("a consulta está vazia".to_string());
    };
    if !LEITORES.contains(&nome.as_str()) {
        return Err(format!(
            "«{nome}» não é um comando de leitura. Os aceitos: {}",
            LEITORES.join(", ")
        ));
    }
    let mut doc = doc_de_json(mapa);
    if nome == "aggregate" {
        if let Some(Json::Array(estagios)) = mapa.get("pipeline") {
            for estagio in estagios {
                if let Json::Object(m) = estagio
                    && m.keys().any(|k| k == "$out" || k == "$merge")
                {
                    return Err(
                        "um aggregate com $out ou $merge escreve, e uma visão não escreve"
                            .to_string(),
                    );
                }
            }
        }
        if !mapa.contains_key("cursor") {
            doc.0
                .push(("cursor".to_string(), bson::Value::Doc(bson::Doc::new())));
        }
    }
    Ok((nome, doc))
}

fn doc_de_json(mapa: &serde_json::Map<String, Json>) -> bson::Doc {
    bson::Doc(
        mapa.iter()
            .map(|(chave, valor)| (chave.clone(), bson_de_json(valor)))
            .collect(),
    )
}

/// JSON em BSON, com as duas convenções que fazem diferença num filtro: `{"$oid": "…"}`
/// vira ObjectId e `{"$date": ms}` vira data — sem isso não há como perguntar por um
/// `_id` ou por «desde ontem».
fn bson_de_json(valor: &Json) -> bson::Value {
    use bson::Value;
    match valor {
        Json::Null => Value::Null,
        Json::Bool(b) => Value::Bool(*b),
        Json::Number(n) => match n.as_i64() {
            Some(i) if i32::try_from(i).is_ok() => Value::Int32(i as i32),
            Some(i) => Value::Int64(i),
            None => Value::Double(n.as_f64().unwrap_or(0.0)),
        },
        Json::String(s) => Value::Text(s.clone()),
        Json::Array(itens) => Value::List(itens.iter().map(bson_de_json).collect()),
        Json::Object(mapa) => {
            if mapa.len() == 1 {
                if let Some(Json::String(hex)) = mapa.get("$oid")
                    && let Some(id) = object_id_de(hex)
                {
                    return Value::ObjectId(id);
                }
                if let Some(Json::Number(ms)) = mapa.get("$date")
                    && let Some(ms) = ms.as_i64()
                {
                    return Value::Time(ms);
                }
            }
            Value::Doc(doc_de_json(mapa))
        }
    }
}

fn object_id_de(hex: &str) -> Option<[u8; 12]> {
    if hex.len() != 24 {
        return None;
    }
    let mut id = [0u8; 12];
    for (i, byte) in id.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(id)
}

/// Documentos em tabela: uma coluna por chave, na ordem em que cada chave apareceu pela
/// primeira vez, e uma linha por documento.
fn tabela_de_docs(docs: &[bson::Doc]) -> Resultado {
    let mut colunas: Vec<String> = Vec::new();
    for doc in docs.iter().take(LINHAS_MAX) {
        for chave in doc.keys() {
            if !colunas.iter().any(|c| c == chave) {
                colunas.push(chave.to_string());
            }
        }
    }
    let linhas = docs
        .iter()
        .take(LINHAS_MAX)
        .map(|doc| {
            colunas
                .iter()
                .map(|coluna| doc.get(coluna).map(texto_bson).unwrap_or_default())
                .collect()
        })
        .collect();
    Resultado {
        colunas,
        linhas,
        truncado: docs.len() > LINHAS_MAX,
        avisos: Vec::new(),
    }
}

/// A resposta de um comando que não devolve cursor — um `serverStatus`, um `dbStats` —,
/// campo a campo. Dois níveis: o de cima abre, para que `mem.resident` seja uma linha e
/// não um JSON inteiro numa célula.
fn tabela_campo_valor(resposta: &bson::Doc) -> Resultado {
    let mut linhas = Vec::new();
    for (chave, valor) in &resposta.0 {
        if RUIDO.contains(&chave.as_str()) {
            continue;
        }
        match valor {
            bson::Value::Doc(dentro) if !dentro.0.is_empty() => {
                for (sub, valor) in &dentro.0 {
                    linhas.push(vec![format!("{chave}.{sub}"), texto_bson(valor)]);
                }
            }
            _ => linhas.push(vec![chave.clone(), texto_bson(valor)]),
        }
    }
    Resultado {
        colunas: vec!["campo".to_string(), "valor".to_string()],
        linhas,
        truncado: false,
        avisos: Vec::new(),
    }
}

/// Um valor BSON como célula: o texto como está, a data por extenso, e um documento ou
/// uma lista como JSON compacto — o conteúdo, e não a forma, que é o que o Sherlock
/// mostra e aqui seria esconder justamente o que se veio ver.
fn texto_bson(valor: &bson::Value) -> String {
    use bson::Value;
    let texto = match valor {
        Value::Text(t) => t.clone(),
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Time(ms) => data_hora(*ms),
        Value::Doc(_) | Value::List(_) => json_de_bson(valor),
        outro => outro.render(),
    };
    clip(&texto, CELULA_MAX)
}

fn json_de_bson(valor: &bson::Value) -> String {
    use bson::Value;
    match valor {
        Value::Text(t) => serde_json::to_string(t).unwrap_or_default(),
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Time(ms) => format!("\"{}\"", data_hora(*ms)),
        Value::ObjectId(_) => format!("\"{}\"", valor.render()),
        Value::Doc(doc) => format!(
            "{{{}}}",
            doc.0
                .iter()
                .map(|(chave, valor)| format!("{chave}: {}", json_de_bson(valor)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::List(itens) => format!(
            "[{}]",
            itens
                .iter()
                .map(json_de_bson)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        outro => outro.render(),
    }
}

/// Milissegundos desde a época como `AAAA-MM-DD hh:mm:ss`, em UTC.
fn data_hora(ms: i64) -> String {
    let segundos = ms.div_euclid(1000);
    let dias = segundos.div_euclid(86_400);
    let resto = segundos.rem_euclid(86_400);
    let (ano, mes, dia) = super::x509::civil_from_days(dias);
    format!(
        "{ano:04}-{mes:02}-{dia:02} {:02}:{:02}:{:02}",
        resto / 3600,
        resto % 3600 / 60,
        resto % 60
    )
}

// --- MySQL ---------------------------------------------------------------------------

/// Um valor do MySQL como célula. Datas e horas chegam em partes; aqui viram o texto que
/// o próprio `mysql` mostraria.
fn texto_mysql(valor: &mysql::Value) -> String {
    use mysql::Value;
    let texto = match valor {
        Value::NULL => String::new(),
        Value::Bytes(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        Value::Int(n) => n.to_string(),
        Value::UInt(n) => n.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Double(f) => f.to_string(),
        Value::Date(ano, mes, dia, hora, min, seg, micro) => {
            if *hora == 0 && *min == 0 && *seg == 0 && *micro == 0 {
                format!("{ano:04}-{mes:02}-{dia:02}")
            } else if *micro == 0 {
                format!("{ano:04}-{mes:02}-{dia:02} {hora:02}:{min:02}:{seg:02}")
            } else {
                format!("{ano:04}-{mes:02}-{dia:02} {hora:02}:{min:02}:{seg:02}.{micro:06}")
            }
        }
        Value::Time(negativo, dias, horas, min, seg, micro) => {
            let sinal = if *negativo { "-" } else { "" };
            let horas = u64::from(*dias) * 24 + u64::from(*horas);
            if *micro == 0 {
                format!("{sinal}{horas:02}:{min:02}:{seg:02}")
            } else {
                format!("{sinal}{horas:02}:{min:02}:{seg:02}.{micro:06}")
            }
        }
    };
    clip(&texto, CELULA_MAX)
}

// --- HTTP: o caminho e as colunas ----------------------------------------------------

/// Um passo de um caminho: entrar numa chave, ou pegar a n-ésima posição.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Passo {
    Chave(String),
    Indice(usize),
}

/// Uma coluna da tabela: o nome no cabeçalho e o caminho dentro de cada item.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Coluna {
    nome: String,
    passos: Vec<Passo>,
}

/// `.a.b[0]."com espaço"` em passos. O pedaço do `jq` que isto fala: `.` é a raiz,
/// `.chave` entra, `[n]` indexa, e uma chave com qualquer caractere vai entre aspas.
/// Sem o ponto inicial também serve — `data.items` é o que se escreve com pressa.
fn parse_caminho(texto: &str) -> Result<Vec<Passo>, String> {
    let texto = texto.trim();
    let mut passos = Vec::new();
    if texto.is_empty() || texto == "." {
        return Ok(passos);
    }
    let chars: Vec<char> = texto.chars().collect();
    let mut i = 0;
    // Um caminho que começa direto na chave ganha o ponto que faltou.
    if chars[0] != '.' && chars[0] != '[' {
        passos.push(Passo::Chave(ler_identificador(&chars, &mut i)?));
    }
    while i < chars.len() {
        match chars[i] {
            '.' => {
                i += 1;
                if i >= chars.len() {
                    return Err(format!("o caminho «{texto}» termina num ponto"));
                }
                match chars[i] {
                    '[' => {}
                    '"' => passos.push(Passo::Chave(ler_entre_aspas(&chars, &mut i, texto)?)),
                    _ => passos.push(Passo::Chave(ler_identificador(&chars, &mut i)?)),
                }
            }
            '[' => {
                i += 1;
                let inicio = i;
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
                if i == inicio || i >= chars.len() || chars[i] != ']' {
                    return Err(format!(
                        "em «{texto}»: entre colchetes vai um número, como [0]"
                    ));
                }
                let numero: String = chars[inicio..i].iter().collect();
                passos.push(Passo::Indice(numero.parse().map_err(|_| {
                    format!("em «{texto}»: «{numero}» não é uma posição")
                })?));
                i += 1;
            }
            outro => {
                return Err(format!(
                    "em «{texto}»: não esperava «{outro}» — um caminho é .chave, [n] ou \"chave com espaço\""
                ));
            }
        }
    }
    Ok(passos)
}

fn ler_identificador(chars: &[char], i: &mut usize) -> Result<String, String> {
    let inicio = *i;
    while *i < chars.len() && (chars[*i].is_alphanumeric() || matches!(chars[*i], '_' | '-' | '$'))
    {
        *i += 1;
    }
    if *i == inicio {
        let texto: String = chars.iter().collect();
        return Err(format!(
            "em «{texto}»: faltou o nome da chave depois do ponto"
        ));
    }
    Ok(chars[inicio..*i].iter().collect())
}

fn ler_entre_aspas(chars: &[char], i: &mut usize, texto: &str) -> Result<String, String> {
    *i += 1;
    let inicio = *i;
    while *i < chars.len() && chars[*i] != '"' {
        *i += 1;
    }
    if *i >= chars.len() {
        return Err(format!("em «{texto}»: aspas sem fechar"));
    }
    let chave: String = chars[inicio..*i].iter().collect();
    *i += 1;
    Ok(chave)
}

/// `.id, .name as nome, .address.city as cidade` em colunas. A vírgula separa, exceto
/// dentro de aspas; o `as` dá o nome, e sem ele o nome é o próprio caminho sem o ponto.
fn parse_colunas(texto: &str) -> Result<Vec<Coluna>, String> {
    let mut colunas = Vec::new();
    for pedaco in separar_por_virgula(texto) {
        let pedaco = pedaco.trim();
        if pedaco.is_empty() {
            continue;
        }
        let (caminho, nome) = match pedaco.rsplit_once(" as ") {
            Some((caminho, nome)) if !nome.trim().is_empty() => {
                (caminho.trim(), nome.trim().to_string())
            }
            _ => (pedaco, pedaco.trim_start_matches('.').to_string()),
        };
        let passos = parse_caminho(caminho)?;
        if passos.is_empty() && nome == pedaco.trim_start_matches('.') && !pedaco.contains(" as ") {
            return Err(format!(
                "a coluna «{pedaco}» precisa de um caminho, como .id"
            ));
        }
        colunas.push(Coluna { nome, passos });
    }
    Ok(colunas)
}

fn separar_por_virgula(texto: &str) -> Vec<String> {
    let mut pedacos = Vec::new();
    let mut atual = String::new();
    let mut entre_aspas = false;
    for c in texto.chars() {
        match c {
            '"' => {
                entre_aspas = !entre_aspas;
                atual.push(c);
            }
            ',' if !entre_aspas => pedacos.push(std::mem::take(&mut atual)),
            _ => atual.push(c),
        }
    }
    pedacos.push(atual);
    pedacos
}

fn seguir<'a>(valor: &'a Json, passos: &[Passo]) -> Option<&'a Json> {
    passos.iter().try_fold(valor, |atual, passo| match passo {
        Passo::Chave(chave) => atual.get(chave.as_str()),
        Passo::Indice(i) => atual.get(*i),
    })
}

/// Um valor JSON como célula: texto como está, número como está, `null` vazio, e objeto
/// ou lista como JSON compacto.
fn celula(valor: Option<&Json>) -> String {
    let texto = match valor {
        None | Some(Json::Null) => String::new(),
        Some(Json::String(s)) => s.clone(),
        Some(Json::Bool(b)) => b.to_string(),
        Some(Json::Number(n)) => n.to_string(),
        Some(outro) => serde_json::to_string(outro).unwrap_or_default(),
    };
    clip(&texto, CELULA_MAX)
}

/// A resposta inteira em tabela: o caminho leva ao array, as colunas tiram de cada item
/// o que mostrar.
fn tabela_de_json(
    raiz: &Json,
    caminho_texto: &str,
    caminho: &[Passo],
    colunas: &[Coluna],
) -> Result<Resultado, String> {
    let mut avisos = Vec::new();
    let alvo = if caminho.is_empty() {
        match raiz {
            // Um objeto na raiz quase sempre é um envelope — `{"data": [...]}` — e o
            // primeiro array dentro dele é o que se veio buscar. Dito no log, para que o
            // dia em que a API mudar de envelope não seja um mistério.
            Json::Object(mapa) => match mapa.iter().find(|(_, v)| v.is_array()) {
                Some((chave, dentro)) => {
                    avisos.push(format!(
                        "caminho vazio: usando o primeiro array da resposta, .{chave}"
                    ));
                    dentro
                }
                None => raiz,
            },
            _ => raiz,
        }
    } else {
        seguir(raiz, caminho).ok_or_else(|| {
            format!(
                "o caminho {caminho_texto} não existe na resposta — ela começa com {}",
                clip(&serde_json::to_string(raiz).unwrap_or_default(), 120)
            )
        })?
    };
    let itens: Vec<&Json> = match alvo {
        Json::Array(itens) => itens.iter().collect(),
        // Um objeto no fim do caminho é uma linha só — o que se quer de um `/status`.
        outro => vec![outro],
    };
    let automaticas;
    let colunas = match colunas.is_empty() {
        true => {
            automaticas = colunas_automaticas(&itens);
            automaticas.as_slice()
        }
        false => colunas,
    };
    let linhas = itens
        .iter()
        .take(LINHAS_MAX)
        .map(|item| {
            colunas
                .iter()
                .map(|coluna| celula(seguir(item, &coluna.passos)))
                .collect()
        })
        .collect();
    Ok(Resultado {
        colunas: colunas.iter().map(|c| c.nome.clone()).collect(),
        linhas,
        truncado: itens.len() > LINHAS_MAX,
        avisos,
    })
}

/// Sem colunas pedidas: todas as chaves de primeiro nível, na ordem em que apareceram.
/// Uma lista de escalares — `[1, 2, 3]` — vira uma coluna chamada «valor».
fn colunas_automaticas(itens: &[&Json]) -> Vec<Coluna> {
    let mut nomes: Vec<String> = Vec::new();
    let mut escalar = false;
    for item in itens.iter().take(LINHAS_MAX) {
        match item {
            Json::Object(mapa) => {
                for chave in mapa.keys() {
                    if !nomes.contains(chave) {
                        nomes.push(chave.clone());
                    }
                }
            }
            _ => escalar = true,
        }
    }
    let mut colunas: Vec<Coluna> = nomes
        .into_iter()
        .map(|nome| Coluna {
            passos: vec![Passo::Chave(nome.clone())],
            nome,
        })
        .collect();
    if escalar {
        colunas.insert(
            0,
            Coluna {
                nome: "valor".to_string(),
                passos: Vec::new(),
            },
        );
    }
    colunas
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chave(s: &str) -> Passo {
        Passo::Chave(s.to_string())
    }

    #[test]
    fn o_caminho_le_chaves_indices_e_aspas() {
        assert_eq!(parse_caminho(".").unwrap(), vec![]);
        assert_eq!(parse_caminho("").unwrap(), vec![]);
        assert_eq!(
            parse_caminho(".data.items[0].rows").unwrap(),
            vec![
                chave("data"),
                chave("items"),
                Passo::Indice(0),
                chave("rows")
            ]
        );
        assert_eq!(
            parse_caminho(".[2].\"com espaço\"").unwrap(),
            vec![Passo::Indice(2), chave("com espaço")]
        );
        assert_eq!(
            parse_caminho("data.items").unwrap(),
            vec![chave("data"), chave("items")]
        );
    }

    #[test]
    fn um_caminho_torto_explica_o_que_esperava() {
        assert!(
            parse_caminho(".data.")
                .unwrap_err()
                .contains("termina num ponto")
        );
        assert!(parse_caminho(".a[x]").unwrap_err().contains("número"));
        assert!(parse_caminho(".a b").unwrap_err().contains("não esperava"));
        assert!(parse_caminho(".\"aberta").unwrap_err().contains("aspas"));
    }

    #[test]
    fn as_colunas_ganham_nome_pelo_as_ou_pelo_caminho() {
        let colunas = parse_colunas(".id, .address.city as cidade, .tags[0]").unwrap();
        assert_eq!(colunas.len(), 3);
        assert_eq!(colunas[0].nome, "id");
        assert_eq!(colunas[1].nome, "cidade");
        assert_eq!(colunas[1].passos, vec![chave("address"), chave("city")]);
        assert_eq!(colunas[2].nome, "tags[0]");
        assert!(parse_colunas("").unwrap().is_empty());
    }

    #[test]
    fn a_virgula_dentro_das_aspas_nao_separa() {
        let colunas = parse_colunas(".\"a,b\" as ab, .c").unwrap();
        assert_eq!(colunas.len(), 2);
        assert_eq!(colunas[0].passos, vec![chave("a,b")]);
    }

    #[test]
    fn o_envelope_e_aberto_sozinho_e_as_colunas_saem_na_ordem() {
        let json: Json = serde_json::from_str(
            r#"{"ok": true, "data": {"items": [{"id": 1, "nome": "a", "extra": {"x": 1}}, {"nome": "b", "id": 2, "outro": null}]}}"#,
        )
        .unwrap();
        let res = tabela_de_json(
            &json,
            ".data.items",
            &parse_caminho(".data.items").unwrap(),
            &[],
        )
        .unwrap();
        assert_eq!(res.colunas, vec!["id", "nome", "extra", "outro"]);
        assert_eq!(res.linhas[0], vec!["1", "a", r#"{"x":1}"#, ""]);
        assert_eq!(res.linhas[1], vec!["2", "b", "", ""]);

        // Sem caminho: o primeiro array de primeiro nível — aqui não há, e a raiz vira
        // uma linha só.
        let res = tabela_de_json(&json, "", &[], &[]).unwrap();
        assert_eq!(res.linhas.len(), 1);
        assert_eq!(res.colunas, vec!["ok", "data"]);

        let json: Json =
            serde_json::from_str(r#"{"total": 2, "results": [{"a": 1}, {"a": 2}]}"#).unwrap();
        let res = tabela_de_json(&json, "", &[], &[]).unwrap();
        assert_eq!(res.linhas.len(), 2);
        assert!(res.avisos[0].contains(".results"));
    }

    #[test]
    fn colunas_escolhidas_seguem_o_caminho_dentro_do_item() {
        let json: Json = serde_json::from_str(
            r#"[{"id": 1, "address": {"city": "Recife"}, "tags": ["x", "y"]}, {"id": 2, "address": {}, "tags": []}]"#,
        )
        .unwrap();
        let colunas = parse_colunas(".id, .address.city as cidade, .tags[0] as tag").unwrap();
        let res = tabela_de_json(&json, "", &[], &colunas).unwrap();
        assert_eq!(res.colunas, vec!["id", "cidade", "tag"]);
        assert_eq!(res.linhas[0], vec!["1", "Recife", "x"]);
        assert_eq!(res.linhas[1], vec!["2", "", ""]);
    }

    #[test]
    fn escalares_viram_a_coluna_valor_e_um_caminho_errado_explica() {
        let json: Json = serde_json::from_str("[10, 20]").unwrap();
        let res = tabela_de_json(&json, "", &[], &[]).unwrap();
        assert_eq!(res.colunas, vec!["valor"]);
        assert_eq!(res.linhas, vec![vec!["10"], vec!["20"]]);

        let erro =
            tabela_de_json(&json, ".nada", &parse_caminho(".nada").unwrap(), &[]).unwrap_err();
        assert!(erro.contains(".nada não existe"));
    }

    #[test]
    fn o_comando_do_mongo_e_a_primeira_chave_e_so_leitor_passa() {
        let (nome, doc) = comando_mongo(r#"{"find": "pedidos", "filter": {"a": 1}}"#).unwrap();
        assert_eq!(nome, "find");
        assert_eq!(doc.0[0].0, "find");
        assert!(
            comando_mongo(r#"{"update": "pedidos"}"#)
                .unwrap_err()
                .contains("não é um comando de leitura")
        );
        assert!(comando_mongo("[1]").unwrap_err().contains("objeto JSON"));
        assert!(comando_mongo("{").unwrap_err().contains("JSON válido"));
    }

    #[test]
    fn o_aggregate_ganha_cursor_e_nao_pode_escrever() {
        let (_, doc) =
            comando_mongo(r#"{"aggregate": "p", "pipeline": [{"$match": {}}]}"#).unwrap();
        assert!(doc.keys().any(|k| k == "cursor"));
        let erro = comando_mongo(r#"{"aggregate": "p", "pipeline": [{"$out": "q"}]}"#).unwrap_err();
        assert!(erro.contains("$out"));
    }

    #[test]
    fn oid_e_date_viram_os_tipos_certos() {
        let (_, doc) = comando_mongo(
            r#"{"find": "p", "filter": {"_id": {"$oid": "507f1f77bcf86cd799439011"}, "em": {"$date": 0}}}"#,
        )
        .unwrap();
        let filtro = doc.doc("filter").unwrap();
        assert!(matches!(filtro.get("_id"), Some(bson::Value::ObjectId(_))));
        assert!(matches!(filtro.get("em"), Some(bson::Value::Time(0))));
        assert_eq!(texto_bson(filtro.get("em").unwrap()), "1970-01-01 00:00:00");
    }

    #[test]
    fn a_data_e_a_hora_do_mysql_saem_legiveis() {
        assert_eq!(
            texto_mysql(&mysql::Value::Date(2024, 5, 1, 0, 0, 0, 0)),
            "2024-05-01"
        );
        assert_eq!(
            texto_mysql(&mysql::Value::Date(2024, 5, 1, 13, 4, 5, 0)),
            "2024-05-01 13:04:05"
        );
        assert_eq!(
            texto_mysql(&mysql::Value::Time(true, 1, 2, 3, 4, 0)),
            "-26:03:04"
        );
        assert_eq!(texto_mysql(&mysql::Value::NULL), "");
        assert_eq!(data_hora(1_700_000_000_000), "2023-11-14 22:13:20");
    }

    #[test]
    fn a_resposta_de_um_comando_abre_um_nivel() {
        let resposta = bson::Doc::new().with("ok", 1.0).with("host", "db1").with(
            "mem",
            bson::Doc::new().with("resident", 512).with("virtual", 1024),
        );
        let res = tabela_campo_valor(&resposta);
        assert_eq!(res.colunas, vec!["campo", "valor"]);
        assert_eq!(
            res.linhas,
            vec![
                vec!["host".to_string(), "db1".to_string()],
                vec!["mem.resident".to_string(), "512".to_string()],
                vec!["mem.virtual".to_string(), "1024".to_string()],
            ]
        );
    }

    #[test]
    fn o_plano_recusa_o_que_nao_presta_ainda_no_formulario() {
        let mut params: HashMap<&'static str, String> = HashMap::new();
        params.insert("fonte", "PostgreSQL".into());
        params.insert("host", "".into());
        assert!(Plano::from(&params).unwrap_err().contains("máquina"));
        params.insert("host", "db".into());
        params.insert("porta", "x".into());
        assert!(Plano::from(&params).unwrap_err().contains("porta"));
        params.insert("porta", "".into());
        assert!(Plano::from(&params).unwrap_err().contains("consulta"));
        params.insert("consulta", "select 1".into());
        params.insert("senha", "segredo".into());
        let plano = Plano::from(&params).unwrap();
        assert_eq!(plano.porta, 5432);
        assert!(!plano.resumo().contains("segredo"));

        params.insert("fonte", "HTTP JSON".into());
        params.insert("url", "ftp://x".into());
        assert!(Plano::from(&params).unwrap_err().contains("http://"));
        params.insert("url", "http://x/y".into());
        params.insert("auth", "Basic".into());
        params.insert("credencial", "semdoispontos".into());
        assert!(Plano::from(&params).unwrap_err().contains("usuario:senha"));
    }

    /// Uma fonte de verdade, pela mão: `VISAO_TESTE_PARAMS='{"fonte":"PostgreSQL",...}'`
    /// com os mesmos nomes do formulário. Imprime a tabela; é o teste de cada banco.
    #[test]
    #[ignore]
    fn banco_de_verdade() {
        let texto = std::env::var("VISAO_TESTE_PARAMS").expect("VISAO_TESTE_PARAMS");
        let dado: HashMap<String, String> = serde_json::from_str(&texto).unwrap();
        let mut params: HashMap<&'static str, String> = HashMap::new();
        for spec in VisaoTool.params() {
            if let Some(valor) = dado.get(spec.key) {
                params.insert(spec.key, valor.clone());
            }
        }
        let plano = Plano::from(&params).unwrap();
        println!("resumo: {}", plano.resumo());
        let inicio = Instant::now();
        let res = plano.consultar().unwrap();
        println!(
            "{} linhas em {:?}; truncado={}; avisos={:?}",
            res.linhas.len(),
            inicio.elapsed(),
            res.truncado,
            res.avisos
        );
        println!("{}", res.colunas.join(" | "));
        for linha in res.linhas.iter().take(12) {
            println!("{}", linha.join(" | "));
        }
    }

    // --- contra o servidor de brincadeira: `VISAO_TESTE_URL=http://127.0.0.1:8765` -----

    fn http(params: &[(&'static str, &str)]) -> Result<Resultado, String> {
        let base = std::env::var("VISAO_TESTE_URL").expect("VISAO_TESTE_URL");
        let mut mapa: HashMap<&'static str, String> = HashMap::new();
        mapa.insert("fonte", "HTTP JSON".into());
        for (k, v) in params {
            let v = v.replace("{base}", &base);
            mapa.insert(k, v);
        }
        Plano::from(&mapa)?.consultar()
    }

    #[test]
    #[ignore]
    fn servidor_array_na_raiz_e_envelope() {
        let res = http(&[("url", "{base}/usuarios")]).unwrap();
        assert_eq!(
            res.colunas,
            vec!["id", "name", "email", "address", "tags", "ativo", "score"]
        );
        assert_eq!(res.linhas.len(), 3);
        assert_eq!(res.linhas[2][2], "");

        let res = http(&[
            ("url", "{base}/envelope"),
            ("caminho", ".data.items"),
            ("colunas", ".id, .address.city as cidade, .tags[0] as tag"),
        ])
        .unwrap();
        assert_eq!(res.colunas, vec!["id", "cidade", "tag"]);
        assert_eq!(res.linhas[0], vec!["1", "Recife", "admin"]);

        let res = http(&[("url", "{base}/paginado")]).unwrap();
        assert_eq!(res.linhas.len(), 2);
        assert!(res.avisos[0].contains(".results"));

        let res = http(&[
            ("url", "{base}/profundo"),
            ("caminho", ".payload.groups[0].rows"),
        ])
        .unwrap();
        assert_eq!(res.colunas, vec!["k", "v"]);
    }

    #[test]
    #[ignore]
    fn servidor_escalares_objeto_vazio_e_misto() {
        assert_eq!(
            http(&[("url", "{base}/escalares")]).unwrap().colunas,
            vec!["valor"]
        );
        let res = http(&[("url", "{base}/objeto")]).unwrap();
        assert_eq!(res.linhas.len(), 1);
        assert_eq!(res.colunas, vec!["nome", "versao", "uptime"]);
        assert!(http(&[("url", "{base}/vazio")]).unwrap().linhas.is_empty());
        let res = http(&[("url", "{base}/misto")]).unwrap();
        assert_eq!(res.colunas, vec!["a", "b", "c"]);
        assert_eq!(res.linhas[2], vec!["5", "", r#"{"x":1}"#]);
    }

    #[test]
    #[ignore]
    fn servidor_autenticacao_e_post() {
        assert!(
            http(&[("url", "{base}/bearer")])
                .unwrap_err()
                .contains("401")
        );
        let res = http(&[
            ("url", "{base}/bearer"),
            ("auth", "Bearer"),
            ("credencial", "segredo"),
        ])
        .unwrap();
        assert_eq!(res.linhas.len(), 3);
        let res = http(&[
            ("url", "{base}/basic"),
            ("auth", "Basic"),
            ("credencial", "will:senha"),
        ])
        .unwrap();
        assert_eq!(res.linhas.len(), 3);
        assert!(
            http(&[
                ("url", "{base}/basic"),
                ("auth", "Basic"),
                ("credencial", "will:errada")
            ])
            .unwrap_err()
            .contains("401")
        );

        let res = http(&[
            ("url", "{base}/busca"),
            ("metodo", "POST"),
            ("corpo", r#"{"cidade": "Natal"}"#),
            ("caminho", ".resultados"),
            ("colunas", ".name as nome"),
        ])
        .unwrap();
        assert_eq!(res.linhas, vec![vec!["Bruno"]]);
        let res = http(&[
            ("url", "{base}/busca"),
            ("metodo", "POST"),
            ("caminho", ".resultados"),
        ])
        .unwrap();
        assert_eq!(res.linhas.len(), 3);
    }

    #[test]
    #[ignore]
    fn servidor_texto_redirecionamento_e_caminho_errado() {
        assert!(
            http(&[("url", "{base}/texto")])
                .unwrap_err()
                .contains("não é JSON")
        );
        assert_eq!(
            http(&[("url", "{base}/redireciona")]).unwrap().linhas.len(),
            3
        );
        assert!(
            http(&[("url", "{base}/usuarios"), ("caminho", ".nada")])
                .unwrap_err()
                .contains("não existe")
        );
        assert!(
            http(&[("url", "{base}/sumiu")])
                .unwrap_err()
                .contains("404")
        );
    }
}
