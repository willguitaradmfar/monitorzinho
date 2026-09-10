//! Juro americano pelo FRED, o banco de séries do Fed de St. Louis.
//!
//! Entra pela mesma conta que fez o Banco Central ser a melhor fonte da aba: **pública,
//! gratuita, sem chave, sem cadastro, estável e oficial**. O endereço de gráfico do FRED
//! devolve a série inteira em CSV sem pedir nada.
//!
//! Ele existe porque o Yahoo **não serve** estes números de forma confiável. O `2YY=F` é
//! um futuro de taxa, e um futuro rola de contrato: medido em 10/09/2026, a chamada de um
//! dia dava 4,404 e a de cinco dias terminava em 4,18 — dois números do mesmo símbolo,
//! porque são contratos diferentes. Uma taxa de juro publicada pelo próprio banco central
//! não tem esse problema.

use std::time::Duration;

use crate::invest::feed::{self, FeedError};
use crate::invest::model::{AssetId, Market, Moeda};
use crate::invest::provider::{Candle, Grade, Provider, Quote, Span};
use crate::invest::tempo;

/// Uma série do FRED: o símbolo curto usado aqui dentro e o id dela no catálogo.
///
/// Sem nome de exibição: quem o dá é a lista de `modules::indices`, que é onde o nome
/// aparece. Guardá-lo aqui também seriam dois lugares para o mesmo rótulo.
pub struct Serie {
    pub simbolo: &'static str,
    pub id: &'static str,
}

/// Conferido em 10/09/2026 contra `fred.stlouisfed.org`, o valor **e** o significado —
/// mesma disciplina do catálogo do Banco Central, e pelo mesmo motivo: um id errado
/// devolve um número plausível de outra coisa.
pub const SERIES: &[Serie] = &[
    // DFF: taxa efetiva dos Fed Funds, diária. 2026-09-08 → 3.63
    Serie {
        simbolo: "FEDFUNDS",
        id: "DFF",
    },
    // DGS2: Treasury de 2 anos, vencimento constante, diária, em porcento ao ano.
    Serie {
        simbolo: "UST2Y",
        id: "DGS2",
    },
];

pub fn serie(simbolo: &str) -> Option<&'static Serie> {
    SERIES.iter().find(|s| s.simbolo == simbolo)
}

pub struct Fred;

/// Lê o CSV do FRED: cabeçalho, e depois `data,valor` por linha.
///
/// Um dia sem publicação vem com `.` no lugar do número — feriado americano, fim de
/// semana. Ele é **descartado**, e não lido como zero: uma taxa de zero por cento é uma
/// afirmação, e a errada.
fn pontos(id: &str, desde: Option<&str>) -> Result<Vec<Candle>, FeedError> {
    let url = match desde {
        Some(d) => format!("https://fred.stlouisfed.org/graph/fredgraph.csv?id={id}&cosd={d}"),
        None => format!("https://fred.stlouisfed.org/graph/fredgraph.csv?id={id}"),
    };
    let corpo = feed::get(&url)?.body;
    let texto = String::from_utf8_lossy(&corpo);
    let mut saida = Vec::new();
    for linha in texto.lines().skip(1) {
        let Some((data, bruto)) = linha.split_once(',') else {
            continue;
        };
        let (Some(em), Ok(v)) = (
            tempo::epoch_de_iso(data.trim()),
            bruto.trim().parse::<f64>(),
        ) else {
            continue;
        };
        saida.push(Candle { em, fechamento: v });
    }
    match saida.is_empty() {
        true => Err(FeedError::Formato("nenhuma observação utilizável".into())),
        false => Ok(saida),
    }
}

