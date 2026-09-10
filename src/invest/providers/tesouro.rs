//! Preço dos títulos do Tesouro Direto, pelos dados abertos do Tesouro Transparente.
//!
//! Entra pela mesma conta do Banco Central e do FRED: **pública, gratuita, sem chave,
//! sem cadastro, estável e oficial**. Antes dela, todo Tesouro em carteira ficava com o
//! preço que veio do extrato da corretora, congelado no dia da importação — quase um
//! milhão de reais de renda fixa marcados a um número que não andava.
//!
//! **O arquivo tem catorze megabytes e a leitura custa quatro kilobytes.** Ele é o
//! histórico inteiro desde 2002, mas vem ordenado do mais recente para o mais antigo:
//! os cinquenta e oito títulos de hoje são as primeiras cinquenta e oito linhas. O
//! servidor anuncia `Accept-Ranges` e ignora o `Range`, então quem corta é o cliente —
//! ver `feed::get_ate`. Medido: 0,28 s.

use std::time::Duration;

use crate::invest::feed::{self, FeedError};
use crate::invest::model::{AssetId, Market, Moeda};
use crate::invest::provider::{Candle, Grade, Provider, Quote, Span};
use crate::invest::tempo::{self, Data};

const URL: &str = "https://www.tesourotransparente.gov.br/ckan/dataset/\
                   df56aa42-484a-4a59-8184-7676580c81e3/resource/\
                   796d2059-14e9-44e3-80c9-2d9e30b405c1/download/PrecoTaxaTesouroDireto.csv";

/// Quanto do arquivo se lê. Oito kilobytes cobrem com folga as cinquenta e oito linhas
/// de um dia — a maior tem cento e poucos bytes — e ainda sobram duas datas atrás, que
/// é o que dá a variação contra o pregão anterior.
const TETO: usize = 32 * 1024;

pub struct Tesouro;

/// O símbolo como esta aba o guarda: `TESOURO-SELIC-2031`.
///
/// É o texto do extrato com espaço virando hífen — ver o importador —, e o CSV traz o
/// tipo e o vencimento em colunas separadas. Montar a chave dos dois lados pela mesma
/// regra é o que faz «Tesouro Selic» com vencimento 01/03/2031 casar com a posição.
fn chave(tipo: &str, vencimento: &str) -> Option<String> {
    // Três partes, e não «a última barra»: `31/2031` tem um ano no fim e não é uma data,
    // e uma chave montada a partir dela casaria com o título errado.
    let partes: Vec<&str> = vencimento.trim().split('/').collect();
    let [_, _, ano] = partes[..] else {
        return None;
    };
    if ano.len() != 4 || !ano.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let tipo: String = tipo
        .trim()
        .to_uppercase()
        .chars()
        .map(|c| match c {
            ' ' => '-',
            'Á' | 'À' | 'Â' | 'Ã' => 'A',
            'É' | 'Ê' => 'E',
            'Í' => 'I',
            'Ó' | 'Ô' | 'Õ' => 'O',
            'Ú' => 'U',
            'Ç' => 'C',
            outro => outro,
        })
        .collect();
    Some(format!("{tipo}-{ano}"))
}

/// Uma observação do arquivo: a chave do título, o dia e o preço de venda.
struct Linha {
    chave: String,
    em: u64,
    /// **O PU de venda**, que é o que se recebe ao resgatar — e portanto o que uma
    /// posição vale hoje. O de compra é o que se paga para entrar, e é maior: usá-lo
    /// marcaria a carteira acima do que ela realmente rende se vendida.
    preco: f64,
}

fn ler(csv: &str) -> Vec<Linha> {
    let mut saida = Vec::new();
    for linha in csv.lines().skip(1) {
        let c: Vec<&str> = linha.split(';').collect();
        // A última linha de um corpo truncado chega pela metade: sem as oito colunas,
        // ela é descartada em vez de virar um preço pela metade.
        if c.len() < 7 {
            continue;
        }
        let (Some(chave), Some(em), Some(preco)) = (
            chave(c[0], c[1]),
            data_br(c[2]),
            crate::invest::calc::ler_numero(c[6]),
        ) else {
            continue;
        };
        saida.push(Linha { chave, em, preco });
    }
    saida
}

/// `09/09/2026` no fim do dia, em Brasília. O carimbo é o **dia de referência** do
/// preço, e não o da leitura — o arquivo publica um preço por pregão.
fn data_br(texto: &str) -> Option<u64> {
    let mut p = texto.trim().split('/');
    let dia: u32 = p.next()?.parse().ok()?;
    let mes: u32 = p.next()?.parse().ok()?;
    let ano: i32 = p.next()?.parse().ok()?;
    Some(Data { ano, mes, dia }.epoch_inicio(tempo::BRT_OFFSET))
}

fn baixar() -> Result<Vec<Linha>, FeedError> {
    let corpo = feed::get_ate(URL, TETO)?.body;
    // O arquivo é latin-1, e um «Ú» mal lido viraria uma chave que não casa com nada.
    let texto = crate::invest::csv::decodificar(&corpo);
    let linhas = ler(&texto);
    match linhas.is_empty() {
        true => Err(FeedError::Formato("nenhuma linha utilizável".into())),
        false => Ok(linhas),
    }
}

