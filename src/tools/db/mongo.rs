//! Speaking to MongoDB on the wire, by hand.
//!
//! One message shape (`OP_MSG`, which is every command since 3.6), one document format
//! (`super::bson`), and one handshake (SCRAM, shared with the Postgres side). Everything
//! the inspection asks for is a command document sent on that envelope and a reply
//! document read back off it — including the ones that pretend to be queries, since
//! `find` and `aggregate` are commands too.
//!
//! Only readers are ever sent. The list is short enough to be checked by eye:
//! `hello`/`isMaster`, `buildInfo`, `serverStatus`, `hostInfo`, `getParameter`,
//! `listDatabases`, `listCollections`, `listIndexes`, `dbStats`, `collStats`,
//! `connectionStatus`, `replSetGetStatus`, `currentOp`, `getLog`, `aggregate` with
//! `$collStats`/`$indexStats`, `find` over `system.profile`, and `getMore`/`killCursors`
//! to finish reading what those return.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use super::bson::{self, Doc, Value};
use super::scram::{Hash, Scram, md5_hex};
use super::url;

/// `OP_MSG`.
const OPCODE: i32 = 2013;
/// The compressed envelope. Never asked for, and named so the error can say what
/// arrived instead of "unexpected opcode 2012".
const OPCODE_COMPRESSED: i32 = 2012;
const MAX_MESSAGE: usize = 64 * 1024 * 1024;
/// Documents to read out of any one cursor. A collection listing on a big cluster can run
/// to thousands, and nothing in the report reads past the first hundreds.
const CURSOR_LIMIT: usize = 2000;
/// O teto de tempo que acompanha todo comando que pode ser caro.
///
/// É o `statement_timeout` do outro lado da ferramenta, e existe pela mesma razão: o
/// tempo limite do soquete faz *este* lado desistir, enquanto o servidor continuaria
/// trabalhando. `maxTimeMS` é o que faz o servidor desistir junto — e é o que transforma
/// «não deve pesar» em «não tem como pesar por muito tempo».
const MAX_TIME_MS: i32 = 15_000;

pub struct Target {
    pub hosts: Vec<(String, u16)>,
    pub user: String,
    pub password: String,
    pub database: String,
    /// Which database holds the user's credentials — usually, but not always, the one
    /// being inspected.
    pub auth_source: String,
    pub tls: bool,
    /// `mongodb+srv://`: the hosts have to be asked for before they can be connected to.
    pub srv: bool,
    pub timeout: Duration,
}

impl Target {
    pub fn parse(text: &str, database: &str) -> Result<Self, String> {
        let srv = text
            .trim()
            .to_ascii_lowercase()
            .starts_with("mongodb+srv://");
        let parts = url::split(text, &["mongodb", "mongodb+srv"])
            .ok_or("a URL precisa começar com mongodb:// (ou mongodb+srv://)")?;
        let hosts: Vec<(String, u16)> = parts
            .hosts
            .iter()
            .map(|(host, port)| (host.clone(), port.unwrap_or(27017)))
            .collect();
        if hosts.is_empty() || hosts[0].0.is_empty() {
            return Err("a URL não diz em qual host conectar".to_string());
        }
        if srv && hosts.len() != 1 {
            return Err("mongodb+srv:// aceita um host só — é ele que guarda a lista".to_string());
        }
        let database = match (database.trim(), parts.path.as_str()) {
            ("", "") => "admin".to_string(),
            ("", path) => path.to_string(),
            (given, _) => given.to_string(),
        };
        let tls = srv
            || ["true", "1"].contains(&parts.option("tls"))
            || ["true", "1"].contains(&parts.option("ssl"));
        let auth_source = match parts.option("authSource") {
            "" => {
                // Where credentials live by default: `admin` for a user created there,
                // which is the usual case, and the database itself when it isn't.
                if parts.user.is_empty() {
                    database.clone()
                } else {
                    "admin".to_string()
                }
            }
            source => source.to_string(),
        };
        Ok(Self {
            hosts,
            user: parts.user.clone(),
            password: parts.password.clone(),
            database,
            auth_source,
            tls,
            srv,
            timeout: Duration::from_secs(15),
        })
    }

