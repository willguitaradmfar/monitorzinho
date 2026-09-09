//! O que aportar para chegar na carteira recomendada.
//!
//! A conta é uma só e vale a pena escrevê-la aqui, fora da tela: para cada ativo do alvo,
//! **quanto ele deveria valer** menos **quanto ele vale**. O resto — reais, porcentagem,
//! quantidade — é a mesma diferença dita de três jeitos.
//!
//! Duas decisões que a conta carrega:
//!
//! * **O total é o da carteira, não o do patrimônio.** Uma carteira recomendada de ações
//!   não deve ser diluída pelo Tesouro que está fora dela. Só entram no total as posições
//!   marcadas como daquela carteira.
//! * **O teto suspende a compra, não o alvo.** Um ativo acima do preço máximo continua
//!   com o alvo dele; o que muda é que a linha diz para não comprar agora. Tirá-lo da
//!   conta redistribuiria o dinheiro dele entre os outros, que é uma decisão de quem
//!   investe e não de uma fórmula.

use crate::invest::carteira::{self, Linha};
use crate::invest::model::{AssetId, Carteira, Portfolio};
use crate::invest::provider::MarketSnapshot;

/// Uma linha do «o que fazer»: um ativo do alvo, e a distância até ele.
pub struct Ajuste {
    /// A posição na recomendação. Quem está na carteira e fora do alvo vem depois de
    /// todos — ele não tem posição na lista porque saiu dela.
    pub ordem: u32,
    pub ativo: AssetId,
    /// O peso que ele deve ter, em porcento.
    pub alvo_pct: f64,
    /// O peso que ele tem hoje **dentro desta carteira**, em porcento.
    pub atual_pct: f64,
    /// Quanto ele vale hoje, em BRL.
    pub atual: f64,
    /// Quanto falta (positivo) ou sobra (negativo), em BRL.
    pub delta: f64,
    /// O preço unitário de agora. `None` quando ninguém sabe dizer — e aí a quantidade
    /// também não existe, em vez de ser zero.
    pub preco: Option<f64>,
    /// Quantas unidades comprar ou vender. Fracionária de propósito: arredondar para lote
    /// é decisão de quem manda a ordem, e depende da corretora.
    pub quantidade: Option<f64>,
    /// O teto de preço, quando há, e se ele está estourado agora.
    ///
    /// **`acima_do_teto` só importa para comprar.** O teto é o preço máximo que se aceita
    /// pagar; vender um papel que passou dele não é problema nenhum — é, aliás, o que se
    /// costuma querer. Quem mostra a linha tem que olhar o sinal do `delta` junto.
    pub teto: Option<f64>,
    pub acima_do_teto: bool,
}

impl Ajuste {
    /// Se esta linha pede compra. Um ativo acima do teto **não** pede, mesmo faltando.
    pub fn comprar(&self) -> bool {
        self.delta > 0.0 && !self.acima_do_teto
    }
}

/// O retrato de uma carteira recomendada contra o que se tem.
pub struct Plano {
    pub total: f64,
    pub ajustes: Vec<Ajuste>,
    /// Ativos marcados como desta carteira que **não estão no alvo**. Eles entram no
    /// total — o dinheiro está lá — e aparecem para vender: alvo zero é alvo.
    pub fora_do_alvo: usize,
}

impl Plano {
    /// O quanto a carteira está longe do alvo: a soma das distâncias, dividida por dois.
    ///
    /// Dividida por dois porque cada real fora do lugar é contado duas vezes — falta num
    /// ativo e sobra noutro. Sem isso, uma carteira 10 p.p. desalinhada mostraria 20.
    pub fn desvio(&self) -> f64 {
        self.ajustes
            .iter()
            .map(|a| (a.alvo_pct - a.atual_pct).abs())
            .sum::<f64>()
            / 2.0
    }
}

