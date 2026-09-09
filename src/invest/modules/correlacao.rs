//! Correlação: quais ativos sobem e descem juntos — ou seja, onde a diversificação que
//! se acha que tem não existe.
//!
//! A armadilha que a tela precisa evitar: correlação **não é estável**. Duas coisas com
//! 0,1 em ano calmo vão para 0,8 num crash — que é exatamente quando a diversificação
//! deveria funcionar. Por isso o período usado está sempre visível, e por isso há
//! períodos curtos e longos: para que a instabilidade fique à vista.

use crossterm::event::{KeyCode, KeyEvent};

use crate::invest::calc;
use crate::invest::historico::{Estado, fechamentos};
use crate::invest::model::AssetId;
use crate::invest::module::{
    Ctx, Escape, Group, InvestModule, Layout, ModuleView, Need, Outcome, Pane, Row, Tone,
};
use crate::invest::modules::comum::{Lista, hint};
use crate::invest::provider::Span;

pub struct Correlacao;

impl InvestModule for Correlacao {
    fn id(&self) -> &'static str {
        "correlacao"
    }
    fn name(&self) -> &'static str {
        "Correlação"
    }
    fn description(&self) -> &'static str {
        "A matriz que mostra a diversificação que não existe"
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
        "matriz pearson diversificação juntos par duplicata"
    }

    fn summary(&self, _ctx: &Ctx) -> String {
        "abra para montar a matriz sobre as séries disponíveis".into()
    }

    fn widget(&self, ctx: &Ctx) -> Option<Pane> {
        use crate::invest::historico::{Estado, fechamentos};
        let ativos = candidatos(ctx);
        if ativos.len() < 2 {
            return None;
        }
        let series: Vec<(String, Vec<f64>)> = ativos
            .iter()
            .filter_map(|a| match ctx.historico.get(ctx.providers, a, Span::Ano) {
                Estado::Pronta(velas) => {
                    let r = calc::retornos(&fechamentos(&velas));
                    (r.len() >= 20).then(|| (a.short().to_string(), r))
                }
                _ => None,
            })
            .collect();
        // Os pares que mais andam juntos. É a pergunta que a matriz responde e que
        // importa numa olhada: diversificação de mentira é a que não se percebe.
        let mut pares: Vec<(String, f64)> = Vec::new();
        for (i, (na, ra)) in series.iter().enumerate() {
            for (nb, rb) in series.iter().skip(i + 1) {
                let n = ra.len().min(rb.len());
                if let Some(c) = calc::correlacao(&ra[ra.len() - n..], &rb[rb.len() - n..]) {
                    pares.push((format!("{na} · {nb}"), c));
                }
            }
        }
        if pares.is_empty() {
            return None;
        }
        pares.sort_by(|a, b| b.1.abs().total_cmp(&a.1.abs()));
        Some(Pane::Facts {
            title: String::new(),
            rows: pares
                .into_iter()
                .take(6)
                .map(|(par, c)| {
                    (
                        par,
                        format!("{c:.2}").replace('.', ","),
                        match c.abs() > 0.7 {
                            // Alta correlação é o aviso: dois papéis que sobem e caem
                            // juntos são, para efeito de risco, um só.
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
            lista: Lista::default(),
        })
    }
}

struct Vista {
    span: usize,
    lista: Lista,
}

fn candidatos(ctx: &Ctx) -> Vec<AssetId> {
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
    v
}

impl Vista {
    fn span(&self) -> Span {
        Span::ALL[self.span.min(Span::ALL.len() - 1)]
    }
}

impl ModuleView for Vista {
    fn title(&self) -> String {
        format!("Correlação · {}", self.span().label())
    }

    fn quer_mercado(&self) -> bool {
        true
    }

    fn layout(&self, ctx: &Ctx) -> Layout {
        let ativos = candidatos(ctx);
        if ativos.len() < 2 {
            return Layout::one(Pane::Empty {
                title: "Correlação".into(),
                note: "Correlação precisa de pelo menos dois ativos com série.\n\n\
                       Têm série: pares da Binance e câmbio. Adicione mais em Cotações."
                    .into(),
            });
        }

        // Carrega o que ainda não foi pedido — a matriz aparece parcial e vai preenchendo,
        // em vez de esperar tudo para mostrar algo.
        let series: Vec<(AssetId, Option<Vec<f64>>)> = ativos
            .iter()
            .map(|a| {
                let s = match ctx.historico.get(ctx.providers, a, self.span()) {
                    Estado::Pronta(c) => Some(calc::retornos(&fechamentos(&c))),
                    _ => None,
                };
                (a.clone(), s)
            })
            .collect();

        let mut headers = vec![String::new()];
        headers.extend(ativos.iter().map(|a| a.short().to_string()));

        let mut rows = Vec::new();
        let mut par_mais_alto: Option<(String, f64)> = None;
        let mut soma = 0.0;
        let mut pares = 0usize;

        for (i, (ativo_i, serie_i)) in series.iter().enumerate() {
            let mut celulas = vec![ativo_i.short().to_string()];
            let mut tons = vec![Tone::Normal];
            for (j, (ativo_j, serie_j)) in series.iter().enumerate() {
                // Só a metade de cima é desenhada: a matriz é simétrica, e desenhar as
                // duas metades gasta espaço para repetir a informação.
                if j < i {
                    celulas.push(String::new());
                    tons.push(Tone::Dim);
                    continue;
                }
                if i == j {
                    celulas.push("1,00".into());
                    tons.push(Tone::Dim);
                    continue;
                }
                match (serie_i, serie_j) {
                    (Some(a), Some(b)) => match calc::correlacao(a, b) {
                        Some(c) => {
                            celulas.push(format!("{c:.2}").replace('.', ","));
                            // Vermelho para perto de 1: correlação alta é o que se quer
                            // notar numa carteira.
                            tons.push(match c {
                                c if c > 0.7 => Tone::Ruim,
                                c if c < -0.3 => Tone::Bom,
                                _ => Tone::Normal,
                            });
                            soma += c;
                            pares += 1;
                            if par_mais_alto.as_ref().is_none_or(|(_, v)| c > *v) {
                                par_mais_alto =
                                    Some((format!("{}–{}", ativo_i.short(), ativo_j.short()), c));
                            }
                        }
                        // Vazio, e nunca zero: zero é uma correlação válida e significa
                        // «não andam juntos»; vazio significa «não sabemos».
                        None => {
                            celulas.push("·".into());
                            tons.push(Tone::Dim);
                        }
                    },
                    _ => {
                        celulas.push("·".into());
                        tons.push(Tone::Dim);
                    }
                }
            }
            rows.push(Row::new(celulas).with_cell_tones(tons));
        }

        let pendentes = ctx.historico.pendentes();
        let mut fatos = Vec::new();
        if let Some((par, v)) = par_mais_alto {
            fatos.push((
                "mais correlacionados".into(),
                format!("{par} · {}", format!("{v:.2}").replace('.', ",")),
                match v > 0.7 {
                    true => Tone::Ruim,
                    false => Tone::Normal,
                },
            ));
        }
        if pares > 0 {
            fatos.push((
                "correlação média".into(),
                format!("{:.2}", soma / pares as f64).replace('.', ","),
                Tone::Destaque,
            ));
        }
        if pendentes > 0 {
            fatos.push((
                "carregando".into(),
                format!("{pendentes} série(s) — a matriz preenche sozinha"),
                Tone::Dim,
            ));
        }
        fatos.push((
            "atenção".into(),
            "correlação não é estável: o que dá 0,1 em ano calmo vai a 0,8 num crash".into(),
            Tone::Aviso,
        ));

        Layout::rows(vec![
            (
                3,
                Layout::one(Pane::Table {
                    title: format!(
                        "Correlação · {} · {} ativos",
                        self.span().label(),
                        ativos.len()
                    ),
                    headers,
                    rows,
                    selected: Some(self.lista.selecionado),
                    query: String::new(),
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
        match key.code {
            KeyCode::Left => {
                self.span = (self.span + Span::ALL.len() - 1) % Span::ALL.len();
                Outcome::Ok
            }
            KeyCode::Right => {
                self.span = (self.span + 1) % Span::ALL.len();
                Outcome::Ok
            }
            KeyCode::Enter => Outcome::Abrir {
                modulo: "comparador",
                alvo: candidatos(ctx).get(self.lista.selecionado).cloned(),
            },
            _ => match self.lista.tecla(key, candidatos(ctx).len()) {
                true => Outcome::Ok,
                false => Outcome::Ignorada,
            },
        }
    }

    fn escape(&mut self) -> Escape {
        Escape::Nao
    }

    fn hint(&self) -> String {
        hint(&["←/→ período", "Enter comparar", "Esc sair"])
    }
}
