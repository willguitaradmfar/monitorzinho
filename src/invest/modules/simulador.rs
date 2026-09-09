//! Simulador: projeção de aporte e backtest simples.
//!
//! Uma projeção é uma **conta, não uma previsão**. A conta está certa; a suposição de que
//! o retorno futuro é X% é do usuário, e é a parte que decide o resultado.
//!
//! Por isso a tela mostra **três cenários sempre**, nunca um só: um número sozinho, por
//! mais bem explicado que esteja, é lido como promessa; três lado a lado são lidos como
//! faixa, que é o que de fato são.

use crossterm::event::{KeyCode, KeyEvent};

use crate::invest::calc;
use crate::invest::carteira;
use crate::invest::model::{AssetId, Market};
use crate::invest::module::{
    Ctx, Escape, Group, InvestModule, Layout, ModuleView, Outcome, Pane, Tone,
};
use crate::invest::modules::comum::hint;

pub struct Simulador;

/// Os parâmetros da projeção.
#[derive(Clone, Copy)]
pub struct Plano {
    pub inicial: f64,
    pub aporte_mensal: f64,
    pub anos: u32,
    /// Retorno nominal anual, em porcento.
    pub retorno: f64,
    /// Inflação anual esperada, para mostrar o valor real ao lado do nominal.
    pub inflacao: f64,
}

/// `valor(n) = inicial × (1+r)ⁿ + aporte × [((1+r)ⁿ − 1) / r]`, com `r` mensal.
///
/// O caso `r = 0` é tratado à parte porque a fórmula divide por `r`: sem isso, retorno
/// zero daria `NaN` em vez do óbvio `inicial + aporte × meses`.
pub fn projetar(plano: &Plano, retorno_anual: f64) -> f64 {
    let meses = plano.anos * 12;
    let r = (1.0 + retorno_anual / 100.0).powf(1.0 / 12.0) - 1.0;
    if r.abs() < 1e-12 {
        return plano.inicial + plano.aporte_mensal * meses as f64;
    }
    let fator = (1.0 + r).powi(meses as i32);
    plano.inicial * fator + plano.aporte_mensal * ((fator - 1.0) / r)
}

/// O valor em poder de compra de hoje.
pub fn descontar(valor: f64, inflacao: f64, anos: u32) -> f64 {
    valor / (1.0 + inflacao / 100.0).powi(anos as i32)
}

impl InvestModule for Simulador {
    fn id(&self) -> &'static str {
        "simulador"
    }
    fn name(&self) -> &'static str {
        "Simulador"
    }
    fn description(&self) -> &'static str {
        "Projeção de aporte em três cenários — conta, não previsão"
    }
    fn destaque(&self) -> u8 {
        1
    }
    fn group(&self) -> Group {
        Group::Analise
    }
    fn keywords(&self) -> &'static str {
        "projeção juros compostos aposentadoria futuro cenário aporte"
    }

    fn summary(&self, _ctx: &Ctx) -> String {
        "projete um aporte mensal em três cenários".into()
    }

    fn widget(&self, ctx: &Ctx) -> Option<Pane> {
        if ctx.portfolio.posicoes.is_empty() {
            return None;
        }
        // Sem rede: o plano padrão sobre o patrimônio de hoje, e o pessimista ancorado no
        // CDI — o mesmo desenho da tela cheia, com os números que já estão em memória.
        let linhas = carteira::linhas(ctx.portfolio, ctx.market, ctx.agora);
        let plano = Plano {
            inicial: carteira::totais(&linhas).mercado,
            aporte_mensal: 2_000.0,
            anos: 10,
            retorno: 10.0,
            inflacao: 4.5,
        };
        let cdi = ctx
            .market
            .quote(&AssetId::new(Market::Bcb, "SELIC-META"))
            .map(|q| q.preco)
            .unwrap_or(10.0);
        Some(Pane::Facts {
            title: String::new(),
            rows: vec![
                (
                    format!(
                        "pessimista ({} a.a.)",
                        calc::pct_simples((cdi - 2.0).max(0.0))
                    ),
                    format!("R$ {}", calc::moeda(projetar(&plano, (cdi - 2.0).max(0.0)))),
                    Tone::Ruim,
                ),
                (
                    format!("base ({} a.a.)", calc::pct_simples(plano.retorno)),
                    format!("R$ {}", calc::moeda(projetar(&plano, plano.retorno))),
                    Tone::Destaque,
                ),
                (
                    format!("otimista ({} a.a.)", calc::pct_simples(plano.retorno + 4.0)),
                    format!("R$ {}", calc::moeda(projetar(&plano, plano.retorno + 4.0))),
                    Tone::Bom,
                ),
                (
                    format!("em {} anos, aportando", plano.anos),
                    format!("R$ {}/mês", calc::moeda(plano.aporte_mensal)),
                    Tone::Dim,
                ),
            ],
        })
    }

    fn open(&self, ctx: &Ctx, _alvo: Option<&AssetId>) -> Box<dyn ModuleView> {
        let linhas = carteira::linhas(ctx.portfolio, ctx.market, ctx.agora);
        let totais = carteira::totais(&linhas);
        Box::new(Vista {
            plano: Plano {
                inicial: totais.mercado,
                aporte_mensal: 2_000.0,
                anos: 10,
                retorno: 10.0,
                inflacao: 4.5,
            },
            campo: 0,
        })
    }
}

