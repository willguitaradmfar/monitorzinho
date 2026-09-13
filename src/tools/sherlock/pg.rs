//! Speaking to Postgres on the wire, by hand.
//!
//! There is no Postgres driver in this tree and this is not a reason to add one: the
//! inspection asks a few dozen `SELECT`s and reads them back as text, which is the
//! simple query protocol and nothing else. What that costs is written out below — a
//! startup packet, an authentication handshake, and a loop over tagged messages.
//!
//! Every connection this module opens is announced to the server as read-only before a
//! single question is asked. See `Conn::harden`.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use super::scram::{Hash, Scram, md5_hex};
use super::{Ssl, url};

/// Protocol 3.0, the version every Postgres since 7.4 speaks.
const PROTOCOL: i32 = 196_608;
/// The magic number that asks "will you do TLS?" before the startup packet.
const SSL_REQUEST: i32 = 80_877_103;
/// Cap on one message. Nothing the inspection asks for comes close; a length prefix
/// saying otherwise is a corrupt stream, and believing it would mean allocating it.
const MAX_MESSAGE: usize = 64 * 1024 * 1024;

/// Where to connect and as whom.
pub struct Target {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub database: String,
    pub ssl: Ssl,
    pub timeout: Duration,
}

impl Target {
    /// `postgres://user:senha@host:5432/base` into its parts, with everything optional
    /// filled in the way libpq would fill it.
    ///
    /// `database` overrides whatever the URL carried — the field exists so the same
    /// connection string can be pointed at a second database without being retyped.
    pub fn parse(text: &str, database: &str) -> Result<Self, String> {
        let parts = url::split(text, &["postgres", "postgresql"])
            .ok_or("a URL precisa começar com postgres:// (ou postgresql://)")?;
        let host = parts.hosts.first().cloned().unwrap_or_default();
        if host.0.is_empty() {
            return Err("a URL não diz em qual host conectar".to_string());
        }
        let user = if parts.user.is_empty() {
            "postgres".to_string()
        } else {
            parts.user.clone()
        };
        let database = match (database.trim(), parts.path.as_str()) {
            ("", "") => user.clone(),
            ("", path) => path.to_string(),
            (given, _) => given.to_string(),
        };
        let ssl = Ssl::from_sslmode(parts.option("sslmode"));
        Ok(Self {
            host: host.0,
            port: host.1.unwrap_or(5432),
            user,
            password: parts.password,
            database,
            ssl,
            timeout: Duration::from_secs(15),
        })
    }

    /// What the row shows: everything but the password.
    pub fn summary(&self) -> String {
        format!(
            "{}@{}:{}/{}",
            self.user, self.host, self.port, self.database
        )
    }
}

/// What went wrong. `code` is the SQLSTATE when Postgres itself complained; `transport`
/// separates that from the connection having died, which is the only distinction the
/// investigation acts on.
pub struct Erro {
    pub code: String,
    pub message: String,
    /// The connection, not the query: nothing more can be asked.
    transport: bool,
}

impl Erro {
    fn io(message: impl Into<String>) -> Self {
        Self {
            code: String::new(),
            message: message.into(),
            transport: true,
        }
    }

    /// Whether the server answered and simply said no.
    ///
    /// Every one of those is survivable and most are expected: this same inspection runs
    /// against servers five major versions apart and roles with wildly different grants,
    /// so a missing view, a missing column and a denied table are ordinary answers. The
    /// check that got one says so and the next one is asked. What is *not* survivable is
    /// the connection itself failing, and that is the whole of the distinction.
    pub fn benign(&self) -> bool {
        !self.transport
    }
}

impl std::fmt::Display for Erro {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.code.is_empty() {
            write!(f, "{}", self.message)
        } else {
            write!(f, "{} ({})", self.message, self.code)
        }
    }
}

/// A result set, every value as the text Postgres sent. The simple query protocol has
/// no other format, and the inspection reads numbers by eye anyway.
pub struct Table {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
}

