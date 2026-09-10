//! Risco: o quanto essa carteira balança, o quanto já caiu, e se o retorno pagou por isso.
//!
//! Este é o módulo com maior chance de produzir um número bonito e errado. Três regras
//! valem em toda linha dele:
//!
//! 1. **Toda medida mostra o tamanho da amostra.** «Sharpe 1,42» não quer dizer nada;
//!    «Sharpe 1,42 · 68 dias» já avisa que é pouco.
//! 2. **Abaixo de um mínimo, a medida não aparece** — aparece quantos dias faltam, que é
//!    informação verdadeira. Sharpe com 20 dias é ruído com duas casas decimais.
//! 3. **Nada é interpretado pelo programa.** Sem «risco alto», sem semáforo. Classificar é
//!    o trabalho de quem investe.

use crossterm::event::{KeyCode, KeyEvent};

use crate::invest::calc;
use crate::invest::carteira;
use crate::invest::historico::{self, Estado, fechamentos};
use crate::invest::model::{AssetId, Market};
use crate::invest::module::{
    Ctx, Escape, Group, InvestModule, Layout, Marcavel, ModuleView, Need, Outcome, Pane, Row, Tone,
};
use crate::invest::modules::comum::{Lista, hint};
use crate::invest::provider::Span;

pub struct Risco;

/// A amostra mínima para cada família de medida. Abaixo disso o número existe mas não
/// significa nada, e mostrá-lo é pior que não mostrar.
const MINIMO_VOL: usize = 30;
const MINIMO_SHARPE: usize = 60;
const MINIMO_VAR: usize = 60;

impl InvestModule for Risco {
    fn id(&self) -> &'static str {
        "risco"
    }
    fn name(&self) -> &'static str {
        "Risco"
    }
    fn description(&self) -> &'static str {
        "Volatilidade, drawdown, Sharpe e VaR — com o tamanho da amostra ao lado"
    }
    fn destaque(&self) -> u8 {
        1
    }
    fn group(&self) -> Group {
        Group::Analise
    }

    fn marcavel(&self) -> Option<Marcavel> {
        Some(crate::invest::marcas::ATIVOS)
    }
    fn needs(&self) -> &'static [Need] {
        &[Need::Posicoes, Need::Historico]
    }
    fn keywords(&self) -> &'static str {
        "volatilidade drawdown sharpe var beta perda queda"
    }

    fn summary(&self, ctx: &Ctx) -> String {
        match ctx.portfolio.posicoes.is_empty() {
            true => "sem posições".into(),
            false => "abra para calcular sobre as séries disponíveis".into(),
        }
    }

    fn widget(&self, ctx: &Ctx) -> Option<Pane> {
        use crate::invest::historico::{Estado, fechamentos};
        let ativos = com_serie(ctx);
        if ativos.is_empty() {
            return None;
        }
        // Os mais agitados primeiro: é o que a pergunta «qual é o meu risco» quer saber de
        // relance. `get` dispara a busca em segundo plano; o cartão desenha o que já há.
        let mut linhas: Vec<(String, f64)> = ativos
            .iter()
            .filter_map(|a| {
                match ctx.historico.get(ctx.providers, a, Span::Ano) {
                    Estado::Pronta(velas) => {
                        let serie = fechamentos(&velas);
                        let r = calc::retornos(&serie);
                        // Uma volatilidade sobre meia dúzia de retornos é ruído com cara
                        // de número; o módulo aberto diz de quantos ela precisa.
                        (r.len() >= 20)
                            .then(|| (a.short().to_string(), calc::volatilidade_anual(&r)))
                    }
                    _ => None,
                }
            })
            .collect();
        if linhas.is_empty() {
            return None;
        }
        linhas.sort_by(|a, b| b.1.total_cmp(&a.1));
        Some(Pane::Facts {
            title: String::new(),
            rows: linhas
                .into_iter()
                .take(6)
                .map(|(nome, vol)| {
                    (
                        nome,
                        format!("vol. {} a.a.", calc::pct_simples(vol)),
                        match vol > 40.0 {
                            true => Tone::Aviso,
                            false => Tone::Normal,
                        },
                    )
                })
                .collect(),
        })
    }

    fn open(&self, _ctx: &Ctx, _alvo: Option<&AssetId>) -> Box<dyn ModuleView> {
        Box::new(Vista {
            span: 4,
            por_ativo: false,
            lista: Lista::default(),
        })
    }
}

