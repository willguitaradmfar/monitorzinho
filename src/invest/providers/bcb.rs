//! Renda fixa brasileira pelo Banco Central (SGS).
//!
//! É a melhor fonte da aba inteira, e a conta é objetiva: **pública, gratuita, sem chave,
//! sem cadastro, estável e oficial** — nenhuma outra tem as seis ao mesmo tempo.
//!
//! Os códigos de série abaixo foram **conferidos contra a API**, um por um, e não
//! escritos de memória. Esse cuidado é o ponto: um código errado devolve um número
//! plausível de outra coisa, e nada na tela denunciaria isso.

use std::time::Duration;

use serde_json::Value;

use crate::invest::feed::{self, FeedError};
use crate::invest::model::{AssetId, Market, Moeda};
use crate::invest::provider::{Candle, Grade, Provider, Quote, Span};
use crate::invest::tempo::{self, Data};

/// Uma série do SGS: o símbolo com que ela é pedida aqui dentro, o código no catálogo do
/// Banco Central, o nome, e se o número que ela publica é diário e composto.
pub struct Serie {
    pub simbolo: &'static str,
    pub codigo: u32,
    pub nome: &'static str,
    /// `true` para taxas diárias (CDI, Selic diária), que se acumulam por produto e não
    /// por soma. `false` para as mensais e para as que já vêm anualizadas.
    pub diaria: bool,
}

/// Conferido em 08/09/2026 contra `api.bcb.gov.br` — **o valor e o significado**.
///
/// Conferir só a resposta não basta, e isso custou um erro: a série 4389 responde 13,90 e
/// eu a rotulei «Selic meta», quando a meta do COPOM é a 432 e vale 14,00. O número
/// chegava, era plausível, e era de outra coisa. Cada entrada abaixo traz a resposta
/// anotada **e** o que ela quer dizer.
pub const SERIES: &[Serie] = &[
    // [{"data":"04/09/2026","valor":"0.051660"}] — taxa diária, em porcento.
    Serie {
        simbolo: "CDI",
        codigo: 12,
        nome: "CDI",
        diaria: true,
    },
    // [{"data":"12/09/2026","valor":"14.00"}] — **a meta do COPOM**: o número que sai no
    // jornal e que o COPOM define nas reuniões. Série diária, constante entre elas, e
    // publicada com data de vigência à frente.
    Serie {
        simbolo: "SELIC-META",
        codigo: 432,
        nome: "Selic (meta)",
        diaria: false,
    },
    // [{"data":"04/09/2026","valor":"13.90"}] — a Selic **efetiva** anualizada, base 252.
    //
    // Dois números diferentes, os dois certos: a meta é a decisão, a efetiva é o que o
    // mercado praticou — e ela fica alguns décimos abaixo. Quem calcula rendimento quer
    // esta; quem quer saber «quanto está a Selic» quer a de cima.
    Serie {
        simbolo: "SELIC",
        codigo: 4389,
        nome: "Selic (efetiva a.a.)",
        diaria: false,
    },
    // [{"data":"04/09/2026","valor":"0.051660"}] — a efetiva, ao dia.
    Serie {
        simbolo: "SELIC-DIA",
        codigo: 11,
        nome: "Selic (diária)",
        diaria: true,
    },
    // [{"data":"01/07/2026","valor":"0.07"}] — variação mensal, em porcento.
    Serie {
        simbolo: "IPCA",
        codigo: 433,
        nome: "IPCA (mensal)",
        diaria: false,
    },
    // O acumulado de doze meses, que é o número que se cita: «a inflação está em 4,4%».
    // O mensal sozinho não responde isso — 0,07% num mês não diz nada sobre o ano.
    // [{"data":"01/07/2026","valor":"4.44"}]
    Serie {
        simbolo: "IPCA12",
        codigo: 13522,
        nome: "IPCA (12 meses)",
        diaria: false,
    },
    // [{"data":"01/08/2026","valor":"-0.22"}]
    Serie {
        simbolo: "IGPM",
        codigo: 189,
        nome: "IGP-M (mensal)",
        diaria: false,
    },
    // [{"data":"04/09/2026","dataFim":"04/10/2026","valor":"0.6474"}] — campo extra a
    // mais na resposta, que o leitor ignora por ler campo a campo.
    Serie {
        simbolo: "POUPANCA",
        codigo: 195,
        nome: "Poupança (mensal)",
        diaria: false,
    },
];

pub fn serie(simbolo: &str) -> Option<&'static Serie> {
    SERIES.iter().find(|s| s.simbolo == simbolo)
}

