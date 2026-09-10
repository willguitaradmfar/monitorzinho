//! Carteiras recomendadas: para onde a carteira deve ir, e o que fazer para chegar lá.
//!
//! Uma carteira recomendada é uma lista de ativos com o peso que cada um deve ter e o
//! preço máximo que se aceita pagar. A tela mostra a distância entre ela e o que se tem —
//! em reais, em pontos percentuais e em quantidade de papéis, porque são três formas da
//! mesma diferença e cada uma responde a uma pergunta: quanto dinheiro, quão longe, e
//! quantas ações mandar comprar.
//!
//! O vínculo mora na posição: cada ativo diz de qual carteira é, e **de uma só**. É isso
//! que faz o total de cada carteira ser o dela, e não uma fatia arbitrária do patrimônio.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::invest::calc;
use crate::invest::carteiras::{Ajuste, plano};
use crate::invest::model::{AlvoCarteira, AssetId, Carteira};
use crate::invest::module::{
    Bar, Cartaz, Ctx, Edit, Escape, Field, Group, InvestModule, Layout, Marcavel, ModuleView,
    Outcome, Pane, Row, Tone,
};
use crate::invest::modules::comum::{Formulario, Lista, hint};

pub struct Carteiras;

/// Em que unidade as barras falam. As três dizem a mesma coisa, e a pergunta é que muda:
/// quanto dinheiro separar, quão longe do alvo, quantas ações mandar comprar.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Unidade {
    Reais,
    Percentual,
    Quantidade,
}

impl Unidade {
    const ALL: [Unidade; 3] = [Unidade::Reais, Unidade::Percentual, Unidade::Quantidade];
    fn label(&self) -> &'static str {
        match self {
            Unidade::Reais => "em reais",
            Unidade::Percentual => "em pontos percentuais",
            Unidade::Quantidade => "em quantidade",
        }
    }
    /// O valor da barra e o texto ao lado dela, para um ajuste.
    fn medir(&self, a: &Ajuste) -> (f64, String) {
        match self {
            Unidade::Reais => (a.delta, format!("R$ {}", calc::moeda(a.delta))),
            Unidade::Percentual => {
                let pp = a.alvo_pct - a.atual_pct;
                (pp, calc::pp(pp))
            }
            Unidade::Quantidade => match a.quantidade {
                Some(q) => (q, format!("{:.0}", q).replace('-', "−")),
                None => (0.0, "sem preço".into()),
            },
        }
    }
}

/// O formulário de um alvo. `None` para um novo, `Some` para editar o que já existe.
///
/// O mesmo formulário nos dois casos de propósito: gravar substitui o alvo daquele ativo
/// em vez de somar outro, então adicionar e editar **são a mesma operação** — e uma tela
/// só é uma tela a menos para divergir.
fn form_do_alvo(existente: Option<(String, f64, Option<f64>, u32)>, proxima: u32) -> Formulario {
    let (ativo, pct, teto, ordem) = match existente {
        Some((a, p, t, o)) => (
            a,
            calc::moeda(p),
            t.map(calc::preco).unwrap_or_default(),
            o.to_string(),
        ),
        None => (
            String::new(),
            String::new(),
            String::new(),
            proxima.to_string(),
        ),
    };
    Formulario::novo(
        match ativo.is_empty() {
            true => "Ativo na carteira".to_string(),
            false => format!("Alvo de {ativo}"),
        },
        vec![
            Field::text("ativo", ativo, "Com o mercado na frente: B3/PETR4"),
            Field::text("percentual", pct, "Quanto ele deve pesar, em %"),
            Field::text("teto", teto, "Preço máximo que você paga. Vazio = sem teto"),
            Field::text("posição", ordem, "A posição dele na lista recomendada"),
        ],
    )
}