struct Vista {
    span: usize,
    por_ativo: bool,
    lista: Lista,
}

/// Os ativos da carteira que têm série. Os que não têm ficam de fora **e a tela diz
/// quanto do patrimônio ficou de fora** — um risco calculado sobre 60% da carteira e
/// apresentado como o risco da carteira seria a falha mais grave deste módulo.
pub fn com_serie(ctx: &Ctx) -> Vec<AssetId> {
    let mut v = Vec::new();
    for p in &ctx.portfolio.posicoes {
        if ctx.providers.tem_historico(&p.ativo) && !v.contains(&p.ativo) {
            v.push(p.ativo.clone());
        }
    }
    v
}

impl Vista {
    fn span(&self) -> Span {
        Span::ALL[self.span.min(Span::ALL.len() - 1)]
    }

    /// A série da carteira: cada ativo pelo peso que ele tem hoje.
    ///
    /// É uma aproximação e está dito na tela: usar o peso de hoje sobre o passado supõe
    /// que a carteira sempre foi assim. Reconstruir os pesos históricos exigiria os
    /// lançamentos completos — o mesmo caminho que o Patrimônio deixou para depois.
    fn serie_carteira(&self, ctx: &Ctx) -> (Vec<f64>, Vec<String>, f64, f64) {
        let linhas = carteira::linhas(ctx.portfolio, ctx.market, ctx.agora);
        let totais = carteira::totais(&linhas);
        let mut series: Vec<(f64, Vec<f64>)> = Vec::new();
        // As datas do eixo saem de **uma** das séries — a mais longa serve, porque a
        // combinada é alinhada pela mais curta e cortada pela direita, que é o fim comum.
        let mut datas: Vec<crate::invest::provider::Candle> = Vec::new();
        let mut coberto = 0.0;

        for ativo in com_serie(ctx) {
            let Estado::Pronta(candles) = ctx.historico.get(ctx.providers, &ativo, self.span())
            else {
                continue;
            };
            let peso: f64 = linhas
                .iter()
                .filter(|l| l.posicao.ativo == ativo)
                .filter_map(|l| l.mercado_brl)
                .sum();
            if peso <= 0.0 {
                continue;
            }
            coberto += peso;
            if candles.len() > datas.len() {
                datas = candles.clone();
            }
            series.push((peso, fechamentos(&candles)));
        }

        if series.is_empty() || coberto <= 0.0 {
            return (Vec::new(), Vec::new(), 0.0, totais.mercado);
        }
        // Alinha pelo mais curto: comparar janelas diferentes é comparar coisas diferentes.
        let n = series.iter().map(|(_, s)| s.len()).min().unwrap_or(0);
        let combinada: Vec<f64> = (0..n)
            .map(|i| {
                series
                    .iter()
                    .map(|(peso, s)| {
                        let base = s[s.len() - n];
                        match base > 0.0 {
                            true => peso / coberto * (s[s.len() - n + i] / base) * 100.0,
                            false => 0.0,
                        }
                    })
                    .sum()
            })
            .collect();
        let rotulos = historico::rotulos(&datas[datas.len().saturating_sub(n)..], self.span());
        (combinada, rotulos, coberto, totais.mercado)
    }

    fn cdi_anual(&self, ctx: &Ctx) -> f64 {
        ctx.market
            .quote(&AssetId::new(Market::Bcb, "SELIC"))
            .map(|q| q.preco)
            // Sem o CDI na mesa, o «sem risco» vira zero — e a tela diz que virou, porque
            // comparar contra zero dá um Sharpe que elogia qualquer coisa.
            .unwrap_or(0.0)
    }
}

