//! Rebalanceamento: quanto comprar para voltar ao alvo.
//!
//! O modo de **aporte** é o padrão, e a razão é fiscal e não estética: no Brasil vender
//! realiza ganho e pode gerar imposto, enquanto comprar não gera nada. Um rebalanceamento
//! que sai vendendo por padrão é um rebalanceamento que custa dinheiro sem avisar.

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;

use crate::invest::calc;
use crate::invest::carteira;
use crate::invest::model::AssetId;
use crate::invest::model::Classe;
use crate::invest::module::{
    Ctx, Escape, Group, InvestModule, Layout, Marcavel, ModuleView, Need, Outcome, Pane, Row, Tone,
};
use crate::invest::modules::alocacao::{self, Dimensao};
use crate::invest::modules::comum::hint;

pub struct Rebalanceamento;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Modo {
    Aporte,
    Ajuste,
}

impl InvestModule for Rebalanceamento {
    fn id(&self) -> &'static str {
        "rebalanceamento"
    }
    fn name(&self) -> &'static str {
        "Rebalanceamento"
    }
    fn description(&self) -> &'static str {
        "Onde pôr o aporte para voltar ao alvo — sem vender por padrão"
    }
    fn destaque(&self) -> u8 {
        2
    }
    fn group(&self) -> Group {
        Group::Carteira
    }

    fn fonte_externa(&self) -> Option<&'static str> {
        Some(crate::invest::store::fonte::COTACAO)
    }

    /// Divide a lista de marcas dos ativos: o que aportar e resgatar por classe.
    fn marcavel(&self) -> Option<Marcavel> {
        Some(crate::invest::marcas::ATIVOS)
    }
    fn needs(&self) -> &'static [Need] {
        &[Need::Posicoes]
    }
    fn keywords(&self) -> &'static str {
        "aporte comprar vender alvo ajuste distribuir"
    }

    fn summary(&self, ctx: &Ctx) -> String {
        if ctx.portfolio.alvos.is_empty() {
            return "sem alvos definidos — defina em Alocação".into();
        }
        let linhas = carteira::linhas(ctx.portfolio, ctx.market, ctx.agora);
        let fatias = alocacao::fatias(&linhas, Dimensao::Classe, ctx);
        match alocacao::desvio_total(&fatias, &ctx.portfolio.alvos) {
            Some(d) => format!("desvio de {}", calc::pct_simples(d)),
            None => "sem alvos".into(),
        }
    }

    fn widget(&self, ctx: &Ctx) -> Option<Pane> {
        if ctx.portfolio.alvos.is_empty() || ctx.portfolio.posicoes.is_empty() {
            return None;
        }
        let linhas = carteira::linhas(ctx.portfolio, ctx.market, ctx.agora);
        let total: f64 = linhas.iter().filter_map(|l| l.mercado_brl).sum();
        let fatias = alocacao::fatias(&linhas, Dimensao::Classe, ctx);
        let mut movimentos: Vec<(String, f64)> = ctx
            .portfolio
            .alvos
            .iter()
            .map(|(classe, alvo)| {
                let atual = fatias
                    .iter()
                    .find(|(nome, _, _)| nome == classe)
                    .map_or(0.0, |(_, _, peso)| *peso);
                // Em reais, e não em pontos percentuais: «faltam 3 p.p.» não diz quanto
                // aportar, e é aportar que a pessoa vai fazer depois de olhar isto.
                (classe.clone(), (alvo - atual) / 100.0 * total)
            })
            .collect();
        movimentos.sort_by(|a, b| b.1.abs().total_cmp(&a.1.abs()));
        Some(Pane::Facts {
            title: String::new(),
            rows: movimentos
                .into_iter()
                .take(8)
                .map(|(classe, reais)| {
                    (
                        classe,
                        match reais >= 0.0 {
                            true => format!("comprar R$ {}", calc::moeda(reais)),
                            false => format!("vender R$ {}", calc::moeda(-reais)),
                        },
                        match reais >= 0.0 {
                            true => Tone::Bom,
                            false => Tone::Ruim,
                        },
                    )
                })
                .collect(),
        })
    }

    fn open(&self, _ctx: &Ctx, _alvo: Option<&AssetId>) -> Box<dyn ModuleView> {
        Box::new(Vista {
            aporte: 3_000.0,
            modo: Modo::Aporte,
        })
    }
}

