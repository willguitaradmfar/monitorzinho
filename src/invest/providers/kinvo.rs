//! B3 pela API aberta da Kinvo.
//!
//! É a **primeira** fonte da B3, à frente da brapi, por uma razão medida e não estética:
//! ela responde **em lote**. Vinte tickers numa chamada, 1,2 s, sem chave — enquanto a
//! brapi sem token faz um pedido por ativo e esbarra no limite anônimo antes de terminar
//! uma carteira média. Menos chamadas é menos cota de terceiro gasta e menos chance de o
//! usuário ver metade da tela com traços.
//!
//! Ela também cobre o **IBOV**, que o resto desta aba não tinha de graça — o módulo de
//! índices dizia «sem fonte sem chave» para ele desde a primeira versão.
//!
//! Também é uma fonte **sem contrato**, como o Yahoo: é a API que alimenta o site da
//! Kinvo, não um produto publicado com compromisso de estabilidade. Por isso ela entrega
//! `Grade::Atrasado` e nunca `AoVivo`, toda linha carrega `fonte: "kinvo"` na tela, e a
//! brapi continua logo atrás na cadeia — se este endereço mudar de forma amanhã, a aba
//! troca de fonte sozinha em vez de ficar sem preço.
//!
//! Medido em 08/09/2026: ações, FIIs, ETFs e IBOV respondem; papel americano não; ticker
//! inexistente volta com as listas vazias em vez de erro, e é descartado aqui.

use std::time::Duration;

use serde_json::Value;

use crate::invest::feed::{self, FeedError};
use crate::invest::model::{AssetId, Market, Moeda};
use crate::invest::provider::{Candle, Grade, Provider, Quote, Span};
use crate::invest::store;

/// Quantos tickers por chamada. Vinte responderam em 1,2 s e 95 KB; o teto existe para
/// que uma carteira grande vire duas chamadas curtas em vez de uma longa que pode estourar
/// o tempo limite no meio e perder tudo.
const POR_LOTE: usize = 20;

/// Pausa entre dois lotes. Nada no serviço anuncia limite — doze pedidos seguidos vieram
/// todos com 200 —, e é justamente por isso que o intervalo é escolhido por nós: uma fonte
/// que não diz o limite é uma em que insistir é aposta.
const ENTRE_LOTES: Duration = Duration::from_millis(300);

pub struct Kinvo;

/// Os índices que ela serve, e como este programa os chama.
///
/// O IBOV vive em `Market::Outro` porque foi assim que o módulo de índices o registrou,
/// quando ele não tinha fonte nenhuma. Traduzir aqui custa uma linha e evita mexer no
/// módulo — e no dia em que o índice ganhar mercado próprio, muda só este mapa.
fn simbolo(ativo: &AssetId) -> Option<&str> {
    match ativo.market {
        Market::B3 => Some(&ativo.symbol),
        Market::Outro if ativo.symbol == "IBOV" => Some("IBOV"),
        _ => None,
    }
}

