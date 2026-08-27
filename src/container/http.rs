//! HTTP/1.1 escrito à mão, sobre socket unix ou TCP (com ou sem TLS).
//!
//! O projeto inteiro fala com o sistema sem crate de binding: netlink `SOCK_DIAG`,
//! `/proc/net/*`, utmp, DNS byte a byte. Um cliente HTTP é a mesma decisão pelo mesmo
//! motivo — o que a API de uma engine de container precisa é montar uma requisição,
//! ler uma resposta e entender duas formas de corpo. Uma dependência para isso
//! traria consigo runtime assíncrono, pool de conexões e uma superfície que este
//! programa nunca usa.
//!
//! Toda requisição manda `Connection: close`. Sem keep-alive não há estado de conexão
//! para manter correto, e o corpo pode ser lido até o fim do fluxo — o custo é um
//! handshake por chamada, que num socket unix é a parte barata da conta.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

/// Onde a engine atende. Um endereço, não uma engine: a mesma forma serve para o
/// socket local, para um daemon remoto em texto claro e para um atrás de TLS.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Endpoint {
    Unix(PathBuf),
    Tcp { host: String, port: u16, tls: bool },
}

impl Endpoint {
    /// Lê `unix:///caminho`, `tcp://host:porta`, `http://…`, `https://…` ou um caminho
    /// solto (tratado como socket unix, que é o que um caminho solto sempre é aqui).
    ///
    /// A porta 2376 é a convenção do Docker para "com TLS" e a 2375 para sem; um
    /// `tcp://` sem esquema explícito na porta 2376 é assumido como TLS, porque essa é
    /// a única razão de alguém escolher aquele número.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("endereço vazio".to_string());
        }
        if let Some(path) = text.strip_prefix("unix://") {
            return Ok(Endpoint::Unix(PathBuf::from(path)));
        }
        if text.starts_with('/') {
            return Ok(Endpoint::Unix(PathBuf::from(text)));
        }
        let (rest, tls) = match text.split_once("://") {
            Some(("https", rest)) => (rest, true),
            Some(("tcp", rest)) => (rest, false),
            Some(("http", rest)) => (rest, false),
            Some((scheme, _)) => return Err(format!("esquema não suportado: {scheme}")),
            None => (text, false),
        };
        let rest = rest.trim_end_matches('/');
        let (host, port) = match rest.rsplit_once(':') {
            Some((host, port)) => {
                let port: u16 = port
                    .parse()
                    .map_err(|_| format!("porta inválida em «{text}»"))?;
                (host.to_string(), port)
            }
            None => (rest.to_string(), 2375),
        };
        if host.is_empty() {
            return Err(format!("host vazio em «{text}»"));
        }
        // 2376 é a porta que só existe por causa do TLS.
        Ok(Endpoint::Tcp {
            tls: tls || port == 2376,
            host,
            port,
        })
    }

    /// Como o endereço se escreve de volta — o que vai para o arquivo de configuração e
    /// para a nota do painel.
    pub fn to_url(&self) -> String {
        match self {
            Endpoint::Unix(path) => format!("unix://{}", path.display()),
            Endpoint::Tcp { host, port, tls } => {
                let scheme = if *tls { "https" } else { "tcp" };
                format!("{scheme}://{host}:{port}")
            }
        }
    }

    /// Se as leituras deste endereço podem ser trocadas pelo cgroup local. Só o socket
    /// unix vale: um daemon remoto administra containers que não existem nesta máquina,
    /// e ler o cgroup daqui responderia sobre outra coisa.
    pub fn is_local(&self) -> bool {
        matches!(self, Endpoint::Unix(_))
    }
}

pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

impl Response {
    /// O corpo como texto, ou o que der para aproveitar dele. A API responde JSON e o
    /// JSON é UTF-8 por definição; uma resposta que não seja não vai virar um erro de
    /// decodificação quando o problema real está na mensagem que ela carrega.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// A mensagem de erro que a engine mandou, sem tradução — quem sabe por que a
    /// operação falhou é ela.
    ///
    /// O corpo de erro do Docker é `{"message": "..."}`; quando não for, o texto cru
    /// serve, e quando nem isso houver sobra o código de status.
    pub fn error_message(&self) -> String {
        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&self.body)
            && let Some(message) = value.get("message").and_then(|m| m.as_str())
            && !message.trim().is_empty()
        {
            return message.trim().to_string();
        }
        let text = self.text();
        let text = text.trim();
        if text.is_empty() {
            format!("HTTP {}", self.status)
        } else {
            format!("HTTP {} — {text}", self.status)
        }
    }
}