/// Quanto aplicar em cada fatia, e onde a alocação fica depois.
///
/// A ordem é «mais abaixo do alvo **em reais**», e não em pontos percentuais: uma classe
/// 5 p.p. abaixo numa carteira de R$ 500 mil precisa de mais dinheiro que a mesma
/// diferença numa de R$ 50 mil, e distribuir por pontos percentuais ignora isso.
pub fn distribuir(
    fatias: &[(String, f64, f64)],
    alvos: &std::collections::BTreeMap<String, f64>,
    total: f64,
    aporte: f64,
) -> Vec<(String, f64, f64)> {
    let alvo_de = |rotulo: &str| -> f64 {
        Classe::ALL
            .iter()
            .find(|c| c.label() == rotulo)
            .and_then(|c| alvos.get(c.code()).copied())
            .unwrap_or(0.0)
    };
    let depois_total = total + aporte;

    // Quanto falta para cada fatia atingir o alvo dentro do patrimônio **já com o aporte**.
    let mut faltas: Vec<(String, f64, f64)> = fatias
        .iter()
        .map(|(rotulo, valor, _)| {
            let ideal = depois_total * alvo_de(rotulo) / 100.0;
            (rotulo.clone(), (ideal - valor).max(0.0), *valor)
        })
        .collect();
    faltas.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let soma_faltas: f64 = faltas.iter().map(|(_, f, _)| f).sum();
    faltas
        .into_iter()
        .map(|(rotulo, falta, valor)| {
            // Cabendo tudo, cada uma recebe o que falta; não cabendo, recebe a fatia
            // proporcional do que falta — que é o que aproxima mais do alvo.
            let aplicar = match soma_faltas > 0.0 {
                true => (falta / soma_faltas * aporte).min(falta),
                false => 0.0,
            };
            let depois = match depois_total > 0.0 {
                true => (valor + aplicar) / depois_total * 100.0,
                false => 0.0,
            };
            (rotulo, aplicar, depois)
        })
        .collect()
}

struct Vista {
    aporte: f64,
    modo: Modo,
}

impl ModuleView for Vista {
    fn title(&self) -> String {
        match self.modo {
            Modo::Aporte => format!(
                "Rebalanceamento · aporte de R$ {}",
                calc::moeda(self.aporte)
            ),
            Modo::Ajuste => "Rebalanceamento · ajuste".into(),
        }
    }

    fn quer_mercado(&self) -> bool {
        true
    }

    fn layout(&self, ctx: &Ctx) -> Layout {
        if ctx.portfolio.alvos.is_empty() {
            return Layout::one(Pane::Empty {
                title: "Rebalanceamento".into(),
                note: "Sem alvos não há para onde voltar.\n\n\
                       Defina em Alocação (Ctrl+A) quanto você quer em cada classe."
                    .into(),
            });
        }
        let soma: f64 = ctx.portfolio.alvos.values().sum();
        if (soma - 100.0).abs() > 0.01 {
            // Aqui, diferente de Alocação, não dá para só mostrar: a conta depende da soma
            // fechar, e normalizar por conta própria mudaria os números que a pessoa
            // escreveu sem avisar.
            return Layout::one(Pane::Empty {
                title: "Rebalanceamento".into(),
                note: format!(
                    "Os seus alvos somam {}, e não 100%.\n\n\
                     A distribuição depende de a soma fechar, e ajustá-la por conta \n\
                     própria mudaria os números que você escreveu. Corrija em Alocação.",
                    calc::pct_simples(soma)
                ),
            });
        }

        let linhas = carteira::linhas(ctx.portfolio, ctx.market, ctx.agora);
        let totais = carteira::totais(&linhas);
        let fatias = alocacao::fatias(&linhas, Dimensao::Classe, ctx);
        let antes = alocacao::desvio_total(&fatias, &ctx.portfolio.alvos).unwrap_or(0.0);

        let aporte = match self.modo {
            Modo::Aporte => self.aporte,
            // Em modo de ajuste, o «aporte» é zero e o que se mostra é o que teria de sair
            // de uma fatia para entrar noutra.
            Modo::Ajuste => 0.0,
        };
        let plano = distribuir(&fatias, &ctx.portfolio.alvos, totais.mercado, aporte);

        let depois_fatias: Vec<(String, f64, f64)> = plano
            .iter()
            .map(|(rotulo, _, depois)| (rotulo.clone(), 0.0, *depois))
            .collect();
        let depois = alocacao::desvio_total(&depois_fatias, &ctx.portfolio.alvos).unwrap_or(0.0);

        let rows: Vec<Row> = plano
            .iter()
            .map(|(rotulo, aplicar, dep)| {
                let real = fatias
                    .iter()
                    .find(|(r, _, _)| r == rotulo)
                    .map(|(_, _, p)| *p)
                    .unwrap_or(0.0);
                let alvo = Classe::ALL
                    .iter()
                    .find(|c| c.label() == rotulo)
                    .and_then(|c| ctx.portfolio.alvos.get(c.code()).copied())
                    .unwrap_or(0.0);
                Row::new(vec![
                    rotulo.clone(),
                    calc::pct_simples(real),
                    calc::pct_simples(alvo),
                    calc::pct_simples(*dep),
                    // `—` é «não coube nesta rodada»; `0,00` seria «a conta deu zero».
                    match *aplicar > 0.005 {
                        true => format!("R$ {}", calc::moeda(*aplicar)),
                        false => "—".into(),
                    },
                ])
                .with_cell_tones(vec![
                    Tone::Normal,
                    Tone::Dim,
                    Tone::Dim,
                    Tone::Normal,
                    match *aplicar > 0.005 {
                        true => Tone::Bom,
                        false => Tone::Dim,
                    },
                ])
            })
            .collect();

        let aplicado: f64 = plano.iter().map(|(_, a, _)| a).sum();
        let fatos = vec![
            (
                "desvio".into(),
                format!(
                    "{} → {}",
                    calc::pct_simples(antes),
                    calc::pct_simples(depois)
                ),
                match depois < antes {
                    true => Tone::Bom,
                    false => Tone::Dim,
                },
            ),
            (
                "distribuído".into(),
                format!(
                    "R$ {} do aporte de R$ {}",
                    calc::moeda(aplicado),
                    calc::moeda(aporte)
                ),
                Tone::Normal,
            ),
            (
                "nada vendido".into(),
                "vender realiza ganho e pode gerar imposto — por isso o padrão só compra".into(),
                Tone::Dim,
            ),
            (
                "dentro da classe".into(),
                "o critério é proporcional ao peso atual — o único derivável dos dados".into(),
                Tone::Dim,
            ),
        ];

        Layout::rows(vec![
            (
                3,
                Layout::one(Pane::Table {
                    title: self.title(),
                    headers: vec![
                        "Classe".into(),
                        "Real".into(),
                        "Alvo".into(),
                        "Depois".into(),
                        "Aplicar".into(),
                    ],
                    rows,
                    selected: None,
                    query: String::new(),
                    note: None,
                }),
            ),
            (
                2,
                Layout::one(Pane::Facts {
                    title: "Resultado".into(),
                    rows: fatos,
                }),
            ),
        ])
    }