impl Provider for Kinvo {
    fn id(&self) -> &'static str {
        "kinvo"
    }

    fn name(&self) -> &'static str {
        "Kinvo (aberta, sem contrato)"
    }

    fn covers(&self, ativo: &AssetId) -> bool {
        simbolo(ativo).is_some()
    }

    fn is_open(&self, _ativo: &AssetId, agora: u64) -> bool {
        crate::invest::provider::b3_aberta(agora)
    }

    fn quotes(&self, ativos: &[AssetId]) -> Result<Vec<Quote>, FeedError> {
        let meus: Vec<&AssetId> = ativos.iter().filter(|a| self.covers(a)).collect();
        if meus.is_empty() {
            return Ok(Vec::new());
        }
        let agora = store::agora();
        let mut saida = Vec::new();
        let mut ultimo_erro = None;

        for (n, lote) in meus.chunks(POR_LOTE).enumerate() {
            if n > 0 {
                std::thread::sleep(ENTRE_LOTES);
            }
            let lista: Vec<&str> = lote.iter().filter_map(|a| simbolo(a)).collect();
            let url = format!(
                "https://open-api.kinvo.com.br/intra-day-series?tickers={}&interval=5m&range=1d",
                lista.join(",")
            );
            let dados = match feed::get_json(&url) {
                Ok(v) => v,
                // Um lote que falha não derruba os que já vieram: devolver o que se tem é
                // a diferença entre meia tela com preço e a tela inteira com traços.
                Err(e) => {
                    ultimo_erro = Some(e);
                    continue;
                }
            };
            for ativo in lote {
                let Some(simbolo) = simbolo(ativo) else {
                    continue;
                };
                let Some(serie) = dados.get("data").and_then(|d| d.get(simbolo)) else {
                    continue;
                };
                if let Some(quote) = ler(ativo, serie, agora) {
                    saida.push(quote);
                }
            }
        }

        match (saida.is_empty(), ultimo_erro) {
            (true, Some(e)) => Err(e),
            _ => Ok(saida),
        }
    }

    fn history(&self, ativo: &AssetId, span: Span) -> Option<Result<Vec<Candle>, FeedError>> {
        let simbolo = simbolo(ativo)?;
        // Só três janelas passam: `3mo`, `6mo` e `max` respondem HTTP 400. As outras são
        // aproximadas pela menor que as cubra e cortadas depois — pedir 1 ano para mostrar
        // seis meses gasta uma chamada igual e evita voltar de mãos vazias.
        // Onde a janela pedida não tem endereço próprio, pede-se a maior que a cubra e
        // corta-se **por data**, não por número de pontos: 182 velas são 182 pregões, e
        // 182 pregões são nove meses. Cortar por contagem fazia «6 meses» mostrar de
        // dezembro a setembro.
        let (range, desde) = match span {
            Span::Mes => ("1mo", None),
            Span::Ano => ("1y", None),
        };
        // O endereço é «histórico do setor»: ele devolve o papel pedido, os pares do
        // setor dele **e o IBOV**. Pedir o histórico de `IBOV` diretamente responde 200
        // com a lista vazia — ele não é um papel de setor nenhum —, então o índice é lido
        // como carona na resposta de uma ação qualquer. `PETR4` serve de portador por ser
        // o papel mais negociado da bolsa: se um dia ele não responder, a bolsa toda
        // também não estará respondendo.
        let portador = match simbolo {
            "IBOV" => "PETR4",
            outro => outro,
        };
        let url = format!(
            "https://open-api.kinvo.com.br/v2/sector-historic-quotation/{portador}?range={range}&interval=1d"
        );
        Some(serie(&url, simbolo, desde))
    }

    /// Os proventos anunciados, do mapa de calor mensal que a Kinvo publica.
    ///
    /// A resposta é uma matriz de doze meses por dez anos, com os pagamentos dentro de
    /// `details` — o que interessa é a data-com (`eventDate`) e o valor por cota. Ela vem
    /// inteira numa chamada, o que é ótimo: dez anos de histórico por papel, uma vez.
    fn proventos(
        &self,
        ativo: &AssetId,
    ) -> Option<Result<Vec<crate::invest::provento::Anunciado>, FeedError>> {
        let simbolo = simbolo(ativo)?;
        let url =
            format!("https://open-api.kinvo.com.br/monthly-dividends-heatmap?tickers={simbolo}");
        Some(anunciados(&url, ativo))
    }

    /// Trinta segundos. A fonte não anuncia limite nenhum, e uma fonte calada sobre a
    /// própria cota é uma em que se anda devagar por escolha.
    fn intervalo(&self) -> Duration {
        Duration::from_secs(30)
    }
}

/// Lê os proventos anunciados do mapa de calor mensal.
fn anunciados(
    url: &str,
    ativo: &AssetId,
) -> Result<Vec<crate::invest::provento::Anunciado>, FeedError> {
    use crate::invest::provento::Anunciado;
    let valor = feed::get_json(url)?;
    // `dateCom` presente e **vazio** é a resposta de quem nunca pagou nada — o AXIA3
    // volta com `{"dateCom": {}}` e HTTP 200. Isso é uma lista vazia, não um formato
    // quebrado: tratá-lo como erro fazia a busca não carimbar, e um papel assim nunca
    // marcava a fonte como respondida.
    let Some(mapa) = valor.get("data").and_then(|d| d.get("dateCom")) else {
        return Err(FeedError::Formato("resposta sem «data.dateCom»".into()));
    };
    let Some(meses) = mapa.get("months").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut saida: Vec<Anunciado> = meses
        .iter()
        .filter_map(|m| m.get("values")?.as_array())
        .flatten()
        .filter_map(|v| v.get("details")?.as_array())
        .flatten()
        .filter_map(|d| {
            let por_cota = d.get("payment")?.as_f64().filter(|v| *v > 0.0)?;
            Some(Anunciado {
                ativo: ativo.clone(),
                em: data_com(d.get("eventDate")?.as_str()?)?,
                por_cota,
                dy: d
                    .get("dividendYield")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0),
            })
        })
        .collect();
    // Do mais recente para o mais antigo: a matriz vem por mês do ano, então a ordem de
    // chegada mistura 2018 com 2026.
    saida.sort_by_key(|a| std::cmp::Reverse(a.em));
    Ok(saida)
}

