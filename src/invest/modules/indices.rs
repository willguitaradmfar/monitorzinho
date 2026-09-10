//! Índices e macro — e o módulo em que a decisão de «só fontes sem chave» mais dói.
//!
//! Juro, inflação, câmbio e cripto respondem ao vivo. IBOV, S&P, DXY, VIX e treasury
//! **não têm fonte pública utilizável**, e por isso são informados aqui.
//!
//! Isso está escrito na própria tela de propósito: este é o módulo com maior chance de
//! motivar a ligação de um provedor com chave, e essa decisão deve ser tomada olhando o
//! que se ganha e o que se aceita — não por atrito acumulado.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::invest::calc;
use crate::invest::model::{AssetId, Market};
use crate::invest::module::{
    Ctx, Edit, Escape, Field, Group, InvestModule, Layout, Marcavel, ModuleView, Outcome, Pane,
    Row, TipoDeMarca, Tone,
};
use crate::invest::modules::comum::{Formulario, Lista, hint, sem_preco};

pub struct Indices;

/// Os indicadores que valem a pena mostrar, e de onde cada um vem hoje.
///
/// As séries do Banco Central **não** estão escritas aqui: elas vêm do catálogo do
/// provedor, em `providers::bcb::SERIES`. Duas listas seriam duas chances de discordarem,
/// e foi para cá que a tela de Renda fixa se mudou — ela existia só para mostrar essas
/// séries, e mostrar juro num módulo e câmbio noutro separava coisas que se leem juntas.
const MACRO: &[(&str, Market, &str)] = &[
    ("USDBRL", Market::Fx, "USD/BRL"),
    ("EURBRL", Market::Fx, "EUR/BRL"),
    ("BTCBRL", Market::Binance, "Bitcoin (BRL)"),
    // O Ibovespa **ganhou fonte**: a API aberta da Kinvo o serve, e o provedor `kinvo`
    // o cobre. Ele continua em `Market::Outro` porque é onde nasceu e mudar o mercado
    // mudaria a chave gravada na carteira de quem já o acompanha.
    ("IBOV", Market::Outro, "Ibovespa"),
    // Estes ainda não têm fonte sem chave. Ficam informados, e a tela diz isso.
    ("SPX", Market::Outro, "S&P 500"),
    ("DXY", Market::Outro, "Dólar (DXY)"),
    ("VIX", Market::Outro, "VIX"),
    ("UST10Y", Market::Outro, "Treasury 10 anos"),
];

/// Todos os indicadores da tela: o catálogo do Banco Central primeiro — juro e inflação
/// são o pano de fundo de tudo — e depois câmbio, cripto e os índices de bolsa.
fn indicadores() -> Vec<(AssetId, &'static str)> {
    crate::invest::providers::bcb::SERIES
        .iter()
        .map(|s| (AssetId::new(Market::Bcb, s.simbolo), s.nome))
        .chain(
            MACRO
                .iter()
                .map(|(simbolo, mercado, nome)| (AssetId::new(*mercado, *simbolo), *nome)),
        )
        .collect()
}

/// O que se pode seguir nesta lista. O id da tabela nunca muda depois de publicado — é
/// com ele que as marcas já gravadas se reconhecem.
static MARCAS: Marcavel = Marcavel {
    tabela: "invest-indices",
    nome: "Índices e macro",
    tipos: &[TipoDeMarca {
        nome: "indicador",
        coluna: "Indicador",
        numerico: false,
        ajuda: "o nome do indicador",
    }],
};

