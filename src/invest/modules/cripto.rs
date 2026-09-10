//! Cripto — o único mercado da aba com dado profissional, ao vivo, gratuito e sem chave.
//!
//! Consequência prática: tudo que os outros módulos de mercado não conseguem fazer por
//! falta de fonte, este consegue. É também onde a arquitetura de provedores é exercitada
//! por inteiro, porque é o único que percorre o caminho completo.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::invest::calc;
use crate::invest::carteira;
use crate::invest::model::{AssetId, Classe, Market, Moeda};
use crate::invest::module::{
    Ctx, Edit, Escape, Field, Group, InvestModule, Layout, Marcavel, ModuleView, Outcome, Pane,
    Row, Tone,
};
use crate::invest::modules::comum::{Formulario, Lista, hint, sem_preco};
use crate::invest::modules::mercado;
use crate::invest::providers::binance::moeda_do_par;

pub struct Cripto;

impl InvestModule for Cripto {
    fn id(&self) -> &'static str {
        "cripto"
    }
    fn name(&self) -> &'static str {
        "Cripto"
    }
    fn description(&self) -> &'static str {
        "Preço ao vivo, amplitude do dia e volume — a única fonte grátis em tempo real"
    }
    fn destaque(&self) -> u8 {
        3
    }
    fn group(&self) -> Group {
        Group::Mercado
    }

    fn fonte_externa(&self) -> Option<&'static str> {
        Some(crate::invest::store::fonte::COTACAO)
    }

    fn marcavel(&self) -> Option<Marcavel> {
        Some(crate::invest::marcas::ATIVOS)
    }
    fn keywords(&self) -> &'static str {
        "bitcoin btc eth binance exchange par usdt"
    }

    fn summary(&self, ctx: &Ctx) -> String {
        let pares = pares(ctx);
        if pares.is_empty() {
            return "nenhum par acompanhado".into();
        }
        match ctx.market.quote(&pares[0]) {
            Some(q) => format!(
                "{} {}{}",
                pares[0].short(),
                calc::preco(q.preco),
                q.variacao()
                    .map(|v| format!(" ({})", calc::pct(v)))
                    .unwrap_or_default()
            ),
            None => format!("{} pares · buscando", pares.len()),
        }
    }

    fn widget(&self, ctx: &Ctx) -> Option<Pane> {
        let pares = pares(ctx);
        if pares.is_empty() {
            return None;
        }
        Some(Pane::Facts {
            title: String::new(),
            rows: pares
                .iter()
                .take(8)
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
            form: None,
        })
    }
}

fn pares(ctx: &Ctx) -> Vec<AssetId> {
    let mut v: Vec<AssetId> = Vec::new();
    for a in ctx
        .portfolio
        .posicoes
        .iter()
        .map(|p| &p.ativo)
        .chain(ctx.portfolio.watchlist.iter())
        .filter(|a| a.market == Market::Binance)
    {
        if !v.contains(a) {
            v.push(a.clone());
        }
    }
    v
}

struct Vista {
    lista: Lista,
    form: Option<Formulario>,
}

impl Vista {
    /// O par a que a linha sob o cursor pertence. Uma linha de conversão não é um par —
    /// ela descreve o de cima —, então ela resolve para ele.
    fn par_sob_o_cursor(&self, ctx: &Ctx) -> Option<AssetId> {
        let pares = pares(ctx);
        let mut dono: Vec<AssetId> = Vec::new();
        for a in &pares {
            dono.push(a.clone());
            let moeda = moeda_do_par(&a.symbol);
            if moeda != Moeda::Brl
                && ctx.market.quote(a).is_some()
                && ctx
                    .market
                    .quote(a)
                    .and_then(|q| ctx.market.para_brl(q.preco, moeda))
                    .is_some()
            {
                dono.push(a.clone());
            }
        }
        let rows: Vec<Row> = dono
            .iter()
            .map(|a| Row::new(vec![a.short().to_string()]))
            .collect();
        self.lista.atual(&rows).and_then(|i| dono.get(i)).cloned()
    }
}