/// `2026-08-31` como instante — **meio-dia de Brasília daquele dia**.
///
/// A data-com é uma data de pregão da B3, sem hora e sem fuso: ela não significa um
/// instante, significa um dia. Quem a lê de volta pergunta `Data::de_epoch(em,
/// BRT_OFFSET)`, e é aí que a escolha do instante importa — lida como meia-noite **UTC**,
/// toda data-com voltava três horas para trás e virava o dia anterior na tela. Um provento
/// anunciado para 31/08 aparecia como 30/08, todos eles, sempre. Meio-dia porque ele está
/// longe das duas bordas: nenhum fuso que a B3 use o empurra para outro dia.
fn data_com(dia: &str) -> Option<u64> {
    crate::invest::tempo::epoch_de_iso(&format!("{dia}T12:00:00-03:00"))
}

/// Uma cotação a partir da série intradiária de um ticker.
///
/// O preço é o do **carimbo mais recente**, e não o último elemento do vetor. A resposta
/// vem fora de ordem: medido em 08/09/2026, o HGLG11 trazia 147,75 no carimbo mais novo e
/// 148,20 no último elemento — quarenta e cinco centavos de diferença que apareceriam na
/// tela como o preço de agora.
fn ler(ativo: &AssetId, serie: &Value, agora: u64) -> Option<Quote> {
    let instantes = serie.get("timestamp")?.as_array()?;
    let cotacoes = serie.get("quotation")?.as_array()?;
    let pontos: Vec<(u64, f64)> = instantes
        .iter()
        .zip(cotacoes)
        .filter_map(|(t, q)| Some((t.as_u64()?, q.get("close")?.as_f64()?)))
        .collect();
    let (em, preco) = pontos.iter().copied().max_by_key(|(t, _)| *t)?;
    let em = em.min(agora);
    // Máxima e mínima **do dia**, tiradas da própria série de cinco minutos. As colunas
    // ficavam vazias na maioria das linhas da tela de Cotações, e não porque faltasse
    // dado: é que só a brapi e o Yahoo as preenchiam, e quem serve a B3 aqui é esta
    // fonte. A Kinvo não publica os campos prontos, mas publica a série — e o máximo
    // dela é o máximo do dia.
    //
    // São os fechamentos de cada barra de cinco minutos, então uma pavio que suba e
    // volte dentro da mesma barra não entra. É por baixo, e é honesto: a alternativa era
    // a coluna continuar vazia.
    let (max24, min24) = pontos
        .iter()
        .map(|(_, p)| *p)
        .fold((f64::MIN, f64::MAX), |(a, b), p| (a.max(p), b.min(p)));
    Some(Quote {
        ativo: ativo.clone(),
        fonte: "kinvo",
        preco,
        anterior: serie.get("previousClose").and_then(Value::as_f64),
        moeda: Moeda::Brl,
        // Nunca `AoVivo`, e o atraso é o **medido**, não um número fixo: a série é de
        // cinco em cinco minutos, então o preço tem a idade que o carimbo disser. Chamar
        // de ao vivo o que pode ter cinco minutos é prometer o que a fonte não dá.
        grade: Grade::Atrasado(agora.saturating_sub(em)),
        // O instante do dado, não o da busca. É o que faz a tela dizer «há 3 min» em vez
        // de «agora» para um preço que a fonte carimbou faz três minutos.
        em,
        // A Kinvo não publica volume em nenhum dos endereços dela — a coluna fica vazia
        // para o que vem daqui, e vazio é o que se sabe.
        volume: None,
        max24: (max24 > f64::MIN).then_some(max24),
        min24: (min24 < f64::MAX).then_some(min24),
    })
}

