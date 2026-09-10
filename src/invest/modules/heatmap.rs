//! Heatmap: o mercado num olhar.
//!
//! Funciona bem num terminal porque um heatmap **é** uma grade de retângulos coloridos com
//! um rótulo dentro, e uma tela de terminal é exatamente isso. Não há nada a aproximar
//! aqui — diferente do candle, que é uma forma que o terminal não tem.
//!
//! A escala de cor é **fixa e escrita na legenda**, e não relativa ao dia. Escala relativa
//! é o erro comum: num dia calmo ela pinta de vermelho forte uma queda de 0,3%, e num
//! crash pinta de amarelo uma queda de 6%. A cor tem que significar a mesma coisa todo
//! dia, senão não ensina nada.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::invest::calc;
use crate::invest::carteira;
use crate::invest::model::AssetId;
use crate::invest::module::{
    Cell, Ctx, Escape, Group, InvestModule, Layout, Marcavel, ModuleView, Need, Outcome, Pane, Tone,
};
use crate::invest::modules::comum::hint;

pub struct Heatmap;

/// Os degraus da escala, em porcento. Fixos, e desenhados na legenda.
const DEGRAUS: [f64; 4] = [0.5, 1.0, 2.0, 5.0];

/// A cor de uma variação. Preço informado ou não-oficial fica cinza — mesma regra de
/// Cotações: colorir daria a ele a autoridade visual de um preço ao vivo.
pub fn tom(variacao: Option<f64>) -> Tone {
    match variacao {
        None => Tone::Dim,
        Some(v) if v >= DEGRAUS[0] => Tone::Bom,
        Some(v) if v <= -DEGRAUS[0] => Tone::Ruim,
        Some(_) => Tone::Normal,
    }
}

impl InvestModule for Heatmap {
    fn id(&self) -> &'static str {
        "heatmap"
    }
    fn name(&self) -> &'static str {
        "Heatmap"
    }
    fn description(&self) -> &'static str {
        "A grade colorida por variação, com o tamanho pelo peso na carteira"
    }
    fn destaque(&self) -> u8 {
        4
    }
    fn group(&self) -> Group {
        Group::Mercado
    }

    fn fonte_externa(&self) -> Option<&'static str> {
        Some(crate::invest::store::fonte::COTACAO)
    }

    /// Divide a lista de marcas dos ativos: cada célula é um papel — a marca acende a moldura dela.
    fn marcavel(&self) -> Option<Marcavel> {
        Some(crate::invest::marcas::ATIVOS)
    }
    fn needs(&self) -> &'static [Need] {
        &[Need::Cotacao]
    }
    fn keywords(&self) -> &'static str {
        "mapa calor grade cores visão geral setor"
    }

    fn summary(&self, ctx: &Ctx) -> String {
        let n = ctx.portfolio.posicoes.len();
        match n {
            0 => "sem posições".into(),
            _ => format!("{n} ativos num olhar"),
        }
    }

    fn widget(&self, ctx: &Ctx) -> Option<Pane> {
        if ctx.portfolio.posicoes.is_empty() {
            return None;
        }
        let linhas = carteira::linhas(ctx.portfolio, ctx.market, ctx.agora);
        Some(Pane::Grid {
            title: String::new(),
            cells: linhas
                .iter()
                .take(12)
                .map(|l| Cell {
                    label: l.posicao.ativo.short().to_string(),
                    sub: l.variacao_pct.map(calc::pct).unwrap_or_else(|| "—".into()),
                    weight: l.mercado_brl.unwrap_or(0.0).max(1.0),
                    tone: tom(l.variacao_pct),
                })
                .collect(),
            selected: None,
            legend: String::new(),
        })
    }

    fn open(&self, _ctx: &Ctx, _alvo: Option<&AssetId>) -> Box<dyn ModuleView> {
        Box::new(Vista {
            selecionado: 0,
            por_peso: true,
        })
    }
}

struct Vista {
    selecionado: usize,
    por_peso: bool,
}

impl ModuleView for Vista {
    fn title(&self) -> String {
        "Heatmap".into()
    }

    fn quer_mercado(&self) -> bool {
        true
    }

    fn layout(&self, ctx: &Ctx) -> Layout {
        let linhas = carteira::linhas(ctx.portfolio, ctx.market, ctx.agora);
        if linhas.is_empty() {
            return Layout::one(Pane::Empty {
                title: "Heatmap".into(),
                note: "Sem posições não há mapa. Comece por Posições.".into(),
            });
        }
        let totais = carteira::totais(&linhas);

        let cells: Vec<Cell> = linhas
            .iter()
            .map(|l| {
                let peso = carteira::peso(l, &totais).unwrap_or(0.0);
                Cell {
                    label: l.posicao.ativo.short().to_string(),
                    sub: match l.variacao_pct {
                        Some(v) => calc::pct(v),
                        None => l
                            .grade
                            .map(|g| match g.marca().is_empty() {
                                true => "—".to_string(),
                                false => g.marca().to_string(),
                            })
                            .unwrap_or("—".into()),
                    },
                    weight: match self.por_peso {
                        true => (peso / 10.0).max(0.4),
                        false => 1.0,
                    },
                    tone: tom(l.variacao_pct),
                }
            })
            .collect();

        let legenda = format!(
            "−{}% ▓  −{}% ▒  0  +{}% ▒  +{}% ▓   ·   cinza = preço não ao vivo   ·   tamanho: {}",
            DEGRAUS[3],
            DEGRAUS[1],
            DEGRAUS[1],
            DEGRAUS[3],
            match self.por_peso {
                true => "peso na carteira",
                false => "igual",
            }
        );

        Layout::one(Pane::Grid {
            title: format!("Heatmap · {}", calc::plural(cells.len(), "ativo", "ativos")),
            cells,
            selected: Some(self.selecionado),
            legend: legenda,
        })
    }

    fn key(&mut self, key: KeyEvent, ctx: &Ctx) -> Outcome {
        let n = ctx.portfolio.posicoes.len();
        match key.code {
            KeyCode::Right | KeyCode::Down if n > 0 => {
                self.selecionado = (self.selecionado + 1) % n;
                Outcome::Ok
            }
            KeyCode::Left | KeyCode::Up if n > 0 => {
                self.selecionado = (self.selecionado + n - 1) % n;
                Outcome::Ok
            }
            KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.por_peso = !self.por_peso;
                Outcome::Ok
            }
            KeyCode::Enter => Outcome::Abrir {
                modulo: "grafico",
                alvo: ctx
                    .portfolio
                    .posicoes
                    .get(self.selecionado)
                    .map(|p| p.ativo.clone()),
            },
            _ => Outcome::Ignorada,
        }
    }

    fn escape(&mut self) -> Escape {
        Escape::Nao
    }

    fn hint(&self) -> String {
        hint(&["setas andar", "Ctrl+W tamanho", "Enter gráfico", "Esc sair"])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_escala_e_fixa_e_nao_relativa_ao_dia() {
        // A mesma variação tem que dar a mesma cor num dia calmo e num de crash.
        assert_eq!(tom(Some(2.0)), Tone::Bom);
        assert_eq!(tom(Some(-2.0)), Tone::Ruim);
        assert_eq!(
            tom(Some(0.1)),
            Tone::Normal,
            "abaixo do primeiro degrau é neutro"
        );
    }

    #[test]
    fn preco_nao_ao_vivo_fica_cinza() {
        assert_eq!(tom(None), Tone::Dim);
    }
}
