//! Cripto pela Binance: pública, gratuita, sem chave, sem cadastro.
//!
//! É a única fonte da aba com dado ao vivo de verdade e de graça, e por isso é onde a
//! arquitetura de provedores é exercitada por inteiro. Também é a única que **se
//! auto-reporta**: a resposta traz `X-MBX-USED-WEIGHT-1m`, então o limitador lê quanto
//! gastou em vez de adivinhar.

use std::time::Duration;

use serde_json::Value;

use crate::invest::feed::{self, FeedError};
use crate::invest::model::{AssetId, Market, Moeda};
use crate::invest::provider::{Candle, Grade, Provider, Quote, Span};
use crate::invest::store;

/// A Binance é a única fonte da aba que **se auto-reporta**: a resposta traz
/// `X-MBX-USED-WEIGHT-1m`. O limitador lê esse número em vez de adivinhar.
#[derive(Default)]
pub struct Binance {
    peso: std::sync::atomic::AtomicU64,
}

/// As moedas em que um par pode terminar, da mais longa para a mais curta — a ordem
/// importa: `BTCUSDT` termina em `USDT` e também em `USD`, e responder «USD» daria a um
/// saldo em USDT o nome errado.
const SUFIXOS: &[(&str, Moeda)] = &[
    ("USDT", Moeda::Usdt),
    ("BRL", Moeda::Brl),
    ("EUR", Moeda::Eur),
    ("USD", Moeda::Usd),
];

/// A moeda em que o par cota, lida do sufixo do símbolo.
pub fn moeda_do_par(simbolo: &str) -> Moeda {
    SUFIXOS
        .iter()
        .find(|(s, _)| simbolo.ends_with(s))
        .map(|(_, m)| *m)
        // Um par com sufixo que não conhecemos é cotado em algo que não sabemos converter.
        // USDT é o palpite menos errado, e o valor aparece com a moeda escrita ao lado.
        .unwrap_or(Moeda::Usdt)
}

/// Percent-encoding do que a Binance espera no parâmetro `symbols`, que é um JSON dentro
/// de uma query string.
fn encode(bruto: &str) -> String {
    bruto
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            outro => format!("%{outro:02X}"),
        })
        .collect()
}

fn numero(v: &Value, campo: &str) -> Option<f64> {
    v.get(campo)?.as_str()?.parse().ok()
}