impl ModuleView for Vista {
    fn title(&self) -> String {
        "Cripto".into()
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

        let pares = pares(ctx);
        if pares.is_empty() {
            return Layout::one(Pane::Empty {
                title: "Cripto".into(),
                note: "Nenhum par acompanhado.\n\n\
                       Ctrl+A adiciona um. Os pares da Binance vão com o mercado na \
                       frente: BINANCE/BTCBRL, BINANCE/ETHUSDT.\n\n\
                       Prefira o par em BRL quando ele existir: BTCUSDT convertido para \
                       real carrega dois spreads e o erro do câmbio, e o par direto não."
                    .into(),
            });
        }

        let mut rows: Vec<Row> = Vec::new();
        for a in &pares {
            rows.push(mercado::linha(
                a,
                ctx.market.quote(a),
                mercado::na_carteira(ctx, a),
                ctx.agora,
                ctx,
            ));
            // Um par que não é em BRL precisa dizer o caminho da conversão. Um preço com
            // duas conversões escondidas é um preço em que se confia demais.
            let moeda = moeda_do_par(&a.symbol);
            if moeda != Moeda::Brl
                && let Some(q) = ctx.market.quote(a)
                && let Some(brl) = ctx.market.para_brl(q.preco, moeda)
            {
                rows.push(
                    Row::tinted(
                        vec![
                            format!("  via {} → BRL", moeda.code()),
                            format!("R$ {}", calc::moeda(brl)),
                            String::new(),
                            String::new(),
                            String::new(),
                            String::new(),
                        ],
                        Tone::Dim,
                    )
                    .at_depth(1),
                );
            }
        }

        let linhas = carteira::linhas(ctx.portfolio, ctx.market, ctx.agora);
        let totais = carteira::totais(&linhas);
        let minha: f64 = linhas
            .iter()
            .filter(|l| l.posicao.classe == Classe::Cripto)
            .filter_map(|l| l.mercado_brl)
            .sum();

        Layout::rows(vec![
            (
                4,
                Layout::one(Pane::Table {
                    title: "Cripto · Binance".into(),
                    headers: mercado::CABECALHO.iter().map(|s| s.to_string()).collect(),
                    rows,
                    selected: Some(self.lista.selecionado),
                    query: self.lista.busca.clone(),
                    note: mercado::nota_fontes(ctx),
                }),
            ),
            (
                1,
                Layout::one(Pane::Facts {
                    title: "Na sua carteira".into(),
                    rows: vec![
                        (
                            "posição em cripto".into(),
                            format!(
                                "R$ {} · {}",
                                calc::moeda(minha),
                                calc::pct_simples(match totais.mercado > 0.0 {
                                    true => minha / totais.mercado * 100.0,
                                    false => 0.0,
                                })
                            ),
                            Tone::Normal,
                        ),
                        (
                            "o mercado nunca fecha".into(),
                            "é o único provedor da aba em que isso é verdade".into(),
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
                let bruto = form.valor("par");
                return match AssetId::parse(&bruto) {
                    Ok(a) if a.market == Market::Binance => {
                        self.form = None;
                        Outcome::Editar(vec![Edit::AddWatch(a)])
                    }
                    Ok(_) => {
                        form.erro = Some("aqui só entram pares da Binance: BINANCE/BTCBRL".into());
                        Outcome::Ok
                    }
                    Err(e) => {
                        form.erro = Some(e);
                        Outcome::Ok
                    }
                };
            }
            return Outcome::Ok;
        }

        let total = pares(ctx).len() * 2;
        match key.code {
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.form = Some(Formulario::novo(
                    "Acompanhar um par",
                    vec![Field::text(
                        "par",
                        "BINANCE/",
                        "BINANCE/BTCBRL, BINANCE/ETHUSDT",
                    )],
                ));
                Outcome::Ok
            }
            // A tabela intercala linhas de conversão («via USDT → BRL»), então o índice
            // do cursor não é o do par: a linha de conversão pertence ao par acima dela.
            KeyCode::Enter => Outcome::Abrir {
                modulo: "grafico",
                alvo: self.par_sob_o_cursor(ctx),
            },
            _ => match self.lista.tecla(key, total) {
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
        hint(&["↑/↓ andar", "Ctrl+A par", "Enter gráfico", "Esc sair"])
    }
}
