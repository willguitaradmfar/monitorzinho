//! Gráfico: como este preço chegou até aqui.
//!
//! **Linha de fechamento, não candle.** Um candle precisa de quatro valores por período e
//! de meia dúzia de células de largura para ser legível; numa tela de 120 colunas cabem
//! uns 110, e o desenho com blocos Unicode fica ambíguo em fonte estreita. A linha é o
//! que cabe bem e o que a base já sabe desenhar. O candle entra quando houver um desenho
//! testado em terminal estreito — está fora por medida, não por preguiça.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::invest::calc;
use crate::invest::historico::{self, Estado};
use crate::invest::model::AssetId;
use crate::invest::module::{
    Ctx, Escape, Field, Group, InvestModule, Layout, Marcavel, ModuleView, Need, Outcome, Pane,
    Tone,
};
use crate::invest::modules::comum::{Formulario, hint};
use crate::invest::provider::Span;

pub struct Grafico;

impl InvestModule for Grafico {
    fn id(&self) -> &'static str {
        "grafico"
    }
    fn name(&self) -> &'static str {
        "Gráfico"
    }
    fn description(&self) -> &'static str {
        "Preço no tempo, com o seu preço médio desenhado por cima"
    }
    fn destaque(&self) -> u8 {
        3
    }
    fn group(&self) -> Group {
        Group::Mercado
    }

    fn fonte_externa(&self) -> Option<&'static str> {
        Some(crate::invest::store::fonte::HISTORICO)
    }

    /// Divide a lista de marcas dos ativos: o cartão lista os papéis com série; a tela é o gráfico de um deles.
    fn marcavel(&self) -> Option<Marcavel> {
        Some(crate::invest::marcas::ATIVOS)
    }
    fn needs(&self) -> &'static [Need] {
        &[Need::Historico]
    }
    fn keywords(&self) -> &'static str {
        "série candle timeframe cotação histórico linha"
    }

    fn summary(&self, ctx: &Ctx) -> String {
        match primeiro_com_serie(ctx) {
            Some(a) => format!("{} e outros com série", a.short()),
            None => "nenhum ativo com histórico — preço informado não tem série".into(),
        }
    }

    fn widget(&self, ctx: &Ctx) -> Option<Pane> {
        let comseries = com_serie(ctx);
        if comseries.is_empty() {
            return None;
        }
        // A série vem de rede, e `widget` não busca. O que se mostra é quem tem série
        // para mostrar — e o preço de agora, que já está no retrato.
        Some(Pane::Facts {
            title: String::new(),
            rows: comseries
                .iter()
                .take(8)
                .map(|a| {
                    let q = ctx.market.quote(a);
                    (
                        a.short().to_string(),
                        q.map(|q| calc::preco(q.preco)).unwrap_or_default(),
                        crate::invest::modules::heatmap::tom(q.and_then(|q| q.variacao())),
                    )
                })
                .collect(),
        })
    }

    fn open(&self, ctx: &Ctx, alvo: Option<&AssetId>) -> Box<dyn ModuleView> {
        Box::new(Vista {
            // O alvo manda. Ele vem de quem pediu — «Enter no PETR4» em Cotações — e só
            // na falta dele é que se escolhe um por conta própria.
            ativo: alvo.cloned().or_else(|| primeiro_com_serie(ctx)),
            span: 3,
            cursor: None,
            form: None,
        })
    }
}

/// Os ativos com série: **perguntado ao provedor**, e não a uma lista de mercados escrita
/// aqui. Assim, ligar uma fonte nova faz os ativos dela aparecerem em todo módulo que
/// desenha no tempo, sem que ninguém precise lembrar de atualizar cinco listas.
fn com_serie(ctx: &Ctx) -> Vec<AssetId> {
    use crate::invest::model::Market;
    let mut v: Vec<AssetId> = Vec::new();
    for a in ctx
        .portfolio
        .posicoes
        .iter()
        .map(|p| &p.ativo)
        .chain(ctx.portfolio.watchlist.iter())
    {
        if ctx.providers.tem_historico(a) && !v.contains(a) {
            v.push(a.clone());
        }
    }
    // O dólar entra sempre: é a série que ancora a conversão de tudo, e quem abre o
    // gráfico numa carteira só de renda fixa ainda tem o que ver.
    let usd = AssetId::new(Market::Fx, "USDBRL");
    if !v.contains(&usd) {
        v.push(usd);
    }
    v
}