impl Table {
    pub fn iter(&self) -> impl Iterator<Item = Row<'_>> {
        self.rows.iter().map(move |values| Row {
            columns: &self.columns,
            values,
        })
    }

    pub fn first(&self) -> Option<Row<'_>> {
        self.iter().next()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }
}

/// One row, read by column name. A name that isn't there and a NULL both read as an
/// empty string: a check that asks for a column this server's version doesn't have
/// should come out saying nothing, not panicking.
pub struct Row<'a> {
    columns: &'a [String],
    values: &'a [Option<String>],
}

impl Row<'_> {
    pub fn get(&self, column: &str) -> &str {
        self.columns
            .iter()
            .position(|name| name == column)
            .and_then(|index| self.values.get(index))
            .and_then(|value| value.as_deref())
            .unwrap_or("")
    }

    /// The column as a number, or 0 — which is the right answer for every counter here.
    pub fn num(&self, column: &str) -> f64 {
        self.get(column).trim().parse().unwrap_or(0.0)
    }

    pub fn int(&self, column: &str) -> i64 {
        self.num(column) as i64
    }

    /// Postgres spells booleans `t` and `f` in text format.
    pub fn flag(&self, column: &str) -> bool {
        self.get(column) == "t"
    }
}

/// A connection, plus what the server volunteered about itself on the way in.
pub struct Conn {
    stream: Stream,
    /// `ParameterStatus` messages from the handshake: server_version, server_encoding,
    /// is_superuser and the rest, free of charge before any query runs.
    pub settings: HashMap<String, String>,
    /// Whether the bytes are encrypted, and how the certificate was treated.
    pub encryption: String,
}

impl Conn {
    /// Connects, negotiates TLS if asked, authenticates, and returns a session that has
    /// already been told it may not write.
    pub fn open(target: &Target) -> Result<Self, String> {
        let address = (target.host.as_str(), target.port)
            .to_socket_addrs()
            .map_err(|e| format!("não consegui resolver {}: {e}", target.host))?
            .next()
            .ok_or_else(|| format!("{} não resolveu para nenhum endereço", target.host))?;
        let tcp = TcpStream::connect_timeout(&address, target.timeout)
            .map_err(|e| format!("não consegui conectar em {address}: {e}"))?;
        let _ = tcp.set_read_timeout(Some(target.timeout));
        let _ = tcp.set_write_timeout(Some(target.timeout));
        let _ = tcp.set_nodelay(true);

        let (stream, encryption) = negotiate(tcp, target)?;
        let mut conn = Self {
            stream,
            settings: HashMap::new(),
            encryption,
        };
        conn.startup(target)?;
        Ok(conn)
    }

    /// The startup packet and everything the server says until it is ready for a query.
    fn startup(&mut self, target: &Target) -> Result<(), String> {
        let mut body = Vec::new();
        body.extend_from_slice(&PROTOCOL.to_be_bytes());
        for (key, value) in [
            ("user", target.user.as_str()),
            ("database", target.database.as_str()),
            // Announced rather than left blank: whoever is looking at pg_stat_activity
            // on the other end deserves to know what this connection is and that it
            // isn't going to change anything.
            ("application_name", "monitorzinho (somente leitura)"),
            ("client_encoding", "UTF8"),
        ] {
            body.extend_from_slice(key.as_bytes());
            body.push(0);
            body.extend_from_slice(value.as_bytes());
            body.push(0);
        }
        body.push(0);
        self.send_untagged(&body)
            .map_err(|e| format!("não consegui enviar a apresentação: {e}"))?;

        let mut scram: Option<Scram> = None;
        loop {
            let (tag, body) = self.recv().map_err(|e| e.message)?;
            match tag {
                b'R' => {
                    if let Some(reply) = self.authenticate(target, &body, &mut scram)? {
                        self.send(reply.0, &reply.1)
                            .map_err(|e| format!("autenticação: {e}"))?;
                    }
                }
                b'S' => {
                    let mut parts = body.split(|byte| *byte == 0);
                    let key = cstr(parts.next().unwrap_or_default());
                    let value = cstr(parts.next().unwrap_or_default());
                    self.settings.insert(key, value);
                }
                // BackendKeyData: the ticket for cancelling a query. Nothing here
                // cancels anything, so it is read and dropped.
                b'K' => {}
                b'E' => return Err(error_of(&body).message),
                b'Z' => return Ok(()),
                b'N' => {}
                other => {
                    return Err(format!(
                        "o servidor mandou uma mensagem inesperada ({}) durante a conexão",
                        other as char
                    ));
                }
            }
        }
    }

