//! Fundamentos: se o preço faz sentido diante do que a empresa é.
//!
//! O documento de planejamento dizia que este módulo não existiria sem chave paga ou sem
//! montar os indicadores a partir dos dados abertos da CVM. **Estava errado**: a brapi
//! serve `defaultKeyStatistics`, `financialData` e `summaryProfile` mesmo com token
//! grátis — conferido em 08/09/2026. O que faltava era medir em vez de supor.

use crossterm::event::{KeyCode, KeyEvent};

use crate::invest::calc;
use crate::invest::fundamento::Estado;
use crate::invest::model::AssetId;
use crate::invest::module::{
    Ctx, Escape, Group, InvestModule, Layout, Marcavel, ModuleView, Outcome, Pane, Row, Tone,
};
use crate::invest::modules::comum::{Lista, hint};

pub struct Fundamentos;

impl InvestModule for Fundamentos {
    fn id(&self) -> &'static str {
        "fundamentos"
    }
    fn name(&self) -> &'static str {
        "Fundamentos"
    }
    fn description(&self) -> &'static str {
        "P/L, margem, EBITDA, dívida e setor — de todos os ativos da carteira"
    }
    fn destaque(&self) -> u8 {
        3
    }
    fn group(&self) -> Group {
        Group::Analise
    }

    fn marcavel(&self) -> Option<Marcavel> {
        Some(crate::invest::marcas::ATIVOS)
    }
    fn keywords(&self) -> &'static str {
        "p/l lpa roe dy balanço dre lucro dívida margem ebitda setor indústria"
    }

    fn summary(&self, ctx: &Ctx) -> String {
        match empresas(ctx).len() {
            0 => "nenhuma empresa na carteira".into(),
            n => calc::plural(n, "empresa", "empresas"),
        }
    }

    fn widget(&self, ctx: &Ctx) -> Option<Pane> {
        let empresas = empresas(ctx);
        if empresas.is_empty() {
            return None;
        }
        // Os múltiplos vêm de rede e ficam no cache do módulo aberto. Aqui vale dizer
        // sobre **quais** empresas eles serão buscados — que é o que o cartão sabe.
        Some(Pane::Facts {
            title: String::new(),
            rows: empresas
                .iter()
                .take(8)
                .map(|a| {
                    (
                        a.short().to_string(),
                        ctx.market
                            .quote(a)
                            .map(|q| calc::preco(q.preco))
                            .unwrap_or_default(),
                        Tone::Normal,
                    )
                })
                .collect(),
        })
    }

    fn open(&self, ctx: &Ctx, alvo: Option<&AssetId>) -> Box<dyn ModuleView> {
        let empresas = empresas(ctx);
        let selecionado = alvo
            .and_then(|a| empresas.iter().position(|x| x == a))
            .unwrap_or(0);
        Box::new(Vista {
            lista: Lista {
                selecionado,
                ..Default::default()
            },
        })
    }
}

/// Os ativos que **são empresas**. Cripto, câmbio e séries do Banco Central não têm
/// balanço, e listá-los aqui seria oferecer uma tela que nunca vai preencher.
fn empresas(ctx: &Ctx) -> Vec<AssetId> {
    let mut v: Vec<AssetId> = Vec::new();
    for a in ctx
        .portfolio
        .posicoes
        .iter()
        .map(|p| &p.ativo)
        .chain(ctx.portfolio.watchlist.iter())
    {
        if ctx.providers.tem_fundamento(a) && !v.contains(a) {
            v.push(a.clone());
        }
    }
    v
}

struct Vista {
    lista: Lista,
}

/// Um número grande em palavras: `637,3 bi`. Um valor de mercado com onze dígitos não se
/// lê; ele só ocupa a coluna.
fn grande(v: f64) -> String {
    let (escala, sufixo) = match v.abs() {
        a if a >= 1e12 => (1e12, " tri"),
        a if a >= 1e9 => (1e9, " bi"),
        a if a >= 1e6 => (1e6, " mi"),
        a if a >= 1e3 => (1e3, " mil"),
        _ => (1.0, ""),
    };
    format!("{}{sufixo}", calc::preco(v / escala))
}

impl ModuleView for Vista {
    fn title(&self) -> String {
        "Fundamentos".into()
    }

    fn quer_mercado(&self) -> bool {
        true
    }

