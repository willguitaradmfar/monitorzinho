//! O cliente HTTP que faltava: «pegue este JSON».
//!
//! Não havia um. `tools/http.rs` mede as quatro fases de uma requisição e **descarta o
//! corpo** — é uma sonda de latência, não um cliente. `container/http.rs` fala com o
//! daemon por socket Unix. Então isto é novo, e é pequeno de propósito: só o que os
//! provedores desta aba usam, para que a superfície seja pequena o bastante para se
//! confiar nela.
//!
//! O que ele deliberadamente não faz: WebSocket, POST e autenticação. Nenhum provedor da
//! v1 precisa. `gzip` ele lê — não por escolha, mas porque servidores comprimem mesmo
//! quando se pede `identity`, e a alternativa era perder o feed. Ver `super::gzip`.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};

/// Quantos redirecionamentos antes de desistir. O mesmo teto de `tools/http.rs`: um sexto
/// é laço.
const MAX_REDIRECTS: u8 = 5;
/// Teto de corpo. Uma resposta que passa disto é erro, não uma leitura pela metade.
const MAX_BODY: usize = 4 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
/// Separado do de conectar: um servidor que aceita a conexão e some é o caso comum, e um
/// timeout único trataria isso como se fosse DNS lento.
const READ_TIMEOUT: Duration = Duration::from_secs(12);

/// O que deu errado. As variantes são separadas porque a **reação** a cada uma é
/// diferente: um 500 é para tentar de novo daqui a pouco, um 429 é para parar e esperar o
/// tempo que mandaram esperar, e um corpo inesperado denuncia que a fonte mudou por baixo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FeedError {
    Rede(String),
    Tempo,
    /// O servidor respondeu, e respondeu que não. `detalhe` é o que ele disse no corpo:
    /// muitas APIs explicam a recusa ali («seu plano permite no máximo 1 ativo»), e
    /// jogá-lo fora transformava um motivo claro num «respondeu 400».
    Status {
        code: u16,
        detalhe: Option<String>,
    },
    Cota {
        retry_after: Option<u64>,
    },
    Formato(String),
}

impl FeedError {
    pub fn frase(&self) -> String {
        match self {
            FeedError::Rede(e) => format!("rede: {e}"),
            FeedError::Tempo => "sem resposta a tempo".to_string(),
            FeedError::Status {
                code,
                detalhe: Some(d),
            } => format!("respondeu {code}: {d}"),
            FeedError::Status {
                code,
                detalhe: None,
            } => format!("respondeu {code}"),
            FeedError::Cota {
                retry_after: Some(s),
            } => format!("cota estourada, espera {s}s"),
            FeedError::Cota { retry_after: None } => "cota estourada".to_string(),
            FeedError::Formato(e) => format!("formato inesperado: {e}"),
        }
    }

    /// Se insistir agora só piora. Um 429 é o caso claro; um erro de formato também é,
    /// porque tentar de novo devolve o mesmo corpo estranho.
    pub fn desiste(&self) -> bool {
        matches!(self, FeedError::Cota { .. } | FeedError::Formato(_))
    }
}

/// A resposta: o corpo, e os cabeçalhos que importam para o limitador.
pub struct Response {
    pub body: Vec<u8>,
    /// Peso já gasto na janela, quando a API se auto-reporta (a Binance manda
    /// `X-MBX-USED-WEIGHT-1m`). Lido em vez de adivinhado — é a única fonte da v1 que diz
    /// quanto custou, e a mais fácil de respeitar sem chutar.
    pub used_weight: Option<u64>,
}

struct Target {
    host: String,
    port: u16,
    path: String,
    tls: bool,
}

impl Target {
    fn parse(url: &str) -> Result<Self, FeedError> {
        let (scheme, rest) = url
            .split_once("://")
            // **Sem a URL na mensagem.** Ela carrega o token do provedor na query, e uma
            // mensagem de erro vai para a tela, para o log e para um relato de problema.
            // Um segredo que aparece porque alguém digitou o endereço errado é um segredo
            // vazado por um caminho que ninguém revisa.
            .ok_or_else(|| FeedError::Rede("endereço sem esquema (http:// ou https://)".into()))?;
        let tls = match scheme {
            "https" => true,
            "http" => false,
            other => {
                return Err(FeedError::Rede(format!(
                    "esquema «{other}» não é suportado"
                )));
            }
        };
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) => (
                h.to_string(),
                p.parse()
                    .map_err(|_| FeedError::Rede("porta inválida".into()))?,
            ),
            None => (authority.to_string(), if tls { 443 } else { 80 }),
        };
        if host.is_empty() {
            return Err(FeedError::Rede("URL sem host".into()));
        }
        Ok(Target {
            host,
            port,
            path: path.to_string(),
            tls,
        })
    }

    fn url(&self) -> String {
        let scheme = if self.tls { "https" } else { "http" };
        format!("{scheme}://{}:{}{}", self.host, self.port, self.path)
    }
}

