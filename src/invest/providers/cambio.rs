//! Câmbio, e por que ele é o provedor mais crítico da aba.
//!
//! O patrimônio é consolidado em BRL. Todo ativo que não é brasileiro passa por aqui
//! antes de virar um número na tela. **Um câmbio errado erra o patrimônio inteiro,
//! silenciosamente** — é o único número da aba com essa propriedade.
//!
//! Duas cotações, de propósito: a de mercado (AwesomeAPI), que converte o que está na
//! tela agora, e a PTAX do Banco Central, que é o número oficial do dia e o que vale para
//! imposto. Não são redundância — são dois números certos para duas perguntas
//! diferentes, e usar um no lugar do outro erra o DARF.

use std::time::Duration;

use serde_json::Value;

use crate::invest::feed::{self, FeedError};
use crate::invest::model::{AssetId, Market, Moeda};
use crate::invest::provider::{Candle, Grade, Provider, Quote, Span};
use crate::invest::store;

pub struct Cambio;

/// Os pares que este provedor sabe buscar. `USDBRL` e `EURBRL` na AwesomeAPI; `PTAX` vem
/// do BCB e é pedido como um símbolo próprio para que as duas nunca se confundam.
const PARES: &[&str] = &["USDBRL", "EURBRL"];
/// Série 1 do SGS: dólar comercial de venda. Conferido em 08/09/2026 —
/// `[{"data":"04/09/2026","valor":"5.1253"}]`.
const PTAX_SGS: u32 = 1;
pub const SIMBOLO_PTAX: &str = "PTAX-USDBRL";

fn texto_para_f64(v: &Value, campo: &str) -> Option<f64> {
    v.get(campo)?.as_str()?.parse().ok()
}