/// As três formas de fluxo que um endpoint pode ter, atrás de uma porta só.
pub enum Conn {
    Unix(UnixStream),
    Tcp(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Conn {
    /// Quanto uma leitura pode esperar antes de desistir e devolver o controle ao laço.
    pub fn set_read_timeout(&self, timeout: Duration) -> std::io::Result<()> {
        match self {
            Conn::Unix(s) => s.set_read_timeout(Some(timeout)),
            Conn::Tcp(s) => s.set_read_timeout(Some(timeout)),
            Conn::Tls(s) => s.get_ref().set_read_timeout(Some(timeout)),
        }
    }
}

impl Read for Conn {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Conn::Unix(s) => s.read(buf),
            Conn::Tcp(s) => s.read(buf),
            Conn::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Conn {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Conn::Unix(s) => s.write(buf),
            Conn::Tcp(s) => s.write(buf),
            Conn::Tls(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Conn::Unix(s) => s.flush(),
            Conn::Tcp(s) => s.flush(),
            Conn::Tls(s) => s.flush(),
        }
    }
}

fn connect(endpoint: &Endpoint, timeout: Duration) -> Result<Conn, String> {
    match endpoint {
        Endpoint::Unix(path) => {
            let stream = UnixStream::connect(path)
                .map_err(|e| format!("não consegui abrir {}: {e}", path.display()))?;
            let _ = stream.set_read_timeout(Some(timeout));
            let _ = stream.set_write_timeout(Some(timeout));
            Ok(Conn::Unix(stream))
        }
        Endpoint::Tcp { host, port, tls } => {
            // Resolvido e conectado com prazo: um host remoto que não responde não pode
            // segurar a thread que o consultou.
            let addrs: Vec<std::net::SocketAddr> =
                std::net::ToSocketAddrs::to_socket_addrs(&(host.as_str(), *port))
                    .map_err(|e| format!("não consegui resolver {host}: {e}"))?
                    .collect();
            let addr = addrs
                .first()
                .ok_or_else(|| format!("{host} não resolveu para nenhum endereço"))?;
            let stream = TcpStream::connect_timeout(addr, timeout)
                .map_err(|e| format!("não consegui conectar em {host}:{port}: {e}"))?;
            let _ = stream.set_read_timeout(Some(timeout));
            let _ = stream.set_write_timeout(Some(timeout));
            if !*tls {
                return Ok(Conn::Tcp(stream));
            }
            // O rustls já é dependência por causa do inspetor de certificados, então
            // falar com um daemon remoto protegido não custa nenhuma dependência nova.
            let client = crate::tools::tls::Client::new(host, host, true)?;
            let session = client.session()?;
            Ok(Conn::Tls(Box::new(rustls::StreamOwned::new(
                session, stream,
            ))))
        }
    }
}

/// Uma requisição, do começo ao fim: conecta, escreve, lê tudo, fecha.
///
/// `body` vazio manda uma requisição sem corpo; com corpo, vai como JSON, que é a única
/// coisa que qualquer engine aqui recebe.
pub fn request(
    endpoint: &Endpoint,
    method: &str,
    path: &str,
    body: Option<&str>,
    timeout: Duration,
) -> Result<Response, String> {
    let mut conn = connect(endpoint, timeout)?;

    // `Host` é obrigatório em HTTP/1.1 e ignorado por um daemon em socket unix, que não
    // tem nome nenhum — `localhost` é o que todo cliente manda nesse caso.
    let host = match endpoint {
        Endpoint::Unix(_) => "localhost".to_string(),
        Endpoint::Tcp { host, port, .. } => format!("{host}:{port}"),
    };
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: monitorzinho/{}\r\nAccept: application/json\r\nConnection: close\r\n",
        env!("CARGO_PKG_VERSION")
    );
    match body {
        Some(body) => {
            head.push_str("Content-Type: application/json\r\n");
            head.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
            head.push_str(body);
        }
        None => head.push_str("\r\n"),
    }
    conn.write_all(head.as_bytes())
        .map_err(|e| format!("não consegui enviar a requisição: {e}"))?;
    conn.flush().ok();

    read_response(&mut conn)
}

/// Lê uma resposta até o fim **dela**, e não até o fim da conexão.
///
/// Ler até o fluxo fechar parece equivalente e não é: quem decide fechar é a outra ponta,
/// e um endpoint que segura a conexão aberta custa o prazo inteiro da requisição — dez
/// segundos parados por uma resposta que já tinha chegado. Foi exatamente isso que fez a
/// abertura de um shell demorar dez segundos, e a causa era uma chamada cuja resposta
/// nunca vinha.
///
/// Então o corpo é lido pelo que o próprio HTTP diz que ele mede: `Content-Length`
/// quando há um, os pedaços de `chunked` até o terminador quando é assim, e nada quando
/// o código de status é dos que não têm corpo. Ler até fechar continua sendo a saída
/// para uma resposta sem nenhuma dessas marcas, que é o que o protocolo manda fazer.
fn read_response(conn: &mut Conn) -> Result<Response, String> {
    let mut raw = Vec::new();
    let mut chunk = [0u8; 8 * 1024];

    // Cabeçalhos primeiro: sem eles não há como saber quanto corpo esperar.
    let head_end = loop {
        if let Some(at) = find(&raw, b"\r\n\r\n") {
            break at;
        }
        match conn.read(&mut chunk) {
            Ok(0) => return Err("a engine fechou antes de terminar o cabeçalho".to_string()),
            Ok(n) => raw.extend_from_slice(&chunk[..n]),
            Err(e) => return Err(format!("não consegui ler a resposta: {e}")),
        }
        if raw.len() > MAX_HEAD {
            return Err("cabeçalho de resposta longo demais".to_string());
        }
    };

    let (status, framing) = parse_head(&raw[..head_end])?;
    let body_at = head_end + 4;

    loop {
        let body = &raw[body_at..];
        match framing {
            // 204, 304 e companhia não têm corpo, mesmo que anunciem um tamanho.
            Framing::Empty => break,
            Framing::Length(n) if body.len() >= n => break,
            Framing::Chunked if chunked_complete(body) => break,
            _ => {}
        }
        match conn.read(&mut chunk) {
            // Fim do fluxo. Numa resposta sem enquadramento é assim que ela termina; nas
            // outras é uma resposta truncada, e o que chegou vale mais que um erro.
            Ok(0) => break,
            Ok(n) => raw.extend_from_slice(&chunk[..n]),
            Err(e) if raw.len() > body_at => {
                // Um TLS encerrado sem `close_notify` chega aqui como erro de I/O apesar
                // de a resposta estar completa.
                let _ = e;
                break;
            }
            Err(e) => return Err(format!("não consegui ler a resposta: {e}")),
        }
    }

    let body = &raw[body_at..];
    let body = match framing {
        Framing::Empty => Vec::new(),
        Framing::Length(n) => body[..n.min(body.len())].to_vec(),
        Framing::Chunked => dechunk(body),
        Framing::UntilClose => body.to_vec(),
    };
    Ok(Response { status, body })
}

/// O maior cabeçalho que vale a pena acumular antes de desistir.
const MAX_HEAD: usize = 64 * 1024;

/// Como o corpo desta resposta diz onde termina.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Framing {
    Empty,
    Length(usize),
    Chunked,
    UntilClose,
}

