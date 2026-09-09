//! B3 e ações americanas pelo endpoint de gráficos do Yahoo Finance.
//!
//! **É uma fonte não-oficial**, e isso não é um detalhe de rodapé: não há contrato, o
//! endpoint pode mudar sem aviso, e ele bloqueia por IP — medido em 08/09/2026, este
//! endereço recebeu **HTTP 429** numa primeira tentativa. Por isso ele é a **reserva**, e
//! não a fonte principal: entra quando a brapi não responde, que é o caso de quem não tem
//! token e esbarrou no limite anônimo dela.
//!
//! Toda linha que vem daqui carrega `fonte: "yahoo"`, e a tela mostra isso. Quem estiver
//! olhando um número precisa poder saber que ele veio de um lugar sem garantia nenhuma.

use std::time::Duration;

use serde_json::Value;

use crate::invest::feed::{self, FeedError};
use crate::invest::model::{AssetId, Market, Moeda};
use crate::invest::provider::{Candle, Grade, Provider, Quote, Span};
use crate::invest::store;

/// Pausa entre dois pedidos, pela mesma razão da brapi — e aqui mais forte, porque a
/// punição por insistir é o bloqueio do endereço e não um erro.
const ENTRE_PEDIDOS: Duration = Duration::from_millis(250);

/// Quantos ativos por volta. Como a brapi anônima, é um pedido por ativo — e aqui o
/// motivo para ir devagar é mais forte, porque a punição por insistir é o bloqueio.
const POR_VOLTA: usize = 4;

#[derive(Default)]
pub struct Yahoo {
    /// Por onde começar a próxima volta. Sem rodízio, com uma carteira maior que o teto,
    /// o último ativo nunca era buscado — e ficava sem preço para sempre.
    cursor: std::sync::atomic::AtomicUsize,
}

/// O símbolo como o Yahoo o chama: ação brasileira leva o sufixo `.SA`.
fn simbolo(ativo: &AssetId) -> String {
    match ativo.market {
        Market::B3 => format!("{}.SA", ativo.symbol),
        _ => ativo.symbol.clone(),
    }
}

/// Lê a série do endpoint de gráfico: os instantes vêm num vetor e os fechamentos noutro,
/// alinhados por posição — e um fechamento pode ser `null` num dia sem negócio, que é
/// descartado em vez de virar zero.
fn historico(url: &str) -> Result<Vec<Candle>, FeedError> {
    let valor = feed::get_json(url)?;
    let resultado = valor
        .get("chart")
        .and_then(|c| c.get("result"))
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .ok_or_else(|| FeedError::Formato("resposta sem «chart.result»".into()))?;
    let instantes = resultado
        .get("timestamp")
        .and_then(Value::as_array)
        .ok_or_else(|| FeedError::Formato("série sem «timestamp»".into()))?;
    let fechamentos = resultado
        .get("indicators")
        .and_then(|i| i.get("quote"))
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(|q| q.get("close"))
        .and_then(Value::as_array)
        .ok_or_else(|| FeedError::Formato("série sem «close»".into()))?;

    let saida: Vec<Candle> = instantes
        .iter()
        .zip(fechamentos)
        .filter_map(|(t, c)| {
            Some(Candle {
                em: t.as_u64()?,
                fechamento: c.as_f64()?,
            })
        })
        .collect();
    match saida.is_empty() {
        true => Err(FeedError::Formato("nenhum ponto utilizável".into())),
        false => Ok(saida),
    }
}

