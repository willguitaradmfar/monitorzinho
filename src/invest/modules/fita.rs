//! A fita: a faixa de uma linha no topo da aba.
//!
//! Ela **não rola nem anima**. Um terminal não é telão de corretora: texto que se move é
//! texto que não se lê, e animar forçaria um redesenho constante — que é exatamente o que
//! o laço principal foi desenhado para evitar (ele só repinta quando algo muda).
//!
//! É o primeiro caso da aba de um módulo cuja tela cheia é só configuração. Isso é
//! aceitável e é a exceção; um segundo caso desses seria sinal de que falta uma tela de
//! preferências.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::invest::calc;
use crate::invest::model::AssetId;
use crate::invest::module::{
    Ctx, Edit, Escape, Field, Group, InvestModule, Layout, ModuleView, Outcome, Pane, Row, Tone,
};
use crate::invest::modules::comum::{Formulario, Lista, hint, sem_preco};

pub struct Fita;

impl InvestModule for Fita {
    fn id(&self) -> &'static str {
        "fita"
    }
    fn name(&self) -> &'static str {
        "Fita"
    }
    fn description(&self) -> &'static str {
        "A faixa do topo da aba: o mínimo que se quer ver sem entrar em nada"
    }
    fn destaque(&self) -> u8 {
        3
    }
    fn group(&self) -> Group {
        Group::Mercado
    }
    fn keywords(&self) -> &'static str {
        "faixa topo ticker tape barra resumo"
    }

    fn summary(&self, ctx: &Ctx) -> String {
        match ctx.portfolio.fita.len() {
            0 => "vazia — não ocupa linha nenhuma".into(),
            n => format!("{n} na faixa do topo"),
        }
    }

    fn widget(&self, ctx: &Ctx) -> Option<Pane> {
        if ctx.portfolio.fita.is_empty() {
            return None;
        }
        Some(Pane::Facts {
            title: String::new(),
            rows: ctx
                .portfolio
                .fita
                .iter()
                .take(8)
                .map(|a| {
                    let q = ctx.market.quote(a);
                    let variacao = q.and_then(|q| q.variacao());
                    (
                        a.short().to_string(),
                        match (q, variacao) {
                            (Some(q), Some(v)) => {
                                format!("{} {}", calc::preco(q.preco), calc::pct(v))
                            }
                            (Some(q), None) => calc::preco(q.preco),
                            (None, _) => sem_preco(ctx),
                        },
                        crate::invest::modules::heatmap::tom(variacao),
                    )
                })
                .collect(),
        })
    }

    fn open(&self, _ctx: &Ctx, _alvo: Option<&AssetId>) -> Box<dyn ModuleView> {
        Box::new(Vista {
            lista: Lista::default(),
            form: None,
        })
    }
}

struct Vista {
    lista: Lista,
    form: Option<Formulario>,
}

impl ModuleView for Vista {
    fn title(&self) -> String {
        "Fita".into()
    }

    fn quer_mercado(&self) -> bool {
        true
    }