/// A série diária de um ticker. A resposta traz o papel pedido, os pares do setor dele e o
/// IBOV — aqui se lê só o que foi pedido, e o resto é descartado sem custo.
fn serie(
    url: &str,
    simbolo: &str,
    ultimos_dias_corridos: Option<u64>,
) -> Result<Vec<Candle>, FeedError> {
    let valor = feed::get_json(url)?;
    let dias = valor
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| FeedError::Formato("resposta sem «data»".into()))?;
    let mut velas: Vec<Candle> = dias
        .iter()
        .filter_map(|d| {
            let fechamento = d
                .get("tickers")?
                .get(simbolo)?
                .get("marketPrice")?
                .as_f64()?;
            Some(Candle {
                em: crate::invest::tempo::epoch_de_iso(d.get("date")?.as_str()?)?,
                fechamento,
            })
        })
        .collect();
    velas.sort_by_key(|c| c.em);
    if let Some(n) = ultimos_dias_corridos {
        let corte = store::agora().saturating_sub(n * 86_400);
        velas.retain(|c| c.em >= corte);
    }
    match velas.is_empty() {
        true => Err(FeedError::Formato(format!("série vazia para {simbolo}"))),
        false => Ok(velas),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// As colunas «Máx 24h» e «Mín 24h» ficavam vazias na maioria das linhas de
    /// Cotações — não por falta de dado, mas porque só a brapi e o Yahoo preenchiam os
    /// campos prontos, e quem serve a B3 aqui é esta fonte. A série intradiária tem a
    /// resposta; era só olhar para ela.
    #[test]
    fn a_maxima_e_a_minima_do_dia_saem_da_propria_serie() {
        let serie: Value = serde_json::from_str(
            r#"{
                "previousClose": 48.45,
                "timestamp": [100, 200, 300],
                "quotation": [{"close": 48.9}, {"close": 49.6}, {"close": 48.2}]
            }"#,
        )
        .expect("json de teste");
        let q = ler(&AssetId::new(Market::B3, "PETR4"), &serie, 1_000).expect("lê a cotação");
        assert_eq!(q.max24, Some(49.6));
        assert_eq!(q.min24, Some(48.2));
        // O preço continua sendo o do carimbo mais recente, e não o maior nem o último.
        assert_eq!(q.preco, 48.2);
        // Volume esta fonte não publica em endereço nenhum, e vazio é o que se sabe.
        assert_eq!(q.volume, None);
    }

    /// Uma série vazia não vira máxima zero: `0` numa coluna de preço se lê como um
    /// preço, e um preço de zero é uma afirmação falsa.
    #[test]
    fn serie_sem_ponto_nenhum_nao_inventa_extremos() {
        let serie: Value =
            serde_json::from_str(r#"{"timestamp": [], "quotation": []}"#).expect("json de teste");
        assert!(ler(&AssetId::new(Market::B3, "PETR4"), &serie, 1_000).is_none());
    }

    #[test]
    fn cobre_a_b3_e_o_ibov_e_mais_nada() {
        assert!(Kinvo.covers(&AssetId::new(Market::B3, "PETR4")));
        assert!(Kinvo.covers(&AssetId::new(Market::B3, "HGLG11")));
        // O IBOV mora em `Outro` desde antes de ter fonte. É o motivo de o mapa existir.
        assert!(Kinvo.covers(&AssetId::new(Market::Outro, "IBOV")));
        assert!(!Kinvo.covers(&AssetId::new(Market::Us, "AAPL")));
        assert!(!Kinvo.covers(&AssetId::new(Market::Outro, "SPX")));
        assert!(!Kinvo.covers(&AssetId::new(Market::Binance, "BTCBRL")));
    }

    #[test]
    fn o_preco_e_o_do_carimbo_mais_novo_e_nao_o_ultimo_do_vetor() {
        // A resposta de verdade vem assim: o vetor termina repetindo um instante anterior.
        // Ler o último elemento dá um preço velho apresentado como o de agora — foi o que
        // aconteceu com o HGLG11 em 08/09/2026, quarenta e cinco centavos abaixo.
        let serie = serde_json::json!({
            "previousClose": 148.35,
            "timestamp": [1788896700, 1788897600, 1788897000],
            "quotation": [
                {"close": 148.10},
                {"close": 147.75},
                {"close": 148.20}
            ]
        });
        let q = ler(&AssetId::new(Market::B3, "HGLG11"), &serie, 1788899000).unwrap();
        assert_eq!(q.preco, 147.75, "tem que ser o do instante mais novo");
        assert_eq!(q.em, 1788897600);
        assert_eq!(q.grade, Grade::Atrasado(1400), "o atraso é o medido");
        assert_eq!(q.anterior, Some(148.35));
        assert_eq!(q.fonte, "kinvo");
    }

    #[test]
    fn a_variacao_do_dia_sai_do_fechamento_anterior() {
        let serie = serde_json::json!({
            "previousClose": 100.0,
            "timestamp": [1788897600],
            "quotation": [{"close": 102.5}],
        });
        let q = ler(&AssetId::new(Market::B3, "PETR4"), &serie, 1788899000).unwrap();
        assert!((q.variacao().unwrap() - 2.5).abs() < 1e-9);
    }

    #[test]
    fn ticker_que_nao_existe_volta_vazio_e_nao_vira_preco_zero() {
        // A API responde 200 com as listas vazias para um símbolo desconhecido. Sem este
        // descarte, ele viraria uma cotação de zero — e a cadeia de reserva nunca teria a
        // chance de perguntar a quem sabe.
        let serie = serde_json::json!({
            "previousClose": Value::Null,
            "timestamp": [],
            "quotation": [],
        });
        assert!(ler(&AssetId::new(Market::B3, "NAOEXISTE99"), &serie, 1).is_none());
    }

    #[test]
    fn um_instante_no_futuro_e_puxado_para_agora() {
        // Um carimbo adiantado faria a tela dizer «há -2 min». O `min` existe por isso.
        let serie = serde_json::json!({
            "previousClose": 10.0,
            "timestamp": [9_999_999_999u64],
            "quotation": [{"close": 11.0}],
        });
        let q = ler(&AssetId::new(Market::B3, "PETR4"), &serie, 1000).unwrap();
        assert_eq!(q.em, 1000);
    }

    /// Ao vivo, e por isso ignorado por padrão: `cargo test -- --ignored kinvo`.
    #[test]
    #[ignore]
    fn ao_vivo_a_b3_e_o_ibov_respondem_num_lote_so() {
        let ativos = vec![
            AssetId::new(Market::B3, "PETR4"),
            AssetId::new(Market::B3, "HGLG11"),
            AssetId::new(Market::Outro, "IBOV"),
        ];
        let quotes = Kinvo.quotes(&ativos).expect("a Kinvo tem que responder");
        assert_eq!(quotes.len(), 3, "os três num pedido só");
        for q in &quotes {
            assert!(q.preco > 0.0, "{} veio sem preço", q.ativo.short());
            assert!(
                q.anterior.is_some(),
                "{} veio sem anterior",
                q.ativo.short()
            );
        }
    }

    #[test]
    #[ignore]
    fn ao_vivo_a_serie_do_ibov_existe_e_esta_ordenada() {
        let ibov = AssetId::new(Market::Outro, "IBOV");
        let velas = Kinvo.history(&ibov, Span::Ano).unwrap().unwrap();
        assert!(velas.len() > 200, "um ano de pregões");
        assert!(velas.windows(2).all(|p| p[0].em <= p[1].em), "em ordem");
        assert!(velas.iter().all(|c| c.fechamento > 1000.0), "é um índice");
    }
}