impl Provider for Fred {
    fn id(&self) -> &'static str {
        "fred"
    }

    fn name(&self) -> &'static str {
        "FRED (Fed de St. Louis)"
    }

    fn covers(&self, ativo: &AssetId) -> bool {
        ativo.market == Market::Outro && serie(&ativo.symbol).is_some()
    }

    fn quotes(&self, ativos: &[AssetId]) -> Result<Vec<Quote>, FeedError> {
        let mut saida = Vec::new();
        let mut ultimo_erro = None;
        // Um mês para trás basta para ter a observação anterior mesmo depois de um
        // feriado longo, e é uma fração do CSV inteiro, que tem décadas.
        let desde = janela(30);
        for ativo in ativos {
            let Some(serie) = serie(&ativo.symbol) else {
                continue;
            };
            match pontos(serie.id, Some(&desde)) {
                Ok(p) => {
                    let Some(ultimo) = p.last() else { continue };
                    saida.push(Quote {
                        fonte: "fred",
                        ativo: ativo.clone(),
                        preco: ultimo.fechamento,
                        // A observação anterior é o que permite mostrar variação em vez
                        // de deixar a coluna vazia.
                        anterior: (p.len() > 1).then(|| p[p.len() - 2].fechamento),
                        moeda: Moeda::Usd,
                        grade: Grade::Fechamento,
                        em: ultimo.em,
                        volume: None,
                        max24: None,
                        min24: None,
                    });
                }
                Err(e) => ultimo_erro = Some(e),
            }
        }
        match (saida.is_empty(), ultimo_erro) {
            (true, Some(e)) => Err(e),
            _ => Ok(saida),
        }
    }

    fn history(&self, ativo: &AssetId, span: Span) -> Option<Result<Vec<Candle>, FeedError>> {
        let serie = serie(&ativo.symbol)?;
        Some(pontos(serie.id, Some(&janela(span.dias()))))
    }

    fn tem_historico(&self) -> bool {
        true
    }

    /// O FRED publica em dia útil americano. Aproximar pelo dia útil brasileiro erra os
    /// feriados dos dois países em direções opostas e acerta o essencial: no sábado não
    /// há número novo, e insistir é gastar banda alheia para receber a mesma resposta.
    fn is_open(&self, _ativo: &AssetId, agora: u64) -> bool {
        crate::invest::provider::dia_util_br(agora)
    }

    /// Uma vez por hora. Estes números mudam uma vez por dia; pedi-los de dois em dois
    /// segundos seria gastar banda alheia para receber a mesma resposta.
    fn intervalo(&self) -> Duration {
        Duration::from_secs(3600)
    }
}

/// A data de `dias` atrás, como o FRED a espera: `AAAA-MM-DD`.
fn janela(dias: u32) -> String {
    let d = tempo::Data::de_epoch(
        crate::invest::store::agora().saturating_sub(u64::from(dias) * 86_400),
        tempo::BRT_OFFSET,
    );
    format!("{:04}-{:02}-{:02}", d.ano, d.mes, d.dia)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cobre_so_o_que_esta_no_catalogo() {
        assert!(Fred.covers(&AssetId::new(Market::Outro, "FEDFUNDS")));
        assert!(Fred.covers(&AssetId::new(Market::Outro, "UST2Y")));
        // Um símbolo que não está no catálogo não vira uma requisição ao FRED.
        assert!(!Fred.covers(&AssetId::new(Market::Outro, "SPX")));
        assert!(!Fred.covers(&AssetId::new(Market::B3, "PETR4")));
    }

    #[test]
    fn cada_simbolo_aparece_uma_vez_so() {
        for s in SERIES {
            assert_eq!(
                SERIES.iter().filter(|o| o.simbolo == s.simbolo).count(),
                1,
                "{} aparece mais de uma vez",
                s.simbolo
            );
        }
    }

    #[test]
    #[ignore]
    fn ao_vivo_o_fred_responde_as_duas_series() {
        for s in SERIES {
            let p = pontos(s.id, Some(&janela(30))).expect("o FRED tem que responder");
            assert!(p.len() >= 2, "{} veio com menos de dois pontos", s.simbolo);
            let ultimo = p.last().expect("há ponto");
            assert!(
                (0.0..20.0).contains(&ultimo.fechamento),
                "{} deu {} — uma taxa de juro não é isso",
                s.simbolo,
                ultimo.fechamento
            );
        }
    }
}
