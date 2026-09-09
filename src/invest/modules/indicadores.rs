//! Indicadores: o que a série de preço diz além do preço.
//!
//! Não é uma tela sozinha — um indicador sem o preço ao lado não significa nada. Ele abre
//! **sobre** o gráfico, acrescentando linhas na área do preço e painéis embaixo.
//!
//! Três armadilhas conhecidas, todas com teste em `calc`: a EMA semeada com a SMA do
//! primeiro período (e não com o primeiro preço), o RSI com a suavização de Wilder (e não
//! média simples), e um período maior que a série que **não desenha nada** em vez de
//! desenhar uma linha errada nos primeiros pontos.

use crossterm::event::{KeyCode, KeyEvent};

use crate::invest::calc;
use crate::invest::historico::{Estado, fechamentos};
use crate::invest::model::{AssetId, Market};
use crate::invest::module::{
    Ctx, Escape, Group, InvestModule, Layout, ModuleView, Need, Outcome, Pane, Tone,
};
use crate::invest::modules::comum::hint;
use crate::invest::provider::Span;

pub struct Indicadores;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Qual {
    Mm20,
    Mm50,
    Ema9,
    Bollinger,
    Rsi,
    Macd,
}

impl Qual {
    const ALL: [Qual; 6] = [
        Qual::Mm20,
        Qual::Mm50,
        Qual::Ema9,
        Qual::Bollinger,
        Qual::Rsi,
        Qual::Macd,
    ];

    fn label(&self) -> &'static str {
        match self {
            Qual::Mm20 => "média móvel 20",
            Qual::Mm50 => "média móvel 50",
            Qual::Ema9 => "EMA 9",
            Qual::Bollinger => "Bollinger 20 · 2σ",
            Qual::Rsi => "RSI 14",
            Qual::Macd => "MACD 12/26/9",
        }
    }

    fn periodo(&self) -> usize {
        match self {
            Qual::Mm20 | Qual::Bollinger => 20,
            Qual::Mm50 => 50,
            Qual::Ema9 => 9,
            Qual::Rsi => 14,
            Qual::Macd => 26,
        }
    }
}

impl InvestModule for Indicadores {
    fn id(&self) -> &'static str {
        "indicadores"
    }
    fn name(&self) -> &'static str {
        "Indicadores"
    }
    fn description(&self) -> &'static str {
        "Médias, Bollinger, RSI e MACD sobre a série de um ativo"
    }
    fn destaque(&self) -> u8 {
        1
    }
    fn group(&self) -> Group {
        Group::Analise
    }
    fn needs(&self) -> &'static [Need] {
        &[Need::Historico]
    }
    fn keywords(&self) -> &'static str {
        "rsi macd média móvel bollinger análise técnica sobrecomprado"
    }

    fn summary(&self, _ctx: &Ctx) -> String {
        "abra sobre um ativo com série".into()
    }

    fn widget(&self, ctx: &Ctx) -> Option<Pane> {
        use crate::invest::historico::{Estado, fechamentos};
        let ativos = crate::invest::modules::risco::com_serie(ctx);
        if ativos.is_empty() {
            return None;
        }
        // O RSI de cada papel, e os extremos primeiro: acima de 70 e abaixo de 30 é onde
        // este módulo tem algo a dizer.
        let mut linhas: Vec<(String, f64)> = ativos
            .iter()
            .filter_map(|a| match ctx.historico.get(ctx.providers, a, Span::Ano) {
                // O RSI é `None` nos primeiros períodos, por definição — ele precisa de
                // catorze retornos antes de existir. O último que **existe** é o de hoje.
                Estado::Pronta(velas) => calc::rsi(&fechamentos(&velas), 14)
                    .iter()
                    .rev()
                    .find_map(|r| *r)
                    .map(|r| (a.short().to_string(), r)),
                _ => None,
            })
            .collect();
        if linhas.is_empty() {
            return None;
        }
        linhas.sort_by(|a, b| (b.1 - 50.0).abs().total_cmp(&(a.1 - 50.0).abs()));
        Some(Pane::Facts {
            title: String::new(),
            rows: linhas
                .into_iter()
                .take(6)
                .map(|(nome, rsi)| {
                    (
                        nome,
                        format!("RSI {:.0}", rsi),
                        match rsi {
                            r if r >= 70.0 => Tone::Ruim,
                            r if r <= 30.0 => Tone::Bom,
                            _ => Tone::Normal,
                        },
                    )
                })
                .collect(),
        })
    }

    fn open(&self, ctx: &Ctx, alvo: Option<&AssetId>) -> Box<dyn ModuleView> {
        // Quem chegou aqui pelo «i» do Gráfico está olhando um ativo específico; abrir
        // noutro seria trocar o assunto no meio.
        let ativo = alvo.cloned().unwrap_or_else(|| {
            ctx.portfolio
                .posicoes
                .iter()
                .map(|p| p.ativo.clone())
                .chain(ctx.portfolio.watchlist.iter().cloned())
                .find(|a| ctx.providers.tem_historico(a))
                .unwrap_or_else(|| AssetId::new(Market::Fx, "USDBRL"))
        });
        Box::new(Vista {
            ativo,
            span: 4,
            ligados: vec![Qual::Mm20, Qual::Rsi],
            selecionado: 0,
        })
    }
}