    /// The first host, as written. What the report calls the server when the server
    /// doesn't name itself.
    pub fn address(&self) -> String {
        self.hosts
            .first()
            .map(|(host, port)| format!("{host}:{port}"))
            .unwrap_or_default()
    }

    /// Who and where, never the password.
    pub fn summary(&self) -> String {
        let host = self.address();
        let who = if self.user.is_empty() {
            String::new()
        } else {
            format!("{}@", self.user)
        };
        format!("{who}{host}/{}", self.database)
    }

    /// Turns a `mongodb+srv` name into the real host list. The DNS this needs is the
    /// same DNS the investigation tool already speaks.
    fn resolve_srv(&mut self) -> Result<String, String> {
        use crate::tools::dns::wire;
        let (name, _) = self.hosts[0].clone();
        let resolver = *wire::system_resolvers()
            .first()
            .ok_or("sem resolvedor de DNS configurado nesta máquina")?;
        let question = format!("_mongodb._tcp.{name}");
        let answer = wire::query(resolver, &question, wire::SRV, self.timeout)
            .map_err(|e| format!("não consegui resolver {question}: {e}"))?;
        let mut hosts = Vec::new();
        for record in answer.of_type(wire::SRV) {
            if let wire::Rdata::Srv { port, target, .. } = &record.data {
                hosts.push((target.trim_end_matches('.').to_string(), *port));
            }
        }
        if hosts.is_empty() {
            return Err(format!("{question} não devolveu nenhum servidor"));
        }
        self.hosts = hosts;
        // The TXT record beside it carries the options the URL didn't: replica set name,
        // authSource. Only authSource changes whether the login works.
        if let Ok(answer) = wire::query(resolver, &name, wire::TXT, self.timeout) {
            for record in answer.of_type(wire::TXT) {
                if let wire::Rdata::Txt(chunks) = &record.data {
                    for option in chunks.join("").split('&') {
                        if let Some(("authSource", value)) = option.split_once('=')
                            && !value.is_empty()
                        {
                            self.auth_source = value.to_string();
                        }
                    }
                }
            }
        }
        Ok(format!(
            "{} servidor(es) descobertos por SRV em {name}",
            self.hosts.len()
        ))
    }
}

/// A command the server refused, or a connection that failed. Only the second kind ends
/// the investigation.
pub struct Erro {
    pub code: i64,
    pub message: String,
    /// The connection, not the command.
    transport: bool,
}

impl Erro {
    fn io(message: impl Into<String>) -> Self {
        Self {
            code: 0,
            message: message.into(),
            transport: true,
        }
    }

    /// Whether the server answered and simply said no — Unauthorized, CommandNotFound,
    /// NoReplicationEnabled, "not running with --replSet". Against a managed cluster,
    /// where most administrative commands are taken away from the user, this is the
    /// normal answer to half the questions here, and none of them is a reason to stop.
    pub fn benign(&self) -> bool {
        !self.transport
    }
}

impl std::fmt::Display for Erro {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.code == 0 {
            write!(f, "{}", self.message)
        } else {
            write!(f, "{} (código {})", self.message, self.code)
        }
    }
}

pub struct Conn {
    stream: Stream,
    request: i32,
    /// What the handshake said about the far side: version, topology, role.
    pub hello: Doc,
    pub version: String,
    pub encryption: String,
    pub discovery: String,
}

impl Conn {
    pub fn open(target: &Target) -> Result<Self, String> {
        let mut target = Target {
            hosts: target.hosts.clone(),
            user: target.user.clone(),
            password: target.password.clone(),
            database: target.database.clone(),
            auth_source: target.auth_source.clone(),
            tls: target.tls,
            srv: target.srv,
            timeout: target.timeout,
        };
        let discovery = if target.srv {
            target.resolve_srv()?
        } else {
            String::new()
        };

        // Every host in turn: a replica set URL lists all of them and only one has to
        // answer for the inspection to have something to read.
        let mut last = String::new();
        for (host, port) in &target.hosts {
            match Self::connect_one(&target, host, *port) {
                Ok(mut conn) => {
                    conn.discovery = discovery;
                    conn.authenticate(&target)?;
                    return Ok(conn);
                }
                Err(problem) => last = problem,
            }
        }
        Err(last)
    }