/// A configuração de TLS, montada uma vez. As raízes do sistema primeiro, e o
/// `webpki-roots` como reserva — a mesma escolha, e pela mesma razão, de `tools/tls.rs`:
/// uma máquina sem loja de certificados do sistema continua conseguindo falar com o mundo.
fn tls_config() -> Arc<ClientConfig> {
    use std::sync::OnceLock;
    static CONFIG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let mut roots = RootCertStore::empty();
            let nativas = rustls_native_certs::load_native_certs();
            roots.add_parsable_certificates(nativas.certs);
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            Arc::new(
                ClientConfig::builder()
                    .with_root_certificates(roots)
                    .with_no_client_auth(),
            )
        })
        .clone()
}

/// GET, seguindo redirecionamento, devolvendo o corpo.
pub fn get(url: &str) -> Result<Response, FeedError> {
    let mut alvo = Target::parse(url)?;
    for _ in 0..=MAX_REDIRECTS {
        match buscar(&alvo)? {
            Buscado::Corpo(r) => return Ok(r),
            Buscado::Redireciona(destino) => {
                alvo = resolver_destino(&alvo, &destino)?;
            }
        }
    }
    Err(FeedError::Rede("redirecionamentos demais".into()))
}

/// GET que já devolve o JSON analisado.
pub fn get_json(url: &str) -> Result<serde_json::Value, FeedError> {
    let response = get(url)?;
    serde_json::from_slice(&response.body).map_err(|e| FeedError::Formato(e.to_string()))
}

enum Buscado {
    Corpo(Response),
    Redireciona(String),
}

fn resolver_destino(de: &Target, location: &str) -> Result<Target, FeedError> {
    if location.contains("://") {
        return Target::parse(location);
    }
    let base = de.url();
    let raiz = base
        .split_once("://")
        .and_then(|(s, r)| r.split_once('/').map(|(a, _)| format!("{s}://{a}")))
        .unwrap_or(base);
    match location.starts_with('/') {
        true => Target::parse(&format!("{raiz}{location}")),
        false => Target::parse(&format!("{raiz}/{location}")),
    }
}