struct Vista {
    ativo: AssetId,
    span: usize,
    ligados: Vec<Qual>,
    selecionado: usize,
}

impl Vista {
    fn span(&self) -> Span {
        Span::ALL[self.span.min(Span::ALL.len() - 1)]
    }
}

impl ModuleView for Vista {
    fn title(&self) -> String {
        format!("Indicadores · {}", self.ativo.short())
    }

    fn quer_mercado(&self) -> bool {
        true
    }

    fn ativos(&self) -> Vec<AssetId> {
        vec![self.ativo.clone()]
    }

    fn layout(&self, ctx: &Ctx) -> Layout {
        let estado = ctx.historico.get(ctx.providers, &self.ativo, self.span());
        let Estado::Pronta(candles) = &estado else {
            return Layout::one(Pane::Empty {
                title: format!("Indicadores · {}", self.ativo.short()),
                note: crate::invest::historico::explicar(&estado, &self.ativo).unwrap_or_default(),
            });
        };
        let serie = fechamentos(candles);
        let rotulos = crate::invest::historico::rotulos(candles, self.span());

        let mut fatos: Vec<(String, String, Tone)> = Vec::new();
        let mut painel_inferior: Option<Pane> = None;
        let mut marks: Vec<(f64, String)> = Vec::new();

        for (i, q) in Qual::ALL.iter().enumerate() {
            let ligado = self.ligados.contains(q);
            let marca = match (ligado, i == self.selecionado) {
                (true, true) => "▸ ●",
                (true, false) => "  ●",
                (false, true) => "▸ ○",
                (false, false) => "  ○",
            };
            // Período maior que a série não é erro: é um indicador que ainda não tem
            // valor. Ele não desenha nada, e diz quantos períodos faltam.
            let valor = match (ligado, serie.len() > q.periodo()) {
                (false, _) => "desligado".to_string(),
                (true, false) => format!("faltam {} períodos", q.periodo() + 1 - serie.len()),
                (true, true) => match q {
                    Qual::Mm20 | Qual::Mm50 => calc::sma(&serie, q.periodo())
                        .last()
                        .and_then(|v| *v)
                        .map(calc::preco)
                        .unwrap_or("—".into()),
                    Qual::Ema9 => calc::ema(&serie, q.periodo())
                        .last()
                        .and_then(|v| *v)
                        .map(calc::preco)
                        .unwrap_or("—".into()),
                    Qual::Bollinger => {
                        let janela = &serie[serie.len() - q.periodo()..];
                        let m = calc::media(janela);
                        let d = calc::desvio(janela);
                        format!(
                            "{} … {}",
                            calc::preco(m - 2.0 * d),
                            calc::preco(m + 2.0 * d)
                        )
                    }
                    Qual::Rsi => calc::rsi(&serie, q.periodo())
                        .last()
                        .and_then(|v| *v)
                        .map(|v| format!("{v:.1}").replace('.', ","))
                        .unwrap_or("—".into()),
                    Qual::Macd => calc::macd(&serie, 12, 26, 9)
                        .last()
                        .and_then(|v| *v)
                        .map(|(l, s, h)| {
                            format!(
                                "linha {} · sinal {} · hist {}",
                                calc::preco(l),
                                calc::preco(s),
                                calc::preco(h)
                            )
                        })
                        .unwrap_or("—".into()),
                },
            };
            fatos.push((
                format!("{marca} {}", q.label()),
                valor,
                match ligado {
                    true => Tone::Normal,
                    false => Tone::Dim,
                },
            ));

            if ligado && serie.len() > q.periodo() {
                match q {
                    Qual::Mm20 | Qual::Mm50 => {
                        if let Some(Some(v)) = calc::sma(&serie, q.periodo()).last() {
                            marks.push((*v, q.label().to_string()));
                        }
                    }
                    Qual::Rsi if painel_inferior.is_none() => {
                        let rsi: Vec<f64> = calc::rsi(&serie, q.periodo())
                            .into_iter()
                            .flatten()
                            .collect();
                        painel_inferior = Some(Pane::Chart {
                            title: "RSI 14".into(),
                            series: rsi,
                            format: |v| format!("{v:.1}").replace('.', ","),
                            // As linhas de referência são desenhadas: um RSI sem elas é um
                            // número entre 0 e 100 sem escala de leitura.
                            marks: vec![
                                (30.0, "sobrevendido".into()),
                                (70.0, "sobrecomprado".into()),
                            ],
                            x_labels: rotulos.clone(),
                            note: None,
                        });
                    }
                    _ => {}
                }
            }
        }

        let grafico = Pane::Chart {
            title: format!("{} · {}", self.ativo.short(), self.span().label()),
            series: serie,
            format: calc::preco,
            marks,
            x_labels: rotulos,
            note: None,
        };

        match painel_inferior {
            Some(inferior) => Layout::rows(vec![
                (3, Layout::one(grafico)),
                (2, Layout::one(inferior)),
                (
                    2,
                    Layout::one(Pane::Facts {
                        title: "Indicadores".into(),
                        rows: fatos,
                    }),
                ),
            ]),
            None => Layout::rows(vec![
                (3, Layout::one(grafico)),
                (
                    2,
                    Layout::one(Pane::Facts {
                        title: "Indicadores".into(),
                        rows: fatos,
                    }),
                ),
            ]),
        }
    }