    fn connect_one(target: &Target, host: &str, port: u16) -> Result<Self, String> {
        let address = (host, port)
            .to_socket_addrs()
            .map_err(|e| format!("não consegui resolver {host}: {e}"))?
            .next()
            .ok_or_else(|| format!("{host} não resolveu para nenhum endereço"))?;
        let tcp = TcpStream::connect_timeout(&address, target.timeout)
            .map_err(|e| format!("não consegui conectar em {address}: {e}"))?;
        let _ = tcp.set_read_timeout(Some(target.timeout));
        let _ = tcp.set_write_timeout(Some(target.timeout));
        let _ = tcp.set_nodelay(true);

        let (stream, encryption) = if target.tls {
            // Encrypted but not authenticated, deliberately: a managed cluster's
            // certificate verifies, a self-hosted one usually doesn't, and refusing to
            // look at a database because of its certificate would be the wrong tool
            // refusing the wrong thing. The report says which it got.
            let client = crate::tools::tls::Client::new(&format!("{host}:{port}"), host, false)?;
            let mut session = client.session()?;
            let mut tcp = tcp;
            while session.is_handshaking() {
                session
                    .complete_io(&mut tcp)
                    .map_err(|e| format!("o handshake TLS com {host} falhou: {e}"))?;
            }
            (
                Stream::Tls(Box::new(rustls::StreamOwned::new(session, tcp))),
                "TLS sem verificar o certificado".to_string(),
            )
        } else {
            (Stream::Plain(tcp), "sem TLS".to_string())
        };

        let mut conn = Self {
            stream,
            request: 0,
            hello: Doc::new(),
            version: String::new(),
            encryption,
            discovery: String::new(),
        };
        conn.handshake(target)?;
        Ok(conn)
    }

    /// The first command on any connection: who are you, and how may I log in.
    fn handshake(&mut self, target: &Target) -> Result<(), String> {
        let client = Doc::new()
            .with(
                "driver",
                Doc::new()
                    .with("name", "monitorzinho")
                    .with("version", env!("CARGO_PKG_VERSION")),
            )
            .with("os", Doc::new().with("type", std::env::consts::OS));
        let mut command = Doc::new()
            .with("hello", 1)
            .with("client", client.clone())
            .with("loadBalanced", false);
        if !target.user.is_empty() {
            command = command.with(
                "saslSupportedMechs",
                format!("{}.{}", target.auth_source, target.user),
            );
        }
        let hello = match self.run("admin", command) {
            Ok(doc) => doc,
            // `hello` is only spelled that way from 5.0 on; before that the same command
            // answered to a name nobody wants to type any more.
            Err(_) => {
                let mut command = Doc::new().with("isMaster", 1).with("client", client);
                if !target.user.is_empty() {
                    command = command.with(
                        "saslSupportedMechs",
                        format!("{}.{}", target.auth_source, target.user),
                    );
                }
                self.run("admin", command)
                    .map_err(|e| format!("o servidor não respondeu à apresentação: {e}"))?
            }
        };
        self.version = hello.text("maxWireVersion");
        self.hello = hello;
        Ok(())
    }

