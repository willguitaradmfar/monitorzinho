//! Proventos: quanto a carteira paga, quando paga, e o quanto isso rende sobre o que se
//! de fato gastou.
//!
//! O *dividend yield* que se lê em qualquer lugar é sobre o preço de hoje. O número que
//! importa para quem já é dono é sobre o **preço médio** — o que se pagou. São dois
//! números certos respondendo perguntas diferentes, e este módulo mostra os dois,
//! nomeados.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::invest::calc;
use crate::invest::model::{AssetId, Provento, TipoProvento};
use crate::invest::module::{
    Ctx, Edit, Escape, Field, Group, InvestModule, Layout, Marcavel, ModuleView, Outcome, Pane,
    Row, Tone,
};
use crate::invest::modules::comum::{Formulario, Lista, hint};
use crate::invest::tempo::{self, Data};

pub struct Proventos;

const ANO: u64 = 365 * 86400;

impl InvestModule for Proventos {
    fn id(&self) -> &'static str {
        "proventos"
    }
    fn name(&self) -> &'static str {
        "Proventos"
    }
    fn description(&self) -> &'static str {
        "Dividendos, JCP e rendimentos — recebidos, agendados, e o yield on cost"
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
        "dividendo jcp rendimento yield renda passiva amortização"
    }

    fn summary(&self, ctx: &Ctx) -> String {
        let doze = recebidos_12m(ctx);
        match ctx.portfolio.proventos.is_empty() {
            true => "nenhum registrado".into(),
            false => format!("12 meses: R$ {}", calc::moeda(doze)),
        }
    }

    fn widget(&self, ctx: &Ctx) -> Option<Pane> {
        // Sem nada registrado, o cartão mostra o que as empresas **anunciaram** — que é o
        // que existe para mostrar, e é o que convida a registrar. Um cartão vazio numa
        // carteira que recebe dividendo todo mês seria o painel escondendo informação que
        // ele tem.
        if ctx.portfolio.proventos.is_empty() {
            let lista = nao_registrados(ctx);
            if lista.is_empty() {
                return None;
            }
            return Some(Pane::Facts {
                title: String::new(),
                rows: lista
                    .iter()
                    .take(7)
                    .map(|a| {
                        let q = quantidade_de(ctx, &a.ativo);
                        (
                            format!(
                                "{:02}/{:02} {}",
                                Data::de_epoch(a.em, tempo::BRT_OFFSET).dia,
                                Data::de_epoch(a.em, tempo::BRT_OFFSET).mes,
                                a.ativo.short()
                            ),
                            format!(
                                "~R$ {} anunciado",
                                calc::moeda(crate::invest::provento::estimado(a, q))
                            ),
                            Tone::Dim,
                        )
                    })
                    .collect(),
            });
        }

        // Os últimos recebidos, do mais recente para o mais antigo — é a pergunta que se
        // faz olhando de passagem: «entrou alguma coisa?».
        let mut recentes: Vec<&Provento> = ctx.portfolio.proventos.iter().collect();
        recentes.sort_by_key(|p| std::cmp::Reverse(p.pago_em));
        let mut rows: Vec<(String, String, Tone)> = recentes
            .iter()
            .take(6)
            .map(|p| {
                (
                    format!(
                        "{} {}",
                        Data::de_epoch(p.pago_em, tempo::BRT_OFFSET).longa(),
                        p.ativo.short()
                    ),
                    format!("R$ {}", calc::moeda(p.bruto)),
                    Tone::Normal,
                )
            })
            .collect();
        rows.push((
            "12 meses".into(),
            format!("R$ {}", calc::moeda(recebidos_12m(ctx))),
            Tone::Destaque,
        ));
        // Quantos anúncios ainda esperam confirmação: é a única coisa acionável aqui.
        let pendentes = nao_registrados(ctx).len();
        if pendentes > 0 {
            rows.push((
                "anunciados".into(),
                format!("{pendentes} a registrar"),
                Tone::Aviso,
            ));
        }
        Some(Pane::Facts {
            title: String::new(),
            rows,
        })
    }

    fn open(&self, ctx: &Ctx, _alvo: Option<&AssetId>) -> Box<dyn ModuleView> {
        Box::new(Vista {
            lista: Lista::default(),
            form: None,
            por_ativo: false,
            // Numa carteira sem provento registrado, a lista útil é a dos anúncios —
            // entrar direto no que está vazio é entrar no lugar errado.
            aba: match ctx.portfolio.proventos.is_empty() {
                true => Aba::Anunciados,
                false => Aba::Recebidos,
            },
        })
    }
}