#[cfg(test)]
mod proventos_tests {
    use super::*;

    /// A data-com é um **dia**, e tem que voltar sendo o mesmo dia em Brasília.
    ///
    /// O erro que este teste tranca era silencioso e valia para todos: lida como
    /// meia-noite UTC, a data-com de 31/08 aparecia na tela como 30/08 — três horas para
    /// trás bastam para trocar o dia, e a tela da Invest lê tudo em BRT.
    #[test]
    fn a_data_com_nao_anda_um_dia_para_tras() {
        use crate::invest::tempo::{BRT_OFFSET, Data};
        for dia in ["2026-08-31", "2026-01-01", "2026-12-31", "2026-03-19"] {
            let em = data_com(dia).expect("lê a data");
            let d = Data::de_epoch(em, BRT_OFFSET);
            assert_eq!(
                format!("{}-{:02}-{:02}", d.ano, d.mes, d.dia),
                dia,
                "a data-com {dia} voltou como outro dia"
            );
        }
    }

    /// Ao vivo: `cargo test -- --ignored proventos`.
    #[test]
    #[ignore]
    fn ao_vivo_os_proventos_anunciados_chegam_com_data_e_valor() {
        let bbse = AssetId::new(Market::B3, "BBSE3");
        let v = Kinvo.proventos(&bbse).unwrap().unwrap();
        assert!(v.len() > 10, "a BBSE3 paga há anos, vieram {}", v.len());
        assert!(
            v.windows(2).all(|p| p[0].em >= p[1].em),
            "do mais recente para o mais antigo"
        );
        assert!(v.iter().all(|a| a.por_cota > 0.0), "sem pagamento zerado");
        assert!(v.iter().all(|a| a.ativo == bbse));
    }

    #[test]
    #[ignore]
    fn ao_vivo_um_papel_que_nao_paga_volta_vazio_e_nao_erra() {
        // Vazio é uma resposta legítima, e ela não pode virar erro: a tela ficaria
        // acusando falha numa empresa que simplesmente não distribuiu.
        let v = Kinvo
            .proventos(&AssetId::new(Market::B3, "PRNR3"))
            .unwrap()
            .expect("responde, mesmo que sem nada");
        assert!(v.iter().all(|a| a.por_cota > 0.0));
    }
}
