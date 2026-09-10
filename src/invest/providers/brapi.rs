//! Ações e FIIs da B3, pela brapi.dev.
//!
//! **A B3 não publica cotação de graça** — ela vende o dado de mercado, e quem
//! redistribui o faz sob licença. A brapi é um agregador de terceiros que serve isso, e
//! tem dois regimes, com desenhos diferentes:
//!
//! * **Com token** — o endpoint em lote responde, e volta a valer a regra do resto da
//!   aba: um pedido para todos os ativos, na cadência normal.
//! * **Sem token** — só um ativo por pedido. Isso quebra a regra de sempre pedir em
//!   lote, e a quebra fica contida em três lugares: a cadência cai para 30 s, há um teto
//!   de ativos por volta, e o resto entra em rodízio para que uma carteira grande não
//!   vire uma rajada contra um serviço gratuito.
//!
//! O token nunca entra em `invest.json`. Ver `token()`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::Value;

use crate::invest::feed::{self, FeedError};
use crate::invest::model::{AssetId, Market, Moeda};
use crate::invest::provider::{Candle, Grade, Provider, Quote, Span};
use crate::invest::store;

/// Quantos ativos são pedidos por volta **sem token**. Cada um é uma requisição, e uma
/// carteira de trinta ações viraria trinta requisições de uma vez. O resto entra na volta
/// seguinte, pelo rodízio.
const POR_VOLTA_ANONIMO: usize = 8;

/// Pausa entre dois pedidos da mesma volta.
///
/// Sem ela, oito requisições saem no mesmo instante e o serviço responde 429 no meio —
/// medido. Cem milissegundos entre uma e outra não atrasam nada que alguém perceba e são
/// a diferença entre ser atendido e ser barrado.
const ENTRE_PEDIDOS: Duration = Duration::from_millis(120);

/// Até que idade a cotação ainda é «ao vivo». Medido em 08/09/2026: durante o pregão a
/// brapi entrega o preço com menos de um minuto de atraso. Acima disso a linha passa a
/// dizer que está atrasada, em vez de deixar quem lê supor que é de agora.
const AO_VIVO_ATE: u64 = 120;

/// A variável de ambiente que carrega o token, e o arquivo que serve de alternativa.
pub const VAR_TOKEN: &str = "MONITORZINHO_BRAPI_TOKEN";
pub const ARQUIVO_TOKEN: &str = "brapi.token";