/// O cartão de uma carteira: o que fazer em cada papel, e na borda de baixo o quanto ela
/// é do patrimônio.
///
/// `Pane::Table` e não `Facts` por causa do rodapé: a fatia do patrimônio vai **na
/// moldura**, como o total em Posições e a variação do dia em Cotações. Como linha de
/// dentro ela disputava espaço com os ajustes e era a primeira a ser cortada — justamente
/// o número que um painel com várias carteiras existe para comparar.
fn cartao(carteira: &Carteira, p: &crate::invest::carteiras::Plano, patrimonio: f64) -> Pane {
    let rodape = match patrimonio > 0.0 {
        true => format!(
            "{} do patrimônio · R$ {}",
            calc::pct_simples(p.total / patrimonio * 100.0),
            calc::moeda(p.total)
        ),
        false => format!("R$ {}", calc::moeda(p.total)),
    };

    // **Sem alvo nenhum não há o que ajustar.** Pela regra do módulo, um ativo fora do
    // alvo tem alvo zero e pede venda — o que numa carteira recém-criada vira «venda
    // tudo», que não é o que ela está dizendo. Ela está dizendo que ainda não foi
    // configurada, e é isso que a tela mostra.
    let rows: Vec<Row> = match carteira.alvos.is_empty() {
        true => vec![Row::tinted(
            vec![
                "sem alvos".into(),
                "Ctrl+T define o peso de cada ativo".into(),
            ],
            Tone::Aviso,
        )],
        // Só o que é acionável: quem já está no alvo não precisa de linha.
        false => p
            .ajustes
            .iter()
            .filter(|a| a.delta.abs() > 0.005 * p.total.max(1.0))
            .take(5)
            .map(|a| {
                let barrado = a.acima_do_teto && a.delta > 0.0;
                Row::new(vec![
                    match a.ordem {
                        u32::MAX => format!("— {}", a.ativo.short()),
                        n => format!("{n} {}", a.ativo.short()),
                    },
                    match (barrado, a.delta) {
                        (true, _) => "acima do teto".into(),
                        (false, d) if d > 0.0 => format!("+R$ {}", calc::moeda(d)),
                        (false, d) => format!("-R$ {}", calc::moeda(-d)),
                    },
                ])
                .with_cell_tones(vec![
                    Tone::Normal,
                    match (barrado, a.delta) {
                        (true, _) => Tone::Ruim,
                        (false, d) if d > 0.0 => Tone::Bom,
                        (false, _) => Tone::Aviso,
                    },
                ])
            })
            .collect(),
    };

    Pane::Table {
        title: String::new(),
        headers: vec![
            "Ativo".into(),
            match carteira.alvos.is_empty() {
                true => String::new(),
                false => format!("desvio {}", calc::pp(p.desvio()).replace('+', "")),
            },
        ],
        rows,
        selected: None,
        query: String::new(),
        note: Some((rodape, Tone::Destaque)),
    }
}

impl InvestModule for Carteiras {
    fn id(&self) -> &'static str {
        "carteiras"
    }
    fn name(&self) -> &'static str {
        "Carteiras recomendadas"
    }
    fn description(&self) -> &'static str {
        "O alvo de cada carteira, e o que aportar para chegar nele"
    }
    /// Depois de «Índices e macro» (6) e antes da Agenda: empatada com ela em 5, e o
    /// desempate é a ordem dos grupos — Carteira vem antes de Informação.
    fn destaque(&self) -> u8 {
        5
    }
    fn group(&self) -> Group {
        Group::Carteira
    }

    fn fonte_externa(&self) -> Option<&'static str> {
        Some(crate::invest::store::fonte::COTACAO)
    }

    fn marcavel(&self) -> Option<Marcavel> {
        Some(crate::invest::marcas::ATIVOS)
    }
    fn keywords(&self) -> &'static str {
        "carteira recomendada alvo aporte alocação teto rebalancear recomendação"
    }

    fn summary(&self, ctx: &Ctx) -> String {
        match ctx.portfolio.carteiras.len() {
            0 => "nenhuma carteira recomendada — Ctrl+A cria uma".into(),
            n => format!("{n} carteira(s) recomendada(s)"),
        }
    }

    /// **Um cartão por carteira.** Cada uma é uma coisa que se acompanha por si; uma
    /// linha «3 carteiras» num painel não responde nada sobre nenhuma delas.
    fn cartazes(&self, ctx: &Ctx) -> Vec<Cartaz> {
        // O patrimônio inteiro, para cada cartão dizer que fatia dele ele é.
        let patrimonio = crate::invest::carteira::totais(&crate::invest::carteira::linhas(
            ctx.portfolio,
            ctx.market,
            ctx.agora,
        ))
        .mercado;
        if ctx.portfolio.carteiras.is_empty() {
            return vec![Cartaz {
                titulo: self.name().to_string(),
                sub: None,
                pane: None,
            }];
        }
        ctx.portfolio
            .carteiras
            .iter()
            .map(|c| {
                let p = plano(c, ctx.portfolio, ctx.market, ctx.agora);
                Cartaz {
                    // O nome do módulo na frente do da carteira. Sem ele, criada a
                    // primeira carteira o cartão «Carteiras recomendadas» sumia da home —
                    // e com ele sumia o único lugar que dizia onde se cria a próxima.
                    titulo: format!("Carteiras · {}", c.nome),
                    sub: Some(c.nome.clone()),
                    pane: Some(cartao(c, &p, patrimonio)),
                }
            })
            .collect()
    }

    fn open_em(&self, ctx: &Ctx, sub: Option<&str>) -> Box<dyn ModuleView> {
        let carteira = sub
            .and_then(|nome| ctx.portfolio.carteiras.iter().position(|c| c.nome == nome))
            .unwrap_or(0);
        Box::new(Vista {
            lista: Lista::default(),
            form: None,
            carteira,
            unidade: 0,
            confirmando: None,
        })
    }

    fn open(&self, _ctx: &Ctx, _alvo: Option<&AssetId>) -> Box<dyn ModuleView> {
        Box::new(Vista {
            lista: Lista::default(),
            form: None,
            carteira: 0,
            unidade: 0,
            confirmando: None,
        })
    }
}