fn parse_head(head: &[u8]) -> Result<(u16, Framing), String> {
    let head = String::from_utf8_lossy(head);
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| format!("resposta HTTP ilegível: «{status_line}»"))?;

    let mut chunked = false;
    let mut length: Option<usize> = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim();
        match name.as_str() {
            "transfer-encoding" => chunked |= value.to_ascii_lowercase().contains("chunked"),
            "content-length" => length = value.parse().ok(),
            _ => {}
        }
    }

    // Estes nunca têm corpo, por definição do protocolo — e a engine responde 204 em
    // quase toda operação que dá certo.
    let framing = if matches!(status, 100..=199 | 204 | 304) {
        Framing::Empty
    } else if chunked {
        Framing::Chunked
    } else if let Some(n) = length {
        Framing::Length(n)
    } else {
        Framing::UntilClose
    };
    Ok((status, framing))
}

/// Se o corpo `chunked` já chegou até o pedaço de tamanho zero que o encerra.
fn chunked_complete(body: &[u8]) -> bool {
    let mut data = body;
    while let Some(eol) = find(data, b"\r\n") {
        let header = String::from_utf8_lossy(&data[..eol]);
        let size_text = header.split(';').next().unwrap_or("").trim();
        let Ok(size) = usize::from_str_radix(size_text, 16) else {
            // Um cabeçalho de pedaço ainda incompleto: falta ler mais.
            return false;
        };
        if size == 0 {
            return true;
        }
        let end = eol + 2 + size + 2;
        if end > data.len() {
            return false;
        }
        data = &data[end..];
    }
    false
}