    /// One authentication request. Returns the message to send back, if the method
    /// needs one.
    fn authenticate(
        &mut self,
        target: &Target,
        body: &[u8],
        scram: &mut Option<Scram>,
    ) -> Result<Option<(u8, Vec<u8>)>, String> {
        let code = i32::from_be_bytes(
            body.get(..4)
                .and_then(|b| b.try_into().ok())
                .ok_or("pedido de autenticação truncado")?,
        );
        let rest = &body[4.min(body.len())..];
        match code {
            0 => Ok(None),
            3 => {
                require_password(target)?;
                let mut reply = target.password.clone().into_bytes();
                reply.push(0);
                Ok(Some((b'p', reply)))
            }
            5 => {
                require_password(target)?;
                let salt = rest
                    .get(..4)
                    .ok_or("o servidor mandou um sal curto demais")?;
                // md5(md5(senha + usuário) + sal), the shape libpq has sent since 2001.
                let inner = md5_hex(format!("{}{}", target.password, target.user).as_bytes());
                let mut hashed = Vec::new();
                hashed.extend_from_slice(inner.as_bytes());
                hashed.extend_from_slice(salt);
                let mut reply = format!("md5{}", md5_hex(&hashed)).into_bytes();
                reply.push(0);
                Ok(Some((b'p', reply)))
            }
            10 => {
                require_password(target)?;
                let mechanisms: Vec<String> = rest
                    .split(|byte| *byte == 0)
                    .map(cstr)
                    .filter(|name| !name.is_empty())
                    .collect();
                if !mechanisms.iter().any(|name| name == "SCRAM-SHA-256") {
                    return Err(format!(
                        "o servidor só aceita {} e este cliente faz SCRAM-SHA-256",
                        mechanisms.join(", ")
                    ));
                }
                let (state, first) = Scram::start(Hash::Sha256, "", &target.password);
                *scram = Some(state);
                let mut reply = Vec::new();
                reply.extend_from_slice(b"SCRAM-SHA-256\0");
                reply.extend_from_slice(&(first.len() as i32).to_be_bytes());
                reply.extend_from_slice(first.as_bytes());
                Ok(Some((b'p', reply)))
            }
            11 => {
                let state = scram.as_mut().ok_or("SCRAM fora de ordem")?;
                let final_message = state.respond(&cstr(rest))?;
                Ok(Some((b'p', final_message.into_bytes())))
            }
            12 => {
                scram
                    .as_ref()
                    .ok_or("SCRAM fora de ordem")?
                    .verify(&cstr(rest))?;
                Ok(None)
            }
            2 | 7 | 9 => {
                Err("este servidor pede Kerberos/GSSAPI, que esta ferramenta não fala".to_string())
            }
            other => Err(format!("método de autenticação {other} desconhecido")),
        }
    }

    /// Declares the session read-only and puts a clock on every statement, before the
    /// inspection asks anything.
    ///
    /// The queries below are all `SELECT`s and could be trusted to behave, but "trust
    /// me" is not what anyone wants to hear about a connection to production. This makes
    /// the server itself refuse a write, and it makes any single question give up rather
    /// than sit on a lock or grind for minutes on a table nobody expected to be huge.
    ///
    /// Returns what could actually be set: a pooler in transaction mode may reject some
    /// of it, and that is worth saying out loud rather than assuming.
    pub fn harden(&mut self, statement_timeout: Duration) -> Vec<String> {
        let millis = statement_timeout.as_millis().max(1000);
        let guards = [
            "SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY".to_string(),
            format!("SET statement_timeout = {millis}"),
            // A lock wait means something else holds it, and this tool has no business
            // queueing behind production work.
            "SET lock_timeout = 2000".to_string(),
            "SET idle_in_transaction_session_timeout = 30000".to_string(),
        ];
        let mut refused = Vec::new();
        for guard in guards {
            if let Err(e) = self.query(&guard) {
                refused.push(format!("{guard}: {e}"));
            }
        }
        refused
    }