    fn key(&mut self, key: KeyEvent, _ctx: &Ctx) -> Outcome {
        match key.code {
            KeyCode::Up => {
                self.selecionado = (self.selecionado + Qual::ALL.len() - 1) % Qual::ALL.len();
                Outcome::Ok
            }
            KeyCode::Down => {
                self.selecionado = (self.selecionado + 1) % Qual::ALL.len();
                Outcome::Ok
            }
            KeyCode::Char(' ') | KeyCode::Enter => {
                let q = Qual::ALL[self.selecionado];
                match self.ligados.iter().position(|x| *x == q) {
                    Some(i) => {
                        self.ligados.remove(i);
                    }
                    None => self.ligados.push(q),
                }
                Outcome::Ok
            }
            KeyCode::Left => {
                self.span = (self.span + Span::ALL.len() - 1) % Span::ALL.len();
                Outcome::Ok
            }
            KeyCode::Right => {
                self.span = (self.span + 1) % Span::ALL.len();
                Outcome::Ok
            }
            _ => Outcome::Ignorada,
        }
    }

    fn escape(&mut self) -> Escape {
        Escape::Nao
    }

    fn hint(&self) -> String {
        hint(&[
            "↑/↓ escolher",
            "espaço ligar/desligar",
            "←/→ período",
            "Esc sair",
        ])
    }
}
