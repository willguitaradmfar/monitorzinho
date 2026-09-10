//! Lançamentos: o histórico. **Opcional, e nunca escreve no preço médio.**
//!
//! A relação com Posições é de mão única:
//!
//! ```text
//! Lançamentos  ──lê──▶  Posições      (para saber que ativos existem)
//! Lançamentos  ──✗──▶  preço médio    (nunca)
//! ```
//!
//! O motivo é o uso real: a carteira vem de várias origens, cada uma já traz o PM dela, e
//! o histórico que produziu esse PM em geral não vem junto. Um módulo de lançamentos com
//! histórico parcial que recalculasse o PM produziria um número errado e apagaria o certo.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::invest::calc;
use crate::invest::model::{AssetId, Lancamento, Moeda, TipoLancamento};
use crate::invest::module::{
    Ctx, Edit, Escape, Field, Group, InvestModule, Layout, Marcavel, ModuleView, Outcome, Pane,
    Row, Tone,
};
use crate::invest::modules::comum::{Formulario, Lista, hint};
use crate::invest::tempo::{self, Data};

pub struct Lancamentos;

impl InvestModule for Lancamentos {
    fn id(&self) -> &'static str {
        "lancamentos"
    }
    fn name(&self) -> &'static str {
        "Lançamentos"
    }
    fn description(&self) -> &'static str {
        "O que aconteceu ao longo do tempo — histórico, nunca preço médio"
    }
    fn destaque(&self) -> u8 {
        3
    }
    fn group(&self) -> Group {
        Group::Carteira
    }

    fn marcavel(&self) -> Option<Marcavel> {
        Some(crate::invest::marcas::ATIVOS)
    }
    fn keywords(&self) -> &'static str {
        "compra venda aporte retirada extrato livro-caixa operações histórico"
    }

    fn summary(&self, ctx: &Ctx) -> String {
        match ctx.portfolio.lancamentos.len() {
            0 => "nenhum registro — a decomposição do patrimônio fica incompleta".into(),
            n => format!("{n} registros"),
        }
    }

    fn widget(&self, ctx: &Ctx) -> Option<Pane> {
        if ctx.portfolio.lancamentos.is_empty() {
            return None;
        }
        let mut recentes: Vec<&Lancamento> = ctx.portfolio.lancamentos.iter().collect();
        recentes.sort_by_key(|l| std::cmp::Reverse(l.em));
        Some(Pane::Facts {
            title: String::new(),
            rows: recentes
                .iter()
                .take(8)
                .map(|l| {
                    (
                        format!(
                            "{} {}",
                            Data::de_epoch(l.em, tempo::BRT_OFFSET).longa(),
                            match l.tipo {
                                TipoLancamento::Compra => "compra",
                                TipoLancamento::Venda => "venda",
                                TipoLancamento::Provento => "provento",
                                TipoLancamento::Aporte => "aporte",
                                TipoLancamento::Retirada => "retirada",
                                _ => "evento",
                            }
                        ),
                        format!("R$ {}", calc::moeda(l.valor)),
                        match l.tipo {
                            TipoLancamento::Aporte => Tone::Bom,
                            TipoLancamento::Retirada => Tone::Ruim,
                            _ => Tone::Normal,
                        },
                    )
                })
                .collect(),
        })
    }

    fn open(&self, _ctx: &Ctx, _alvo: Option<&AssetId>) -> Box<dyn ModuleView> {
        Box::new(Vista {
            lista: Lista::default(),
            form: None,
            confirmando: None,
        })
    }
}

struct Vista {
    lista: Lista,
    form: Option<Formulario>,
    confirmando: Option<usize>,
}

/// Lê `08/09/2026`, ou hoje quando vazio.
pub fn ler_data(texto: &str, agora: u64) -> Result<u64, String> {
    let texto = texto.trim();
    if texto.is_empty() {
        return Ok(agora);
    }
    let partes: Vec<&str> = texto.split('/').collect();
    if partes.len() != 3 {
        return Err("data no formato dd/mm/aaaa".into());
    }
    let (Ok(dia), Ok(mes), Ok(ano)) = (
        partes[0].parse::<u32>(),
        partes[1].parse::<u32>(),
        partes[2].parse::<i32>(),
    ) else {
        return Err("data no formato dd/mm/aaaa".into());
    };
    if !(1..=31).contains(&dia) || !(1..=12).contains(&mes) {
        return Err("dia ou mês fora da faixa".into());
    }
    Ok(Data { ano, mes, dia }.epoch_inicio(tempo::BRT_OFFSET))
}