    fn layout(&self, ctx: &Ctx) -> Layout {
        let empresas = empresas(ctx);
        if empresas.is_empty() {
            return Layout::one(Pane::Empty {
                title: "Fundamentos".into(),
                note: "Nenhuma empresa na carteira nem na watchlist.\n\n\
                       Fundamento é balanço, e só empresa tem balanço: cripto, câmbio e as \n\
                       séries do Banco Central não entram aqui.\n\n\
                       Adicione uma ação ou um FII em Posições ou em Cotações."
                    .into(),
            });
        }

        let rows: Vec<Row> = empresas
            .iter()
            .map(|a| match ctx.fundamentos.get(ctx.providers, a) {
                Estado::Pronto(f) => Row::new(vec![
                    a.short().to_string(),
                    f.setor.clone().unwrap_or_else(|| "—".into()),
                    // P/L de empresa com prejuízo não é desenhado: é uma divisão que não
                    // significa nada, e um número negativo aqui se leria como «barato».
                    f.p_l
                        .filter(|v| *v > 0.0)
                        .map(|v| format!("{v:.1}").replace('.', ","))
                        .unwrap_or_else(|| "—".into()),
                    f.margem_liquida
                        .map(calc::pct_simples)
                        .unwrap_or_else(|| "—".into()),
                    f.divida_sobre_ebitda()
                        .map(|v| format!("{v:.2}x").replace('.', ","))
                        .unwrap_or_else(|| "—".into()),
                    f.valor_de_mercado.map(grande).unwrap_or_else(|| "—".into()),
                ])
                .with_cell_tones(vec![
                    Tone::Normal,
                    Tone::Dim,
                    Tone::Normal,
                    match f.margem_liquida {
                        Some(m) if m > 0.0 => Tone::Bom,
                        Some(_) => Tone::Ruim,
                        None => Tone::Dim,
                    },
                    match f.divida_sobre_ebitda() {
                        Some(d) if d > 3.0 => Tone::Aviso,
                        _ => Tone::Normal,
                    },
                    Tone::Dim,
                ]),
                outro => Row::tinted(
                    vec![
                        a.short().to_string(),
                        // Curto: a frase inteira vai no painel de detalhe, que tem espaço.
                        crate::invest::fundamento::motivo_curto(&outro).to_string(),
                        "—".into(),
                        "—".into(),
                        "—".into(),
                        "—".into(),
                    ],
                    Tone::Dim,
                ),
            })
            .collect();

        // O detalhe da linha sob o cursor: o que não cabe numa coluna.
        let detalhe = self
            .lista
            .atual(&rows)
            .and_then(|i| empresas.get(i))
            .map(|a| (a.clone(), ctx.fundamentos.get(ctx.providers, a)));
        let fatos: Vec<(String, String, Tone)> = match &detalhe {
            Some((a, Estado::Pronto(f))) => vec![
                ("ativo".into(), a.to_string(), Tone::Destaque),
                (
                    "indústria".into(),
                    f.industria.clone().unwrap_or_else(|| "—".into()),
                    Tone::Dim,
                ),
                (
                    "lucro por ação".into(),
                    f.lpa.map(calc::preco).unwrap_or_else(|| "—".into()),
                    Tone::Normal,
                ),
                (
                    "EBITDA".into(),
                    f.ebitda.map(grande).unwrap_or_else(|| "—".into()),
                    Tone::Normal,
                ),
                (
                    "dívida total · caixa".into(),
                    match (f.divida_total, f.caixa) {
                        (Some(d), Some(c)) => format!("{} · {}", grande(d), grande(c)),
                        _ => "—".into(),
                    },
                    Tone::Normal,
                ),
                (
                    "liquidez corrente".into(),
                    f.liquidez_corrente
                        .map(|v| format!("{v:.2}").replace('.', ","))
                        .unwrap_or_else(|| "—".into()),
                    match f.liquidez_corrente {
                        Some(l) if l < 1.0 => Tone::Aviso,
                        _ => Tone::Normal,
                    },
                ),
                (
                    "ações emitidas".into(),
                    f.acoes_emitidas.map(grande).unwrap_or_else(|| "—".into()),
                    Tone::Dim,
                ),
            ],
            Some((a, outro)) => vec![(
                a.short().to_string(),
                crate::invest::fundamento::explicar(outro, a).unwrap_or_default(),
                Tone::Aviso,
            )],
            None => Vec::new(),
        };

        let pendentes = ctx.fundamentos.pendentes();
        Layout::rows(vec![
            (
                3,
                Layout::one(Pane::Table {
                    title: format!(
                        "Fundamentos · {}{}",
                        calc::plural(empresas.len(), "empresa", "empresas"),
                        match pendentes {
                            0 => String::new(),
                            n => format!(" · buscando {n}"),
                        }
                    ),
                    headers: vec![
                        "Ativo".into(),
                        "Setor".into(),
                        "P/L".into(),
                        "Margem".into(),
                        "Dív. líq./EBITDA".into(),
                        "Valor de mercado".into(),
                    ],
                    rows,
                    selected: Some(self.lista.selecionado),
                    query: self.lista.busca.clone(),
                    note: Some((
                        "P/L de empresa com prejuízo não é desenhado — é uma divisão sem significado"
                            .into(),
                        Tone::Aviso,
                    )),
                }),
            ),
            (2, Layout::one(Pane::Facts { title: "Detalhe".into(), rows: fatos })),
        ])
    }

    fn key(&mut self, key: KeyEvent, ctx: &Ctx) -> Outcome {
        match key.code {
            KeyCode::Enter => Outcome::Abrir {
                modulo: "grafico",
                alvo: empresas(ctx).get(self.lista.selecionado).cloned(),
            },
            _ => match self.lista.tecla(key, empresas(ctx).len()) {
                true => Outcome::Ok,
                false => Outcome::Ignorada,
            },
        }
    }

    fn escape(&mut self) -> Escape {
        self.lista.escape()
    }

    fn hint(&self) -> String {
        hint(&[
            "↑/↓ andar",
            "digite para buscar",
            "Enter gráfico",
            "Esc sair",
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeros_grandes_viram_palavras() {
        // Um valor de mercado com onze dígitos não se lê; ele só ocupa a coluna.
        assert_eq!(grande(637_284_320_565.0), "637,28 bi");
        assert_eq!(grande(1_229_707_100_000.0), "1,23 tri");
        assert_eq!(grande(53_764_000_000.0), "53,76 bi");
        assert_eq!(grande(1_500.0), "1,50 mil");
    }
}