/// Monta o plano de uma carteira recomendada.
pub fn plano(
    carteira: &Carteira,
    portfolio: &Portfolio,
    market: &MarketSnapshot,
    agora: u64,
) -> Plano {
    let linhas = carteira::linhas(portfolio, market, agora);
    // Só o que foi marcado como desta carteira. É o que faz o total ser o dela.
    let minhas: Vec<&Linha> = linhas
        .iter()
        .filter(|l| l.posicao.carteira.as_deref() == Some(carteira.nome.as_str()))
        .collect();
    let total: f64 = minhas.iter().filter_map(|l| l.mercado_brl).sum();

    let valor_de = |ativo: &AssetId| -> f64 {
        minhas
            .iter()
            .filter(|l| l.posicao.ativo == *ativo)
            .filter_map(|l| l.mercado_brl)
            .sum()
    };

    let mut ajustes: Vec<Ajuste> = carteira
        .alvos
        .iter()
        .map(|alvo| {
            linha(
                alvo.ordem,
                &alvo.ativo,
                alvo.percentual,
                alvo.teto,
                valor_de(&alvo.ativo),
                total,
                market,
            )
        })
        .collect();

    // O que está na carteira e não está no alvo: alvo zero, e a linha pede venda.
    let mut fora_do_alvo = 0;
    for l in &minhas {
        let ativo = &l.posicao.ativo;
        if carteira.alvos.iter().any(|a| a.ativo == *ativo)
            || ajustes.iter().any(|a| a.ativo == *ativo)
        {
            continue;
        }
        fora_do_alvo += 1;
        ajustes.push(linha(
            u32::MAX,
            ativo,
            0.0,
            None,
            valor_de(ativo),
            total,
            market,
        ));
    }

    // **A ordem da recomendação**, e não a do que falta comprar. A lista já vem
    // priorizada por quem a publicou, e reordená-la pelo nosso cálculo trocaria o critério
    // do analista pelo nosso. Empate — dois com a mesma posição, ou os que saíram do alvo
    // — desempata por quem precisa de mais dinheiro.
    ajustes.sort_by(|a, b| a.ordem.cmp(&b.ordem).then(b.delta.total_cmp(&a.delta)));
    Plano {
        total,
        ajustes,
        fora_do_alvo,
    }
}

