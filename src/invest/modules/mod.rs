//! O registro de módulos. Espelha `tools::all_tools()`, pelo mesmo motivo que aquele
//! existe: uma lista num lugar só, e um módulo novo é uma linha nela.

pub mod agenda;
pub mod alertas;
pub mod alocacao;
pub mod book;
pub mod cambio;
pub mod carteiras;
pub mod comum;
pub mod corretoras;
pub mod cotacoes;
pub mod cripto;
pub mod fundamentos;
pub mod grafico;
pub mod heatmap;
pub mod importacao;
pub mod indicadores;
pub mod indices;
pub mod lancamentos;
pub mod mercado;
pub mod noticias;
pub mod patrimonio;
pub mod posicoes;
pub mod proventos;
pub mod rebalanceamento;
pub mod risco;

use crate::invest::module::InvestModule;

/// A leitura de uma data escrita por gente, compartilhada por quem pede uma. Uma cópia só
/// para que «dd/mm/aaaa» signifique a mesma coisa em toda tela.
pub use lancamentos::ler_data as lancamentos_data;

/// A ordem aqui é a ordem em que os módulos aparecem para quem nunca abriu nenhum — e é
/// a ordem em que alguém constrói o uso: primeiro o que é meu, depois o que está
/// acontecendo, depois o que isso quer dizer.
pub fn todos() -> Vec<Box<dyn InvestModule>> {
    vec![
        // Carteira
        Box::new(posicoes::Posicoes),
        Box::new(carteiras::Carteiras),
        Box::new(alocacao::Alocacao),
        Box::new(rebalanceamento::Rebalanceamento),
        Box::new(patrimonio::Patrimonio),
        Box::new(corretoras::Corretoras),
        Box::new(proventos::Proventos),
        Box::new(lancamentos::Lancamentos),
        // Mercado
        Box::new(cotacoes::Cotacoes),
        Box::new(cambio::Cambio),
        Box::new(cripto::Cripto),
        Box::new(grafico::Grafico),
        Box::new(indices::Indices),
        Box::new(heatmap::Heatmap),
        Box::new(book::Book),
        // Análise
        Box::new(risco::Risco),
        Box::new(indicadores::Indicadores),
        Box::new(fundamentos::Fundamentos),
        // Operação
        Box::new(importacao::Importacao),
        // Informação
        Box::new(alertas::Alertas),
        Box::new(noticias::Noticias),
        Box::new(agenda::Agenda),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invest::marcas;
    use crate::invest::model::{
        Alerta, AssetId, Carteira, Classe, Lancamento, Market, Moeda, Portfolio, Position,
        Provento, RegraAlerta, TipoLancamento, TipoProvento,
    };
    use crate::invest::module::{Pane, ctx_de_teste, providers_de_teste};
    use crate::invest::provider::MarketSnapshot;

    fn posicao(ticker: &str, classe: Classe, carteira: Option<&str>) -> Position {
        Position {
            fonte: "manual".into(),
            conta: String::new(),
            ativo: AssetId::new(Market::B3, ticker),
            classe,
            quantidade: 100.0,
            preco_medio: Some(10.0),
            moeda: Moeda::Brl,
            preco_manual: Some(12.0),
            preco_manual_em: Some(0),
            atualizado_em: 0,
            carteira: carteira.map(str::to_string),
        }
    }

    /// Uma carteira com alguma coisa em toda lista que os módulos desenham, para que o
    /// teste abaixo alcance o máximo de telas possível.
    pub(super) fn carteira_cheia() -> Portfolio {
        Portfolio {
            posicoes: vec![
                posicao("PETR4", Classe::Acao, Some("Nord")),
                posicao("HGLG11", Classe::Fii, None),
                Position {
                    ativo: AssetId::new(Market::Binance, "BTCBRL"),
                    classe: Classe::Cripto,
                    ..posicao("PETR4", Classe::Cripto, None)
                },
            ],
            feeds: vec!["https://exemplo.invalido/rss".into()],
            carteiras: vec![Carteira {
                nome: "Nord".into(),
                alvos: Vec::new(),
            }],
            lancamentos: vec![Lancamento {
                em: 0,
                tipo: TipoLancamento::Compra,
                ativo: Some(AssetId::new(Market::B3, "PETR4")),
                quantidade: 100.0,
                preco: 10.0,
                taxas: 0.0,
                valor: 1000.0,
                moeda: Some(Moeda::Brl),
                nota: String::new(),
            }],
            alertas: vec![Alerta {
                ativo: AssetId::new(Market::B3, "PETR4"),
                regra: RegraAlerta::Acima,
                valor: 40.0,
                ligado: true,
                armado: false,
                ultimo_disparo: None,
            }],
            proventos: vec![Provento {
                ativo: AssetId::new(Market::B3, "PETR4"),
                tipo: TipoProvento::Dividendo,
                pago_em: 0,
                bruto: 100.0,
                retido: 0.0,
                moeda: Moeda::Brl,
                quantidade: Some(100.0),
            }],
            ..Default::default()
        }
    }

    /// Toda coluna declarada em `marcavel()` existe de verdade na tabela que o módulo
    /// desenha.
    ///
    /// É o erro que este desenho torna possível e silencioso: a coluna é dita pelo texto
    /// do cabeçalho, então um «Ativo» que virou «Papel» não quebra compilação nenhuma —
    /// só faz o `Ctrl+E` daquela tela parar de fazer qualquer coisa, sem uma mensagem.
    #[test]
    fn as_colunas_declaradas_existem_nas_tabelas_que_os_modulos_desenham() {
        let portfolio = carteira_cheia();
        let market = MarketSnapshot::default();
        let providers = providers_de_teste();
        let ctx = ctx_de_teste(&portfolio, &market, &providers);

        let mut conferidos = 0;
        for modulo in todos() {
            let Some(alvo) = modulo.marcavel() else {
                continue;
            };
            let vista = modulo.open(&ctx, None);
            let layout = vista.layout(&ctx);
            // Um módulo que, com esta carteira, ainda não desenha tabela (Notícias sem
            // feed, Agenda sem evento) não é conferido aqui — e não é fingido que foi.
            let Some(Pane::Table { headers, .. }) = layout.primeira_tabela() else {
                continue;
            };
            let tipos = marcas::tipos(&alvo, headers);
            assert!(
                !tipos.is_empty(),
                "{}: nenhuma coluna declarada existe em {headers:?}",
                modulo.id()
            );
            conferidos += 1;
        }
        // O teste não pode degradar em silêncio: hoje ele alcança treze dos catorze
        // módulos marcáveis — o que fica de fora é Risco, que só desenha tabela com série
        // histórica, e história não se inventa numa carteira de teste.
        assert!(conferidos >= 13, "só {conferidos} módulos foram conferidos");
    }

    /// O mesmo, no cartão da home. Ele mostra menos colunas que a tela aberta e às vezes
    /// noutra ordem — que é exatamente o motivo de a coluna ser dita pelo cabeçalho.
    #[test]
    fn as_colunas_declaradas_existem_tambem_nos_cartoes_da_home() {
        let portfolio = carteira_cheia();
        let market = MarketSnapshot::default();
        let providers = providers_de_teste();
        let ctx = ctx_de_teste(&portfolio, &market, &providers);

        let mut conferidos = 0;
        for modulo in todos() {
            let Some(alvo) = modulo.marcavel() else {
                continue;
            };
            for cartaz in modulo.cartazes(&ctx) {
                let Some(Pane::Table { headers, .. }) = cartaz.pane.as_ref() else {
                    continue;
                };
                assert!(
                    !marcas::tipos(&alvo, headers).is_empty(),
                    "{}: o cartão não tem nenhuma coluna declarada em {headers:?}",
                    modulo.id()
                );
                conferidos += 1;
            }
        }
        // Três: Posições, Cotações e a carteira recomendada. O cartão de Proventos só
        // nasce com anúncios em cache, e cache buscado é coisa que um teste não tem.
        assert!(conferidos >= 3, "só {conferidos} cartões foram conferidos");
    }

    /// Toda linha tem tantas células quanto a tabela tem colunas.
    ///
    /// É o erro que uma coluna nova provoca e que nada no compilador pega: a seta de
    /// momento entrou entre «PM» e «Atual» em Posições, e a linha de cabeçalho de grupo
    /// — montada à mão, com células vazias contadas na unha — passou a pôr o total do
    /// grupo na coluna do preço. Uma linha curta não estoura nada; ela só mostra o
    /// número debaixo do rótulo errado.
    #[test]
    fn toda_linha_tem_uma_celula_por_coluna() {
        let portfolio = carteira_cheia();
        let market = MarketSnapshot::default();
        let providers = providers_de_teste();
        let ctx = ctx_de_teste(&portfolio, &market, &providers);

        let mut conferidas = 0;
        for modulo in todos() {
            let mut panes = vec![modulo.open(&ctx, None).layout(&ctx)];
            let mut tabelas: Vec<(&str, &Vec<String>, &Vec<crate::invest::module::Row>)> =
                Vec::new();
            for layout in &panes {
                if let Some(Pane::Table { headers, rows, .. }) = layout.primeira_tabela() {
                    tabelas.push((modulo.id(), headers, rows));
                }
            }
            for (id, headers, rows) in &tabelas {
                for (i, linha) in rows.iter().enumerate() {
                    assert_eq!(
                        linha.cells.len(),
                        headers.len(),
                        "{id}: linha {i} tem {} células para {} colunas — {:?}",
                        linha.cells.len(),
                        headers.len(),
                        linha.cells
                    );
                    conferidas += 1;
                }
            }
            panes.clear();
            // E os cartões da home, que montam as linhas deles à parte.
            for cartaz in modulo.cartazes(&ctx) {
                let Some(Pane::Table { headers, rows, .. }) = cartaz.pane.as_ref() else {
                    continue;
                };
                for (i, linha) in rows.iter().enumerate() {
                    assert_eq!(
                        linha.cells.len(),
                        headers.len(),
                        "{}: cartão, linha {i} com {} células para {} colunas",
                        modulo.id(),
                        linha.cells.len(),
                        headers.len()
                    );
                    conferidas += 1;
                }
            }
        }
        assert!(conferidas > 20, "só {conferidas} linhas foram conferidas");
    }

    /// Um id pode ser dividido — as dez telas de papéis dividem o mesmo —, mas então o
    /// nome tem que ser o mesmo também. Duas telas com um id e dois nomes fariam a mesma
    /// marca se apresentar de dois jeitos na lista, conforme de onde tivesse nascido.
    #[test]
    fn um_id_de_tabela_tem_um_nome_so() {
        let modulos = todos();
        let mut por_id: std::collections::BTreeMap<&str, &str> = Default::default();
        for modulo in &modulos {
            let Some(alvo) = modulo.marcavel() else {
                continue;
            };
            match por_id.entry(alvo.tabela) {
                std::collections::btree_map::Entry::Vacant(v) => {
                    v.insert(alvo.nome);
                }
                std::collections::btree_map::Entry::Occupied(o) => assert_eq!(
                    *o.get(),
                    alvo.nome,
                    "{} divide o id «{}» com outro nome",
                    modulo.id(),
                    alvo.tabela
                ),
            }
        }
    }

    /// As dez telas que listam papéis dividem mesmo o id — é o que faz uma marca em
    /// Posições acender em Cotações e na carteira recomendada.
    #[test]
    fn as_listas_de_papeis_dividem_uma_lista_de_marcas_so() {
        let esperado = [
            "posicoes",
            "cotacoes",
            "cripto",
            "cambio",
            "carteiras",
            "proventos",
            "lancamentos",
            "alertas",
            "fundamentos",
            "risco",
        ];
        for modulo in todos() {
            if !esperado.contains(&modulo.id()) {
                continue;
            }
            let alvo = modulo.marcavel().expect("lista de papéis é marcável");
            assert_eq!(
                alvo.tabela,
                crate::invest::marcas::ATIVOS.tabela,
                "{} saiu da lista de papéis",
                modulo.id()
            );
        }
    }
}

#[cfg(test)]
mod inventario {
    use super::*;
    use crate::invest::module::{Pane, ctx_de_teste, providers_de_teste};
    use crate::invest::provider::MarketSnapshot;

    #[test]
    #[ignore]
    fn listar() {
        let portfolio = super::tests::carteira_cheia();
        let market = MarketSnapshot::default();
        let providers = providers_de_teste();
        let ctx = ctx_de_teste(&portfolio, &market, &providers);
        for m in todos() {
            let marcavel = m.marcavel().map(|a| a.tabela).unwrap_or("—");
            for c in m.cartazes(&ctx) {
                let tipo = match c.pane.as_ref() {
                    Some(Pane::Table { headers, .. }) => format!("Table {headers:?}"),
                    Some(Pane::Chart { .. }) => "Chart".into(),
                    Some(Pane::Facts { rows, .. }) => format!("Facts {}", rows.len()),
                    Some(Pane::Bars { rows, .. }) => format!("Bars {}", rows.len()),
                    Some(Pane::Grid { cells, .. }) => format!("Grid {}", cells.len()),
                    Some(Pane::Text { lines, .. }) => format!("Text {}", lines.len()),
                    Some(Pane::Empty { .. }) => "Empty".into(),
                    Some(Pane::Form { .. }) => "Form".into(),
                    None => "—".into(),
                };
                println!("{:<14} {:<16} {}", m.id(), marcavel, tipo);
            }
        }
    }
}