    fn key(&mut self, key: KeyEvent, _ctx: &Ctx) -> Outcome {
        // Nesta tela não há busca digitada, então letra pura basta — a regra geral: onde
        // há busca, a ação é `Ctrl+`; onde não há, letra basta.
        match key.code {
            KeyCode::Char('+') | KeyCode::Right => {
                self.aporte += 500.0;
                Outcome::Ok
            }
            KeyCode::Char('-') | KeyCode::Left => {
                self.aporte = (self.aporte - 500.0).max(0.0);
                Outcome::Ok
            }
            KeyCode::Up => {
                self.aporte += 5_000.0;
                Outcome::Ok
            }
            KeyCode::Down => {
                self.aporte = (self.aporte - 5_000.0).max(0.0);
                Outcome::Ok
            }
            KeyCode::Char('m') => {
                self.modo = match self.modo {
                    Modo::Aporte => Modo::Ajuste,
                    Modo::Ajuste => Modo::Aporte,
                };
                Outcome::Ok
            }
            KeyCode::Char('a') => Outcome::Abrir {
                modulo: "alocacao",
                alvo: None,
            },
            _ => Outcome::Ignorada,
        }
    }

    fn escape(&mut self) -> Escape {
        Escape::Nao
    }

    fn hint(&self) -> String {
        hint(&["←/→ ±500", "↑/↓ ±5.000", "m modo", "a alocação", "Esc sair"])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn alvos() -> BTreeMap<String, f64> {
        let mut a = BTreeMap::new();
        a.insert("acao".into(), 50.0);
        a.insert("renda_fixa".into(), 50.0);
        a
    }

    #[test]
    fn o_aporte_e_distribuido_por_inteiro() {
        let fatias = vec![
            ("ação".to_string(), 7_000.0, 70.0),
            ("renda fixa".to_string(), 3_000.0, 30.0),
        ];
        let plano = distribuir(&fatias, &alvos(), 10_000.0, 2_000.0);
        let soma: f64 = plano.iter().map(|(_, a, _)| a).sum();
        assert!((soma - 2_000.0).abs() < 0.01, "deu {soma}");
    }

    #[test]
    fn o_dinheiro_vai_para_quem_esta_mais_atras() {
        let fatias = vec![
            ("ação".to_string(), 7_000.0, 70.0),
            ("renda fixa".to_string(), 3_000.0, 30.0),
        ];
        let plano = distribuir(&fatias, &alvos(), 10_000.0, 2_000.0);
        let rf = plano.iter().find(|(r, _, _)| r == "renda fixa").unwrap();
        let acao = plano.iter().find(|(r, _, _)| r == "ação").unwrap();
        assert!(
            rf.1 > acao.1,
            "a renda fixa está atrás e tem que receber mais"
        );
        assert_eq!(acao.1, 0.0, "quem já passou do alvo não recebe");
    }

    #[test]
    fn aporte_zero_nao_muda_nada() {
        let fatias = vec![("ação".to_string(), 10_000.0, 100.0)];
        let plano = distribuir(&fatias, &alvos(), 10_000.0, 0.0);
        assert!(plano.iter().all(|(_, a, _)| *a == 0.0));
    }

    #[test]
    fn aporte_grande_leva_todos_ao_alvo() {
        let fatias = vec![
            ("ação".to_string(), 7_000.0, 70.0),
            ("renda fixa".to_string(), 3_000.0, 30.0),
        ];
        let plano = distribuir(&fatias, &alvos(), 10_000.0, 100_000.0);
        for (_, _, depois) in &plano {
            assert!((depois - 50.0).abs() < 1.0, "depois deu {depois}");
        }
    }
}