fn primeiro_com_serie(ctx: &Ctx) -> Option<AssetId> {
    com_serie(ctx).into_iter().next()
}

pub struct Vista {
    ativo: Option<AssetId>,
    span: usize,
    /// Onde o cursor está, como índice na série. `None` é cursor desligado.
    cursor: Option<usize>,
    form: Option<Formulario>,
}

impl Vista {
    fn span(&self) -> Span {
        Span::ALL[self.span.min(Span::ALL.len() - 1)]
    }

    /// O preço médio do ativo na carteira, para a linha desenhada por cima. É o único
    /// número que transforma um gráfico numa decisão.
    fn preco_medio(&self, ctx: &Ctx) -> Option<f64> {
        let ativo = self.ativo.as_ref()?;
        let posicoes: Vec<&crate::invest::model::Position> = ctx
            .portfolio
            .posicoes
            .iter()
            .filter(|p| p.ativo == *ativo)
            .collect();
        crate::invest::carteira::preco_medio_consolidado(&posicoes)
    }
}

impl ModuleView for Vista {
    fn title(&self) -> String {
        match &self.ativo {
            Some(a) => format!("{} · {}", a.short(), self.span().label()),
            None => "Gráfico".into(),
        }
    }

    fn quer_mercado(&self) -> bool {
        true
    }

    fn ativos(&self) -> Vec<AssetId> {
        self.ativo.iter().cloned().collect()
    }

    fn layout(&self, ctx: &Ctx) -> Layout {
        if let Some(form) = &self.form {
            return Layout::one(Pane::Form {
                title: form.titulo.clone(),
                fields: form.campos.clone(),
                selected: form.selecionado,
                error: form.erro.clone(),
                hint: hint(&["←/→ escolher", "Enter abrir", "Esc cancelar"]),
            });
        }

        let Some(ativo) = &self.ativo else {
            return Layout::one(Pane::Empty {
                title: "Gráfico".into(),
                note: "Nenhum ativo com histórico.\n\n\
                       Têm série: pares da Binance (BINANCE/BTCBRL), câmbio (FX/USDBRL) e \n\
                       as séries do Banco Central (BCB/CDI).\n\n\
                       Ação da B3 e dos EUA usam preço informado nesta versão, e um preço \n\
                       informado não tem série — não é um gráfico vazio, é a ausência de \n\
                       uma fonte."
                    .into(),
            });
        };

        let estado = ctx.historico.get(ctx.providers, ativo, self.span());
        let Estado::Pronta(candles) = &estado else {
            return Layout::one(Pane::Empty {
                title: format!("{} · {}", ativo.short(), self.span().label()),
                note: historico::explicar(&estado, ativo).unwrap_or_default(),
            });
        };

        let serie = historico::fechamentos(candles);
        let rotulos = historico::rotulos(candles, self.span());
        let mut marks = Vec::new();
        if let Some(pm) = self.preco_medio(ctx) {
            marks.push((pm, "seu PM".to_string()));
        }

        let mut fatos = Vec::new();
        if let (Some(primeiro), Some(ultimo)) = (serie.first(), serie.last())
            && *primeiro > 0.0
        {
            let retorno = (ultimo / primeiro - 1.0) * 100.0;
            fatos.push((
                format!("no período ({})", self.span().label()),
                calc::pct(retorno),
                match retorno >= 0.0 {
                    true => Tone::Bom,
                    false => Tone::Ruim,
                },
            ));
        }
        if let (Some(pm), Some(ultimo)) = (self.preco_medio(ctx), serie.last())
            && pm > 0.0
        {
            fatos.push((
                "contra o seu preço médio".into(),
                calc::pct((ultimo / pm - 1.0) * 100.0),
                match ultimo >= &pm {
                    true => Tone::Bom,
                    false => Tone::Ruim,
                },
            ));
        }
        // Com o cursor ligado, ler um valor no meio da série deixa de ser adivinhar pela
        // altura.
        if let Some(i) = self.cursor
            && let (Some(v), Some(c)) = (serie.get(i), candles.get(i))
        {
            let d = crate::invest::tempo::Data::de_epoch(c.em, crate::invest::tempo::BRT_OFFSET);
            fatos.push((
                "cursor".into(),
                format!("{} em {}", calc::preco(*v), d.longa()),
                Tone::Destaque,
            ));
        }

        Layout::rows(vec![
            (
                3,
                Layout::one(Pane::Chart {
                    title: format!("{} · {}", ativo.short(), self.span().label()),
                    series: serie,
                    format: calc::preco,
                    marks,
                    x_labels: rotulos,
                    note: None,
                }),
            ),
            (
                1,
                Layout::one(Pane::Facts {
                    title: "Leitura".into(),
                    rows: fatos,
                }),
            ),
        ])
    }