impl Provider for Cambio {
    fn id(&self) -> &'static str {
        "cambio"
    }

    fn name(&self) -> &'static str {
        "AwesomeAPI + PTAX"
    }

    fn covers(&self, ativo: &AssetId) -> bool {
        ativo.market == Market::Fx
            && (PARES.contains(&ativo.symbol.as_str()) || ativo.symbol == SIMBOLO_PTAX)
    }

    fn quotes(&self, ativos: &[AssetId]) -> Result<Vec<Quote>, FeedError> {
        let agora = store::agora();
        let mut saida = Vec::new();

        let mercado: Vec<&AssetId> = ativos
            .iter()
            .filter(|a| PARES.contains(&a.symbol.as_str()))
            .collect();
        if !mercado.is_empty() {
            // Em lote, como toda leitura desta aba.
            let lista = mercado
                .iter()
                .map(|a| format!("{}-{}", &a.symbol[..3], &a.symbol[3..]))
                .collect::<Vec<_>>()
                .join(",");
            let url = format!("https://economia.awesomeapi.com.br/json/last/{lista}");
            let valor = feed::get_json(&url)?;
            let objeto = valor
                .as_object()
                .ok_or_else(|| FeedError::Formato("esperava um objeto de pares".into()))?;
            for ativo in mercado {
                let Some(item) = objeto.get(&ativo.symbol) else {
                    continue;
                };
                // `bid` é o preço de compra, que é o que se usa para avaliar o que se tem.
                let Some(preco) = texto_para_f64(item, "bid") else {
                    continue;
                };
                // A API dá a variação percentual, não o fechamento anterior. Reconstruir
                // o anterior a partir dela mantém uma coisa só no resto do programa.
                let anterior = texto_para_f64(item, "pctChange")
                    .filter(|p| *p != -100.0)
                    .map(|p| preco / (1.0 + p / 100.0));
                saida.push(Quote {
                    fonte: "cambio",
                    ativo: ativo.clone(),
                    preco,
                    anterior,
                    moeda: Moeda::Brl,
                    grade: Grade::AoVivo,
                    em: item
                        .get("timestamp")
                        .and_then(Value::as_str)
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(agora),
                    volume: None,
                    max24: texto_para_f64(item, "high"),
                    min24: texto_para_f64(item, "low"),
                });
            }
        }

        // A PTAX é buscada mesmo quando a de mercado respondeu: ela é o número que vale
        // para o imposto, e quem confundir as duas erra o DARF.
        if ativos.iter().any(|a| a.symbol == SIMBOLO_PTAX) {
            let url = format!(
                "https://api.bcb.gov.br/dados/serie/bcdata.sgs.{PTAX_SGS}/dados/ultimos/1?formato=json"
            );
            if let Ok(valor) = feed::get_json(&url)
                && let Some(item) = valor.as_array().and_then(|a| a.last())
                && let Some(preco) = item
                    .get("valor")
                    .and_then(Value::as_str)
                    .and_then(|s| s.parse::<f64>().ok())
            {
                saida.push(Quote {
                    fonte: "cambio",
                    ativo: AssetId::new(Market::Fx, SIMBOLO_PTAX),
                    preco,
                    anterior: None,
                    moeda: Moeda::Brl,
                    grade: Grade::Fechamento,
                    em: agora,
                    volume: None,
                    max24: None,
                    min24: None,
                });
            }
        }

        Ok(saida)
    }

    fn history(&self, ativo: &AssetId, span: Span) -> Option<Result<Vec<Candle>, FeedError>> {
        if !self.covers(ativo) || ativo.symbol != "USDBRL" {
            return None;
        }
        // Pela série do BCB: a AwesomeAPI dá histórico curto, e para um gráfico de meses
        // a série oficial é melhor e é a mesma que o imposto usa. Pelo caminho do `bcb`,
        // que já sabe do teto de 20 pontos do `ultimos/N`.
        Some(super::bcb::serie_por_codigo(
            PTAX_SGS,
            span.dias().min(2000),
        ))
    }

    /// O câmbio não anda no fim de semana, e a PTAX só sai em dia útil.
    fn is_open(&self, _ativo: &AssetId, agora: u64) -> bool {
        crate::invest::provider::dia_util_br(agora)
    }

    fn intervalo(&self) -> Duration {
        // Compartilha a volta com as cotações: se tivesse cadência própria, haveria um
        // instante em que o preço de um ativo em dólar já mudou e o câmbio ainda não, e o
        // valor em BRL na tela seria de um par que nunca existiu ao mesmo tempo.
        Duration::from_secs(10)
    }
}

/// Os pares que a aba sempre acompanha, porque o BRL como âncora depende deles.
pub fn pares_essenciais() -> Vec<AssetId> {
    let mut v: Vec<AssetId> = PARES.iter().map(|p| AssetId::new(Market::Fx, *p)).collect();
    v.push(AssetId::new(Market::Fx, SIMBOLO_PTAX));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cobre_so_os_pares_que_sabe() {
        assert!(Cambio.covers(&AssetId::new(Market::Fx, "USDBRL")));
        assert!(Cambio.covers(&AssetId::new(Market::Fx, SIMBOLO_PTAX)));
        assert!(!Cambio.covers(&AssetId::new(Market::Fx, "JPYBRL")));
        assert!(!Cambio.covers(&AssetId::new(Market::B3, "PETR4")));
    }

    #[test]
    fn anterior_e_reconstruido_da_variacao() {
        // A API dá pctChange, não o fechamento. Reconstruir mantém uma coisa só no resto
        // do programa: todo Quote tem `anterior`, venha de onde vier.
        let preco = 5.0842_f64;
        let pct = -0.813512_f64;
        let anterior = preco / (1.0 + pct / 100.0);
        assert!((anterior - 5.1259).abs() < 0.001, "deu {anterior}");
        // E a volta bate.
        assert!(((preco / anterior - 1.0) * 100.0 - pct).abs() < 1e-9);
    }

    #[test]
    fn pares_essenciais_incluem_a_ptax() {
        let p = pares_essenciais();
        assert!(p.iter().any(|a| a.symbol == "USDBRL"));
        assert!(p.iter().any(|a| a.symbol == SIMBOLO_PTAX));
    }
}