fn recebidos_12m(ctx: &Ctx) -> f64 {
    ctx.portfolio
        .proventos
        .iter()
        .filter(|p| p.pago_em <= ctx.agora && ctx.agora.saturating_sub(p.pago_em) <= ANO)
        .map(|p| p.liquido())
        .sum()
}

/// Yield on cost de um ativo: proventos de 12 meses sobre `quantidade × preço médio`.
///
/// `None` quando não há posição — e nunca estimado. Se o PM veio de uma importação
/// parcial, o número herda o erro, e a tela mostra a origem do PM.
pub fn yield_on_cost(ctx: &Ctx, ativo: &AssetId) -> Option<f64> {
    // Yield **on cost** precisa de custo. Sem preço médio informado, o número não
    // existe — e mostrá-lo sobre custo zero daria um yield infinito.
    let posicoes: Vec<&crate::invest::model::Position> = ctx
        .portfolio
        .posicoes
        .iter()
        .filter(|p| p.ativo == *ativo)
        .collect();
    if posicoes.is_empty() || posicoes.iter().any(|p| p.custo().is_none()) {
        return None;
    }
    let custo: f64 = posicoes.iter().filter_map(|p| p.custo()).sum();
    if custo <= 0.0 {
        return None;
    }
    let recebido: f64 = ctx
        .portfolio
        .proventos
        .iter()
        .filter(|p| p.ativo == *ativo && p.pago_em <= ctx.agora)
        .filter(|p| ctx.agora.saturating_sub(p.pago_em) <= ANO)
        .map(|p| p.liquido())
        .sum();
    Some(recebido / custo * 100.0)
}

/// Qual das duas listas está sob o cursor.
///
/// Duas listas na mesma tela precisam de **uma** que responda às setas, senão a navegação
/// não funciona em nenhuma. `Tab` alterna, que é como o módulo de Notícias já faz.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Aba {
    Recebidos,
    Anunciados,
}

struct Vista {
    lista: Lista,
    form: Option<Formulario>,
    por_ativo: bool,
    aba: Aba,
}

impl Vista {
    /// O que as empresas anunciaram e ainda não foi registrado.
    ///
    /// **Sugestão, e a tela diz isso.** O valor mostrado usa a quantidade de hoje, que não
    /// é necessariamente a que se tinha na data-com — quem comprou depois não recebeu
    /// nada, e quem tinha o dobro recebeu o dobro. Por isso `Ctrl+I` **abre o formulário
    /// preenchido** em vez de gravar direto: o número passa por quem sabe.
    fn anunciados(&self, ctx: &Ctx) -> Pane {
        let pendentes = ctx.anunciados.pendentes();
        let lista = nao_registrados(ctx);
        if lista.is_empty() {
            return Pane::Empty {
                title: "Anunciados".into(),
                note: match pendentes {
                    0 => "Nada anunciado que não esteja registrado.\n\n\
                          A lista vem da fonte de mercado: ela sabe o que a empresa \n\
                          declarou por cota e quando, e não sabe quanto você tinha na \n\
                          data — por isso ela sugere, e você confirma."
                        .into(),
                    n => format!("Buscando os anúncios de {n} papéis…"),
                },
            };
        }
        Pane::Table {
            // A lista ativa se anuncia no título. Duas listas na tela e um cursor só: sem
            // dizer qual manda, as setas parecem não funcionar em uma delas.
            title: match self.aba {
                Aba::Anunciados => format!("▸ Anunciados · {} a registrar", lista.len()),
                Aba::Recebidos => format!("Anunciados · {} a registrar", lista.len()),
            },
            headers: vec![
                "Data-com".into(),
                "Ativo".into(),
                "Por cota".into(),
                "Estimado hoje".into(),
            ],
            rows: lista
                .iter()
                .take(40)
                .map(|a| {
                    let q = quantidade_de(ctx, &a.ativo);
                    Row::new(vec![
                        Data::de_epoch(a.em, tempo::BRT_OFFSET).longa(),
                        a.ativo.short().to_string(),
                        calc::preco(a.por_cota),
                        format!(
                            "R$ {}",
                            calc::moeda(crate::invest::provento::estimado(a, q))
                        ),
                    ])
                })
                .collect(),
            selected: match self.aba {
                Aba::Anunciados => Some(self.lista.selecionado.min(lista.len().saturating_sub(1))),
                Aba::Recebidos => None,
            },
            query: String::new(),
            note: Some(match self.aba {
                Aba::Anunciados => (
                    "estimado com a quantidade de hoje · Enter abre o formulário preenchido".into(),
                    Tone::Aviso,
                ),
                Aba::Recebidos => ("Tab para navegar aqui".to_string(), Tone::Dim),
            }),
        }
    }
}