    /// Whether the server agrees this session cannot write. Asked rather than assumed,
    /// and reported in the log — it is the one claim this tool makes that a reader would
    /// want evidence for.
    pub fn read_only(&mut self) -> bool {
        self.query("SHOW transaction_read_only")
            .ok()
            .and_then(|table| {
                table
                    .first()
                    .map(|row| row.get("transaction_read_only") == "on")
            })
            .unwrap_or(false)
    }

    /// Runs one statement and reads everything it produces.
    pub fn query(&mut self, sql: &str) -> Result<Table, Erro> {
        let mut body = sql.as_bytes().to_vec();
        body.push(0);
        self.send(b'Q', &body)
            .map_err(|e| Erro::io(format!("não consegui enviar a consulta: {e}")))?;

        let mut table = Table {
            columns: Vec::new(),
            rows: Vec::new(),
        };
        let mut failure: Option<Erro> = None;
        loop {
            let (tag, body) = self.recv()?;
            match tag {
                b'T' => {
                    table.columns = row_description(&body);
                    table.rows.clear();
                }
                b'D' => table.rows.push(data_row(&body)),
                b'E' => failure = Some(error_of(&body)),
                // ReadyForQuery closes every exchange, error or not. Reading up to it is
                // what leaves the connection usable for the next question.
                b'Z' => {
                    return match failure {
                        Some(error) => Err(error),
                        None => Ok(table),
                    };
                }
                // CommandComplete, EmptyQuery, notices, parameter changes, copy: nothing
                // a read-only inspection needs to act on.
                _ => {}
            }
        }
    }

    fn send(&mut self, tag: u8, body: &[u8]) -> std::io::Result<()> {
        let mut message = Vec::with_capacity(body.len() + 5);
        message.push(tag);
        message.extend_from_slice(&((body.len() + 4) as i32).to_be_bytes());
        message.extend_from_slice(body);
        self.stream.write_all(&message)?;
        self.stream.flush()
    }

    /// The startup packet is the one message with no tag byte.
    fn send_untagged(&mut self, body: &[u8]) -> std::io::Result<()> {
        let mut message = Vec::with_capacity(body.len() + 4);
        message.extend_from_slice(&((body.len() + 4) as i32).to_be_bytes());
        message.extend_from_slice(body);
        self.stream.write_all(&message)?;
        self.stream.flush()
    }

    fn recv(&mut self) -> Result<(u8, Vec<u8>), Erro> {
        let mut header = [0u8; 5];
        self.stream
            .read_exact(&mut header)
            .map_err(|e| Erro::io(format!("a conexão com o banco falhou: {e}")))?;
        let length = i32::from_be_bytes([header[1], header[2], header[3], header[4]]);
        let length = usize::try_from(length - 4)
            .ok()
            .filter(|size| *size <= MAX_MESSAGE)
            .ok_or_else(|| Erro::io("o servidor mandou uma mensagem de tamanho impossível"))?;
        let mut body = vec![0u8; length];
        self.stream
            .read_exact(&mut body)
            .map_err(|e| Erro::io(format!("a conexão com o banco falhou: {e}")))?;
        Ok((header[0], body))
    }
}

fn require_password(target: &Target) -> Result<(), String> {
    if target.password.is_empty() {
        return Err(format!(
            "o servidor pediu senha para {} e a URL não traz nenhuma",
            target.user
        ));
    }
    Ok(())
}