fn ler_form(form: &Formulario, agora: u64) -> Result<Lancamento, String> {
    let tipo = TipoLancamento::ALL
        .iter()
        .find(|t| t.label() == form.valor("tipo"))
        .copied()
        .ok_or("tipo desconhecido")?;
    let em = ler_data(&form.valor("data"), agora)?;

    let precisa_ativo = matches!(
        tipo,
        TipoLancamento::Compra | TipoLancamento::Venda | TipoLancamento::Provento
    );
    let ativo = match form.valor("ativo").trim().is_empty() {
        true if precisa_ativo => return Err("este tipo precisa de um ativo".into()),
        true => None,
        false => Some(AssetId::parse(&form.valor("ativo"))?),
    };

    let numero = |campo: &str| -> f64 { calc::ler_numero(&form.valor(campo)).unwrap_or(0.0) };
    let (quantidade, preco) = match tipo {
        TipoLancamento::Compra | TipoLancamento::Venda => {
            let q = numero("quantidade");
            let p = numero("preço");
            if q <= 0.0 {
                return Err("quantidade tem que ser maior que zero".into());
            }
            if p <= 0.0 {
                return Err("preço tem que ser maior que zero".into());
            }
            (q, p)
        }
        _ => (0.0, 0.0),
    };
    let valor = match tipo {
        TipoLancamento::Compra | TipoLancamento::Venda => 0.0,
        _ => {
            let v = numero("valor");
            if v <= 0.0 {
                return Err("valor tem que ser maior que zero".into());
            }
            v
        }
    };

    Ok(Lancamento {
        em,
        tipo,
        ativo,
        quantidade,
        preco,
        taxas: numero("taxas"),
        valor,
        moeda: Some(Moeda::Brl),
        nota: form.valor("nota"),
    })
}

/// As linhas da tabela, na mesma ordem de `portfolio.lancamentos` — é o que permite
/// resolver o cursor filtrado para o registro certo. Ver `Lista::atual`.
fn linhas(ctx: &Ctx) -> Vec<Row> {
    ctx.portfolio
        .lancamentos
        .iter()
        .map(|l| {
            let data = Data::de_epoch(l.em, tempo::BRT_OFFSET);
            let total = l.fluxo();
            Row::new(vec![
                data.longa(),
                l.tipo.label().to_string(),
                l.ativo
                    .as_ref()
                    .map(|a| a.short().to_string())
                    .unwrap_or("—".into()),
                match l.quantidade > 0.0 {
                    true => calc::preco(l.quantidade),
                    false => "—".into(),
                },
                match l.preco > 0.0 {
                    true => calc::preco(l.preco),
                    false => "—".into(),
                },
                match l.taxas > 0.0 {
                    true => calc::moeda(l.taxas),
                    false => "—".into(),
                },
                calc::moeda(total.abs()),
            ])
            .with_cell_tones(vec![
                Tone::Dim,
                Tone::Normal,
                Tone::Normal,
                Tone::Dim,
                Tone::Dim,
                Tone::Dim,
                match total >= 0.0 {
                    true => Tone::Bom,
                    false => Tone::Ruim,
                },
            ])
        })
        .collect()
}

impl ModuleView for Vista {
    fn title(&self) -> String {
        "Lançamentos".into()
    }

    fn layout(&self, ctx: &Ctx) -> Layout {
        if let Some(form) = &self.form {
            return Layout::one(Pane::Form {
                title: form.titulo.clone(),
                fields: form.campos.clone(),
                selected: form.selecionado,
                error: form.erro.clone(),
                hint: hint(&["↑/↓ campo", "←/→ tipo", "Enter gravar", "Esc cancelar"]),
            });
        }

        let rows = linhas(ctx);

        if rows.is_empty() {
            return Layout::one(Pane::Empty {
                title: "Lançamentos".into(),
                note: "Nenhum registro ainda.\n\n\
                       Ctrl+A adiciona um. Isto é histórico: ele alimenta a decomposição \n\
                       do Patrimônio, os Proventos e a estimativa de imposto.\n\n\
                       O que ele NÃO faz: preço médio não é calculado daqui. Ele é \n\
                       informado em Posições, e nada neste módulo o toca."
                    .into(),
            });
        }

        // O aviso é fixo e não sai nunca. Esta é a tela onde a expectativa contrária
        // nasce, e é aqui que ele precisa estar.
        let mut nota =
            "preço médio não é calculado daqui — ele é informado em Posições".to_string();
        if let Some(i) = self.confirmando
            && let Some(l) = ctx.portfolio.lancamentos.get(i)
        {
            nota = format!(
                "apagar o {} de {}? Enter confirma, Esc desiste",
                l.tipo.label(),
                Data::de_epoch(l.em, tempo::BRT_OFFSET).longa()
            );
        }

        Layout::one(Pane::Table {
            title: format!(
                "Lançamentos · {}",
                calc::plural(rows.len(), "registro", "registros")
            ),
            headers: vec![
                "Data".into(),
                "Tipo".into(),
                "Ativo".into(),
                "Qtd".into(),
                "Preço".into(),
                "Taxas".into(),
                "Total".into(),
            ],
            rows,
            selected: Some(self.lista.selecionado),
            query: self.lista.busca.clone(),
            note: Some((nota, Tone::Aviso)),
        })
    }