impl InvestModule for Indices {
    fn id(&self) -> &'static str {
        "indices"
    }
    fn name(&self) -> &'static str {
        "Índices e macro"
    }
    fn description(&self) -> &'static str {
        "O pano de fundo: juro, inflação, câmbio, bolsa e o medo"
    }
    fn destaque(&self) -> u8 {
        6
    }
    fn group(&self) -> Group {
        Group::Mercado
    }

    fn marcavel(&self) -> Option<Marcavel> {
        Some(MARCAS)
    }
    fn keywords(&self) -> &'static str {
        "ibovespa s&p nasdaq dxy vix treasury macro juro inflação"
    }

    fn summary(&self, ctx: &Ctx) -> String {
        let todos = indicadores();
        let vivos = todos
            .iter()
            .filter(|(a, _)| ctx.market.quote(a).is_some())
            .count();
        format!("{vivos} de {} ao vivo · o resto é informado", todos.len())
    }

    fn widget(&self, ctx: &Ctx) -> Option<Pane> {
        Some(Pane::Facts {
            title: String::new(),
            rows: indicadores()
                .into_iter()
                // Quem tem fonte de verdade. O Ibovespa entra por aqui desde que a Kinvo
                // passou a servi-lo, sem que esta linha precisasse saber o nome dele.
                .filter(|(ativo, _)| ctx.providers.quem_busca(ativo).is_some())
                .map(|(ativo, nome)| {
                    let mercado = &ativo.market;
                    let Some(q) = ctx.market.quote(&ativo) else {
                        return ((*nome).to_string(), sem_preco(ctx), Tone::Dim);
                    };
                    // Taxa do Banco Central sai com «%»; preço sai como preço. Sem isso
                    // «0,0517» ao lado de «5,09» parece a mesma unidade e não é.
                    let valor = match mercado {
                        Market::Bcb => {
                            calc::pct_casas(q.preco, if ativo.symbol == "CDI" { 4 } else { 2 })
                        }
                        _ => calc::preco(q.preco),
                    };
                    match (q.anterior, mercado) {
                        (Some(a), Market::Bcb) => (
                            (*nome).to_string(),
                            format!("{valor} ({})", calc::pp(q.preco - a)),
                            Tone::Normal,
                        ),
                        _ => match q.variacao() {
                            Some(v) => (
                                (*nome).to_string(),
                                format!("{valor} {}", calc::pct(v)),
                                crate::invest::modules::heatmap::tom(Some(v)),
                            ),
                            None => ((*nome).to_string(), valor, Tone::Normal),
                        },
                    }
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
        "Índices e macro".into()
    }

    fn quer_mercado(&self) -> bool {
        true
    }

    fn ativos(&self) -> Vec<AssetId> {
        // Todos, inclusive os de `Outro`: quem decide se há o que buscar é o despacho,
        // que só pergunta a quem cobre. Filtrar aqui por mercado era o que mantinha o
        // Ibovespa fora da busca mesmo depois de ele ganhar uma fonte.
        indicadores().into_iter().map(|(a, _)| a).collect()
    }

    fn layout(&self, ctx: &Ctx) -> Layout {
        if let Some(form) = &self.form {
            return Layout::one(Pane::Form {
                title: form.titulo.clone(),
                fields: form.campos.clone(),
                selected: form.selecionado,
                error: form.erro.clone(),
                hint: hint(&["Enter gravar", "Esc cancelar"]),
            });
        }

        let hoje =
            crate::invest::tempo::Data::de_epoch(ctx.agora, crate::invest::tempo::BRT_OFFSET);
        let mut vivos: Vec<Row> = Vec::new();
        let mut informados = Vec::new();
        for (ativo, nome) in indicadores() {
            let mercado = &ativo.market;
            // Um indicador que **tem fonte** e ainda não respondeu não é «informado»: ele
            // está a caminho. Misturar os dois faria a coluna da direita acusar de falta
            // de fonte um número que chega em dois segundos.
            //
            // Quem responde isso são os provedores, e não uma lista escrita aqui: no dia
            // em que uma fonte nova cobriu o Ibovespa, ele passou sozinho da tabela dos
            // informados para a dos ao vivo, sem que ninguém precisasse lembrar de mexer
            // nesta linha.
            let tem_fonte = ctx.providers.quem_busca(&ativo).is_some();
            match (ctx.market.quote(&ativo), tem_fonte) {
                (Some(q), true) => {
                    // Uma série do Banco Central é uma **taxa**: sem o «%» ao lado,
                    // «0,0517» não diz o que é. E taxa diária precisa de quatro casas.
                    let valor = match mercado {
                        Market::Bcb => {
                            calc::pct_casas(q.preco, if ativo.symbol == "CDI" { 4 } else { 2 })
                        }
                        _ => calc::preco(q.preco),
                    };
                    // A variação contra a **leitura anterior**, e na unidade certa: um
                    // preço varia em porcento, uma taxa varia em pontos percentuais.
                    // «O IPCA caiu de 0,16 para 0,07» é −0,09 p.p., não −56%.
                    let (variacao, tom) = match (q.anterior, mercado) {
                        (Some(a), Market::Bcb) => {
                            let d = q.preco - a;
                            (
                                format!("{} (de {})", calc::pp(d), calc::pct_casas(a, 2)),
                                match d {
                                    d if d > 0.0 => Tone::Bom,
                                    d if d < 0.0 => Tone::Ruim,
                                    _ => Tone::Dim,
                                },
                            )
                        }
                        (Some(_), _) => match q.variacao() {
                            Some(v) => (
                                calc::pct(v),
                                match v >= 0.0 {
                                    true => Tone::Bom,
                                    false => Tone::Ruim,
                                },
                            ),
                            None => (String::new(), Tone::Dim),
                        },
                        (None, _) => (String::new(), Tone::Dim),
                    };
                    let (proximo, exato) =
                        crate::invest::modules::agenda::proximo_anuncio(&ativo.symbol, &hoje)
                            .unwrap_or_default();
                    vivos.push(
                        Row::new(vec![
                            nome.to_string(),
                            valor,
                            variacao,
                            crate::invest::tempo::Data::de_epoch(
                                q.em,
                                crate::invest::tempo::BRT_OFFSET,
                            )
                            .longa(),
                            match (proximo.is_empty(), exato) {
                                (true, _) => String::new(),
                                // Uma janela de costume não pode parecer uma data marcada.
                                (false, false) => format!("~ {proximo}"),
                                (false, true) => proximo,
                            },
                        ])
                        .with_cell_tones(vec![
                            Tone::Normal,
                            Tone::Destaque,
                            tom,
                            Tone::Dim,
                            match exato {
                                true => Tone::Normal,
                                false => Tone::Dim,
                            },
                        ]),
                    );
                }
                (None, true) => vivos.push(Row::tinted(
                    vec![
                        nome.to_string(),
                        "—".into(),
                        String::new(),
                        "buscando…".into(),
                        String::new(),
                    ],
                    Tone::Dim,
                )),
                (Some(q), false) => informados.push((
                    nome.to_string(),
                    format!("{} informado", calc::preco(q.preco)),
                    Tone::Normal,
                )),
                (None, false) => {
                    informados.push((nome.to_string(), "— informe com Ctrl+A".into(), Tone::Dim))
                }
            }
        }

        Layout::rows(vec![
            (
                3,
                Layout::one(Pane::Table {
                    title: "Ao vivo".into(),
                    headers: vec![
                        "Indicador".into(),
                        "Agora".into(),
                        "Contra a leitura anterior".into(),
                        "Data".into(),
                        "Próximo anúncio".into(),
                    ],
                    rows: vivos,
                    selected: Some(self.lista.selecionado),
                    query: String::new(),
                    note: Some((
                        "«~» marca janela de costume, e não data publicada — só o COPOM tem calendário fechado"
                            .into(),
                        Tone::Aviso,
                    )),
                }),
            ),
            (
                1,
                Layout::one(Pane::Facts {
                    title: "Informados — sem fonte sem chave".into(),
                    rows: informados,
                }),
            ),
            (
                1,
                Layout::one(Pane::Empty {
                    title: "Por que metade está em branco".into(),
                    note: "Índice de bolsa é dado vendido. O que existe de graça ou pede \n\
                           cadastro, ou é raspado de página e quebra sem aviso — e a decisão \n\
                           desta versão foi não depender de fonte que quebra.\n\n\
                           Ctrl+A informa um valor à mão. Ele fica gravado, aparece com a \n\
                           marca «informado», e nunca é colorido como se fosse ao vivo.\n\n\
                           Ligar um provedor com chave um dia troca a fonte e não muda \n\
                           nenhum módulo — é para isso que o `trait Provider` existe."
                        .into(),
                }),
            ),
        ])
    }

    fn key(&mut self, key: KeyEvent, ctx: &Ctx) -> Outcome {
        if let Some(form) = &mut self.form {
            if form.tecla(key) {
                let simbolo = form.valor("indicador");
                let Some(valor) = calc::ler_numero(&form.valor("valor")) else {
                    form.erro = Some("valor não é um número".into());
                    return Outcome::Ok;
                };
                let Some((ativo, _)) = indicadores().into_iter().find(|(a, _)| a.symbol == simbolo)
                else {
                    form.erro = Some("indicador desconhecido — use ←/→".into());
                    return Outcome::Ok;
                };
                self.form = None;
                return Outcome::Editar(vec![Edit::PrecoManual(ativo, valor)]);
            }
            return Outcome::Ok;
        }

        match key.code {
            // `Ctrl+A` de anotar — o `Ctrl+E` que era virou a tecla de marcar, em toda
            // tela do programa.
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // Só o que continua sem fonte. Oferecer «informar o Ibovespa» depois de
                // ele ganhar uma seria convidar a digitar por cima de um dado buscado.
                let opcoes: Vec<String> = MACRO
                    .iter()
                    .filter(|(s, m, _)| ctx.providers.quem_busca(&AssetId::new(*m, *s)).is_none())
                    .map(|(s, _, _)| s.to_string())
                    .collect();
                let primeiro = opcoes.first().cloned().unwrap_or_default();
                self.form = Some(Formulario::novo(
                    "Informar um indicador",
                    vec![
                        Field::choice("indicador", primeiro, opcoes, "←/→ escolhe qual"),
                        Field::text("valor", "", "O número. Aparecerá marcado como informado"),
                    ],
                ));
                Outcome::Ok
            }
            _ => match self.lista.tecla(key, indicadores().len()) {
                true => Outcome::Ok,
                false => Outcome::Ignorada,
            },
        }
    }

    fn escape(&mut self) -> Escape {
        if self.form.take().is_some() {
            return Escape::Consumido;
        }
        self.lista.escape()
    }

    fn hint(&self) -> String {
        hint(&["Ctrl+A informar um valor", "Esc sair"])
    }
}