/// Uma resposta inteira já lida, separada em cabeçalho e corpo.
///
/// Só para os testes, que têm os bytes na mão em vez de um fluxo. A leitura de verdade é
/// `read_response`, que tem o problema mais difícil: precisa saber onde o corpo termina
/// *antes* de o ter todo, porque esperar a conexão fechar é o que custava dez segundos.
#[cfg(test)]
fn parse(raw: &[u8]) -> Result<Response, String> {
    let split = find(raw, b"\r\n\r\n").ok_or("resposta HTTP sem cabeçalho completo")?;
    let (status, framing) = parse_head(&raw[..split])?;
    let rest = &raw[split + 4..];
    let body = match framing {
        Framing::Empty => Vec::new(),
        Framing::Chunked => dechunk(rest),
        Framing::Length(n) => rest[..n.min(rest.len())].to_vec(),
        Framing::UntilClose => rest.to_vec(),
    };
    Ok(Response { status, body })
}

/// Uma requisição que deixa de ser HTTP no meio: manda os cabeçalhos, confere que a
/// outra ponta aceitou trocar de protocolo, e devolve o fluxo cru.
///
/// É como se abre um shell dentro de um container: a engine responde `101 Switching
/// Protocols` e a partir dali os bytes são do terminal, nos dois sentidos, sem envelope
/// nenhum.
pub fn upgrade(
    endpoint: &Endpoint,
    path: &str,
    body: &str,
    timeout: Duration,
) -> Result<Conn, String> {
    let mut conn = connect(endpoint, timeout)?;
    let host = match endpoint {
        Endpoint::Unix(_) => "localhost".to_string(),
        Endpoint::Tcp { host, port, .. } => format!("{host}:{port}"),
    };
    let head = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: monitorzinho/{}\r\nContent-Type: application/json\r\nConnection: Upgrade\r\nUpgrade: tcp\r\nContent-Length: {}\r\n\r\n{body}",
        env!("CARGO_PKG_VERSION"),
        body.len()
    );
    conn.write_all(head.as_bytes())
        .map_err(|e| format!("não consegui enviar a requisição: {e}"))?;
    conn.flush().ok();

    // Byte a byte até o fim dos cabeçalhos: ler em blocos engoliria saída do shell que
    // já vem atrás deles, e depois do upgrade não há mais enquadramento para devolvê-la
    // ao lugar certo. São duzentos bytes uma vez por sessão.
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match conn.read(&mut byte) {
            Ok(0) => return Err("a engine fechou antes de responder".to_string()),
            Ok(_) => head.push(byte[0]),
            Err(e) => return Err(format!("não consegui ler a resposta: {e}")),
        }
        if head.len() > MAX_HEAD {
            return Err("cabeçalho de resposta longo demais".to_string());
        }
    }
    let (status, _) = parse_head(&head[..head.len() - 4])?;
    if status != 101 && status != 200 {
        // O corpo do erro veio junto ou vem logo atrás; lê-lo dá a mensagem da engine
        // em vez de um número solto.
        let mut rest = Vec::new();
        let _ = conn.read_to_end(&mut rest);
        return Err(Response { status, body: rest }.error_message());
    }
    Ok(conn)
}