/// As medidas de uma série, cada uma com o mínimo de amostra que ela exige.
fn medidas(serie: &[f64], cdi: f64) -> Vec<(String, String, Tone)> {
    let retornos = calc::retornos(serie);
    let n = retornos.len();
    let falta = |minimo: usize| format!("faltam {} dias", minimo.saturating_sub(n));

    let mut v = Vec::new();
    v.push((
        "amostra".to_string(),
        format!("{n} retornos"),
        match n >= MINIMO_SHARPE {
            true => Tone::Dim,
            false => Tone::Aviso,
        },
    ));
    v.push((
        "volatilidade (a.a.)".to_string(),
        match n >= MINIMO_VOL {
            true => calc::pct_simples(calc::volatilidade_anual(&retornos)),
            false => falta(MINIMO_VOL),
        },
        Tone::Normal,
    ));

    let (pior, _, _) = calc::drawdown_maximo(serie);
    v.push(("drawdown máximo".to_string(), calc::pct(pior), Tone::Ruim));
    v.push((
        "drawdown atual".to_string(),
        calc::pct(calc::drawdown_atual(serie)),
        Tone::Normal,
    ));
    v.push((
        format!("Sharpe (sem risco: {})", calc::pct_simples(cdi)),
        match n >= MINIMO_SHARPE {
            true => calc::sharpe(&retornos, cdi)
                .map(|s| format!("{s:.2}").replace('.', ","))
                .unwrap_or("—".into()),
            false => falta(MINIMO_SHARPE),
        },
        Tone::Normal,
    ));
    v.push((
        "VaR 95% (1 dia)".to_string(),
        match n >= MINIMO_VAR {
            true => calc::var_historico(&retornos, 5.0)
                .map(calc::pct)
                .unwrap_or("—".into()),
            false => falta(MINIMO_VAR),
        },
        Tone::Ruim,
    ));
    v
}

impl ModuleView for Vista {
    fn title(&self) -> String {
        format!("Risco · {}", self.span().label())
    }

    fn quer_mercado(&self) -> bool {
        true
    }

    fn ativos(&self) -> Vec<AssetId> {
        vec![AssetId::new(Market::Bcb, "SELIC")]
    }

    fn layout(&self, ctx: &Ctx) -> Layout {
        if ctx.portfolio.posicoes.is_empty() {
            return Layout::one(Pane::Empty {
                title: "Risco".into(),
                note: "Sem posições não há risco a medir. Comece por Posições.".into(),
            });
        }

        let (serie, rotulos, coberto, total) = self.serie_carteira(ctx);
        let fora = (total - coberto).max(0.0);
        let pct_fora = match total > 0.0 {
            true => fora / total * 100.0,
            false => 0.0,
        };

        if serie.len() < 3 {
            let pendentes = ctx.historico.pendentes();
            return Layout::one(Pane::Empty {
                title: "Risco".into(),
                note: match pendentes > 0 {
                    true => format!("buscando {pendentes} série(s)…"),
                    false => format!(
                        "Nenhum ativo da carteira tem série histórica.\n\n\
                         Têm: pares da Binance, câmbio e séries do Banco Central. Ação da \n\
                         B3 e dos EUA usam preço informado nesta versão, e um preço \n\
                         informado não tem série sobre a qual medir risco.\n\n\
                         {} do patrimônio ficaria de fora do cálculo.",
                        calc::pct_simples(pct_fora)
                    ),
                },
            });
        }

        let cdi = self.cdi_anual(ctx);
        let mut fatos = medidas(&serie, cdi);
        // A ressalva mais importante do módulo, e ela nunca sai.
        if fora > 0.01 {
            fatos.push((
                "atenção".into(),
                format!(
                    "{} do patrimônio ficou de fora — os ativos sem série não entram",
                    calc::pct_simples(pct_fora)
                ),
                Tone::Aviso,
            ));
        }
        if cdi <= 0.0 {
            fatos.push((
                "sem CDI".into(),
                "o «sem risco» virou zero — abra Renda fixa para buscá-lo".into(),
                Tone::Aviso,
            ));
        }
        fatos.push((
            "como a série foi montada".into(),
            "pelo peso de hoje aplicado ao passado — supõe que a carteira sempre foi assim".into(),
            Tone::Dim,
        ));

        if self.por_ativo {
            let rows: Vec<Row> = com_serie(ctx)
                .into_iter()
                .filter_map(|a| {
                    let Estado::Pronta(c) = ctx.historico.peek(&a, self.span())? else {
                        return None;
                    };
                    let s = fechamentos(&c);
                    let r = calc::retornos(&s);
                    let (dd, _, _) = calc::drawdown_maximo(&s);
                    Some(Row::new(vec![
                        a.short().to_string(),
                        r.len().to_string(),
                        match r.len() >= MINIMO_VOL {
                            true => calc::pct_simples(calc::volatilidade_anual(&r)),
                            false => "—".into(),
                        },
                        calc::pct(dd),
                        calc::pct(calc::drawdown_atual(&s)),
                    ]))
                })
                .collect();
            return Layout::one(Pane::Table {
                title: format!("Risco por ativo · {}", self.span().label()),
                headers: vec![
                    "Ativo".into(),
                    "Amostra".into(),
                    "Volatilidade".into(),
                    "Drawdown máx".into(),
                    "Drawdown atual".into(),
                ],
                rows,
                selected: Some(self.lista.selecionado),
                query: String::new(),
                note: Some((
                    "cada ativo sobre a própria série — nada é somado aqui".into(),
                    Tone::Aviso,
                )),
            });
        }

        Layout::rows(vec![
            (
                2,
                Layout::one(Pane::Chart {
                    title: format!("Carteira · {} (base 100)", self.span().label()),
                    series: serie,
                    format: |v| format!("{v:.1}").replace('.', ","),
                    marks: vec![(100.0, "base".into())],
                    x_labels: rotulos,
                    note: None,
                }),
            ),
            (
                3,
                Layout::one(Pane::Facts {
                    title: "Medidas".into(),
                    rows: fatos,
                }),
            ),
        ])
    }