/// As linhas da visão «recebidos», em ordem cronológica inversa — a mesma ordem que o
/// cursor indexa. Ver `Lista::atual`.
/// Os proventos anunciados que **ainda não estão registrados**, do mais recente para o
/// mais antigo.
///
/// «Já registrado» é por ativo e dia: se existe um `Provento` do mesmo papel na mesma
/// data, o anúncio some da lista de sugestões. Sem isso, confirmar um deixaria a
/// sugestão na tela e convidaria a registrar duas vezes.
fn nao_registrados(ctx: &Ctx) -> Vec<crate::invest::provento::Anunciado> {
    let ativos: Vec<AssetId> = ctx
        .portfolio
        .posicoes
        .iter()
        .map(|p| p.ativo.clone())
        .collect();
    ctx.anunciados
        .get(ctx.providers, &ativos)
        .into_iter()
        .filter(|a| {
            !ctx.portfolio.proventos.iter().any(|p| {
                p.ativo == a.ativo
                    && Data::de_epoch(p.pago_em, tempo::BRT_OFFSET)
                        == Data::de_epoch(a.em, tempo::BRT_OFFSET)
            })
        })
        .collect()
}

/// A quantidade que se tem hoje de um ativo, somando as corretoras.
fn quantidade_de(ctx: &Ctx, ativo: &AssetId) -> f64 {
    ctx.portfolio
        .posicoes
        .iter()
        .filter(|p| p.ativo == *ativo)
        .map(|p| p.quantidade)
        .sum()
}

fn linhas_recebidos(ctx: &Ctx) -> Vec<Row> {
    let mut ordenados: Vec<&Provento> = ctx.portfolio.proventos.iter().collect();
    ordenados.sort_by_key(|p| std::cmp::Reverse(p.pago_em));
    ordenados
        .iter()
        .map(|p| {
            let futuro = p.pago_em > ctx.agora;
            Row::new(vec![
                Data::de_epoch(p.pago_em, tempo::BRT_OFFSET).longa(),
                p.ativo.short().to_string(),
                p.tipo.label().to_string(),
                calc::moeda(p.bruto),
                calc::moeda(p.retido),
                calc::moeda(p.liquido()),
                match futuro {
                    true => "agendado".into(),
                    false => String::new(),
                },
            ])
            .with_cell_tones(vec![
                Tone::Dim,
                Tone::Normal,
                Tone::Dim,
                Tone::Normal,
                match p.retido > 0.0 {
                    true => Tone::Ruim,
                    false => Tone::Dim,
                },
                Tone::Bom,
                Tone::Aviso,
            ])
        })
        .collect()
}

fn ler_form(form: &Formulario, agora: u64) -> Result<Provento, String> {
    let ativo = AssetId::parse(&form.valor("ativo"))?;
    let tipo = TipoProvento::ALL
        .iter()
        .find(|t| t.label() == form.valor("tipo"))
        .copied()
        .ok_or("tipo desconhecido")?;
    let bruto = calc::ler_numero(&form.valor("bruto")).ok_or("valor bruto não é um número")?;
    if bruto <= 0.0 {
        return Err("o valor bruto tem que ser maior que zero".into());
    }
    let retido = calc::ler_numero(&form.valor("retido")).unwrap_or(0.0);
    if retido > bruto {
        return Err("o retido não pode ser maior que o bruto".into());
    }
    let pago_em = super::lancamentos_data(&form.valor("data"), agora)?;
    Ok(Provento {
        ativo,
        tipo,
        pago_em,
        bruto,
        retido,
        moeda: crate::invest::model::Moeda::Brl,
        quantidade: calc::ler_numero(&form.valor("quantidade")),
    })
}