    fn key(&mut self, key: KeyEvent, ctx: &Ctx) -> Outcome {
        if let Some(form) = &mut self.form {
            if form.tecla(key) {
                match AssetId::parse(&form.valor("ativo")) {
                    Ok(a) => {
                        self.ativo = Some(a);
                        self.cursor = None;
                        self.form = None;
                    }
                    Err(e) => form.erro = Some(e),
                }
            }
            return Outcome::Ok;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            // Com o cursor ligado, ←/→ deixam de trocar o período e passam a andar no
            // tempo. O rodapé diz qual dos dois está valendo — um mesmo par de teclas com
            // dois significados precisa dizer qual é.
            KeyCode::Left if self.cursor.is_some() => {
                if let Some(c) = &mut self.cursor {
                    *c = c.saturating_sub(1);
                }
                Outcome::Ok
            }
            KeyCode::Right if self.cursor.is_some() => {
                if let Some(c) = &mut self.cursor {
                    *c += 1;
                }
                Outcome::Ok
            }
            KeyCode::Left => {
                self.span = (self.span + Span::ALL.len() - 1) % Span::ALL.len();
                self.cursor = None;
                Outcome::Ok
            }
            KeyCode::Right => {
                self.span = (self.span + 1) % Span::ALL.len();
                self.cursor = None;
                Outcome::Ok
            }
            KeyCode::Char('x') if ctrl => {
                self.cursor = match self.cursor {
                    Some(_) => None,
                    None => Some(0),
                };
                Outcome::Ok
            }
            KeyCode::Char('t') if ctrl => {
                let opcoes: Vec<String> = com_serie(ctx).iter().map(|a| a.to_string()).collect();
                let atual = self
                    .ativo
                    .as_ref()
                    .map(|a| a.to_string())
                    .unwrap_or_default();
                self.form = Some(Formulario::novo(
                    "Trocar de ativo",
                    vec![Field::choice(
                        "ativo",
                        atual,
                        opcoes,
                        "←/→ anda pelos que têm série",
                    )],
                ));
                Outcome::Ok
            }
            // Levam o ativo que está na tela: abrir os indicadores de outro papel que
            // não o que se está olhando seria trocar o assunto no meio.
            KeyCode::Char('i') => Outcome::Abrir {
                modulo: "indicadores",
                alvo: self.ativo.clone(),
            },
            KeyCode::Char('c') => Outcome::Abrir {
                modulo: "comparador",
                alvo: self.ativo.clone(),
            },
            _ => Outcome::Ignorada,
        }
    }

    fn escape(&mut self) -> Escape {
        if self.form.take().is_some() || self.cursor.take().is_some() {
            return Escape::Consumido;
        }
        Escape::Nao
    }

    fn hint(&self) -> String {
        match self.cursor.is_some() {
            true => hint(&["←/→ andar no tempo", "Ctrl+X desliga o cursor", "Esc sair"]),
            false => hint(&[
                "←/→ período",
                "Ctrl+X cursor",
                "Ctrl+T trocar ativo",
                "i indicadores",
                "c comparar",
                "Esc sair",
            ]),
        }
    }
}