/// The `SSLRequest` dance, then the handshake if the server said yes.
fn negotiate(mut tcp: TcpStream, target: &Target) -> Result<(Stream, String), String> {
    if target.ssl == Ssl::Disable {
        return Ok((
            Stream::Plain(tcp),
            "sem TLS (desligado na configuração)".to_string(),
        ));
    }
    let mut request = Vec::new();
    request.extend_from_slice(&8i32.to_be_bytes());
    request.extend_from_slice(&SSL_REQUEST.to_be_bytes());
    tcp.write_all(&request)
        .map_err(|e| format!("não consegui pedir TLS: {e}"))?;
    let mut answer = [0u8; 1];
    tcp.read_exact(&mut answer)
        .map_err(|e| format!("o servidor não respondeu ao pedido de TLS: {e}"))?;

    if answer[0] != b'S' {
        return match target.ssl {
            Ssl::Prefer => Ok((
                Stream::Plain(tcp),
                "sem TLS (o servidor não oferece)".to_string(),
            )),
            _ => Err("o servidor recusou TLS e a configuração exige".to_string()),
        };
    }

    let verify = target.ssl == Ssl::Verify;
    let client = crate::tools::tls::Client::new(
        &format!("{}:{}", target.host, target.port),
        &target.host,
        verify,
    )?;
    let mut session = client.session()?;
    while session.is_handshaking() {
        session
            .complete_io(&mut tcp)
            .map_err(|e| format!("o handshake TLS falhou: {e}"))?;
    }
    let note = if verify {
        "TLS com certificado verificado".to_string()
    } else {
        "TLS sem verificar o certificado".to_string()
    };
    Ok((
        Stream::Tls(Box::new(rustls::StreamOwned::new(session, tcp))),
        note,
    ))
}

/// Plaintext or TLS, behind one pair of `Read`/`Write` so the protocol above never has
/// to know which it got.
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

/// Column names out of a `RowDescription`. Only the name is kept: every value arrives as
/// text, so the type OIDs alongside it would answer a question nobody asks here.
fn row_description(body: &[u8]) -> Vec<String> {
    let count =
        u16::from_be_bytes([*body.first().unwrap_or(&0), *body.get(1).unwrap_or(&0)]) as usize;
    let mut names = Vec::with_capacity(count);
    let mut at = 2;
    for _ in 0..count {
        let end = match body[at..].iter().position(|byte| *byte == 0) {
            Some(offset) => at + offset,
            None => break,
        };
        names.push(cstr(&body[at..end]));
        // The name, its terminator, and the 18 bytes of type information after it.
        at = end + 1 + 18;
        if at > body.len() {
            break;
        }
    }
    names
}

/// One `DataRow`: a count, then each value as a length and its bytes. A length of -1 is
/// NULL, which is not the same as the empty string and is kept apart from it.
fn data_row(body: &[u8]) -> Vec<Option<String>> {
    let count =
        u16::from_be_bytes([*body.first().unwrap_or(&0), *body.get(1).unwrap_or(&0)]) as usize;
    let mut values = Vec::with_capacity(count);
    let mut at = 2;
    for _ in 0..count {
        let Some(header) = body.get(at..at + 4).and_then(|b| b.try_into().ok()) else {
            break;
        };
        at += 4;
        let length = i32::from_be_bytes(header);
        if length < 0 {
            values.push(None);
            continue;
        }
        let end = (at + length as usize).min(body.len());
        values.push(Some(String::from_utf8_lossy(&body[at..end]).into_owned()));
        at = end;
    }
    values
}

/// An `ErrorResponse`: fields tagged by a letter, ending at an empty one. `M` is the
/// message a person reads, `C` the SQLSTATE a program reads.
fn error_of(body: &[u8]) -> Erro {
    let mut error = Erro {
        code: String::new(),
        message: String::new(),
        transport: false,
    };
    let mut detail = String::new();
    for field in body.split(|byte| *byte == 0) {
        let Some((tag, text)) = field.split_first() else {
            continue;
        };
        match tag {
            b'M' => error.message = cstr(text),
            b'C' => error.code = cstr(text),
            b'D' | b'H' => detail = cstr(text),
            _ => {}
        }
    }
    if error.message.is_empty() {
        error.message = "o servidor recusou a consulta sem dizer por quê".to_string();
    }
    if !detail.is_empty() {
        error.message = format!("{} — {detail}", error.message);
    }
    error
}

fn cstr(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim_end_matches('\0')
        .to_string()
}