const CAMPOS: [&str; 4] = [
    "aporte mensal",
    "anos",
    "retorno nominal (a.a.)",
    "inflação (a.a.)",
];

struct Vista {
    plano: Plano,
    campo: usize,
}

impl ModuleView for Vista {
    fn title(&self) -> String {
        "Simulador".into()
    }

    fn quer_mercado(&self) -> bool {
        true
    }

    fn ativos(&self) -> Vec<AssetId> {
        vec![AssetId::new(Market::Bcb, "SELIC")]
    }

    fn layout(&self, ctx: &Ctx) -> Layout {
        let cdi = ctx
            .market
            .quote(&AssetId::new(Market::Bcb, "SELIC"))
            .map(|q| q.preco)
            .unwrap_or(10.0);

        // Três cenários, sempre. O pessimista ancorado no CDI menos dois pontos, para que
        // ele não seja um número escolhido por gosto.
        let cenarios = [
            ("pessimista", (cdi - 2.0).max(0.0), Tone::Ruim),
            (
                "base (o que você informou)",
                self.plano.retorno,
                Tone::Destaque,
            ),
            ("otimista", self.plano.retorno + 4.0, Tone::Bom),
        ];

        let mut fatos: Vec<(String, String, Tone)> = cenarios
            .iter()
            .map(|(nome, taxa, tom)| {
                let nominal = projetar(&self.plano, *taxa);
                let real = descontar(nominal, self.plano.inflacao, self.plano.anos);
                (
                    format!("{nome} · {}", calc::pct_simples(*taxa)),
                    format!(
                        "R$ {}  ·  em poder de compra de hoje R$ {}",
                        calc::moeda(nominal),
                        calc::moeda(real)
                    ),
                    *tom,
                )
            })
            .collect();

        // A linha do aportado é obrigatória: ela separa o que veio de dinheiro colocado
        // do que veio de rendimento — a mesma decomposição que o Patrimônio faz com o
        // passado.
        let aportado = self.plano.aporte_mensal * (self.plano.anos * 12) as f64;
        fatos.push((
            "você terá aportado".into(),
            format!("R$ {}", calc::moeda(aportado)),
            Tone::Normal,
        ));
        fatos.push((
            "partindo de".into(),
            format!("R$ {} (sua carteira hoje)", calc::moeda(self.plano.inicial)),
            Tone::Dim,
        ));
        fatos.push((
            "isto é uma conta, não uma previsão".into(),
            "o retorno futuro é sua suposição — a faixa existe por isso".into(),
            Tone::Aviso,
        ));

        let campos: Vec<(String, String, Tone)> = CAMPOS
            .iter()
            .enumerate()
            .map(|(i, nome)| {
                let valor = match i {
                    0 => format!("R$ {}", calc::moeda(self.plano.aporte_mensal)),
                    1 => format!("{} anos", self.plano.anos),
                    2 => calc::pct_simples(self.plano.retorno),
                    _ => calc::pct_simples(self.plano.inflacao),
                };
                (
                    format!("{} {nome}", if i == self.campo { "▸" } else { " " }),
                    valor,
                    match i == self.campo {
                        true => Tone::Destaque,
                        false => Tone::Dim,
                    },
                )
            })
            .collect();

        // A curva do cenário base, ano a ano.
        let curva: Vec<f64> = (0..=self.plano.anos)
            .map(|ano| {
                projetar(
                    &Plano {
                        anos: ano,
                        ..self.plano
                    },
                    self.plano.retorno,
                )
            })
            .collect();

        Layout::rows(vec![
            (
                2,
                Layout::one(Pane::Chart {
                    title: format!("Cenário base · {} anos", self.plano.anos),
                    series: curva,
                    format: |v| format!("R$ {}", calc::moeda(v)),
                    marks: vec![(self.plano.inicial, "hoje".into())],
                    // O eixo aqui não é calendário: é o ano da projeção.
                    x_labels: (0..=self.plano.anos).map(|a| format!("ano {a}")).collect(),
                    note: None,
                }),
            ),
            (
                2,
                Layout::cols(vec![
                    (
                        1,
                        Layout::one(Pane::Facts {
                            title: "Parâmetros".into(),
                            rows: campos,
                        }),
                    ),
                    (
                        2,
                        Layout::one(Pane::Facts {
                            title: "Cenários".into(),
                            rows: fatos,
                        }),
                    ),
                ]),
            ),
        ])
    }

