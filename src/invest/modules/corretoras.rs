//! Corretoras: o mesmo patrimônio, visto por onde ele está guardado.
//!
//! É módulo e não uma dimensão de Alocação porque a pergunta é outra. Alocação pergunta
//! «estou bem distribuído»; aqui a pergunta é operacional: de onde veio cada importação,
//! o que está desatualizado, e onde eu entro para mexer nisso.

use crossterm::event::{KeyCode, KeyEvent};

use crate::invest::calc;
use crate::invest::carteira;
use crate::invest::model::AssetId;
use crate::invest::module::{
    Bar, Ctx, Escape, Group, InvestModule, Layout, Marcavel, ModuleView, Need, Outcome, Pane, Row,
    TipoDeMarca, Tone,
};
use crate::invest::modules::comum::{Lista, hint};
use crate::invest::tempo;

pub struct Corretoras;

/// A partir de quantos dias uma importação passa a ser velha o bastante para ser dita.
/// Não é erro: é que o P&L está sendo calculado sobre quantidades de um mês atrás.
const VELHA: u64 = 30 * 86400;

/// O que se pode seguir nesta lista. O id da tabela nunca muda depois de publicado — é
/// com ele que as marcas já gravadas se reconhecem.
static MARCAS: Marcavel = Marcavel {
    tabela: "invest-corretoras",
    nome: "Corretoras",
    tipos: &[TipoDeMarca {
        nome: "corretora",
        coluna: "Fonte",
        numerico: false,
        ajuda: "o nome da corretora",
    }],
};

impl InvestModule for Corretoras {
    fn id(&self) -> &'static str {
        "corretoras"
    }
    fn name(&self) -> &'static str {
        "Corretoras"
    }
    fn description(&self) -> &'static str {
        "Onde o patrimônio está custodiado, e o que precisa ser reimportado"
    }
    fn destaque(&self) -> u8 {
        3
    }
    fn group(&self) -> Group {
        Group::Carteira
    }

    fn fonte_externa(&self) -> Option<&'static str> {
        Some(crate::invest::store::fonte::COTACAO)
    }

    fn marcavel(&self) -> Option<Marcavel> {
        Some(MARCAS)
    }
    fn needs(&self) -> &'static [Need] {
        &[Need::Posicoes]
    }
    fn keywords(&self) -> &'static str {
        "custódia conta fonte importação concentração"
    }

    fn summary(&self, ctx: &Ctx) -> String {
        let fontes = ctx.portfolio.fontes();
        match fontes.len() {
            0 => "sem posições".into(),
            1 => format!("tudo em {}", fontes[0]),
            n => format!("{n} fontes"),
        }
    }

    fn widget(&self, ctx: &Ctx) -> Option<Pane> {
        if ctx.portfolio.posicoes.is_empty() {
            return None;
        }
        let linhas = carteira::linhas(ctx.portfolio, ctx.market, ctx.agora);
        let mut por_fonte = carteira::agrupar(&linhas, |l| l.posicao.fonte.clone());
        por_fonte.sort_by(|a, b| b.1.total_cmp(&a.1));
        Some(Pane::Bars {
            title: String::new(),
            rows: por_fonte
                .into_iter()
                .take(8)
                .map(|(fonte, valor)| Bar {
                    label: fonte,
                    value: valor,
                    target: None,
                    text: format!("R$ {}", calc::moeda(valor)),
                    tone: Tone::Normal,
                })
                .collect(),
            full: None,
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

impl ModuleView for Vista {
    fn title(&self) -> String {
        "Corretoras".into()
    }

    fn quer_mercado(&self) -> bool {
        true
    }

    fn layout(&self, ctx: &Ctx) -> Layout {
        if ctx.portfolio.posicoes.is_empty() {
            return Layout::one(Pane::Empty {
                title: "Corretoras".into(),
                note: "Sem posições ainda. Comece por Posições ou por Importação.".into(),
            });
        }

        let linhas = carteira::linhas(ctx.portfolio, ctx.market, ctx.agora);
        let totais = carteira::totais(&linhas);
        let mut rows: Vec<Row> = Vec::new();
        let mut maior: (String, f64) = (String::new(), 0.0);

        for fonte in ctx.portfolio.fontes() {
            let da_fonte: Vec<&carteira::Linha> =
                linhas.iter().filter(|l| l.posicao.fonte == fonte).collect();
            let valor: f64 = da_fonte.iter().filter_map(|l| l.mercado_brl).sum();
            let peso = match totais.mercado > 0.0 {
                true => valor / totais.mercado * 100.0,
                false => 0.0,
            };
            if peso > maior.1 {
                maior = (fonte.clone(), peso);
            }
            // A idade da importação, que é a informação que decide se o P&L vale algo.
            // Ela é por fonte, e some quando os números são consolidados por ativo.
            let mais_antigo = da_fonte
                .iter()
                .map(|l| l.posicao.atualizado_em)
                .min()
                .unwrap_or(0);
            let idade = ctx.agora.saturating_sub(mais_antigo);
            let velha = mais_antigo > 0 && idade > VELHA;
            rows.push(
                Row::new(vec![
                    fonte.clone(),
                    da_fonte
                        .iter()
                        .map(|l| l.posicao.conta.as_str())
                        .find(|c| !c.is_empty())
                        .unwrap_or("—")
                        .to_string(),
                    da_fonte.len().to_string(),
                    calc::moeda(valor),
                    calc::pct_simples(peso),
                    match mais_antigo {
                        0 => "—".to_string(),
                        _ => tempo::idade(idade),
                    },
                ])
                .with_cell_tones(vec![
                    Tone::Normal,
                    Tone::Dim,
                    Tone::Dim,
                    Tone::Normal,
                    Tone::Dim,
                    match velha {
                        true => Tone::Aviso,
                        false => Tone::Dim,
                    },
                ]),
            );
        }

        let mut fatos = vec![(
            "maior concentração".into(),
            match maior.0.is_empty() {
                true => "—".into(),
                false => format!("{} com {}", maior.0, calc::pct_simples(maior.1)),
            },
            match maior.1 > 60.0 {
                true => Tone::Aviso,
                false => Tone::Normal,
            },
        )];
        fatos.push((
            "total".into(),
            format!("R$ {}", calc::moeda(totais.mercado)),
            Tone::Destaque,
        ));
        if !totais.ressalva().is_empty() {
            fatos.push(("atenção".into(), totais.ressalva(), Tone::Aviso));
        }

        Layout::rows(vec![
            (
                3,
                Layout::one(Pane::Table {
                    title: format!(
                        "Corretoras · {}",
                        calc::plural(rows.len(), "fonte", "fontes")
                    ),
                    headers: vec![
                        "Fonte".into(),
                        "Conta".into(),
                        "Ativos".into(),
                        "Valor".into(),
                        "Peso".into(),
                        "Importada".into(),
                    ],
                    rows,
                    selected: Some(self.lista.selecionado),
                    query: self.lista.busca.clone(),
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
            KeyCode::Enter => Outcome::Abrir {
                modulo: "posicoes",
                alvo: None,
            },
            KeyCode::Char('i') => Outcome::Abrir {
                modulo: "importacao",
                alvo: None,
            },
            _ => match self.lista.tecla(key, ctx.portfolio.fontes().len()) {
                true => Outcome::Ok,
                false => Outcome::Ignorada,
            },
        }
    }

    fn escape(&mut self) -> Escape {
        self.lista.escape()
    }

    fn hint(&self) -> String {
        hint(&["↑/↓ andar", "Enter posições", "i importar", "Esc sair"])
    }
}