fn buscar(alvo: &Target) -> Result<Buscado, FeedError> {
    let endereco = (alvo.host.as_str(), alvo.port)
        .to_socket_addrs()
        .map_err(|e| FeedError::Rede(e.to_string()))?
        .next()
        .ok_or_else(|| FeedError::Rede(format!("«{}» não resolve", alvo.host)))?;

    let stream = TcpStream::connect_timeout(&endereco, CONNECT_TIMEOUT).map_err(|e| {
        match e.kind() == std::io::ErrorKind::TimedOut {
            true => FeedError::Tempo,
            false => FeedError::Rede(e.to_string()),
        }
    })?;
    stream
        .set_read_timeout(Some(READ_TIMEOUT))
        .map_err(|e| FeedError::Rede(e.to_string()))?;
    stream
        .set_write_timeout(Some(READ_TIMEOUT))
        .map_err(|e| FeedError::Rede(e.to_string()))?;

    let pedido = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: monitorzinho/{}\r\nAccept: application/json, text/xml, */*\r\nAccept-Encoding: identity\r\nConnection: close\r\n\r\n",
        alvo.path,
        alvo.host,
        env!("CARGO_PKG_VERSION"),
    );

    if alvo.tls {
        let nome = ServerName::try_from(alvo.host.clone())
            .map_err(|_| FeedError::Rede(format!("«{}» não é um nome válido", alvo.host)))?;
        let conexao = ClientConnection::new(tls_config(), nome)
            .map_err(|e| FeedError::Rede(e.to_string()))?;
        let mut tls = StreamOwned::new(conexao, stream);
        tls.write_all(pedido.as_bytes())
            .map_err(|e| FeedError::Rede(e.to_string()))?;
        ler(&mut tls)
    } else {
        let mut plano = stream;
        plano
            .write_all(pedido.as_bytes())
            .map_err(|e| FeedError::Rede(e.to_string()))?;
        ler(&mut plano)
    }
}

fn ler(stream: &mut impl Read) -> Result<Buscado, FeedError> {
    let mut cru = Vec::new();
    let mut buffer = [0u8; 16 * 1024];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                cru.extend_from_slice(&buffer[..n]);
                if cru.len() > MAX_BODY + 64 * 1024 {
                    return Err(FeedError::Formato("resposta grande demais".into()));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Err(FeedError::Tempo),
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => return Err(FeedError::Tempo),
            // Um servidor que fecha o TLS sem o adeus formal é comum e não é erro: o que
            // interessa é se o corpo veio inteiro, e isso quem diz é o cabeçalho.
            Err(_) if !cru.is_empty() => break,
            Err(e) => return Err(FeedError::Rede(e.to_string())),
        }
    }

    let fim = achar(&cru, b"\r\n\r\n").ok_or_else(|| FeedError::Formato("sem cabeçalho".into()))?;
    let cabecalho = String::from_utf8_lossy(&cru[..fim]).to_string();
    let corpo = &cru[fim + 4..];

    let status = status_de(&cabecalho)?;
    if (300..400).contains(&status)
        && let Some(destino) = header(&cabecalho, "location")
    {
        return Ok(Buscado::Redireciona(destino));
    }
    if status == 429 || status == 418 {
        let espera = header(&cabecalho, "retry-after").and_then(|v| v.trim().parse().ok());
        return Err(FeedError::Cota {
            retry_after: espera,
        });
    }
    if !(200..300).contains(&status) {
        return Err(FeedError::Status {
            code: status,
            detalhe: detalhe_do_corpo(corpo),
        });
    }

    let corpo = match header(&cabecalho, "transfer-encoding")
        .is_some_and(|v| v.to_lowercase().contains("chunked"))
    {
        true => deschunk(corpo)?,
        false => match header(&cabecalho, "content-length")
            .and_then(|v| v.trim().parse::<usize>().ok())
        {
            Some(n) if n <= corpo.len() => corpo[..n].to_vec(),
            _ => corpo.to_vec(),
        },
    };

    // Descomprimir vem **depois** de desmontar o `chunked`: o gzip está dentro dos
    // pedaços, e inflar antes seria inflar os cabeçalhos de tamanho junto.
    //
    // Alguns servidores comprimem mesmo sem que se peça — o RSS do G1 devolve gzip tendo
    // recebido `Accept-Encoding: identity`. Por isso a decisão não sai só do cabeçalho:
    // se os bytes começam com a assinatura do gzip, são gzip, tenha ou não o servidor
    // dito. O contrário — confiar no cabeçalho — deixava o corpo binário virar «zero
    // itens legíveis» e o feed sumia da tela sem explicação.
    let corpo = match super::gzip::parece_gzip(&corpo) {
        true => super::gzip::inflar(&corpo).map_err(|e| FeedError::Formato(e.to_string()))?,
        false => corpo,
    };

    if corpo.len() > MAX_BODY {
        return Err(FeedError::Formato("corpo grande demais".into()));
    }

    Ok(Buscado::Corpo(Response {
        body: corpo,
        used_weight: header(&cabecalho, "x-mbx-used-weight-1m").and_then(|v| v.trim().parse().ok()),
    }))
}

/// A explicação que a API deu no corpo de uma recusa.
///
/// Procura um campo `message` no JSON e cai para o texto cru quando não há um. Truncado,
/// porque isto vai para uma linha de rodapé — e nunca ecoa a requisição, que carregaria o
/// token de volta para a tela.
fn detalhe_do_corpo(corpo: &[u8]) -> Option<String> {
    const MAX: usize = 160;
    let texto = String::from_utf8_lossy(corpo);
    let texto = texto.trim();
    if texto.is_empty() {
        return None;
    }
    let bruto = serde_json::from_str::<serde_json::Value>(texto)
        .ok()
        .and_then(|v| {
            v.get("message")
                .or_else(|| v.get("error"))
                .and_then(|m| m.as_str().map(str::to_string))
        })
        .unwrap_or_else(|| texto.to_string());
    let limpo: String = bruto.chars().take(MAX).collect();
    (!limpo.trim().is_empty()).then_some(limpo)
}

fn achar(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn status_de(cabecalho: &str) -> Result<u16, FeedError> {
    cabecalho
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| FeedError::Formato("primeira linha ilegível".into()))
}

fn header(cabecalho: &str, nome: &str) -> Option<String> {
    cabecalho.lines().skip(1).find_map(|linha| {
        let (chave, valor) = linha.split_once(':')?;
        chave
            .trim()
            .eq_ignore_ascii_case(nome)
            .then(|| valor.trim().to_string())
    })
}

/// Desfaz o `Transfer-Encoding: chunked`. Necessário porque várias APIs o usam, e sem
/// isto o JSON chega com os tamanhos hexadecimais no meio.
fn deschunk(mut corpo: &[u8]) -> Result<Vec<u8>, FeedError> {
    let mut saida = Vec::new();
    loop {
        let fim =
            achar(corpo, b"\r\n").ok_or_else(|| FeedError::Formato("chunk sem fim".into()))?;
        let linha = String::from_utf8_lossy(&corpo[..fim]);
        let tamanho = usize::from_str_radix(linha.split(';').next().unwrap_or("").trim(), 16)
            .map_err(|_| FeedError::Formato(format!("tamanho de chunk ilegível: {linha}")))?;
        corpo = &corpo[fim + 2..];
        if tamanho == 0 {
            break;
        }
        if tamanho > corpo.len() {
            return Err(FeedError::Formato("chunk maior que o que chegou".into()));
        }
        saida.extend_from_slice(&corpo[..tamanho]);
        corpo = &corpo[(tamanho + 2).min(corpo.len())..];
        if saida.len() > MAX_BODY {
            return Err(FeedError::Formato("corpo grande demais".into()));
        }
    }
    Ok(saida)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_e_partida_como_se_espera() {
        let t = Target::parse("https://api.bcb.gov.br/dados/serie?formato=json").unwrap();
        assert_eq!(t.host, "api.bcb.gov.br");
        assert_eq!(t.port, 443);
        assert_eq!(t.path, "/dados/serie?formato=json");
        assert!(t.tls);

        let t = Target::parse("http://127.0.0.1:8080/x").unwrap();
        assert_eq!(t.port, 8080);
        assert!(!t.tls);

        assert!(Target::parse("api.example.com/x").is_err());
        // A mensagem **não** pode conter a URL: ela carrega o token na query, e vai para
        // a tela, para o log e para um relato de problema.
        let Err(erro) = Target::parse("brapi.dev/api/quote/PETR4?token=segredo") else {
            panic!("um endereço sem esquema tem que ser recusado");
        };
        assert!(
            !erro.frase().contains("segredo"),
            "a mensagem vazou a query: «{}»",
            erro.frase()
        );
        assert!(Target::parse("ftp://example.com").is_err());
    }

    #[test]
    fn redirecionamento_relativo_vira_absoluto() {
        let de = Target::parse("https://example.com/a/b").unwrap();
        let para = resolver_destino(&de, "/c").unwrap();
        assert_eq!(para.host, "example.com");
        assert_eq!(para.path, "/c");
    }

    #[test]
    fn deschunk_remonta_o_corpo() {
        let cru = b"4\r\nJSON\r\n3\r\n123\r\n0\r\n\r\n";
        assert_eq!(deschunk(cru).unwrap(), b"JSON123");
    }

    #[test]
    fn deschunk_recusa_tamanho_mentiroso() {
        // Um chunk que diz ser maior do que o que chegou é corpo truncado, e tratá-lo
        // como válido entregaria meio JSON como se fosse inteiro.
        let cru = b"FF\r\nabc\r\n";
        assert!(matches!(deschunk(cru), Err(FeedError::Formato(_))));
    }

    #[test]
    fn cabecalho_e_lido_sem_diferenciar_maiuscula() {
        let h = "HTTP/1.1 200 OK\r\nContent-Length: 12\r\nX-MBX-USED-WEIGHT-1m: 40";
        assert_eq!(status_de(h).unwrap(), 200);
        assert_eq!(header(h, "content-length").as_deref(), Some("12"));
        assert_eq!(header(h, "x-mbx-used-weight-1m").as_deref(), Some("40"));
        assert_eq!(header(h, "nao-existe"), None);
    }

    #[test]
    fn erros_dizem_se_vale_insistir() {
        assert!(
            FeedError::Cota {
                retry_after: Some(30)
            }
            .desiste()
        );
        assert!(FeedError::Formato("x".into()).desiste());
        assert!(
            !FeedError::Status {
                code: 500,
                detalhe: None
            }
            .desiste()
        );
        assert!(!FeedError::Tempo.desiste());
    }
}

#[cfg(test)]
mod detalhe_tests {
    use super::detalhe_do_corpo;

    #[test]
    fn a_explicacao_da_recusa_vem_do_corpo() {
        // Sem isto, uma recusa de plano virava «respondeu 400» e ninguém sabia por quê.
        let corpo =
            r#"{"error":true,"message":"Seu plano permite no máximo 1 ativo(s) por requisição."}"#;
        let d = detalhe_do_corpo(corpo.as_bytes()).unwrap();
        assert!(d.contains("no máximo 1"));
    }

    #[test]
    fn corpo_que_nao_e_json_ainda_diz_alguma_coisa() {
        assert_eq!(
            detalhe_do_corpo(b"Too Many Requests").as_deref(),
            Some("Too Many Requests")
        );
        assert_eq!(detalhe_do_corpo(b"   "), None);
        assert_eq!(detalhe_do_corpo(b""), None);
    }

    #[test]
    fn detalhe_longo_e_truncado() {
        let longo = vec![b'x'; 1000];
        assert!(detalhe_do_corpo(&longo).unwrap().len() <= 160);
    }
}

#[cfg(test)]
mod compressao_tests {
    use super::*;

    fn comprimir(dados: &[u8]) -> Vec<u8> {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let mut p = Command::new("gzip")
            .arg("-c")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("gzip precisa existir para este teste");
        p.stdin.take().unwrap().write_all(dados).unwrap();
        p.wait_with_output().unwrap().stdout
    }

    fn ler_resposta(resposta: &[u8]) -> Result<Vec<u8>, FeedError> {
        match ler(&mut std::io::Cursor::new(resposta.to_vec()))? {
            Buscado::Corpo(r) => Ok(r.body),
            Buscado::Redireciona(_) => panic!("não era para redirecionar"),
        }
    }

    #[test]
    fn um_corpo_comprimido_chega_legivel_ate_quem_pediu() {
        // O caso do G1: responde gzip tendo recebido `Accept-Encoding: identity`. Antes
        // isto virava «respondeu sem itens legíveis», que manda procurar o problema no
        // feed em vez de no cliente.
        let xml = b"<?xml version=\"1.0\"?><rss><channel><item>ol\xc3\xa1</item></channel></rss>";
        let corpo = comprimir(xml);
        let mut resposta = format!(
            "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\n\r\n",
            corpo.len()
        )
        .into_bytes();
        resposta.extend_from_slice(&corpo);
        assert_eq!(ler_resposta(&resposta).unwrap(), xml);
    }

    #[test]
    fn comprimido_dentro_de_chunked_sai_dos_dois_na_ordem_certa() {
        // A ordem importa: o gzip está **dentro** dos pedaços. Inflar antes de desmontar
        // o `chunked` infla os cabeçalhos de tamanho junto, e não descomprime nada.
        let texto = "<rss>".to_string() + &"<item>a</item>".repeat(500) + "</rss>";
        let corpo = comprimir(texto.as_bytes());
        let (a, b) = corpo.split_at(corpo.len() / 2);
        let mut resposta = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
        for pedaco in [a, b] {
            resposta.extend_from_slice(format!("{:x}\r\n", pedaco.len()).as_bytes());
            resposta.extend_from_slice(pedaco);
            resposta.extend_from_slice(b"\r\n");
        }
        resposta.extend_from_slice(b"0\r\n\r\n");
        assert_eq!(ler_resposta(&resposta).unwrap(), texto.as_bytes());
    }

    #[test]
    fn texto_puro_continua_passando_intacto() {
        // A detecção olha os bytes, não o cabeçalho. Um XML nunca começa com 1f 8b 08, e
        // este teste é o que garante que a heurística não morde o caso comum.
        let xml = b"<?xml version=\"1.0\"?><rss/>";
        let mut resposta =
            format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", xml.len()).into_bytes();
        resposta.extend_from_slice(xml);
        assert_eq!(ler_resposta(&resposta).unwrap(), xml);
    }
}