    fn key(&mut self, key: KeyEvent, ctx: &Ctx) -> Outcome {
        if let Some(form) = &mut self.form {
            if form.tecla(key) {
                match ler_form(form, ctx.agora) {
                    Ok(l) => {
                        self.form = None;
                        return Outcome::Editar(vec![Edit::AddLancamento(Box::new(l))]);
                    }
                    Err(e) => form.erro = Some(e),
                }
            }
            return Outcome::Ok;
        }

        if let Some(i) = self.confirmando.take() {
            return match key.code {
                KeyCode::Enter => Outcome::Editar(vec![Edit::RemoverLancamento(i)]),
                _ => Outcome::Ok,
            };
        }

        match key.code {
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.form = Some(Formulario::novo(
                    "Novo lançamento",
                    vec![
                        Field::choice(
                            "tipo",
                            TipoLancamento::Compra.label(),
                            TipoLancamento::ALL
                                .iter()
                                .map(|t| t.label().to_string())
                                .collect(),
                            "←/→ escolhe. Compra e venda pedem ativo, quantidade e preço",
                        ),
                        Field::text("data", "", "dd/mm/aaaa. Vazio é hoje"),
                        Field::text("ativo", "", "B3/PETR4 — só para compra, venda e provento"),
                        Field::text("quantidade", "", "Quantas unidades"),
                        Field::text("preço", "", "Preço unitário da operação"),
                        Field::text("taxas", "", "Corretagem e emolumentos, se houver"),
                        Field::text("valor", "", "Para aporte, retirada e provento"),
                        Field::text("nota", "", "O que você quiser lembrar depois"),
                    ],
                ));
                Outcome::Ok
            }
            KeyCode::Delete => {
                let rows_len = ctx.portfolio.lancamentos.len();
                self.confirmando =
                    (self.lista.selecionado < rows_len).then_some(self.lista.selecionado);
                Outcome::Ok
            }
            _ => match self.lista.tecla(key, ctx.portfolio.lancamentos.len()) {
                true => Outcome::Ok,
                false => Outcome::Ignorada,
            },
        }
    }

    fn escape(&mut self) -> Escape {
        if self.form.take().is_some() || self.confirmando.take().is_some() {
            return Escape::Consumido;
        }
        self.lista.escape()
    }

    fn hint(&self) -> String {
        hint(&["↑/↓ andar", "Ctrl+A adicionar", "Del apagar", "Esc sair"])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_vazia_e_hoje_e_data_torta_e_recusada() {
        assert_eq!(ler_data("", 12345).unwrap(), 12345);
        assert!(ler_data("08/09/2026", 0).is_ok());
        assert!(ler_data("32/09/2026", 0).is_err());
        assert!(ler_data("08-09-2026", 0).is_err());
        assert!(ler_data("oi", 0).is_err());
    }

    #[test]
    fn fluxo_de_compra_e_negativo_e_de_venda_positivo() {
        let compra = Lancamento {
            em: 0,
            tipo: TipoLancamento::Compra,
            ativo: None,
            quantidade: 100.0,
            preco: 38.0,
            taxas: 4.9,
            valor: 0.0,
            moeda: None,
            nota: String::new(),
        };
        assert!((compra.fluxo() + 3804.9).abs() < 1e-9);
        let mut venda = compra.clone();
        venda.tipo = TipoLancamento::Venda;
        assert!((venda.fluxo() - 3795.1).abs() < 1e-9);
    }
}