    fn authenticate(&mut self, target: &Target) -> Result<(), String> {
        if target.user.is_empty() {
            return Ok(());
        }
        if target.password.is_empty() {
            return Err(format!(
                "a URL traz o usuário {} e nenhuma senha",
                target.user
            ));
        }
        let offered: Vec<String> = self
            .hello
            .list("saslSupportedMechs")
            .iter()
            .filter_map(|value| value.as_text().map(str::to_string))
            .collect();
        // SHA-256 when the server has it, which is everything from 4.0 on. SHA-1 is kept
        // because a user created on an older server keeps its SHA-1 credential even
        // after the server is upgraded.
        let hash = if offered.is_empty() || offered.iter().any(|name| name == "SCRAM-SHA-256") {
            Hash::Sha256
        } else if offered.iter().any(|name| name == "SCRAM-SHA-1") {
            Hash::Sha1
        } else {
            return Err(format!(
                "o servidor só aceita {} e esta ferramenta faz SCRAM",
                offered.join(", ")
            ));
        };
        // The SHA-1 mechanism hashes the password with the username before the handshake
        // ever starts; the SHA-256 one uses it as typed.
        let secret = match hash {
            Hash::Sha1 => md5_hex(format!("{}:mongo:{}", target.user, target.password).as_bytes()),
            Hash::Sha256 => target.password.clone(),
        };
        let (mut scram, first) = Scram::start(hash, &target.user, &secret);

        let start = Doc::new()
            .with("saslStart", 1)
            .with("mechanism", hash.mechanism())
            .with("payload", Value::Binary(0, first.into_bytes()))
            .with("options", Doc::new().with("skipEmptyExchange", true));
        let reply = self
            .run(&target.auth_source, start)
            .map_err(|e| format!("autenticação recusada: {e}"))?;
        let conversation = reply.int("conversationId") as i32;
        let server_first = payload_of(&reply)?;
        let final_message = scram.respond(&server_first)?;

        let step = Doc::new()
            .with("saslContinue", 1)
            .with("conversationId", conversation)
            .with("payload", Value::Binary(0, final_message.into_bytes()));
        let reply = self
            .run(&target.auth_source, step)
            .map_err(|e| format!("autenticação recusada: {e}"))?;
        scram.verify(&payload_of(&reply)?)?;

        // Without `skipEmptyExchange` the server wants one more empty round before it
        // considers the conversation finished.
        if !reply.flag("done") {
            let step = Doc::new()
                .with("saslContinue", 1)
                .with("conversationId", conversation)
                .with("payload", Value::Binary(0, Vec::new()));
            self.run(&target.auth_source, step)
                .map_err(|e| format!("autenticação recusada no fim: {e}"))?;
        }
        Ok(())
    }

    /// Sends one command and returns its reply, or the server's complaint about it.
    pub fn run(&mut self, database: &str, command: Doc) -> Result<Doc, Erro> {
        let command = command.with("$db", database);
        self.write(&command).map_err(Erro::io)?;
        let reply = self.read().map_err(Erro::io)?;
        if reply.num("ok") == 1.0 {
            return Ok(reply);
        }
        Err(Erro {
            code: reply.int("code"),
            message: match reply.path("errmsg") {
                Some(value) => value.render(),
                None => "o servidor recusou o comando sem dizer por quê".to_string(),
            },
            transport: false,
        })
    }

    /// A command that answers with a cursor, read to the end (or to `CURSOR_LIMIT`).
    ///
    /// Todo comando que passa por aqui — `find`, `aggregate`, `listCollections`,
    /// `listIndexes` — é dos que podem custar, e todos saem com o teto de tempo junto.
    pub fn cursor(&mut self, database: &str, command: Doc) -> Result<Vec<Doc>, Erro> {
        let reply = self.run(database, command.with("maxTimeMS", MAX_TIME_MS))?;
        let Some(cursor) = reply.doc("cursor") else {
            return Ok(Vec::new());
        };
        let namespace = cursor.text("ns");
        let collection = namespace.rsplit('.').next().unwrap_or_default().to_string();
        let mut out: Vec<Doc> = cursor
            .list("firstBatch")
            .iter()
            .filter_map(|value| value.as_doc().cloned())
            .collect();
        let mut id = cursor.int("id");
        while id != 0 && out.len() < CURSOR_LIMIT {
            let more = self.run(
                database,
                Doc::new()
                    .with("getMore", id)
                    .with("collection", collection.clone())
                    .with("batchSize", 200)
                    .with("maxTimeMS", MAX_TIME_MS),
            )?;
            let Some(cursor) = more.doc("cursor") else {
                break;
            };
            out.extend(
                cursor
                    .list("nextBatch")
                    .iter()
                    .filter_map(|value| value.as_doc().cloned()),
            );
            id = cursor.int("id");
        }
        // A cursor left open holds resources on the server until it times out. Closing
        // our own is the one command here that changes anything, and what it changes is
        // ours.
        if id != 0 {
            let _ = self.run(
                database,
                Doc::new()
                    .with("killCursors", collection)
                    .with("cursors", vec![Value::Int64(id)]),
            );
        }
        Ok(out)
    }

    fn write(&mut self, command: &Doc) -> Result<(), String> {
        let body = command.encode();
        self.request = self.request.wrapping_add(1);
        let length = (16 + 4 + 1 + body.len()) as i32;
        let mut message = Vec::with_capacity(length as usize);
        message.extend_from_slice(&length.to_le_bytes());
        message.extend_from_slice(&self.request.to_le_bytes());
        message.extend_from_slice(&0i32.to_le_bytes());
        message.extend_from_slice(&OPCODE.to_le_bytes());
        // No flags: no checksum, no exhaust, nothing that changes the reply's shape.
        message.extend_from_slice(&0u32.to_le_bytes());
        message.push(0);
        message.extend_from_slice(&body);
        self.stream
            .write_all(&message)
            .map_err(|e| format!("não consegui enviar o comando: {e}"))?;
        self.stream
            .flush()
            .map_err(|e| format!("não consegui enviar o comando: {e}"))
    }