    fn key(&mut self, key: KeyEvent, _ctx: &Ctx) -> Outcome {
        match key.code {
            KeyCode::Left => {
                self.span = (self.span + Span::ALL.len() - 1) % Span::ALL.len();
                Outcome::Ok
            }
            KeyCode::Right => {
                self.span = (self.span + 1) % Span::ALL.len();
                Outcome::Ok
            }
            KeyCode::Char('a') => {
                self.por_ativo = !self.por_ativo;
                Outcome::Ok
            }
            KeyCode::Char('c') => Outcome::Abrir {
                modulo: "correlacao",
                alvo: None,
            },
            _ => match self.lista.tecla(key, 10) {
                true => Outcome::Ok,
                false => Outcome::Ignorada,
            },
        }
    }

    fn escape(&mut self) -> Escape {
        Escape::Nao
    }

    fn hint(&self) -> String {
        hint(&["←/→ período", "a por ativo", "c correlação", "Esc sair"])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn medida_abaixo_do_minimo_diz_quantos_dias_faltam() {
        let serie: Vec<f64> = (0..15).map(|i| 100.0 + i as f64).collect();
        let m = medidas(&serie, 10.0);
        let sharpe = m.iter().find(|(r, _, _)| r.starts_with("Sharpe")).unwrap();
        assert!(sharpe.1.contains("faltam"), "deu «{}»", sharpe.1);
        // Ruído com duas casas decimais seria pior que a verdade.
        assert!(!sharpe.1.contains(','));
    }

    #[test]
    fn com_amostra_suficiente_a_medida_aparece() {
        let serie: Vec<f64> = (0..200)
            .map(|i| 100.0 + (i as f64 * 0.3).sin() * 5.0 + i as f64 * 0.05)
            .collect();
        let m = medidas(&serie, 10.0);
        let vol = m
            .iter()
            .find(|(r, _, _)| r.starts_with("volatilidade"))
            .unwrap();
        assert!(vol.1.ends_with('%'), "deu «{}»", vol.1);
    }

    #[test]
    fn a_amostra_e_sempre_dita() {
        let serie: Vec<f64> = (0..100).map(|i| 100.0 + i as f64).collect();
        let m = medidas(&serie, 10.0);
        assert_eq!(m[0].0, "amostra");
        assert!(m[0].1.contains("99 retornos"));
    }
}