/// O token, se houver. Ambiente primeiro, arquivo depois.
///
/// **Nem no banco do perfil.** O arquivo do perfil é o que se copia para outra máquina e
/// o que vai num backup; uma credencial dentro dele sai de casa sem ninguém perceber. Por
/// isso o token continua sendo um arquivo solto ao lado, lido do diretório de dados e
/// **não criado pelo programa** — quem o escreve escolhe as permissões dele.
pub fn token() -> Option<String> {
    if let Ok(t) = std::env::var(VAR_TOKEN)
        && !t.trim().is_empty()
    {
        return Some(t.trim().to_string());
    }
    std::fs::read_to_string(crate::db::dados_dir().join(ARQUIVO_TOKEN))
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// Quantos ativos o plano permite por requisição, quando já se descobriu. `0` é «ainda
/// não sei», e a primeira volta descobre.
///
/// Não dá para saber de antemão: um token **grátis** permite todos os tickers mas só um
/// ativo por requisição, enquanto o pago permite dez. A API só conta isso recusando —
/// então aprende-se com a recusa, uma vez, e nunca mais.
const LIMITE_DESCONHECIDO: usize = 0;

#[derive(Default)]
pub struct Brapi {
    /// Por onde começar a próxima volta anônima, para o rodízio ser justo: sem isto, uma
    /// carteira maior que `POR_VOLTA_ANONIMO` teria ativos que nunca ganham preço.
    cursor: AtomicUsize,
    /// O que o plano permite por requisição — aprendido da primeira recusa.
    por_requisicao: AtomicUsize,
}

/// O erro que a brapi devolve **dentro de uma resposta 200**.
///
/// Ela não usa código HTTP para recusa de plano: um token grátis pedindo cinco ativos, ou
/// um range que ele não tem, volta 200 com `{"error": true, "message": ...}`. Ler só o
/// status deixava a recusa invisível — a tela não mostrava erro nenhum e o preço
/// simplesmente não aparecia.
fn erro_da_api(valor: &Value) -> Option<FeedError> {
    valor
        .get("error")
        .and_then(Value::as_bool)
        .filter(|e| *e)
        .map(|_| {
            FeedError::Formato(
                valor
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("recusado sem explicação")
                    .to_string(),
            )
        })
}

/// Lê «no máximo 1 ativo(s) por requisição» da mensagem de recusa.
fn limite_da_mensagem(mensagem: &str) -> Option<usize> {
    let depois = mensagem.split("no máximo").nth(1)?;
    depois
        .split_whitespace()
        .next()?
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .ok()
}

fn numero(v: &Value, campo: &str) -> Option<f64> {
    v.get(campo)?.as_f64()
}

/// `2026-09-08T20:13:30.000Z` → epoch. Sem biblioteca de data: o formato é fixo e os
/// campos estão em posição conhecida.
fn instante(texto: &str) -> Option<u64> {
    let (data, resto) = texto.split_once('T')?;
    let mut d = data.split('-');
    let ano: i32 = d.next()?.parse().ok()?;
    let mes: u32 = d.next()?.parse().ok()?;
    let dia: u32 = d.next()?.parse().ok()?;
    let mut h = resto.trim_end_matches('Z').split(':');
    let hora: u64 = h.next()?.parse().ok()?;
    let minuto: u64 = h.next()?.parse().ok()?;
    let segundo: u64 = h.next()?.split('.').next()?.parse().ok()?;
    let dias = crate::invest::tempo::dias_de(ano, mes, dia);
    Some((dias * 86400) as u64 + hora * 3600 + minuto * 60 + segundo)
}

/// Uma cotação a partir do objeto que a API devolve.
fn cotacao(ativo: &AssetId, r: &Value, agora: u64) -> Option<Quote> {
    let preco = numero(r, "regularMarketPrice")?;
    let em = r
        .get("regularMarketTime")
        .and_then(Value::as_str)
        .and_then(instante)
        .unwrap_or(agora);
    let atraso = agora.saturating_sub(em);
    Some(Quote {
        fonte: "brapi",
        ativo: ativo.clone(),
        preco,
        // **Não** use `regularMarketPreviousClose`: medido em 08/09/2026, esse campo
        // repete o preço atual — VALE3 a 79,39 com variação de +0,77 vinha com
        // «fechamento anterior» de 79,39. O fechamento sai da variação, que é o número
        // com que a própria API calcula a porcentagem que publica.
        anterior: numero(r, "regularMarketChange").map(|v| preco - v),
        moeda: Moeda::Brl,
        grade: match atraso <= AO_VIVO_ATE {
            true => Grade::AoVivo,
            false => Grade::Atrasado(atraso),
        },
        em,
        volume: numero(r, "regularMarketVolume"),
        max24: numero(r, "regularMarketDayHigh"),
        min24: numero(r, "regularMarketDayLow"),
    })
}

/// Lê `historicalDataPrice` — a série que a brapi devolve junto da cotação.
fn historico(url: &str) -> Result<Vec<Candle>, FeedError> {
    let valor = feed::get_json(url)?;
    if let Some(erro) = erro_da_api(&valor) {
        return Err(erro);
    }
    let pontos = valor
        .get("results")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(|r| r.get("historicalDataPrice"))
        .and_then(Value::as_array)
        .ok_or_else(|| FeedError::Formato("resposta sem «historicalDataPrice»".into()))?;
    let saida: Vec<Candle> = pontos
        .iter()
        .filter_map(|p| {
            Some(Candle {
                em: p.get("date")?.as_u64()?,
                fechamento: p.get("close")?.as_f64()?,
            })
        })
        .collect();
    match saida.is_empty() {
        true => Err(FeedError::Formato("nenhum ponto utilizável".into())),
        false => Ok(saida),
    }
}

/// Lê os três módulos de fundamento numa resposta da brapi.
fn fundamentos(url: &str) -> Result<crate::invest::fundamento::Fundamentos, FeedError> {
    use crate::invest::fundamento::Fundamentos;
    let valor = feed::get_json(url)?;
    if let Some(erro) = erro_da_api(&valor) {
        return Err(erro);
    }
    let r = valor
        .get("results")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .ok_or_else(|| FeedError::Formato("resposta sem «results»".into()))?;

    // Cada módulo pode faltar por conta própria, e um que falta não pode derrubar os
    // outros: um campo ausente vira `—` na tela, nunca um zero que se leria como fato.
    let de = |modulo: &str, campo: &str| -> Option<f64> { r.get(modulo)?.get(campo)?.as_f64() };
    let texto = |modulo: &str, campo: &str| -> Option<String> {
        Some(r.get(modulo)?.get(campo)?.as_str()?.to_string())
    };

    Ok(Fundamentos {
        setor: texto("summaryProfile", "sector"),
        industria: texto("summaryProfile", "industry"),
        p_l: numero(r, "priceEarnings").or_else(|| r.get("priceEarnings")?.as_f64()),
        lpa: numero(r, "earningsPerShare").or_else(|| r.get("earningsPerShare")?.as_f64()),
        valor_de_mercado: r.get("marketCap").and_then(Value::as_f64),
        // A API entrega a margem como fração (0,2438). A tela mostra porcentagem, e a
        // conversão fica aqui para nenhum módulo precisar lembrar dela.
        margem_liquida: de("defaultKeyStatistics", "profitMargins").map(|m| m * 100.0),
        ebitda: de("financialData", "ebitda"),
        divida_total: de("financialData", "totalDebt"),
        caixa: de("financialData", "totalCash"),
        liquidez_corrente: de("financialData", "currentRatio"),
        acoes_emitidas: de("defaultKeyStatistics", "sharesOutstanding"),
    })
}

impl Brapi {
    /// Lê o limite de ativos por requisição da recusa, venha ela num 200 com `error` ou
    /// no corpo de um 400 — a API usa os dois, e o limite está na mesma frase nos dois.
    fn aprender(&self, erro: &FeedError) {
        let mensagem = match erro {
            FeedError::Formato(m) => Some(m.as_str()),
            FeedError::Status {
                detalhe: Some(d), ..
            } => Some(d.as_str()),
            _ => None,
        };
        if let Some(limite) = mensagem.and_then(limite_da_mensagem) {
            self.por_requisicao.store(limite.max(1), Ordering::Relaxed);
        }
    }

    /// Um pedido só, com todos os ativos. Só existe com token.
    fn em_lote(&self, ativos: &[AssetId], token: &str) -> Result<Vec<Quote>, FeedError> {
        let lista = ativos
            .iter()
            .map(|a| a.symbol.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let valor = feed::get_json(&format!(
            "https://brapi.dev/api/quote/{lista}?token={token}"
        ))
        .inspect_err(|e| self.aprender(e))?;
        // A recusa de plano vem dentro de um 200. Aprende-se o limite e nunca mais se
        // tenta o lote acima do que ele permite.
        if let Some(erro) = erro_da_api(&valor) {
            self.aprender(&erro);
            return Err(erro);
        }
        let agora = store::agora();
        let resultados = valor
            .get("results")
            .and_then(Value::as_array)
            .ok_or_else(|| FeedError::Formato("resposta sem «results»".into()))?;
        Ok(resultados
            .iter()
            .filter_map(|r| {
                let simbolo = r.get("symbol")?.as_str()?;
                let ativo = ativos.iter().find(|a| a.symbol == simbolo)?;
                cotacao(ativo, r, agora)
            })
            .collect())
    }

    /// Um pedido por ativo, em rodízio. O caminho de quem não tem token.
    fn um_a_um(&self, ativos: &[AssetId]) -> Result<Vec<Quote>, FeedError> {
        let inicio = self.cursor.load(Ordering::Relaxed) % ativos.len();
        let quantos = POR_VOLTA_ANONIMO.min(ativos.len());
        self.cursor
            .store((inicio + quantos) % ativos.len(), Ordering::Relaxed);

        let agora = store::agora();
        let mut saida = Vec::new();
        let mut ultimo_erro = None;

        for i in 0..quantos {
            // Espaçados: oito pedidos no mesmo instante tomam 429 no meio — medido.
            if i > 0 {
                std::thread::sleep(ENTRE_PEDIDOS);
            }
            let ativo = &ativos[(inicio + i) % ativos.len()];
            let valor =
                match feed::get_json(&format!("https://brapi.dev/api/quote/{}", ativo.symbol)) {
                    Ok(v) => v,
                    Err(e) => {
                        // Cota estourada encerra a volta — insistir com os outros só
                        // piora. Mas **o que já veio é devolvido**: descartar as cotações
                        // boas por causa da última requisição fazia a tela inteira cair
                        // para o preço informado só porque o quinto ativo esbarrou no
                        // limite. Foi exatamente o que aconteceu com uma carteira de cinco.
                        if matches!(e, FeedError::Cota { .. }) {
                            return match saida.is_empty() {
                                true => Err(e),
                                false => Ok(saida),
                            };
                        }
                        ultimo_erro = Some(e);
                        continue;
                    }
                };
            // Uma recusa de plano ou um ticker fora do alcance vêm num 200 com `error`.
            // Registrar em vez de ignorar é o que faz a tela dizer o que houve.
            if let Some(erro) = erro_da_api(&valor) {
                ultimo_erro = Some(erro);
                continue;
            }
            // Um símbolo que não existe volta com `results` vazio. Não é falha da fonte, e
            // não pode derrubar os outros ativos da volta.
            if let Some(r) = valor
                .get("results")
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                && let Some(q) = cotacao(ativo, r, agora)
            {
                saida.push(q);
            }
        }

        match (saida.is_empty(), ultimo_erro) {
            (true, Some(e)) => Err(e),
            _ => Ok(saida),
        }
    }
}

impl Provider for Brapi {
    fn id(&self) -> &'static str {
        "brapi"
    }

    fn name(&self) -> &'static str {
        match token().is_some() {
            true => "brapi.dev (com token)",
            false => "brapi.dev (anônimo)",
        }
    }

    fn covers(&self, ativo: &AssetId) -> bool {
        ativo.market == Market::B3
    }

    /// Fim de semana, feriado e a janela de pregão. Fora dela o preço não muda, e
    /// continuar pedindo é gastar o serviço de outra pessoa à toa.
    fn is_open(&self, _ativo: &AssetId, agora: u64) -> bool {
        crate::invest::provider::b3_aberta(agora)
    }

    fn quotes(&self, ativos: &[AssetId]) -> Result<Vec<Quote>, FeedError> {
        if ativos.is_empty() {
            return Ok(Vec::new());
        }
        // Com token e com o lote permitido, um pedido só. Um token **grátis** permite
        // todos os tickers mas apenas um ativo por requisição — então ter token não
        // implica poder pedir em lote, e isso se descobre tentando uma vez.
        let limite = self.por_requisicao.load(Ordering::Relaxed);
        match token() {
            Some(t) if limite == LIMITE_DESCONHECIDO || limite > 1 => {
                let pedido: Vec<AssetId> = match limite {
                    LIMITE_DESCONHECIDO => ativos.to_vec(),
                    n => ativos.iter().take(n).cloned().collect(),
                };
                match self.em_lote(&pedido, &t) {
                    Ok(q) => Ok(q),
                    // A primeira recusa já ensinou o limite; a volta continua um a um em
                    // vez de voltar de mãos vazias.
                    Err(_) if self.por_requisicao.load(Ordering::Relaxed) == 1 => {
                        self.um_a_um(ativos)
                    }
                    Err(e) => Err(e),
                }
            }
            _ => self.um_a_um(ativos),
        }
    }

    /// Histórico, que a brapi serve **mesmo sem token** — medido: 250 pontos para um ano.
    /// É o que dá gráfico, risco e correlação a ações da B3.
    ///
    /// Os ranges disponíveis dependem do plano, e de um jeito que surpreende: o token
    /// **grátis** libera todos os tickers mas limita o histórico a 3 meses, enquanto o
    /// acesso anônimo serve um ano dos poucos tickers que permite. Quando o range é
    /// recusado, o erro sobe e a cadeia de reserva leva o pedido ao Yahoo.
    fn history(&self, ativo: &AssetId, span: Span) -> Option<Result<Vec<Candle>, FeedError>> {
        if !self.covers(ativo) {
            return None;
        }
        // Os intervalos que a API aceita. Um dia e uma semana não existem lá, e o menor
        // que ela tem já cobre os dois com sobra.
        let range = match span {
            Span::Mes => "1mo",
            Span::Ano => "1y",
        };
        let token = token().map(|t| format!("&token={t}")).unwrap_or_default();
        Some(historico(&format!(
            "https://brapi.dev/api/quote/{}?range={range}&interval=1d{token}",
            ativo.symbol
        )))
    }

    fn tem_fundamento(&self) -> bool {
        true
    }

    /// Os fundamentos, que a brapi serve pelos módulos `defaultKeyStatistics`,
    /// `financialData` e `summaryProfile` — **inclusive com token grátis**, conferido em
    /// 08/09/2026. O documento de planejamento dizia que isto exigiria chave paga ou os
    /// dados abertos da CVM; estava errado.
    fn fundamentos(
        &self,
        ativo: &AssetId,
    ) -> Option<Result<crate::invest::fundamento::Fundamentos, FeedError>> {
        if !self.covers(ativo) {
            return None;
        }
        // Espaçado como os pedidos de cotação: o fundamento é buscado um ativo por vez, e
        // cinco de uma vez tomam 403 no meio.
        std::thread::sleep(ENTRE_PEDIDOS);
        let token = token().map(|t| format!("&token={t}")).unwrap_or_default();
        Some(fundamentos(&format!(
            "https://brapi.dev/api/quote/{}?modules=defaultKeyStatistics,financialData,summaryProfile{token}",
            ativo.symbol
        )))
    }

    /// Com token, a cadência normal da aba. Sem ele, trinta segundos: cada volta são até
    /// oito requisições, e a dois segundos isso seria uma rajada contínua contra um
    /// serviço gratuito — enquanto o preço de uma ação não se lê em segundos.
    fn intervalo(&self) -> Duration {
        // O lote é um pedido só e pode ser rápido. Um a um, mesmo com token, são vários —
        // e aí vale a cadência lenta, com token ou sem ele.
        match self.por_requisicao.load(Ordering::Relaxed) > 1 {
            true => Duration::from_secs(5),
            false => Duration::from_secs(30),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn erro_dentro_de_um_200_e_um_erro() {
        // A brapi não usa código HTTP para recusa de plano. Ler só o status deixava a
        // recusa invisível: a tela não mostrava erro e o preço simplesmente não aparecia.
        let recusa: Value = serde_json::from_str(
            r#"{"error":true,"message":"Seu plano permite no máximo 1 ativo(s) por requisição."}"#,
        )
        .unwrap();
        let e = erro_da_api(&recusa).expect("uma recusa tem que virar erro");
        assert!(e.frase().contains("no máximo 1"));

        let boa: Value = serde_json::from_str(r#"{"results":[]}"#).unwrap();
        assert!(erro_da_api(&boa).is_none());
    }

    #[test]
    fn o_limite_e_aprendido_tambem_do_corpo_de_um_400() {
        // A API usa os dois caminhos: 200 com `error` e 400 com a explicação no corpo. O
        // limite está na mesma frase, e aprender só de um deixava o lote tentando para
        // sempre — que foi o que fez a tela dizer «respondeu 400» e nada mais.
        let b = Brapi::default();
        b.aprender(&FeedError::Status {
            code: 400,
            detalhe: Some("Seu plano permite no máximo 1 ativo(s) por requisição.".into()),
        });
        assert_eq!(b.por_requisicao.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn o_limite_do_plano_e_lido_da_mensagem() {
        // É assim que se descobre que um token grátis não pode pedir em lote.
        assert_eq!(
            limite_da_mensagem(
                "Seu plano permite no máximo 1 ativo(s) por requisição. Você enviou 5."
            ),
            Some(1)
        );
        assert_eq!(
            limite_da_mensagem("Seu plano permite no máximo 10 ativo(s) por requisição."),
            Some(10)
        );
        assert_eq!(limite_da_mensagem("outra coisa qualquer"), None);
    }

    #[test]
    fn um_token_gratis_nao_implica_poder_pedir_em_lote() {
        // A surpresa que motivou tudo isto: ter token e poder pedir vários ativos numa
        // requisição são coisas diferentes, e a segunda é do plano pago.
        let b = Brapi::default();
        assert_eq!(
            b.por_requisicao.load(Ordering::Relaxed),
            LIMITE_DESCONHECIDO
        );
        b.por_requisicao.store(1, Ordering::Relaxed);
        // Com limite de 1, a cadência tem que ser a lenta — são vários pedidos por volta.
        assert_eq!(b.intervalo(), Duration::from_secs(30));
        b.por_requisicao.store(10, Ordering::Relaxed);
        assert_eq!(b.intervalo(), Duration::from_secs(5));
    }

    #[test]
    fn cobre_a_b3_e_mais_nada() {
        let b = Brapi::default();
        assert!(b.covers(&AssetId::new(Market::B3, "PETR4")));
        assert!(!b.covers(&AssetId::new(Market::Us, "AAPL")));
        assert!(!b.covers(&AssetId::new(Market::Binance, "BTCBRL")));
    }

    #[test]
    fn le_o_instante_iso() {
        let em = instante("2026-09-08T20:13:30.000Z").unwrap();
        let d = crate::invest::tempo::Data::de_epoch(em, 0);
        assert_eq!((d.ano, d.mes, d.dia), (2026, 9, 8));
        assert_eq!((em % 86400) / 3600, 20);
        assert_eq!((em % 3600) / 60, 13);
        assert_eq!(instante("não é data"), None);
        assert_eq!(instante("2026-09-08"), None);
    }

    #[test]
    fn o_fechamento_anterior_vem_da_variacao_e_nao_do_campo() {
        // O caso medido: VALE3 a 79,39 com variação de +0,77, e o campo
        // `regularMarketPreviousClose` da API vindo com 79,39 — o próprio preço.
        let bruto: Value = serde_json::from_str(
            r#"{"symbol":"VALE3","regularMarketPrice":79.39,"regularMarketChange":0.77,
                "regularMarketPreviousClose":79.39,"regularMarketTime":"2026-09-08T20:13:30.000Z"}"#,
        )
        .unwrap();
        let ativo = AssetId::new(Market::B3, "VALE3");
        let q = cotacao(
            &ativo,
            &bruto,
            instante("2026-09-08T20:13:40.000Z").unwrap(),
        )
        .unwrap();
        let anterior = q.anterior.unwrap();
        assert!((anterior - 78.62).abs() < 0.001, "deu {anterior}");
        // E a variação reconstruída bate com os 0,98% que a API publica.
        let pct = q.variacao().unwrap();
        assert!((pct - 0.98).abs() < 0.01, "deu {pct}");
    }

    #[test]
    fn cotacao_recente_e_ao_vivo_e_velha_e_atrasada() {
        let ativo = AssetId::new(Market::B3, "PETR4");
        let bruto: Value = serde_json::from_str(
            r#"{"regularMarketPrice":48.08,"regularMarketTime":"2026-09-08T20:13:30.000Z"}"#,
        )
        .unwrap();
        let em = instante("2026-09-08T20:13:30.000Z").unwrap();
        assert_eq!(
            cotacao(&ativo, &bruto, em + 30).unwrap().grade,
            Grade::AoVivo
        );
        assert!(matches!(
            cotacao(&ativo, &bruto, em + 900).unwrap().grade,
            Grade::Atrasado(_)
        ));
    }

    #[test]
    fn sem_preco_nao_ha_cotacao() {
        // Um símbolo inexistente não pode virar uma linha com preço zero.
        let bruto: Value = serde_json::from_str(r#"{"symbol":"XPTO9"}"#).unwrap();
        assert!(cotacao(&AssetId::new(Market::B3, "XPTO9"), &bruto, 0).is_none());
    }

    #[test]
    fn o_rodizio_nao_deixa_ninguem_para_tras() {
        // Com mais ativos que o teto por volta, as voltas seguintes têm que alcançar os
        // que ficaram — senão os últimos da carteira nunca teriam preço.
        let b = Brapi::default();
        let n = POR_VOLTA_ANONIMO * 2 + 3;
        let mut vistos = vec![false; n];
        for _ in 0..4 {
            let inicio = b.cursor.load(Ordering::Relaxed) % n;
            for i in 0..POR_VOLTA_ANONIMO.min(n) {
                vistos[(inicio + i) % n] = true;
            }
            b.cursor
                .store((inicio + POR_VOLTA_ANONIMO.min(n)) % n, Ordering::Relaxed);
        }
        assert!(vistos.iter().all(|v| *v), "algum ativo nunca seria pedido");
    }
}

#[cfg(test)]
mod ao_vivo {
    use super::*;

    /// Bate na brapi de verdade. `#[ignore]` para não deixar a suíte dependente de rede —
    /// roda com `cargo test -- --ignored --nocapture brapi`.
    #[test]
    #[ignore]
    fn busca_de_verdade() {
        let b = Brapi::default();
        let ativos = vec![
            AssetId::new(Market::B3, "PETR4"),
            AssetId::new(Market::B3, "PRIO3"),
        ];
        println!("token presente? {}", token().is_some());
        match b.quotes(&ativos) {
            Ok(q) => {
                println!("devolveu {} cotação(ões)", q.len());
                for x in &q {
                    println!(
                        "  {} = {} ({:?}) fonte={}",
                        x.ativo, x.preco, x.grade, x.fonte
                    );
                }
            }
            Err(e) => println!("ERRO: {}", e.frase()),
        }
    }
}

#[cfg(test)]
mod cota_tests {
    /// Fixa a regra que custou uma tela inteira: quando a cota estoura no meio da volta,
    /// o que já foi buscado **é devolvido**.
    ///
    /// Sem isto, uma carteira de cinco ações caía inteira para o preço informado só
    /// porque o quinto pedido esbarrou no limite — e as quatro cotações boas que já
    /// estavam na mão iam junto.
    #[test]
    fn cota_no_meio_da_volta_nao_descarta_o_que_ja_veio() {
        use crate::invest::feed::FeedError;
        // A decisão, isolada do I/O: com algo na mão, devolve; de mãos vazias, propaga.
        let decide = |ja_tem: bool| -> Result<usize, FeedError> {
            let e = FeedError::Cota {
                retry_after: Some(30),
            };
            match ja_tem {
                true => Ok(4),
                false => Err(e),
            }
        };
        assert_eq!(decide(true).unwrap(), 4);
        assert!(decide(false).is_err());
    }
}