impl Provider for Binance {
    fn id(&self) -> &'static str {
        "binance"
    }

    fn name(&self) -> &'static str {
        "Binance"
    }

    fn covers(&self, ativo: &AssetId) -> bool {
        ativo.market == Market::Binance
    }

    /// Nunca fecha. É o único provedor da aba de que isso é verdade.
    fn is_open(&self, _ativo: &AssetId, _agora: u64) -> bool {
        true
    }

    fn quotes(&self, ativos: &[AssetId]) -> Result<Vec<Quote>, FeedError> {
        if ativos.is_empty() {
            return Ok(Vec::new());
        }
        // Em lote, sempre: a API cobra por requisição e não por símbolo, então trinta
        // preços em trinta chamadas seriam trinta vezes a cota pelo mesmo dado.
        let lista = ativos
            .iter()
            .map(|a| format!("\"{}\"", a.symbol))
            .collect::<Vec<_>>()
            .join(",");
        let url = format!(
            "https://api.binance.com/api/v3/ticker/24hr?symbols={}",
            encode(&format!("[{lista}]"))
        );
        let resposta = feed::get(&url)?;
        if let Some(peso) = resposta.used_weight {
            self.peso.store(peso, std::sync::atomic::Ordering::Relaxed);
        }
        let valor: Value = serde_json::from_slice(&resposta.body)
            .map_err(|e| FeedError::Formato(e.to_string()))?;
        let itens = valor
            .as_array()
            .ok_or_else(|| FeedError::Formato("esperava uma lista de tickers".into()))?;

        let agora = store::agora();
        let mut saida = Vec::new();
        for item in itens {
            let Some(simbolo) = item.get("symbol").and_then(|s| s.as_str()) else {
                continue;
            };
            let Some(preco) = numero(item, "lastPrice") else {
                continue;
            };
            let ativo = AssetId::new(Market::Binance, simbolo);
            saida.push(Quote {
                fonte: "binance",
                moeda: moeda_do_par(&ativo.symbol),
                ativo,
                preco,
                // `openPrice` é a abertura da janela de 24 h, que é a referência que a
                // própria Binance usa para a variação que ela publica.
                anterior: numero(item, "openPrice").or_else(|| numero(item, "prevClosePrice")),
                grade: Grade::AoVivo,
                em: agora,
                volume: numero(item, "quoteVolume"),
                max24: numero(item, "highPrice"),
                min24: numero(item, "lowPrice"),
            });
        }
        Ok(saida)
    }

    fn history(&self, ativo: &AssetId, span: Span) -> Option<Result<Vec<Candle>, FeedError>> {
        if !self.covers(ativo) {
            return None;
        }
        // Cada janela pede uma granularidade diferente: um dia em velas diárias é um
        // ponto só, e cinco anos em velas de cinco minutos são meio milhão de pontos.
        let (intervalo, limite) = match span {
            Span::Dia => ("5m", 288),
            Span::Semana => ("1h", 120),
            Span::Mes => ("4h", 180),
            Span::SeisMeses => ("1d", 182),
            Span::Ano => ("1d", 365),
            Span::CincoAnos => ("1w", 260),
        };
        let url = format!(
            "https://api.binance.com/api/v3/klines?symbol={}&interval={intervalo}&limit={limite}",
            ativo.symbol
        );
        Some(buscar_klines(&url))
    }

    fn tem_historico(&self) -> bool {
        true
    }

    fn intervalo(&self) -> Duration {
        Duration::from_secs(2)
    }

    fn peso_usado(&self) -> Option<u64> {
        match self.peso.load(std::sync::atomic::Ordering::Relaxed) {
            0 => None,
            p => Some(p),
        }
    }
}

fn buscar_klines(url: &str) -> Result<Vec<Candle>, FeedError> {
    let valor = feed::get_json(url)?;
    let linhas = valor
        .as_array()
        .ok_or_else(|| FeedError::Formato("esperava uma lista de velas".into()))?;
    let mut saida = Vec::with_capacity(linhas.len());
    for linha in linhas {
        // Uma vela é um array posicional: [abertura, open, high, low, close, ...]. O
        // fechamento é o índice 4 e o instante de abertura é o 0, em milissegundos.
        let Some(campos) = linha.as_array() else {
            continue;
        };
        let (Some(em), Some(fechamento)) = (
            campos.first().and_then(|v| v.as_u64()),
            campos
                .get(4)
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok()),
        ) else {
            continue;
        };
        saida.push(Candle {
            em: em / 1000,
            fechamento,
        });
    }
    match saida.is_empty() {
        true => Err(FeedError::Formato("nenhuma vela utilizável".into())),
        false => Ok(saida),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sufixo_mais_longo_ganha() {
        // BTCUSDT termina em USDT e também em USD. Responder «USD» daria a um saldo em
        // USDT o nome errado.
        assert_eq!(moeda_do_par("BTCUSDT"), Moeda::Usdt);
        assert_eq!(moeda_do_par("BTCBRL"), Moeda::Brl);
        assert_eq!(moeda_do_par("BTCEUR"), Moeda::Eur);
    }

    #[test]
    fn encode_escapa_o_json_da_query() {
        assert_eq!(encode("[\"BTCBRL\"]"), "%5B%22BTCBRL%22%5D");
        assert_eq!(encode("ABC-123_x.y~z"), "ABC-123_x.y~z");
    }

    #[test]
    fn nao_cobre_o_que_nao_e_dele() {
        assert!(Binance::default().covers(&AssetId::new(Market::Binance, "BTCBRL")));
        assert!(!Binance::default().covers(&AssetId::new(Market::B3, "PETR4")));
    }
}