    fn key(&mut self, key: KeyEvent, _ctx: &Ctx) -> Outcome {
        let passo = |campo: usize| -> f64 {
            match campo {
                0 => 100.0,
                1 => 1.0,
                _ => 0.5,
            }
        };
        match key.code {
            KeyCode::Up => {
                self.campo = (self.campo + CAMPOS.len() - 1) % CAMPOS.len();
                Outcome::Ok
            }
            KeyCode::Down => {
                self.campo = (self.campo + 1) % CAMPOS.len();
                Outcome::Ok
            }
            KeyCode::Right | KeyCode::Left => {
                let sinal = if key.code == KeyCode::Right {
                    1.0
                } else {
                    -1.0
                };
                let d = passo(self.campo) * sinal;
                match self.campo {
                    0 => self.plano.aporte_mensal = (self.plano.aporte_mensal + d).max(0.0),
                    1 => self.plano.anos = (self.plano.anos as f64 + d).clamp(1.0, 60.0) as u32,
                    2 => self.plano.retorno = (self.plano.retorno + d).max(0.0),
                    _ => self.plano.inflacao = (self.plano.inflacao + d).max(0.0),
                }
                Outcome::Ok
            }
            _ => Outcome::Ignorada,
        }
    }

    fn escape(&mut self) -> Escape {
        Escape::Nao
    }

    fn hint(&self) -> String {
        hint(&["↑/↓ campo", "←/→ ajustar", "Esc sair"])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plano(retorno: f64) -> Plano {
        Plano {
            inicial: 0.0,
            aporte_mensal: 1_000.0,
            anos: 10,
            retorno,
            inflacao: 0.0,
        }
    }

    #[test]
    fn retorno_zero_e_exatamente_o_que_se_aportou() {
        // Sem o caso especial, a fórmula dividiria por zero e daria NaN.
        let v = projetar(&plano(0.0), 0.0);
        assert!((v - 120_000.0).abs() < 1e-6, "deu {v}");
    }

    #[test]
    fn juros_compostos_batem_com_a_conta_conhecida() {
        // R$ 1.000/mês por 10 anos a 10% a.a. compostos mensalmente.
        let v = projetar(&plano(10.0), 10.0);
        assert!(v > 120_000.0, "tem que passar do aportado");
        assert!(v > 195_000.0 && v < 210_000.0, "deu {v}");
    }

    #[test]
    fn o_desconto_pela_inflacao_reduz_o_poder_de_compra() {
        let nominal = 200_000.0;
        let real = descontar(nominal, 4.5, 10);
        assert!(real < nominal);
        assert!((real - 128_760.0).abs() < 1_000.0, "deu {real}");
        // Inflação zero não muda nada.
        assert_eq!(descontar(nominal, 0.0, 10), nominal);
    }

    #[test]
    fn valor_inicial_entra_capitalizado() {
        let mut p = plano(10.0);
        p.aporte_mensal = 0.0;
        p.inicial = 100_000.0;
        let v = projetar(&p, 10.0);
        assert!((v - 259_374.0).abs() < 500.0, "deu {v}");
    }
}