    fn layout(&self, ctx: &Ctx) -> Layout {
        if let Some(form) = &self.form {
            return Layout::one(Pane::Form {
                title: form.titulo.clone(),
                fields: form.campos.clone(),
                selected: form.selecionado,
                error: form.erro.clone(),
                hint: hint(&["Enter adicionar", "Esc cancelar"]),
            });
        }

        let rows: Vec<Row> = ctx
            .portfolio
            .fita
            .iter()
            .map(|a| {
                let q = ctx.market.quote(a);
                Row::new(vec![
                    a.to_string(),
                    q.map(|q| calc::preco(q.preco)).unwrap_or("—".into()),
                    q.and_then(|q| q.variacao().filter(|_| q.grade.ao_vivo()))
                        .map(calc::pct)
                        .unwrap_or_default(),
                ])
                .with_cell_tones(vec![Tone::Normal, Tone::Normal, Tone::Dim])
            })
            .collect();

        if rows.is_empty() {
            return Layout::one(Pane::Empty {
                title: "Fita".into(),
                note: "A fita está vazia — e vazia ela não ocupa linha nenhuma no topo, \n\
                       em vez de deixar uma faixa em branco.\n\n\
                       Ctrl+A põe um ativo nela. Ela aparece só na tela principal da aba: \n\
                       dentro de um módulo ela some, porque um módulo é onde o programa \n\
                       sai da frente.\n\n\
                       Ela também não rola nem pisca — texto que se move é texto que não \n\
                       se lê, e animar faria a tela repintar sem parar."
                    .into(),
            });
        }

        Layout::rows(vec![
            (
                3,
                Layout::one(Pane::Table {
                    title: format!(
                        "Fita · {}, nesta ordem",
                        calc::plural(rows.len(), "ativo", "ativos")
                    ),
                    headers: vec!["Ativo".into(), "Último".into(), "Var%".into()],
                    rows,
                    selected: Some(self.lista.selecionado),
                    query: self.lista.busca.clone(),
                    note: Some(
                        "a ordem é o que decide quem sobrevive ao corte num terminal estreito"
                            .into(),
                    ),
                }),
            ),
            (
                1,
                Layout::one(Pane::Facts {
                    title: "Como ela se comporta".into(),
                    rows: vec![
                        (
                            "onde aparece".into(),
                            "só na tela principal da aba Invest".into(),
                            Tone::Dim,
                        ),
                        (
                            "custo com a aba fechada".into(),
                            "nenhum — ela lê o retrato que os módulos alimentam".into(),
                            Tone::Dim,
                        ),
                    ],
                }),
            ),
        ])
    }

    fn key(&mut self, key: KeyEvent, ctx: &Ctx) -> Outcome {
        if let Some(form) = &mut self.form {
            if form.tecla(key) {
                return match AssetId::parse(&form.valor("ativo")) {
                    Ok(a) => {
                        let mut fita = ctx.portfolio.fita.clone();
                        if !fita.contains(&a) {
                            fita.push(a);
                        }
                        self.form = None;
                        Outcome::Editar(vec![Edit::SetFita(fita)])
                    }
                    Err(e) => {
                        form.erro = Some(e);
                        Outcome::Ok
                    }
                };
            }
            return Outcome::Ok;
        }

        // Pelo índice visível — com busca ativa, `selecionado` é da lista filtrada, e
        // reordenar ou apagar pelo número dele mexeria na linha errada.
        let rows: Vec<Row> = ctx
            .portfolio
            .fita
            .iter()
            .map(|a| Row::new(vec![a.to_string()]))
            .collect();
        let i = self.lista.atual(&rows).unwrap_or(usize::MAX);
        let n = ctx.portfolio.fita.len();
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('a') if ctrl => {
                self.form = Some(Formulario::novo(
                    "Pôr na fita",
                    vec![Field::text(
                        "ativo",
                        "",
                        "BINANCE/BTCBRL, FX/USDBRL, BCB/CDI, B3/PETR4",
                    )],
                ));
                Outcome::Ok
            }
            KeyCode::Up if ctrl && i > 0 && i < n => {
                let mut fita = ctx.portfolio.fita.clone();
                fita.swap(i, i - 1);
                self.lista.selecionado = self.lista.selecionado.saturating_sub(1);
                Outcome::Editar(vec![Edit::SetFita(fita)])
            }
            KeyCode::Down if ctrl && i + 1 < n => {
                let mut fita = ctx.portfolio.fita.clone();
                fita.swap(i, i + 1);
                self.lista.selecionado += 1;
                Outcome::Editar(vec![Edit::SetFita(fita)])
            }
            KeyCode::Delete if i < n => {
                let mut fita = ctx.portfolio.fita.clone();
                fita.remove(i);
                Outcome::Editar(vec![Edit::SetFita(fita)])
            }
            _ => match self.lista.tecla(key, rows.len()) {
                true => Outcome::Ok,
                false => Outcome::Ignorada,
            },
        }
    }

    fn escape(&mut self) -> Escape {
        match self.form.take().is_some() {
            true => Escape::Consumido,
            false => self.lista.escape(),
        }
    }

    fn hint(&self) -> String {
        hint(&[
            "Ctrl+A adicionar",
            "Ctrl+↑/↓ reordenar",
            "Del tirar",
            "Esc sair",
        ])
    }
}