    fn read(&mut self) -> Result<Doc, String> {
        let mut header = [0u8; 16];
        self.stream
            .read_exact(&mut header)
            .map_err(|e| format!("a conexão com o banco falhou: {e}"))?;
        let length = i32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
        let opcode = i32::from_le_bytes([header[12], header[13], header[14], header[15]]);
        if opcode == OPCODE_COMPRESSED {
            return Err(
                "o servidor respondeu comprimido, o que esta ferramenta não pediu".to_string(),
            );
        }
        if opcode != OPCODE {
            return Err(format!("o servidor respondeu com o opcode {opcode}"));
        }
        let length = length
            .checked_sub(16)
            .filter(|size| *size <= MAX_MESSAGE)
            .ok_or("o servidor mandou uma mensagem de tamanho impossível")?;
        let mut body = vec![0u8; length];
        self.stream
            .read_exact(&mut body)
            .map_err(|e| format!("a conexão com o banco falhou: {e}"))?;

        let flags = u32::from_le_bytes([body[0], body[1], body[2], body[3]]);
        // Bit 0 says the last four bytes are a checksum rather than payload.
        let end = if flags & 1 == 1 {
            body.len().saturating_sub(4)
        } else {
            body.len()
        };
        let mut at = 4;
        let mut document: Option<Doc> = None;
        let mut sequences: Vec<(String, Vec<Value>)> = Vec::new();
        while at < end {
            let kind = body[at];
            at += 1;
            match kind {
                0 => {
                    let doc = bson::decode(&body[at..])?;
                    at += doc_length(&body, at);
                    document = Some(doc);
                }
                // A document sequence: the reply's big list, sent outside the document
                // to save copying it. Folded back in under its own name.
                1 => {
                    let size =
                        i32::from_le_bytes([body[at], body[at + 1], body[at + 2], body[at + 3]])
                            as usize;
                    let end_of_sequence = at + size;
                    let name_end = body[at + 4..end_of_sequence]
                        .iter()
                        .position(|byte| *byte == 0)
                        .map(|offset| at + 4 + offset)
                        .ok_or("sequência sem nome")?;
                    let name = String::from_utf8_lossy(&body[at + 4..name_end]).into_owned();
                    let mut cursor = name_end + 1;
                    let mut items = Vec::new();
                    while cursor < end_of_sequence {
                        let doc = bson::decode(&body[cursor..])?;
                        cursor += doc_length(&body, cursor);
                        items.push(Value::Doc(doc));
                    }
                    sequences.push((name, items));
                    at = end_of_sequence;
                }
                other => return Err(format!("seção desconhecida na resposta: {other}")),
            }
        }
        let mut document = document.ok_or("resposta sem documento")?;
        for (name, items) in sequences {
            document.0.push((name, Value::List(items)));
        }
        Ok(document)
    }
}

/// The length of the BSON document starting at `at`, read off its own header.
fn doc_length(bytes: &[u8], at: usize) -> usize {
    bytes
        .get(at..at + 4)
        .map(|header| i32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize)
        .unwrap_or(0)
        .max(5)
}

/// The SASL payload out of a reply, as text.
fn payload_of(reply: &Doc) -> Result<String, String> {
    match reply.path("payload") {
        Some(Value::Binary(_, bytes)) => Ok(String::from_utf8_lossy(bytes).into_owned()),
        Some(Value::Text(text)) => Ok(text.clone()),
        _ => Err("o servidor respondeu à autenticação sem payload".to_string()),
    }
}

enum Stream {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Stream::Plain(tcp) => tcp.read(buf),
            Stream::Tls(tls) => tls.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Stream::Plain(tcp) => tcp.write(buf),
            Stream::Tls(tls) => tls.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Stream::Plain(tcp) => tcp.flush(),
            Stream::Tls(tls) => tls.flush(),
        }
    }
}