impl Provider for Tesouro {
    fn id(&self) -> &'static str {
        "tesouro"
    }

    fn name(&self) -> &'static str {
        "Tesouro Transparente"
    }

    /// Todo `OUTRO/TESOURO-…`. Quem decide se ele existe de verdade é a resposta: um
    /// título que não estiver no arquivo simplesmente não volta, e a linha cai para o
    /// preço informado como antes.
    fn covers(&self, ativo: &AssetId) -> bool {
        ativo.market == Market::Outro && ativo.symbol.starts_with("TESOURO-")
    }

    fn quotes(&self, ativos: &[AssetId]) -> Result<Vec<Quote>, FeedError> {
        let meus: Vec<&AssetId> = ativos.iter().filter(|a| self.covers(a)).collect();
        if meus.is_empty() {
            return Ok(Vec::new());
        }
        // Uma leitura só para todos os títulos: o arquivo traz os cinquenta e oito de
        // uma vez, e pedir por papel seria pedir o mesmo arquivo cinquenta e oito vezes.
        let linhas = baixar()?;
        Ok(meus
            .iter()
            .filter_map(|ativo| {
                let minhas: Vec<&Linha> =
                    linhas.iter().filter(|l| l.chave == ativo.symbol).collect();
                let atual = minhas.iter().max_by_key(|l| l.em)?;
                Some(Quote {
                    fonte: "tesouro",
                    ativo: (*ativo).clone(),
                    preco: atual.preco,
                    // O pregão anterior, que é o que dá a variação do dia.
                    anterior: minhas
                        .iter()
                        .filter(|l| l.em < atual.em)
                        .max_by_key(|l| l.em)
                        .map(|l| l.preco),
                    moeda: Moeda::Brl,
                    // Fechamento e não ao vivo: o Tesouro publica um preço de manhã por
                    // pregão, e chamá-lo de ao vivo prometeria o que a fonte não dá.
                    grade: Grade::Fechamento,
                    em: atual.em,
                    volume: None,
                    max24: None,
                    min24: None,
                })
            })
            .collect())
    }

    fn history(&self, ativo: &AssetId, span: Span) -> Option<Result<Vec<Candle>, FeedError>> {
        if !self.covers(ativo) {
            return None;
        }
        // Só o que couber no teto de leitura: são cinquenta e oito títulos por dia, então
        // trinta e dois kilobytes dão algumas dezenas de pregões. Pedir um ano exigiria
        // o arquivo inteiro, e catorze megas por um gráfico não se paga.
        let corte = crate::invest::store::agora().saturating_sub(u64::from(span.dias()) * 86_400);
        Some(baixar().map(|linhas| {
            let mut velas: Vec<Candle> = linhas
                .iter()
                .filter(|l| l.chave == ativo.symbol && l.em >= corte)
                .map(|l| Candle {
                    em: l.em,
                    fechamento: l.preco,
                })
                .collect();
            velas.sort_by_key(|c| c.em);
            velas
        }))
    }

    /// O Tesouro publica em dia útil, de manhã. Insistir fora disso é gastar banda alheia
    /// para receber a mesma resposta.
    fn is_open(&self, _ativo: &AssetId, agora: u64) -> bool {
        crate::invest::provider::dia_util_br(agora)
    }

    /// Uma vez por hora. O preço muda uma vez por pregão.
    fn intervalo(&self) -> Duration {
        Duration::from_secs(3600)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chave_casa_com_o_simbolo_do_extrato() {
        assert_eq!(
            chave("Tesouro Selic", "01/03/2031").as_deref(),
            Some("TESOURO-SELIC-2031")
        );
        assert_eq!(
            chave("Tesouro Prefixado", "01/01/2031").as_deref(),
            Some("TESOURO-PREFIXADO-2031")
        );
        assert_eq!(
            chave("Tesouro IPCA+ com Juros Semestrais", "15/05/2035").as_deref(),
            Some("TESOURO-IPCA+-COM-JUROS-SEMESTRAIS-2035")
        );
        // Vencimento ilegível não vira chave — melhor não casar do que casar errado.
        assert_eq!(chave("Tesouro Selic", "31/2031"), None);
        assert_eq!(chave("Tesouro Selic", ""), None);
    }

    #[test]
    fn a_linha_cortada_pela_metade_e_descartada() {
        let csv = "Tipo Titulo;Data Vencimento;Data Base;a;b;c;d;e\n\
                   Tesouro Selic;01/03/2031;09/09/2026;0,07;0,08;19780,32;19761,22;19761,22\n\
                   Tesouro Prefi";
        let linhas = ler(csv);
        assert_eq!(linhas.len(), 1);
        assert_eq!(linhas[0].chave, "TESOURO-SELIC-2031");
        assert_eq!(linhas[0].preco, 19761.22);
    }

    #[test]
    fn cobre_so_o_tesouro() {
        assert!(Tesouro.covers(&AssetId::new(Market::Outro, "TESOURO-SELIC-2031")));
        assert!(!Tesouro.covers(&AssetId::new(Market::Outro, "SPX")));
        assert!(!Tesouro.covers(&AssetId::new(Market::B3, "PETR4")));
    }

    #[test]
    #[ignore]
    fn ao_vivo_o_arquivo_responde_os_titulos_de_hoje() {
        let linhas = baixar().expect("o Tesouro tem que responder");
        // Cinquenta e oito títulos por pregão, e o teto de leitura pega mais de um dia.
        assert!(linhas.len() > 58, "vieram só {} linhas", linhas.len());
        let selic: Vec<&Linha> = linhas
            .iter()
            .filter(|l| l.chave == "TESOURO-SELIC-2031")
            .collect();
        assert!(!selic.is_empty(), "o Selic 2031 tem que estar no arquivo");
        let p = selic.iter().max_by_key(|l| l.em).unwrap().preco;
        assert!(
            (10_000.0..30_000.0).contains(&p),
            "o PU do Selic deu {p} — não é um PU"
        );
    }
}