pub struct Bcb;

/// `"04/09/2026"` → epoch do início daquele dia em Brasília.
fn data_br(texto: &str) -> Option<u64> {
    let mut partes = texto.split('/');
    let dia: u32 = partes.next()?.parse().ok()?;
    let mes: u32 = partes.next()?.parse().ok()?;
    let ano: i32 = partes.next()?.parse().ok()?;
    Some(Data { ano, mes, dia }.epoch_inicio(tempo::BRT_OFFSET))
}

/// **O SGS recusa `ultimos/N` acima de 20** — responde 400 com «a quantidade máxima de
/// valores deve ser 20». Descobri isso pedindo 30 e recebendo uma coluna vazia na tela.
///
/// Então: até 20 pontos usa `ultimos/N`, que é uma URL curta; acima disso usa o endpoint
/// por intervalo de datas, que não tem esse teto (251 pregões num ano vêm numa resposta
/// só). O corte fica aqui, num lugar só, para nenhum chamador precisar saber disso.
const MAX_ULTIMOS: u32 = 20;

fn serie_pontos(codigo: u32, quantos: u32) -> Result<Vec<Candle>, FeedError> {
    if quantos <= MAX_ULTIMOS {
        return pontos_publicos(&format!(
            "https://api.bcb.gov.br/dados/serie/bcdata.sgs.{codigo}/dados/ultimos/{quantos}?formato=json"
        ));
    }
    // Uma janela de calendário generosa: pregões são ~70% dos dias corridos, e pedir
    // demais custa alguns kilobytes enquanto pedir de menos perde o começo da série.
    let hoje = crate::invest::store::agora();
    let inicio = Data::de_epoch(
        hoje.saturating_sub((quantos as u64) * 86400 * 3 / 2),
        tempo::BRT_OFFSET,
    );
    let fim = Data::de_epoch(hoje, tempo::BRT_OFFSET);
    pontos_publicos(&format!(
        "https://api.bcb.gov.br/dados/serie/bcdata.sgs.{codigo}/dados?formato=json&dataInicial={:02}/{:02}/{}&dataFinal={:02}/{:02}/{}",
        inicio.dia, inicio.mes, inicio.ano, fim.dia, fim.mes, fim.ano
    ))
}

/// A leitura de uma série do SGS por código, exposta porque o câmbio busca a PTAX pelo
/// mesmo caminho — e duas cópias da mesma leitura seriam dois lugares para errar o teto
/// de 20 pontos.
pub fn serie_por_codigo(codigo: u32, quantos: u32) -> Result<Vec<Candle>, FeedError> {
    serie_pontos(codigo, quantos)
}

/// A leitura crua de uma URL do SGS.
pub fn pontos_publicos(url: &str) -> Result<Vec<Candle>, FeedError> {
    let valor = feed::get_json(url)?;
    let linhas = valor
        .as_array()
        .ok_or_else(|| FeedError::Formato("esperava uma lista de observações".into()))?;
    let mut saida = Vec::with_capacity(linhas.len());
    for linha in linhas {
        let (Some(data), Some(bruto)) = (
            linha.get("data").and_then(Value::as_str),
            linha.get("valor").and_then(Value::as_str),
        ) else {
            continue;
        };
        let (Some(em), Ok(v)) = (data_br(data), bruto.parse::<f64>()) else {
            continue;
        };
        saida.push(Candle { em, fechamento: v });
    }
    match saida.is_empty() {
        true => Err(FeedError::Formato("nenhuma observação utilizável".into())),
        false => Ok(saida),
    }
}