impl ModuleView for Vista {
    fn title(&self) -> String {
        "Proventos".into()
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
                hint: hint(&["↑/↓ campo", "←/→ tipo", "Enter gravar", "Esc cancelar"]),
            });
        }
        if ctx.portfolio.proventos.is_empty() {
            // Nada registrado é exatamente quando as sugestões servem para alguma coisa:
            // a tela mostra o que as empresas anunciaram, e a explicação ao lado.
            return Layout::rows(vec![
                (3, Layout::one(self.anunciados(ctx))),
                (
                    2,
                    Layout::one(Pane::Empty {
                        title: "Proventos".into(),
                        note: "Nada registrado ainda.\n\n\
                               A lista acima vem da fonte de mercado: ela sabe o que a \n\
                               empresa declarou por cota e quando. O que ela não sabe é \n\
                               quanto você tinha na data-com — e sem isso o valor recebido \n\
                               seria um palpite gravado como fato.\n\n\
                               Ctrl+I abre o formulário já preenchido com a estimativa; \n\
                               quem confirma é você. Ctrl+A adiciona um do zero.\n\n\
                               O tipo importa e não é cosmético: dividendo é isento, JCP \n\
                               tem 15% retido na fonte, e amortização é devolução de \n\
                               capital — que reduz o preço médio. O programa avisa; quem \n\
                               edita o PM é você."
                            .into(),
                    }),
                ),
            ]);
        }

        if self.por_ativo {
            let mut ativos: Vec<AssetId> = Vec::new();
            for p in &ctx.portfolio.proventos {
                if !ativos.contains(&p.ativo) {
                    ativos.push(p.ativo.clone());
                }
            }
            let rows: Vec<Row> = ativos
                .iter()
                .map(|a| {
                    let recebido: f64 = ctx
                        .portfolio
                        .proventos
                        .iter()
                        .filter(|p| p.ativo == *a && p.pago_em <= ctx.agora)
                        .map(|p| p.liquido())
                        .sum();
                    let yoc = yield_on_cost(ctx, a);
                    // O yield sobre o preço de hoje, ao lado. Os dois nomeados.
                    let atual = ctx.market.quote(a).and_then(|q| {
                        let doze: f64 = ctx
                            .portfolio
                            .proventos
                            .iter()
                            .filter(|p| p.ativo == *a && ctx.agora.saturating_sub(p.pago_em) <= ANO)
                            .map(|p| p.liquido())
                            .sum();
                        let qtd: f64 = ctx
                            .portfolio
                            .posicoes
                            .iter()
                            .filter(|p| p.ativo == *a)
                            .map(|p| p.quantidade)
                            .sum();
                        (qtd > 0.0 && q.preco > 0.0).then(|| doze / (qtd * q.preco) * 100.0)
                    });
                    Row::new(vec![
                        a.short().to_string(),
                        calc::moeda(recebido),
                        yoc.map(calc::pct_simples).unwrap_or("sem posição".into()),
                        atual.map(calc::pct_simples).unwrap_or("—".into()),
                    ])
                    .with_cell_tones(vec![
                        Tone::Normal,
                        Tone::Normal,
                        Tone::Bom,
                        Tone::Dim,
                    ])
                })
                .collect();
            return Layout::one(Pane::Table {
                title: "Proventos · por ativo".into(),
                headers: vec![
                    "Ativo".into(),
                    "Recebido".into(),
                    "Yield on cost (12m)".into(),
                    "Yield atual (12m)".into(),
                ],
                rows,
                selected: Some(self.lista.selecionado),
                query: self.lista.busca.clone(),
                note: Some((
                    "yield on cost é sobre o que você pagou; yield atual é sobre o preço de hoje"
                        .into(),
                    Tone::Aviso,
                )),
            });
        }

        let rows = linhas_recebidos(ctx);

        let doze = recebidos_12m(ctx);
        // Só a carteira inteira com custo conhecido dá um yield on cost da carteira: com
        // metade das posições sem preço médio, o número seria grande e falso.
        let todas_com_custo = ctx.portfolio.posicoes.iter().all(|p| p.custo().is_some());
        let custo_total: f64 = ctx
            .portfolio
            .posicoes
            .iter()
            .filter_map(|p| p.custo())
            .sum();
        let yoc_carteira =
            (todas_com_custo && custo_total > 0.0).then(|| doze / custo_total * 100.0);

        Layout::rows(vec![
            (
                4,
                Layout::one(Pane::Table {
                    title: match self.aba {
                        Aba::Recebidos => format!(
                            "▸ Proventos · {}",
                            calc::plural(rows.len(), "registro", "registros")
                        ),
                        Aba::Anunciados => format!(
                            "Proventos · {}",
                            calc::plural(rows.len(), "registro", "registros")
                        ),
                    },
                    headers: vec![
                        "Data".into(),
                        "Ativo".into(),
                        "Tipo".into(),
                        "Bruto".into(),
                        "Retido".into(),
                        "Líquido".into(),
                        String::new(),
                    ],
                    rows,
                    selected: match self.aba {
                        Aba::Recebidos => Some(self.lista.selecionado),
                        Aba::Anunciados => None,
                    },
                    query: self.lista.busca.clone(),
                    note: match self.aba {
                        Aba::Recebidos => None,
                        Aba::Anunciados => Some(("Tab para navegar aqui".into(), Tone::Dim)),
                    },
                }),
            ),
            (
                2,
                Layout::cols(vec![
                    (1, Layout::one(self.anunciados(ctx))),
                    (
                        1,
                        Layout::one(Pane::Facts {
                            title: "12 meses".into(),
                            rows: vec![
                        (
                            "recebido (líquido)".into(),
                            format!("R$ {}", calc::moeda(doze)),
                            Tone::Bom,
                        ),
                        (
                            "yield on cost da carteira".into(),
                            yoc_carteira.map(calc::pct_simples).unwrap_or("—".into()),
                            Tone::Destaque,
                        ),
                        (
                            "sobre o quê".into(),
                            "sobre o preço médio informado — se ele veio errado, isto herda".into(),
                            Tone::Dim,
                        ),
                    ],
                        }),
                    ),
                ]),
            ),
        ])
    }

    fn key(&mut self, key: KeyEvent, ctx: &Ctx) -> Outcome {
        if let Some(form) = &mut self.form {
            if form.tecla(key) {
                match ler_form(form, ctx.agora) {
                    Ok(p) => {
                        self.form = None;
                        return Outcome::Editar(vec![Edit::AddProvento(Box::new(p))]);
                    }
                    Err(e) => form.erro = Some(e),
                }
            }
            return Outcome::Ok;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('a') if ctrl => {
                self.form = Some(Formulario::novo(
                    "Novo provento",
                    vec![
                        Field::text("ativo", "", "B3/HGLG11"),
                        Field::choice(
                            "tipo",
                            TipoProvento::Dividendo.label(),
                            TipoProvento::ALL
                                .iter()
                                .map(|t| t.label().to_string())
                                .collect(),
                            "JCP tem 15% retido; amortização reduz o preço médio",
                        ),
                        Field::text("data", "", "dd/mm/aaaa do pagamento. Vazio é hoje"),
                        Field::text("bruto", "", "Valor total recebido, antes do imposto"),
                        Field::text("retido", "", "Imposto retido na fonte, se houve"),
                        Field::text(
                            "quantidade",
                            "",
                            "Quanto você tinha na data. Vazio = não sei",
                        ),
                    ],
                ));
                Outcome::Ok
            }
            // `Tab` alterna a lista sob o cursor. **Não** `Ctrl+I`: num terminal essa
            // combinação *é* o Tab — as duas mandam o mesmo byte 9 —, então a tecla nunca
            // chegava aqui como `Char('i')` e a função parecia quebrada.
            KeyCode::Tab => {
                self.aba = match self.aba {
                    Aba::Recebidos => Aba::Anunciados,
                    Aba::Anunciados => Aba::Recebidos,
                };
                self.lista.selecionado = 0;
                Outcome::Ok
            }
            // O anúncio vira formulário preenchido, e não registro direto.
            //
            // O valor estimado usa a quantidade de **hoje**, que pode não ser a da
            // data-com: quem comprou depois não recebeu nada. Gravar direto poria um
            // número inventado na carteira com cara de extrato; abrir o formulário deixa
            // a conferência onde ela tem que estar, e ainda assim tira toda a digitação.
            KeyCode::Enter if self.aba == Aba::Anunciados => {
                let lista = nao_registrados(ctx);
                let Some(a) = lista.get(self.lista.selecionado.min(lista.len().saturating_sub(1)))
                else {
                    return Outcome::Ok;
                };
                let q = quantidade_de(ctx, &a.ativo);
                let d = Data::de_epoch(a.em, tempo::BRT_OFFSET);
                self.form = Some(Formulario::novo(
                    format!("Registrar provento de {}", a.ativo.short()),
                    vec![
                        Field::text("ativo", a.ativo.to_string(), "B3/HGLG11"),
                        Field::choice(
                            "tipo",
                            TipoProvento::Dividendo.label(),
                            TipoProvento::ALL
                                .iter()
                                .map(|t| t.label().to_string())
                                .collect(),
                            "a fonte não diz se foi dividendo ou JCP — confira no extrato",
                        ),
                        Field::text("data", d.longa(), "a data-com anunciada"),
                        Field::text(
                            "bruto",
                            calc::moeda(crate::invest::provento::estimado(a, q)),
                            "estimado com a quantidade de hoje — corrija se ela mudou",
                        ),
                        Field::text("retido", "", "Imposto retido na fonte, se houve"),
                        Field::text(
                            "quantidade",
                            calc::moeda(q),
                            "Quanto você tinha na data. Vazio = não sei",
                        ),
                    ],
                ));
                Outcome::Ok
            }
            KeyCode::Char('t') if ctrl => {
                self.por_ativo = !self.por_ativo;
                self.lista.selecionado = 0;
                Outcome::Ok
            }
            KeyCode::Delete if !self.por_ativo => {
                // Dois mapeamentos em sequência, e os dois são necessários: o cursor é da
                // lista **filtrada**, a tela mostra em ordem **inversa**, e o índice do
                // arquivo é um terceiro. Pular qualquer um apaga o provento errado.
                let mut ordenados: Vec<(usize, &Provento)> =
                    ctx.portfolio.proventos.iter().enumerate().collect();
                ordenados.sort_by_key(|(_, p)| std::cmp::Reverse(p.pago_em));
                if self.aba != Aba::Recebidos {
                    return Outcome::Ok;
                }
                let rows = linhas_recebidos(ctx);
                match self.lista.atual(&rows).and_then(|v| ordenados.get(v)) {
                    Some((i, _)) => Outcome::Editar(vec![Edit::RemoverProvento(*i)]),
                    None => Outcome::Ok,
                }
            }
            // As setas andam na lista **que está sob o cursor**, e não sempre na de
            // registrados: numa carteira sem provento nenhum aquela lista tem zero
            // linhas, e a navegação inteira parecia morta.
            _ => {
                let total = match self.aba {
                    Aba::Recebidos => linhas_recebidos(ctx).len(),
                    Aba::Anunciados => nao_registrados(ctx).len(),
                };
                match self.lista.tecla(key, total) {
                    true => Outcome::Ok,
                    false => Outcome::Ignorada,
                }
            }
        }
    }

    fn escape(&mut self) -> Escape {
        if self.form.take().is_some() {
            return Escape::Consumido;
        }
        self.lista.escape()
    }

    fn hint(&self) -> String {
        hint(&[
            "↑/↓ andar",
            "Tab troca de lista",
            "Enter registra o anúncio",
            "Ctrl+A adicionar",
            "Ctrl+T por ativo",
            "Del apagar",
            "Esc sair",
        ])
    }
}