impl Provider for Yahoo {
    fn id(&self) -> &'static str {
        "yahoo"
    }

    fn name(&self) -> &'static str {
        "Yahoo (não-oficial)"
    }

    fn covers(&self, ativo: &AssetId) -> bool {
        matches!(ativo.market, Market::B3 | Market::Us)
    }

    fn is_open(&self, ativo: &AssetId, agora: u64) -> bool {
        match ativo.market {
            Market::B3 => crate::invest::provider::b3_aberta(agora),
            // A bolsa americana, em horário de Brasília. A aproximação por hora cheia é
            // deliberada: errar meia hora custa uma volta a mais, e acertar exigiria a
            // tabela de horário de verão americano, que muda de data todo ano.
            _ => {
                crate::invest::provider::dia_util_br(agora)
                    && (11..19).contains(&crate::invest::tempo::hora(
                        agora,
                        crate::invest::tempo::BRT_OFFSET,
                    ))
            }
        }
    }

    fn quotes(&self, ativos: &[AssetId]) -> Result<Vec<Quote>, FeedError> {
        if ativos.is_empty() {
            return Ok(Vec::new());
        }
        let agora = store::agora();
        let mut saida = Vec::new();
        let mut ultimo_erro = None;

        use std::sync::atomic::Ordering;
        let inicio = self.cursor.load(Ordering::Relaxed) % ativos.len().max(1);
        let quantos = POR_VOLTA.min(ativos.len());
        self.cursor
            .store((inicio + quantos) % ativos.len().max(1), Ordering::Relaxed);

        for i in 0..quantos {
            let ativo = &ativos[(inicio + i) % ativos.len()];
            if i > 0 {
                std::thread::sleep(ENTRE_PEDIDOS);
            }
            let url = format!(
                "https://query1.finance.yahoo.com/v8/finance/chart/{}?interval=1d&range=1d",
                simbolo(ativo)
            );
            let valor = match feed::get_json(&url) {
                Ok(v) => v,
                Err(e) => {
                    // Um 429 aqui é o bloqueio por IP começando. Parar a volta é o que
                    // evita transformar um aviso em banimento — mas o que já veio é
                    // devolvido, em vez de ser jogado fora junto.
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
            let Some(meta) = valor
                .get("chart")
                .and_then(|c| c.get("result"))
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                .and_then(|r| r.get("meta"))
            else {
                ultimo_erro = Some(FeedError::Formato(format!(
                    "{} veio sem «chart.result.meta»",
                    ativo.short()
                )));
                continue;
            };
            let Some(preco) = meta.get("regularMarketPrice").and_then(Value::as_f64) else {
                continue;
            };
            let em = meta
                .get("regularMarketTime")
                .and_then(Value::as_u64)
                .unwrap_or(agora);
            let atraso = agora.saturating_sub(em);
            saida.push(Quote {
                fonte: "yahoo",
                ativo: ativo.clone(),
                preco,
                anterior: meta
                    .get("chartPreviousClose")
                    .or_else(|| meta.get("previousClose"))
                    .and_then(Value::as_f64),
                moeda: match ativo.market {
                    Market::B3 => Moeda::Brl,
                    _ => Moeda::Usd,
                },
                // Nunca `AoVivo`, por mais fresco que o carimbo esteja: a marca
                // «não-oficial» é o que diz que este número não tem garantia nenhuma
                // atrás dele, e essa é a informação que importa aqui.
                grade: Grade::NaoOficial,
                em: match atraso < 86400 * 7 {
                    true => em,
                    false => agora,
                },
                volume: meta.get("regularMarketVolume").and_then(Value::as_f64),
                max24: meta.get("regularMarketDayHigh").and_then(Value::as_f64),
                min24: meta.get("regularMarketDayLow").and_then(Value::as_f64),
            });
        }

        match (saida.is_empty(), ultimo_erro) {
            (true, Some(e)) => Err(e),
            _ => Ok(saida),
        }
    }

    /// Série histórica — o mesmo endpoint da cotação, com outro `range`. Vem de graça, e
    /// é o que dá gráfico a um ativo que a brapi só entrega com token.
    fn history(&self, ativo: &AssetId, span: Span) -> Option<Result<Vec<Candle>, FeedError>> {
        if !self.covers(ativo) {
            return None;
        }
        let range = match span {
            Span::Dia => "1d",
            Span::Semana => "5d",
            Span::Mes => "1mo",
            Span::SeisMeses => "6mo",
            Span::Ano => "1y",
            Span::CincoAnos => "5y",
        };
        Some(historico(&format!(
            "https://query1.finance.yahoo.com/v8/finance/chart/{}?interval=1d&range={range}",
            simbolo(ativo)
        )))
    }

    fn tem_historico(&self) -> bool {
        true
    }

    /// Um minuto. Devagar de propósito: a fonte não tem contrato, e a punição por
    /// insistir não é um erro — é o bloqueio do endereço.
    fn intervalo(&self) -> Duration {
        Duration::from_secs(60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acao_brasileira_ganha_o_sufixo_do_yahoo() {
        assert_eq!(simbolo(&AssetId::new(Market::B3, "PETR4")), "PETR4.SA");
        assert_eq!(simbolo(&AssetId::new(Market::Us, "AAPL")), "AAPL");
    }

    #[test]
    fn cobre_b3_e_eua_e_mais_nada() {
        assert!(Yahoo::default().covers(&AssetId::new(Market::B3, "PETR4")));
        assert!(Yahoo::default().covers(&AssetId::new(Market::Us, "AAPL")));
        assert!(!Yahoo::default().covers(&AssetId::new(Market::Binance, "BTCBRL")));
        assert!(!Yahoo::default().covers(&AssetId::new(Market::Bcb, "CDI")));
    }
}