struct Vista {
    lista: Lista,
    form: Option<Formulario>,
    /// Qual carteira está aberta. As setas andam nos ativos dela; `←/→` troca de carteira.
    carteira: usize,
    unidade: usize,
    /// O alvo cuja remoção espera confirmação.
    confirmando: Option<usize>,
}

impl Vista {
    fn unidade(&self) -> Unidade {
        Unidade::ALL[self.unidade % Unidade::ALL.len()]
    }

    fn atual<'a>(&self, ctx: &'a Ctx) -> Option<&'a Carteira> {
        ctx.portfolio.carteiras.get(self.carteira)
    }
}

impl ModuleView for Vista {
    fn title(&self) -> String {
        "Carteiras recomendadas".into()
    }

    fn quer_mercado(&self) -> bool {
        true
    }

    fn ativos(&self) -> Vec<AssetId> {
        Vec::new()
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
        let Some(carteira) = self.atual(ctx) else {
            return Layout::one(Pane::Empty {
                title: "Carteiras recomendadas".into(),
                note: "Nenhuma carteira ainda.\n\n\
                       Ctrl+A cria uma: um nome, e depois os ativos com o peso que cada \n\
                       um deve ter e o preço máximo que você aceita pagar.\n\n\
                       Em Posições, cada ativo diz de qual carteira ele é — e de uma só. \n\
                       É esse vínculo que faz o total de cada carteira ser o dela, em vez \n\
                       de uma fatia arbitrária do patrimônio.\n\n\
                       A partir daí a tela mostra quanto falta em cada papel, em reais, \n\
                       em pontos percentuais e em quantidade de ações."
                    .into(),
            });
        };

        let p = plano(carteira, ctx.portfolio, ctx.market, ctx.agora);
        let unidade = self.unidade();

        let rows: Vec<Row> = p
            .ajustes
            .iter()
            .map(|a| {
                Row::new(vec![
                    match a.ordem {
                        // Quem saiu do alvo não tem posição na lista — ele saiu dela.
                        u32::MAX => "—".to_string(),
                        n => n.to_string(),
                    },
                    a.ativo.short().to_string(),
                    calc::pct_simples(a.alvo_pct),
                    calc::pct_simples(a.atual_pct),
                    format!("R$ {}", calc::moeda(a.atual)),
                    // O preço de agora ao lado do teto: sem ele, «acima do teto» é uma
                    // afirmação que a tela pede para acreditar.
                    match (a.preco, a.teto) {
                        (Some(p), Some(t)) => format!("{} / {}", calc::preco(p), calc::preco(t)),
                        (Some(p), None) => format!("{} / —", calc::preco(p)),
                        (None, _) => "—".into(),
                    },
                    // O teto barra a **compra**. Uma linha que pede venda não é barrada
                    // por estar cara — é justamente por isso que ela pede venda.
                    match (a.comprar(), a.acima_do_teto && a.delta > 0.0, a.delta) {
                        (_, true, _) => "acima do teto".into(),
                        (true, _, d) => match a.quantidade {
                            Some(q) => format!("comprar {:.0} · R$ {}", q, calc::moeda(d)),
                            None => format!("comprar R$ {}", calc::moeda(d)),
                        },
                        (false, _, d) if d < 0.0 => match a.quantidade {
                            Some(q) => format!("vender {:.0} · R$ {}", -q, calc::moeda(-d)),
                            None => format!("vender R$ {}", calc::moeda(-d)),
                        },
                        _ => "no alvo".into(),
                    },
                ])
                .with_cell_tones(vec![
                    Tone::Dim,
                    Tone::Normal,
                    Tone::Destaque,
                    Tone::Normal,
                    Tone::Normal,
                    match a.acima_do_teto && a.delta > 0.0 {
                        true => Tone::Ruim,
                        false => Tone::Dim,
                    },
                    match (a.acima_do_teto && a.delta > 0.0, a.delta) {
                        (true, _) => Tone::Ruim,
                        (false, d) if d > 0.0 => Tone::Bom,
                        (false, d) if d < 0.0 => Tone::Aviso,
                        _ => Tone::Dim,
                    },
                ])
            })
            .collect();

        // As barras crescem para os dois lados a partir do zero — comprar e vender são
        // sinais opostos da mesma medida, e desenhar as duas para a direita esconderia
        // qual é qual. O `full` é o maior módulo dos dois lados.
        let maior = p
            .ajustes
            .iter()
            .map(|a| unidade.medir(a).0.abs())
            .fold(0.0, f64::max)
            .max(f64::EPSILON);
        let barras: Vec<Bar> = p
            .ajustes
            .iter()
            .map(|a| {
                let (v, texto) = unidade.medir(a);
                Bar {
                    label: a.ativo.short().to_string(),
                    value: v.abs() / maior * 100.0,
                    target: None,
                    text: texto,
                    tone: match (a.acima_do_teto && a.delta > 0.0, v) {
                        (true, _) => Tone::Ruim,
                        (false, v) if v > 0.0 => Tone::Bom,
                        (false, v) if v < 0.0 => Tone::Aviso,
                        _ => Tone::Dim,
                    },
                }
            })
            .collect();

        let nota = match self.confirmando {
            // `usize::MAX` é a carteira inteira — ver a tecla `Ctrl+Del`.
            Some(usize::MAX) => format!(
                "Ctrl+Del de novo apaga a carteira «{}» e desmarca os ativos dela",
                carteira.nome
            ),
            Some(i) if p.ajustes.get(i).is_some() => {
                format!("Del de novo tira {} do alvo", p.ajustes[i].ativo.short())
            }
            _ => {
                let mut partes = vec![format!(
                    "{} de {} · Ctrl+A cria outra",
                    self.carteira + 1,
                    ctx.portfolio.carteiras.len()
                )];
                let soma = carteira.soma();
                if (soma - 100.0).abs() > 0.01 {
                    // Somar noventa é caixa; somar cento e dez é erro de digitação. Os
                    // dois são informação, e corrigir em silêncio esconderia o segundo.
                    partes.push(format!("os alvos somam {}", calc::pct_simples(soma)));
                }
                if p.fora_do_alvo > 0 {
                    partes.push(format!("{} na carteira e fora do alvo", p.fora_do_alvo));
                }
                if p.total <= 0.0 {
                    partes.push("nenhuma posição marcada como desta carteira".into());
                }
                partes.join(" · ")
            }
        };

        Layout::rows(vec![
            (
                3,
                Layout::one(Pane::Table {
                    title: match carteira.alvos.is_empty() {
                        // Sem alvos não há desvio: não é que a carteira esteja 50 p.p.
                        // fora do lugar, é que não há lugar definido.
                        true => format!(
                            "{} · R$ {} · sem alvos",
                            carteira.nome,
                            calc::moeda(p.total)
                        ),
                        false => format!(
                            "{} · R$ {} · desvio {}",
                            carteira.nome,
                            calc::moeda(p.total),
                            calc::pp(p.desvio()).replace('+', "")
                        ),
                    },
                    headers: vec![
                        "#".into(),
                        "Ativo".into(),
                        "Alvo".into(),
                        "Hoje".into(),
                        "Valor".into(),
                        "Preço / teto".into(),
                        "O que fazer".into(),
                    ],
                    rows,
                    selected: Some(self.lista.selecionado),
                    query: String::new(),
                    note: Some((nota, Tone::Aviso)),
                }),
            ),
            (
                2,
                Layout::one(Pane::Bars {
                    title: format!("O que aportar · {}", unidade.label()),
                    rows: barras,
                    full: Some(100.0),
                }),
            ),
        ])
    }