impl Provider for Bcb {
    fn id(&self) -> &'static str {
        "bcb"
    }

    fn name(&self) -> &'static str {
        "Banco Central"
    }

    fn covers(&self, ativo: &AssetId) -> bool {
        ativo.market == Market::Bcb && serie(&ativo.symbol).is_some()
    }

    fn quotes(&self, ativos: &[AssetId]) -> Result<Vec<Quote>, FeedError> {
        let mut saida = Vec::new();
        let mut ultimo_erro = None;
        for ativo in ativos {
            let Some(serie) = serie(&ativo.symbol) else {
                continue;
            };
            // Duas observações, não uma: a anterior é o que permite mostrar variação em
            // vez de deixar a coluna vazia.
            match serie_pontos(serie.codigo, 2) {
                Ok(p) => {
                    let Some(ultimo) = p.last() else { continue };
                    saida.push(Quote {
                        fonte: "bcb",
                        ativo: ativo.clone(),
                        preco: ultimo.fechamento,
                        anterior: (p.len() > 1).then(|| p[p.len() - 2].fechamento),
                        moeda: Moeda::Brl,
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
        // Uma série que falhou não derruba as outras — mas se nenhuma respondeu, o erro
        // tem que subir, senão a fonte apareceria como «ok» sem ter dito nada.
        match (saida.is_empty(), ultimo_erro) {
            (true, Some(e)) => Err(e),
            _ => Ok(saida),
        }
    }

    fn history(&self, ativo: &AssetId, span: Span) -> Option<Result<Vec<Candle>, FeedError>> {
        let serie = serie(&ativo.symbol)?;
        // Série mensal em dias daria uma requisição enorme para uma dúzia de pontos.
        let quantos = match serie.diaria {
            true => span.dias().min(2000),
            false => (span.dias() / 30).clamp(2, 120),
        };
        Some(serie_pontos(serie.codigo, quantos))
    }

    /// O SGS publica em dia útil. No fim de semana e em feriado não há número novo, e
    /// insistir é gastar cota alheia para receber a mesma resposta.
    fn is_open(&self, _ativo: &AssetId, agora: u64) -> bool {
        crate::invest::provider::dia_util_br(agora)
    }

    fn intervalo(&self) -> Duration {
        Duration::from_secs(6 * 3600)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_brasileira_e_lida() {
        let em = data_br("04/09/2026").unwrap();
        let d = Data::de_epoch(em, tempo::BRT_OFFSET);
        assert_eq!((d.ano, d.mes, d.dia), (2026, 9, 4));
        assert_eq!(data_br("nao-e-data"), None);
        assert_eq!(data_br("04/09"), None);
    }

    #[test]
    fn so_cobre_series_que_existem_no_catalogo() {
        assert!(Bcb.covers(&AssetId::new(Market::Bcb, "CDI")));
        assert!(Bcb.covers(&AssetId::new(Market::Bcb, "IPCA")));
        // Uma série inventada não é coberta: melhor não responder do que responder outra coisa.
        assert!(!Bcb.covers(&AssetId::new(Market::Bcb, "INVENTADA")));
        assert!(!Bcb.covers(&AssetId::new(Market::B3, "PETR4")));
    }

    #[test]
    fn acima_de_vinte_pontos_o_caminho_muda() {
        // O SGS recusa `ultimos/N` acima de 20. O corte tem que existir, senão a tela
        // fica com colunas vazias e nenhuma explicação.
        assert_eq!(MAX_ULTIMOS, 20);
    }

    #[test]
    fn a_meta_e_a_efetiva_sao_series_diferentes() {
        // O erro que motivou este teste: a série da efetiva estava rotulada «meta», e a
        // tela mostrava 13,90 onde se esperava 14,00 sem nada denunciar a troca.
        let meta = serie("SELIC-META").expect("a meta tem que existir");
        let efetiva = serie("SELIC").expect("a efetiva tem que existir");
        assert_eq!(meta.codigo, 432, "a meta do COPOM é a série 432");
        assert_ne!(
            meta.codigo, efetiva.codigo,
            "meta e efetiva não são a mesma série"
        );
        assert!(meta.nome.contains("meta"));
        assert!(efetiva.nome.contains("efetiva"));
    }

    /// Confere no Banco Central que os códigos significam o que o rótulo diz.
    /// `#[ignore]`: depende de rede. `cargo test -- --ignored bcb`.
    #[test]
    #[ignore]
    fn os_codigos_significam_o_que_o_rotulo_diz() {
        let valor = |simbolo: &str| -> f64 {
            let s = serie(simbolo).unwrap();
            serie_pontos(s.codigo, 1)
                .unwrap()
                .last()
                .unwrap()
                .fechamento
        };
        let meta = valor("SELIC-META");
        let efetiva = valor("SELIC");
        println!("meta {meta} · efetiva {efetiva}");
        // A invariante que não depende dos números do dia: a Selic efetiva fica **abaixo**
        // da meta, por alguns décimos. Se elas se inverterem ou coincidirem, um dos dois
        // códigos está apontando para a série errada.
        assert!(
            efetiva < meta && meta - efetiva < 1.0,
            "meta {meta} e efetiva {efetiva} não têm a relação esperada"
        );
    }

    #[test]
    fn todo_simbolo_do_catalogo_e_unico() {
        for s in SERIES {
            let quantos = SERIES.iter().filter(|o| o.simbolo == s.simbolo).count();
            assert_eq!(quantos, 1, "{} aparece mais de uma vez", s.simbolo);
        }
    }
}