/// Junta um corpo `chunked` de volta. Um pedaço malformado encerra a leitura em vez de
/// virar erro: o que já foi remontado é resposta de verdade, e a alternativa é jogar
/// fora uma listagem inteira por causa do último byte.
fn dechunk(mut data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    while let Some(eol) = find(data, b"\r\n") {
        let header = String::from_utf8_lossy(&data[..eol]);
        // Extensões de chunk (`1a;nome=valor`) vêm depois de um ponto-e-vírgula.
        let size_text = header.split(';').next().unwrap_or("").trim();
        let Ok(size) = usize::from_str_radix(size_text, 16) else {
            break;
        };
        if size == 0 {
            break;
        }
        let start = eol + 2;
        let end = start + size;
        if end > data.len() {
            out.extend_from_slice(&data[start.min(data.len())..]);
            break;
        }
        out.extend_from_slice(&data[start..end]);
        // Pula o CRLF que fecha o pedaço.
        data = &data[(end + 2).min(data.len())..];
    }
    out
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|&i| &haystack[i..i + needle.len()] == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_endpoints() {
        assert_eq!(
            Endpoint::parse("unix:///var/run/docker.sock").unwrap(),
            Endpoint::Unix(PathBuf::from("/var/run/docker.sock"))
        );
        assert_eq!(
            Endpoint::parse("/run/user/1000/docker.sock").unwrap(),
            Endpoint::Unix(PathBuf::from("/run/user/1000/docker.sock"))
        );
        assert_eq!(
            Endpoint::parse("tcp://10.0.0.5:2375").unwrap(),
            Endpoint::Tcp {
                host: "10.0.0.5".to_string(),
                port: 2375,
                tls: false
            }
        );
        // 2376 é a porta que só existe por causa do TLS.
        assert!(matches!(
            Endpoint::parse("tcp://10.0.0.5:2376").unwrap(),
            Endpoint::Tcp { tls: true, .. }
        ));
        assert!(Endpoint::parse("ftp://x").is_err());
    }

    #[test]
    fn parses_a_chunked_response() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n1\r\n!\r\n0\r\n\r\n";
        let response = parse(raw).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.text(), "hello!");
    }

    #[test]
    fn parses_a_counted_response() {
        let raw = b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n";
        let response = parse(raw).unwrap();
        assert_eq!(response.status, 204);
        assert!(response.body.is_empty());
    }

    #[test]
    fn a_body_less_status_reads_as_empty() {
        // A engine responde 204 em quase toda operação que dá certo, e um 204 não tem
        // corpo por definição do protocolo — mesmo que anuncie um tamanho.
        let raw = b"HTTP/1.1 204 No Content\r\nContent-Length: 5\r\n\r\nsobra";
        let response = parse(raw).unwrap();
        assert_eq!(response.status, 204);
        assert!(response.body.is_empty());
    }

    #[test]
    fn a_chunked_body_says_when_it_is_finished() {
        // É o que permite parar de ler quando a resposta acabou, em vez de esperar a
        // engine fechar a conexão — que é o que fazia a abertura de um shell custar dez
        // segundos.
        assert!(chunked_complete(b"5\r\nhello\r\n0\r\n\r\n"));
        // Faltando o terminador: ainda há o que ler.
        assert!(!chunked_complete(b"5\r\nhello\r\n"));
        // Pedaço anunciado mas ainda incompleto.
        assert!(!chunked_complete(b"5\r\nhel"));
        assert!(!chunked_complete(b""));
    }

    #[test]
    fn framing_comes_from_the_head() {
        let head = |text: &str| parse_head(text.as_bytes()).unwrap().1;
        assert!(matches!(
            head("HTTP/1.1 200 OK\r\nContent-Length: 12"),
            Framing::Length(12)
        ));
        assert!(matches!(
            head("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked"),
            Framing::Chunked
        ));
        assert!(matches!(head("HTTP/1.1 204 No Content"), Framing::Empty));
        // Sem marca nenhuma, o fim da resposta é o fim da conexão — o que o protocolo
        // manda fazer, e a única situação em que esperar por ela é correto.
        assert!(matches!(head("HTTP/1.1 200 OK"), Framing::UntilClose));
    }

    #[test]
    fn reads_the_engines_own_error_message() {
        let raw =
            b"HTTP/1.1 409 Conflict\r\nContent-Length: 34\r\n\r\n{\"message\":\"container em uso\"}   ";
        let response = parse(raw).unwrap();
        assert_eq!(response.error_message(), "container em uso");
    }
}