    fn key(&mut self, key: KeyEvent, ctx: &Ctx) -> Outcome {
        if let Some(form) = &mut self.form {
            if form.tecla(key) {
                return self.gravar(ctx);
            }
            return Outcome::Ok;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let total_carteiras = ctx.portfolio.carteiras.len();
        match key.code {
            // Nova carteira: só o nome. Os ativos entram depois, um a um.
            KeyCode::Char('a') if ctrl => {
                self.form = Some(Formulario::novo(
                    "Nova carteira recomendada",
                    vec![Field::text("nome", "", "Dividendos, Small caps, Cripto…")],
                ));
                Outcome::Ok
            }
            // Novo ativo na carteira aberta.
            KeyCode::Char('t') if ctrl && total_carteiras > 0 => {
                let proxima = self
                    .atual(ctx)
                    .map(|c| c.alvos.iter().map(|a| a.ordem).max().unwrap_or(0) + 1)
                    .unwrap_or(1);
                self.form = Some(form_do_alvo(None, proxima));
                Outcome::Ok
            }
            // Editar o alvo e o teto da linha sob o cursor.
            //
            // `Enter` edita a linha, como em todo módulo desta aba. Sem isto, mudar um
            // percentual exigia `Ctrl+T` e redigitar o ticker — e nada na tela dizia que
            // aquilo substituía em vez de duplicar.
            //
            // Uma linha que está na carteira e **fora do alvo** também abre: preencher o
            // percentual dela é justamente como ela entra no alvo.
            KeyCode::Enter if total_carteiras > 0 => {
                let Some(carteira) = self.atual(ctx).cloned() else {
                    return Outcome::Ok;
                };
                let p = plano(&carteira, ctx.portfolio, ctx.market, ctx.agora);
                let Some(ajuste) = p.ajustes.get(self.lista.selecionado) else {
                    return Outcome::Ok;
                };
                let alvo = carteira.alvos.iter().find(|a| a.ativo == ajuste.ativo);
                let proxima = carteira.alvos.iter().map(|a| a.ordem).max().unwrap_or(0) + 1;
                self.form = Some(form_do_alvo(
                    Some((
                        ajuste.ativo.to_string(),
                        alvo.map(|a| a.percentual).unwrap_or(0.0),
                        alvo.and_then(|a| a.teto),
                        alvo.map(|a| a.ordem).unwrap_or(proxima),
                    )),
                    proxima,
                ));
                Outcome::Ok
            }
            KeyCode::Left if total_carteiras > 0 => {
                self.carteira = (self.carteira + total_carteiras - 1) % total_carteiras;
                self.lista.selecionado = 0;
                Outcome::Ok
            }
            KeyCode::Right if total_carteiras > 0 => {
                self.carteira = (self.carteira + 1) % total_carteiras;
                self.lista.selecionado = 0;
                Outcome::Ok
            }
            // A unidade das barras: reais, pontos percentuais, quantidade.
            KeyCode::Char('u') => {
                self.unidade += 1;
                Outcome::Ok
            }
            // Apagar a carteira inteira. Maiúsculo de propósito: `Del` tira um ativo do
            // alvo, e apagar a carteira toda é outra ordem de grandeza — ela desmarca
            // todos os ativos que apontavam para ela.
            KeyCode::Delete if ctrl => {
                let Some(carteira) = self.atual(ctx).cloned() else {
                    return Outcome::Ok;
                };
                if self.confirmando != Some(usize::MAX) {
                    self.confirmando = Some(usize::MAX);
                    return Outcome::Ok;
                }
                self.confirmando = None;
                self.carteira = self.carteira.saturating_sub(1);
                Outcome::Editar(vec![Edit::RemoverCarteira(carteira.nome)])
            }
            // Apagar pede confirmação na própria linha, como o resto da aba.
            KeyCode::Delete => {
                let Some(carteira) = self.atual(ctx).cloned() else {
                    return Outcome::Ok;
                };
                let p = plano(&carteira, ctx.portfolio, ctx.market, ctx.agora);
                let i = self.lista.selecionado;
                if self.confirmando != Some(i) {
                    self.confirmando = Some(i);
                    return Outcome::Ok;
                }
                self.confirmando = None;
                let Some(ajuste) = p.ajustes.get(i) else {
                    return Outcome::Ok;
                };
                let mut nova = carteira;
                nova.alvos.retain(|a| a.ativo != ajuste.ativo);
                Outcome::Editar(vec![Edit::SetCarteira(Box::new(nova))])
            }
            _ => {
                self.confirmando = None;
                let total = self
                    .atual(ctx)
                    .map(|c| plano(c, ctx.portfolio, ctx.market, ctx.agora).ajustes.len())
                    .unwrap_or(0);
                match self.lista.tecla(key, total) {
                    true => Outcome::Ok,
                    false => Outcome::Ignorada,
                }
            }
        }
    }

    fn escape(&mut self) -> Escape {
        if self.form.take().is_some() || self.confirmando.take().is_some() {
            return Escape::Consumido;
        }
        self.lista.escape()
    }

    fn hint(&self) -> String {
        hint(&[
            "↑/↓ ativo",
            "Enter alvo e teto",
            "←/→ carteira",
            "u unidade",
            "Ctrl+A nova carteira",
            "Ctrl+T ativo",
            "Del tira o ativo",
            "Ctrl+Del apaga a carteira",
            "Esc sair",
        ])
    }
}

impl Vista {
    /// Grava o que o formulário aberto pediu.
    fn gravar(&mut self, ctx: &Ctx) -> Outcome {
        // A carteira aberta é lida **antes** de pegar o formulário emprestado: as duas
        // vivem em `self`, e o compilador não deixa segurar as duas ao mesmo tempo.
        let aberta = self.atual(ctx).cloned();
        let Some(form) = &mut self.form else {
            return Outcome::Ok;
        };
        // Uma carteira nova tem só o campo do nome; a de ativo tem três.
        if form.campos.len() == 1 {
            let nome = form.valor("nome").trim().to_string();
            if nome.is_empty() {
                form.erro = Some("a carteira precisa de um nome".into());
                return Outcome::Ok;
            }
            if ctx.portfolio.carteiras.iter().any(|c| c.nome == nome) {
                form.erro = Some(format!("já existe uma carteira «{nome}»"));
                return Outcome::Ok;
            }
            self.form = None;
            // A nova entra no fim, e a tela vai para ela: quem acabou de criar quer vê-la.
            self.carteira = ctx.portfolio.carteiras.len();
            return Outcome::Editar(vec![Edit::SetCarteira(Box::new(Carteira {
                nome,
                alvos: Vec::new(),
            }))]);
        }

        let Some(carteira) = aberta else {
            return Outcome::Ok;
        };
        let ativo = match AssetId::parse(&form.valor("ativo")) {
            Ok(a) => a,
            Err(e) => {
                form.erro = Some(e);
                return Outcome::Ok;
            }
        };
        let Some(percentual) = calc::ler_numero(&form.valor("percentual")) else {
            form.erro = Some("o percentual não é um número".into());
            return Outcome::Ok;
        };
        if !(0.0..=100.0).contains(&percentual) {
            form.erro = Some("o percentual tem que estar entre 0 e 100".into());
            return Outcome::Ok;
        }
        let bruto_teto = form.valor("teto");
        let teto = match bruto_teto.trim().is_empty() {
            true => None,
            false => match calc::ler_numero(&bruto_teto).filter(|v| *v > 0.0) {
                Some(v) => Some(v),
                None => {
                    form.erro = Some("o teto não é um preço — deixe vazio para não ter".into());
                    return Outcome::Ok;
                }
            },
        };

        let mut nova = carteira;
        // O mesmo ativo duas vezes seria contado duas vezes: substitui em vez de somar.
        nova.alvos.retain(|a| a.ativo != ativo);
        let ordem = calc::ler_numero(&form.valor("posição"))
            .filter(|v| *v >= 1.0)
            .map(|v| v as u32)
            .unwrap_or(u32::MAX);
        nova.alvos.push(AlvoCarteira {
            ordem,
            ativo,
            percentual,
            teto,
        });
        self.form = None;
        Outcome::Editar(vec![Edit::SetCarteira(Box::new(nova))])
    }
}