#[allow(clippy::too_many_arguments)]
fn linha(
    ordem: u32,
    ativo: &AssetId,
    alvo_pct: f64,
    teto: Option<f64>,
    atual: f64,
    total: f64,
    market: &MarketSnapshot,
) -> Ajuste {
    let alvo_valor = total * alvo_pct / 100.0;
    let delta = alvo_valor - atual;
    let preco = market.quote(ativo).map(|q| q.preco);
    Ajuste {
        ordem,
        ativo: ativo.clone(),
        alvo_pct,
        atual_pct: match total > 0.0 {
            true => atual / total * 100.0,
            false => 0.0,
        },
        atual,
        delta,
        preco,
        quantidade: preco.filter(|p| *p > 0.0).map(|p| delta / p),
        teto,
        acima_do_teto: matches!((preco, teto), (Some(p), Some(t)) if p > t),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invest::model::{AlvoCarteira, Classe, Moeda, Position};
    use crate::invest::provider::{Grade, Quote};

    fn pos(ativo: &str, qtd: f64, carteira: Option<&str>) -> Position {
        Position {
            fonte: "t".into(),
            conta: String::new(),
            ativo: AssetId::parse(ativo).unwrap(),
            classe: Classe::Acao,
            quantidade: qtd,
            preco_medio: None,
            moeda: Moeda::Brl,
            preco_manual: None,
            preco_manual_em: None,
            atualizado_em: 0,
            carteira: carteira.map(|c| c.to_string()),
        }
    }

    fn mercado(precos: &[(&str, f64)]) -> MarketSnapshot {
        let mut m = MarketSnapshot::default();
        for (a, p) in precos {
            let ativo = AssetId::parse(a).unwrap();
            m.quotes.insert(
                ativo.clone(),
                Quote {
                    fonte: "t",
                    ativo,
                    preco: *p,
                    anterior: None,
                    moeda: Moeda::Brl,
                    grade: Grade::AoVivo,
                    em: 0,
                    volume: None,
                    max24: None,
                    min24: None,
                },
            );
        }
        m
    }

    fn alvo(ativo: &str, pct: f64, teto: Option<f64>) -> AlvoCarteira {
        AlvoCarteira {
            ordem: 0,
            ativo: AssetId::parse(ativo).unwrap(),
            percentual: pct,
            teto,
        }
    }

    #[test]
    fn o_total_e_o_da_carteira_e_nao_o_do_patrimonio() {
        // A regra que faz o número significar alguma coisa: uma carteira recomendada de
        // ações não pode ser diluída pelo Tesouro que está fora dela.
        let mut p = Portfolio::default();
        p.posicoes.push(pos("B3/PETR4", 100.0, Some("Dividendos")));
        p.posicoes.push(pos("B3/VALE3", 100.0, Some("Dividendos")));
        p.posicoes.push(pos("OUTRO/TESOURO", 1000.0, None));
        let m = mercado(&[
            ("B3/PETR4", 50.0),
            ("B3/VALE3", 50.0),
            ("OUTRO/TESOURO", 100.0),
        ]);
        let c = Carteira {
            nome: "Dividendos".into(),
            alvos: vec![alvo("B3/PETR4", 50.0, None), alvo("B3/VALE3", 50.0, None)],
        };
        let plano = plano(&c, &p, &m, 0);
        assert_eq!(plano.total, 10_000.0, "só as duas marcadas");
        assert!(
            plano.ajustes.iter().all(|a| a.delta.abs() < 1e-9),
            "já no alvo"
        );
        assert!(plano.desvio() < 1e-9);
    }

    #[test]
    fn o_delta_vira_reais_porcentagem_e_quantidade() {
        let mut p = Portfolio::default();
        p.posicoes.push(pos("B3/PETR4", 100.0, Some("C")));
        p.posicoes.push(pos("B3/VALE3", 100.0, Some("C")));
        // PETR4 vale 8 mil, VALE3 2 mil: 80/20 quando o alvo é 50/50.
        let m = mercado(&[("B3/PETR4", 80.0), ("B3/VALE3", 20.0)]);
        let c = Carteira {
            nome: "C".into(),
            alvos: vec![alvo("B3/PETR4", 50.0, None), alvo("B3/VALE3", 50.0, None)],
        };
        let plano = plano(&c, &p, &m, 0);
        assert_eq!(plano.total, 10_000.0);

        let vale = plano
            .ajustes
            .iter()
            .find(|a| a.ativo.symbol == "VALE3")
            .unwrap();
        assert!((vale.delta - 3_000.0).abs() < 1e-9, "faltam 3 mil na VALE3");
        assert!((vale.atual_pct - 20.0).abs() < 1e-9);
        assert!((vale.quantidade.unwrap() - 150.0).abs() < 1e-9, "3000 / 20");
        assert!(vale.comprar());

        let petr = plano
            .ajustes
            .iter()
            .find(|a| a.ativo.symbol == "PETR4")
            .unwrap();
        assert!((petr.delta + 3_000.0).abs() < 1e-9, "sobram 3 mil na PETR4");
        assert!(!petr.comprar(), "sobrando, não se compra");
        // Trinta pontos fora do lugar, e não sessenta: cada real conta uma vez.
        assert!((plano.desvio() - 30.0).abs() < 1e-9);
    }

    #[test]
    fn o_teto_barra_a_compra_e_nao_a_venda() {
        // O teto é o preço máximo que se aceita **pagar**. Vender um papel que passou dele
        // não é problema — é o que se costuma querer. A primeira versão da tela escrevia
        // «acima do teto» no lugar de «vender», e escondia a ordem que importava.
        let mut p = Portfolio::default();
        p.posicoes.push(pos("B3/PETR4", 100.0, Some("C")));
        p.posicoes.push(pos("B3/VALE3", 100.0, Some("C")));
        // A VALE3 está cara **e** sobrando: 80% da carteira contra um alvo de 20%.
        let m = mercado(&[("B3/PETR4", 20.0), ("B3/VALE3", 80.0)]);
        let c = Carteira {
            nome: "C".into(),
            alvos: vec![
                alvo("B3/PETR4", 80.0, None),
                alvo("B3/VALE3", 20.0, Some(50.0)),
            ],
        };
        let plano = plano(&c, &p, &m, 0);
        let vale = plano
            .ajustes
            .iter()
            .find(|a| a.ativo.symbol == "VALE3")
            .unwrap();
        assert!(vale.acima_do_teto, "80 é mais que o teto de 50");
        assert!(vale.delta < 0.0, "e ela está sobrando");
        assert!(!vale.comprar(), "não se compra");
        // O sinal do delta é o que a tela olha para decidir o que escrever.
        assert!(
            vale.acima_do_teto && vale.delta < 0.0,
            "cara e sobrando ao mesmo tempo: a ordem é vender"
        );
    }

    #[test]
    fn acima_do_teto_a_compra_para_mas_o_alvo_fica() {
        // Tirar o ativo da conta redistribuiria o dinheiro dele entre os outros — uma
        // decisão de quem investe, não de uma fórmula. O alvo continua; a compra é que não.
        let mut p = Portfolio::default();
        p.posicoes.push(pos("B3/PETR4", 100.0, Some("C")));
        p.posicoes.push(pos("B3/VALE3", 100.0, Some("C")));
        let m = mercado(&[("B3/PETR4", 80.0), ("B3/VALE3", 20.0)]);
        let c = Carteira {
            nome: "C".into(),
            alvos: vec![
                alvo("B3/PETR4", 50.0, None),
                alvo("B3/VALE3", 50.0, Some(15.0)),
            ],
        };
        let plano = plano(&c, &p, &m, 0);
        let vale = plano
            .ajustes
            .iter()
            .find(|a| a.ativo.symbol == "VALE3")
            .unwrap();
        assert!(vale.acima_do_teto, "20 é mais que o teto de 15");
        assert!((vale.delta - 3_000.0).abs() < 1e-9, "o alvo não mudou");
        assert!(!vale.comprar(), "mas não se compra acima do teto");
    }

    #[test]
    fn o_que_esta_na_carteira_e_fora_do_alvo_aparece_para_vender() {
        // Alvo zero é alvo. Um papel que entrou na carteira e saiu da recomendação não
        // pode sumir da tela — ele é justamente o que precisa ser desfeito.
        let mut p = Portfolio::default();
        p.posicoes.push(pos("B3/PETR4", 100.0, Some("C")));
        p.posicoes.push(pos("B3/SHUL4", 100.0, Some("C")));
        let m = mercado(&[("B3/PETR4", 50.0), ("B3/SHUL4", 50.0)]);
        let c = Carteira {
            nome: "C".into(),
            alvos: vec![alvo("B3/PETR4", 100.0, None)],
        };
        let plano = plano(&c, &p, &m, 0);
        assert_eq!(plano.fora_do_alvo, 1);
        let shul = plano
            .ajustes
            .iter()
            .find(|a| a.ativo.symbol == "SHUL4")
            .unwrap();
        assert_eq!(shul.alvo_pct, 0.0);
        assert!(shul.delta < 0.0, "tem que sair");
    }

    #[test]
    fn a_ordem_e_a_da_recomendacao_e_nao_a_do_que_falta_comprar() {
        // A lista publicada já vem priorizada, e a prioridade é parte da recomendação:
        // o primeiro é o primeiro por uma razão que este programa não conhece. Ordenar
        // pelo que falta comprar trocaria o critério do analista pelo nosso.
        let mut p = Portfolio::default();
        p.posicoes.push(pos("B3/PETR4", 100.0, Some("C")));
        p.posicoes.push(pos("B3/VALE3", 100.0, Some("C")));
        let m = mercado(&[("B3/PETR4", 80.0), ("B3/VALE3", 20.0)]);
        let c = Carteira {
            nome: "C".into(),
            // A VALE3 é a que mais precisa de dinheiro, e é a **segunda** da lista.
            alvos: vec![
                AlvoCarteira {
                    ordem: 1,
                    ..alvo("B3/PETR4", 50.0, None)
                },
                AlvoCarteira {
                    ordem: 2,
                    ..alvo("B3/VALE3", 50.0, None)
                },
            ],
        };
        let plano = plano(&c, &p, &m, 0);
        let ordem: Vec<&str> = plano
            .ajustes
            .iter()
            .map(|a| a.ativo.symbol.as_str())
            .collect();
        assert_eq!(
            ordem,
            ["PETR4", "VALE3"],
            "a ordem da lista, não a do delta"
        );
    }

    #[test]
    fn quem_saiu_do_alvo_vai_para_o_fim_da_lista() {
        // Ele não tem posição na recomendação porque saiu dela — e sem isso ele apareceria
        // na frente de todos, já que `0` é o menor número.
        let mut p = Portfolio::default();
        p.posicoes.push(pos("B3/PETR4", 100.0, Some("C")));
        p.posicoes.push(pos("B3/SHUL4", 100.0, Some("C")));
        let m = mercado(&[("B3/PETR4", 50.0), ("B3/SHUL4", 50.0)]);
        let c = Carteira {
            nome: "C".into(),
            alvos: vec![AlvoCarteira {
                ordem: 9,
                ..alvo("B3/PETR4", 100.0, None)
            }],
        };
        let plano = plano(&c, &p, &m, 0);
        assert_eq!(plano.ajustes.last().unwrap().ativo.symbol, "SHUL4");
        assert_eq!(plano.ajustes.last().unwrap().ordem, u32::MAX);
    }

    #[test]
    fn carteira_vazia_nao_divide_por_zero() {
        let p = Portfolio::default();
        let m = mercado(&[]);
        let c = Carteira {
            nome: "C".into(),
            alvos: vec![alvo("B3/PETR4", 100.0, None)],
        };
        let plano = plano(&c, &p, &m, 0);
        assert_eq!(plano.total, 0.0);
        let a = &plano.ajustes[0];
        assert_eq!(a.atual_pct, 0.0);
        assert_eq!(a.delta, 0.0);
        assert!(a.quantidade.is_none(), "sem preço não há quantidade");
    }
}
