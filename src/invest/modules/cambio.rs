//! Câmbio — o provedor mais crítico da aba.
//!
//! O patrimônio é consolidado em BRL, então todo ativo que não é brasileiro passa por
//! aqui antes de virar um número na tela. **Um câmbio errado erra o patrimônio inteiro,
//! silenciosamente** — é o único número da aba com essa propriedade.

use crossterm::event::{KeyCode, KeyEvent};

use crate::invest::calc;
use crate::invest::carteira;
use crate::invest::model::{AssetId, Market, Moeda};
use crate::invest::module::{
    Ctx, Escape, Group, InvestModule, Layout, Marcavel, ModuleView, Outcome, Pane, Row, Tone,
};
use crate::invest::modules::comum::{Lista, hint, sem_preco};
use crate::invest::modules::mercado;
use crate::invest::providers::cambio::SIMBOLO_PTAX;

pub struct Cambio;

impl InvestModule for Cambio {
    fn id(&self) -> &'static str {
        "cambio"
    }
    fn name(&self) -> &'static str {
        "Câmbio"
    }
    fn description(&self) -> &'static str {
        "USD/BRL e EUR/BRL ao vivo, mais a PTAX — o número que vale para o imposto"
    }
    fn destaque(&self) -> u8 {
        3
    }
    fn group(&self) -> Group {
        Group::Mercado
    }

    fn marcavel(&self) -> Option<Marcavel> {
        Some(crate::invest::marcas::ATIVOS)
    }
    fn keywords(&self) -> &'static str {
        "dólar euro moeda ptax conversão exposição"
    }

    fn summary(&self, ctx: &Ctx) -> String {
        match ctx.market.taxa(Moeda::Usd) {
            Some(t) => format!("USD/BRL {}", calc::preco(t)),
            None => "sem câmbio — ativo estrangeiro fica fora do total".into(),
        }
    }

    fn widget(&self, ctx: &Ctx) -> Option<Pane> {
        Some(Pane::Facts {
            title: String::new(),
            rows: pares(ctx)
                .iter()
                .take(6)
                .map(|par| {
                    let q = ctx.market.quote(par);
                    let variacao = q.and_then(|q| q.variacao());
                    (
                        par.short().to_string(),
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
        })
    }
}

struct Vista {
    lista: Lista,
}

/// Os pares essenciais **mais** os que o usuário acompanha. Uma lista fixa aqui era mais
/// um lugar onde adicionar um ativo não refletia: quem punha `FX/EURBRL` em Cotações não
/// o via nesta tela.
fn pares(ctx: &Ctx) -> Vec<AssetId> {
    let mut v = vec![
        AssetId::new(Market::Fx, "USDBRL"),
        AssetId::new(Market::Fx, "EURBRL"),
        AssetId::new(Market::Fx, SIMBOLO_PTAX),
    ];
    for a in ctx
        .portfolio
        .watchlist
        .iter()
        .chain(ctx.portfolio.posicoes.iter().map(|p| &p.ativo))
        .filter(|a| a.market == Market::Fx)
    {
        if !v.contains(a) {
            v.push(a.clone());
        }
    }
    v
}

impl ModuleView for Vista {
    fn title(&self) -> String {
        "Câmbio".into()
    }

    fn quer_mercado(&self) -> bool {
        true
    }

    fn ativos(&self) -> Vec<AssetId> {
        // Os essenciais; os do usuário já entram por `ativos_de_interesse`.
        crate::invest::providers::cambio::pares_essenciais()
    }

    fn layout(&self, ctx: &Ctx) -> Layout {
        let pares = pares(ctx);
        let rows: Vec<Row> = pares
            .iter()
            .map(|a| mercado::linha(a, ctx.market.quote(a), false, ctx.agora, ctx))
            .collect();

        // Quanto do patrimônio depende deste número estar certo. É a linha que liga este
        // módulo à carteira, e a razão de ele ser crítico.
        let linhas = carteira::linhas(ctx.portfolio, ctx.market, ctx.agora);
        let totais = carteira::totais(&linhas);
        let estrangeiro: f64 = linhas
            .iter()
            .filter(|l| l.posicao.moeda != Moeda::Brl)
            .filter_map(|l| l.mercado_brl)
            .sum();
        let peso = match totais.mercado > 0.0 {
            true => estrangeiro / totais.mercado * 100.0,
            false => 0.0,
        };

        let mut fatos = vec![
            (
                "exposição em moeda estrangeira".into(),
                format!(
                    "R$ {} ({})",
                    calc::moeda(estrangeiro),
                    calc::pct_simples(peso)
                ),
                match peso > 0.0 {
                    true => Tone::Destaque,
                    false => Tone::Dim,
                },
            ),
            (
                "PTAX é o que vale para imposto".into(),
                "a de mercado converte a tela; a PTAX converte o DARF".into(),
                Tone::Dim,
            ),
        ];
        if totais.fora_sem_cambio > 0 {
            fatos.push((
                "fora do total".into(),
                format!(
                    "{} posição(ões) — sem câmbio, converter seria inventar",
                    totais.fora_sem_cambio
                ),
                Tone::Aviso,
            ));
        }

        Layout::rows(vec![
            (
                2,
                Layout::one(Pane::Table {
                    title: "Câmbio".into(),
                    headers: mercado::CABECALHO.iter().map(|s| s.to_string()).collect(),
                    rows,
                    selected: Some(self.lista.selecionado),
                    query: String::new(),
                    note: mercado::nota_fontes(ctx),
                }),
            ),
            (
                1,
                Layout::one(Pane::Facts {
                    title: "Na sua carteira".into(),
                    rows: fatos,
                }),
            ),
        ])
    }

    fn key(&mut self, key: KeyEvent, ctx: &Ctx) -> Outcome {
        match key.code {
            KeyCode::Enter => Outcome::Abrir {
                modulo: "grafico",
                alvo: pares(ctx).get(self.lista.selecionado).cloned(),
            },
            _ => match self.lista.tecla(key, pares(ctx).len()) {
                true => Outcome::Ok,
                false => Outcome::Ignorada,
            },
        }
    }

    fn escape(&mut self) -> Escape {
        self.lista.escape()
    }

    fn hint(&self) -> String {
        hint(&["↑/↓ andar", "Enter gráfico", "Esc sair"])
    }
}
