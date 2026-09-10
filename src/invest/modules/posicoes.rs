//! Posições — o módulo central. Tudo em `docs/invest/` ou lê daqui, ou existe para
//! alimentar isto.
//!
//! A regra que define o módulo: **o preço médio é informado, nunca calculado**. O que o
//! programa calcula é valor de mercado, custo, P&L, variação do dia e peso — e o preço
//! médio não aparece nessa lista, que é o ponto.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::invest::calc;
use crate::invest::carteira::{self, Linha};
use crate::invest::model::{AssetId, Classe, Moeda, Position};
use crate::invest::module::{
    Ctx, Edit, Escape, Field, Group, InvestModule, Layout, Marcavel, ModuleView, Need, Outcome,
    Pane, Row, Tone,
};
use crate::invest::modules::comum::{Formulario, Lista, hint};

pub struct Posicoes;

impl InvestModule for Posicoes {
    fn id(&self) -> &'static str {
        "posicoes"
    }

    fn name(&self) -> &'static str {
        "Posições"
    }

    fn description(&self) -> &'static str {
        "Ativo, quantidade, preço médio informado, preço atual e P&L"
    }

    fn destaque(&self) -> u8 {
        8
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

    fn needs(&self) -> &'static [Need] {
        &[]
    }

    fn keywords(&self) -> &'static str {
        "carteira ativos pm preço médio lucro prejuízo p&l patrimônio"
    }

    fn summary(&self, ctx: &Ctx) -> String {
        if ctx.portfolio.posicoes.is_empty() {
            return "nenhuma posição — importe ou adicione".to_string();
        }
        let linhas = carteira::linhas(ctx.portfolio, ctx.market, ctx.agora);
        let t = carteira::totais(&linhas);
        let mut frase = format!(
            "{} · R$ {}",
            calc::plural(ctx.portfolio.posicoes.len(), "ativo", "ativos"),
            calc::moeda(t.mercado)
        );
        if let Some(pct) = t.pnl_pct() {
            frase.push_str(&format!(" · {}", calc::pct(pct)));
        }
        if !t.completo() {
            frase.push_str(&format!(" · {}", t.ressalva()));
        }
        frase
    }

    fn widget(&self, ctx: &Ctx) -> Option<Pane> {
        if ctx.portfolio.posicoes.is_empty() {
            return None;
        }
        let mut linhas = carteira::linhas(ctx.portfolio, ctx.market, ctx.agora);
        let totais = carteira::totais(&linhas);
        // Pela variação do dia, da maior alta para a maior queda. É a pergunta que se faz
        // de relance — «o que mexeu hoje?» —, e não «o que é grande», que muda de mês em
        // mês. Quem ainda não tem variação vai para o fim: um traço não é uma queda.
        linhas.sort_by(|a, b| match (a.variacao_pct, b.variacao_pct) {
            (Some(x), Some(y)) => y.total_cmp(&x),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => b
                .mercado_brl
                .unwrap_or(0.0)
                .total_cmp(&a.mercado_brl.unwrap_or(0.0)),
        });
        let rows: Vec<Row> = linhas
            .iter()
            .take(8)
            .map(|l| {
                Row::new(vec![
                    l.posicao.ativo.short().to_string(),
                    l.mercado_brl.map(calc::moeda).unwrap_or_else(|| "—".into()),
                    l.variacao_pct.map(calc::pct).unwrap_or_else(|| "—".into()),
                ])
                .with_cell_tones(vec![
                    Tone::Normal,
                    Tone::Normal,
                    crate::invest::modules::heatmap::tom(l.variacao_pct),
                ])
            })
            .collect();
        Some(Pane::Table {
            title: String::new(),
            headers: vec!["Ativo".into(), "Valor".into(), "No dia".into()],
            rows,
            selected: None,
            query: String::new(),
            // O P&L nunca sai sem dizer sobre o quê. Numa carteira em que só duas de
            // vinte e uma linhas informam preço médio, um «P&L» solto no rodapé é o
            // número mais enganoso da tela.
            // Colorido pelo sinal, como o resto dos números desta aba: um P&L negativo em
            // amarelo se lê como um positivo, e a cor é o que o olho pega primeiro.
            note: Some(match totais.sem_custo {
                0 => (
                    format!(
                        "R$ {} · P&L R$ {}{}",
                        calc::moeda(totais.mercado),
                        calc::moeda(totais.pnl),
                        totais
                            .pnl_pct()
                            .map(|p| format!(" ({})", calc::pct(p)))
                            .unwrap_or_default()
                    ),
                    crate::invest::modules::heatmap::tom(totais.pnl_pct()),
                ),
                n => (
                    format!(
                        "R$ {} · {} sem preço médio informado",
                        calc::moeda(totais.mercado),
                        n
                    ),
                    Tone::Aviso,
                ),
            }),
        })
    }

    fn open(&self, _ctx: &Ctx, _alvo: Option<&AssetId>) -> Box<dyn ModuleView> {
        Box::new(Vista {
            lista: Lista::default(),
            form: None,
            agrupamento: Agrupamento::Plano,
            confirmando_remocao: None,
        })
    }
}

/// Por que dimensão a tabela agrupa. Vira árvore quando não é plano.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Agrupamento {
    Plano,
    Classe,
    Corretora,
    Moeda,
}

impl Agrupamento {
    fn label(&self) -> &'static str {
        match self {
            Agrupamento::Plano => "plano",
            Agrupamento::Classe => "por classe",
            Agrupamento::Corretora => "por corretora",
            Agrupamento::Moeda => "por moeda",
        }
    }

    fn proximo(&self) -> Agrupamento {
        match self {
            Agrupamento::Plano => Agrupamento::Classe,
            Agrupamento::Classe => Agrupamento::Corretora,
            Agrupamento::Corretora => Agrupamento::Moeda,
            Agrupamento::Moeda => Agrupamento::Plano,
        }
    }

    fn chave(&self, l: &Linha) -> Option<String> {
        match self {
            Agrupamento::Plano => None,
            Agrupamento::Classe => Some(l.posicao.classe.label().to_string()),
            Agrupamento::Corretora => Some(l.posicao.fonte.clone()),
            Agrupamento::Moeda => Some(l.posicao.moeda.code().to_string()),
        }
    }
}

struct Vista {
    lista: Lista,
    form: Option<Formulario>,
    agrupamento: Agrupamento,
    /// A linha cuja remoção está sendo confirmada dentro do módulo.
    confirmando_remocao: Option<usize>,
}

const CABECALHO: [&str; 8] = [
    "Ativo", "Classe", "Qtd", "PM", "Atual", "Mercado", "P&L", "Peso",
];

impl Vista {
    /// As linhas na ordem em que a tabela as mostra — que é a mesma ordem que o cursor
    /// indexa. Uma segunda ordem em qualquer lugar faria o `Enter` editar a linha errada.
    fn ordenadas(&self, ctx: &Ctx) -> Vec<Linha> {
        let mut linhas = carteira::linhas(ctx.portfolio, ctx.market, ctx.agora);
        match self.agrupamento {
            Agrupamento::Plano => linhas.sort_by(|a, b| {
                b.mercado_brl
                    .unwrap_or(0.0)
                    .partial_cmp(&a.mercado_brl.unwrap_or(0.0))
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
            g => linhas.sort_by(|a, b| {
                g.chave(a).cmp(&g.chave(b)).then_with(|| {
                    b.mercado_brl
                        .unwrap_or(0.0)
                        .partial_cmp(&a.mercado_brl.unwrap_or(0.0))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
            }),
        }
        linhas
    }

    fn linhas_tabela(&self, ctx: &Ctx, linhas: &[Linha]) -> Vec<Row> {
        let totais = carteira::totais(linhas);
        let mut saida = Vec::new();
        let mut grupo_atual: Option<String> = None;

        for l in linhas {
            if let Some(chave) = self.agrupamento.chave(l)
                && grupo_atual.as_deref() != Some(chave.as_str())
            {
                let soma: f64 = linhas
                    .iter()
                    .filter(|x| self.agrupamento.chave(x).as_deref() == Some(chave.as_str()))
                    .filter_map(|x| x.mercado_brl)
                    .sum();
                saida.push(Row::tinted(
                    vec![
                        chave.clone(),
                        String::new(),
                        String::new(),
                        String::new(),
                        String::new(),
                        calc::moeda(soma),
                        String::new(),
                        calc::pct_simples(match totais.mercado > 0.0 {
                            true => soma / totais.mercado * 100.0,
                            false => 0.0,
                        }),
                    ],
                    Tone::Destaque,
                ));
                grupo_atual = Some(chave);
            }

            let atual = match l.preco {
                Some(p) => {
                    let origem = l.origem();
                    match origem.is_empty() {
                        true => calc::preco(p),
                        false => format!("{} {origem}", calc::preco(p)),
                    }
                }
                None => "—".to_string(),
            };
            let pnl = match (l.pnl_brl, l.pnl_pct) {
                (Some(v), Some(p)) => format!("{} ({})", calc::moeda(v), calc::pct(p)),
                _ => "—".to_string(),
            };
            let tom_pnl = match l.pnl_brl {
                Some(v) if v > 0.0 => Tone::Bom,
                Some(v) if v < 0.0 => Tone::Ruim,
                _ => Tone::Dim,
            };
            // A conta que este módulo faz, e a que ele não faz: o PM é mostrado como veio.
            saida.push(
                Row::new(vec![
                    l.posicao.ativo.short().to_string(),
                    l.posicao.classe.label().to_string(),
                    calc::preco(l.posicao.quantidade),
                    // Sem preço médio, um traço — e não um zero, que se leria como
                    // «custou nada» e faria o P&L parecer lucro de cem por cento.
                    l.posicao
                        .preco_medio
                        .map(calc::preco)
                        .unwrap_or_else(|| "—".into()),
                    atual,
                    l.mercado_brl.map(calc::moeda).unwrap_or("—".into()),
                    pnl,
                    carteira::peso(l, &totais)
                        .map(calc::pct_simples)
                        .unwrap_or("—".into()),
                ])
                .with_cell_tones(vec![
                    Tone::Normal,
                    Tone::Dim,
                    Tone::Normal,
                    Tone::Normal,
                    match l.grade.is_some_and(|g| g.ao_vivo()) {
                        true => Tone::Normal,
                        false => Tone::Dim,
                    },
                    Tone::Normal,
                    tom_pnl,
                    Tone::Dim,
                ])
                .at_depth(usize::from(self.agrupamento != Agrupamento::Plano)),
            );
        }
        let _ = ctx;
        saida
    }

    fn abrir_form(&mut self, existente: Option<&Position>, ctx: &Ctx) {
        let p = existente.cloned();
        // As carteiras recomendadas cadastradas, mais a opção de não estar em nenhuma.
        // Um campo de escolha e não de texto: o nome tem que casar com uma que existe,
        // e digitar à mão é a forma mais fácil de criar um vínculo que não aponta para
        // lugar nenhum.
        let mut carteiras = vec![SEM_CARTEIRA.to_string()];
        carteiras.extend(ctx.portfolio.carteiras.iter().map(|c| c.nome.clone()));
        let carteira_atual = p
            .as_ref()
            .and_then(|p| p.carteira.clone())
            .filter(|c| carteiras.iter().any(|x| x == c))
            .unwrap_or_else(|| SEM_CARTEIRA.to_string());
        let classes: Vec<String> = Classe::ALL.iter().map(|c| c.code().to_string()).collect();
        let moedas: Vec<String> = Moeda::ALL.iter().map(|m| m.code().to_string()).collect();
        self.form = Some(Formulario::novo(
            match existente {
                Some(_) => "Editar posição",
                None => "Nova posição",
            },
            vec![
                Field::text(
                    "ativo",
                    p.as_ref().map(|p| p.ativo.to_string()).unwrap_or_default(),
                    "Com o mercado na frente: B3/PETR4, US/AAPL, BINANCE/BTCBRL",
                ),
                Field::choice(
                    "classe",
                    p.as_ref()
                        .map(|p| p.classe.code().to_string())
                        .unwrap_or_else(|| Classe::Acao.code().to_string()),
                    classes,
                    "Informada, e não deduzida do símbolo — a convenção de sufixo tem exceção",
                ),
                Field::text(
                    "quantidade",
                    p.as_ref()
                        .map(|p| calc::preco(p.quantidade))
                        .unwrap_or_default(),
                    "Quantas unidades você tem",
                ),
                Field::text(
                    "preço médio",
                    p.as_ref()
                        .and_then(|p| p.preco_medio)
                        .map(calc::preco)
                        .unwrap_or_default(),
                    "Opcional. Sem ele, P&L e custo não são calculados — nada é inventado",
                ),
                Field::choice(
                    "moeda",
                    p.as_ref()
                        .map(|p| p.moeda.code().to_string())
                        .unwrap_or_else(|| Moeda::Brl.code().to_string()),
                    moedas,
                    "Em que moeda o ativo é cotado",
                ),
                Field::text(
                    "fonte",
                    p.as_ref()
                        .map(|p| p.fonte.clone())
                        .unwrap_or_else(|| "manual".into()),
                    "De onde a linha veio. Reimportar esta fonte substitui só as linhas dela",
                ),
                Field::text(
                    "conta",
                    p.as_ref().map(|p| p.conta.clone()).unwrap_or_default(),
                    "Qual conta dentro da fonte. Deixe vazio se só há uma",
                ),
                Field::text(
                    "setor",
                    String::new(),
                    "Informado — nenhuma fonte sem chave dá setor confiável",
                ),
                Field::text(
                    "preço atual",
                    p.as_ref()
                        .and_then(|p| p.preco_manual)
                        .map(calc::preco)
                        .unwrap_or_default(),
                    "Só onde não há cotação ao vivo. É valor de mercado, não custo",
                ),
                Field::choice(
                    "carteira",
                    carteira_atual,
                    carteiras,
                    "De qual carteira recomendada este ativo é. Uma só por ativo",
                ),
            ],
        ));
    }
}

/// O rótulo de «não pertence a carteira nenhuma». Um valor de escolha e não o vazio,
/// porque um campo de escolha em branco parece um campo por preencher.
pub const SEM_CARTEIRA: &str = "(nenhuma)";

/// Monta a posição a partir do formulário, recusando com a caixa aberta o que não pode
/// ser gravado. Função livre e não método: ela não olha para o estado da vista, e sendo
/// livre ela pode ser chamada com o formulário emprestado mutavelmente.
fn ler_form(form: &Formulario) -> Result<Position, String> {
    let ativo = AssetId::parse(&form.valor("ativo"))?;
    let quantidade =
        calc::ler_numero(&form.valor("quantidade")).ok_or("quantidade não é um número")?;
    // Vazio é uma resposta legítima: a posição existe, e o que depende do custo
    // simplesmente não é calculado.
    let bruto_pm = form.valor("preço médio");
    let preco_medio = match bruto_pm.trim().is_empty() {
        true => None,
        false => Some(calc::ler_numero(&bruto_pm).ok_or("preço médio não é um número")?),
    };
    let classe = Classe::parse(&form.valor("classe")).unwrap_or(Classe::Outro);
    let moeda = Moeda::parse(&form.valor("moeda")).unwrap_or(Moeda::Brl);
    let bruto_atual = form.valor("preço atual");
    let preco_manual = match bruto_atual.trim().is_empty() {
        true => None,
        false => Some(calc::ler_numero(&bruto_atual).ok_or("preço atual não é um número")?),
    };
    let fonte = match form.valor("fonte").trim().is_empty() {
        true => "manual".to_string(),
        false => form.valor("fonte").trim().to_string(),
    };
    let p = Position {
        fonte,
        conta: form.valor("conta").trim().to_string(),
        ativo,
        classe,
        quantidade,
        preco_medio,
        moeda,
        preco_manual,
        preco_manual_em: preco_manual.map(|_| 0),
        atualizado_em: 0,
        carteira: None,
    };
    p.validate()?;
    Ok(p)
}

impl ModuleView for Vista {
    fn title(&self) -> String {
        format!("Posições · {}", self.agrupamento.label())
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
                hint: hint(&["↑/↓ campo", "←/→ opções", "Enter gravar", "Esc cancelar"]),
            });
        }

        if ctx.portfolio.posicoes.is_empty() {
            return Layout::one(Pane::Empty {
                title: "Posições".into(),
                note: "Nenhuma posição ainda.\n\n\
                       Há dois caminhos, e o primeiro é o que se usa:\n\
                       • Importação — leia o CSV que a corretora exporta.\n\
                       • Ctrl+A — digite uma linha à mão.\n\n\
                       O preço médio é seu: ele vem do que você informar, e o programa \
                       nunca o recalcula."
                    .into(),
            });
        }

        let linhas = self.ordenadas(ctx);
        let totais = carteira::totais(&linhas);
        let rows = self.linhas_tabela(ctx, &linhas);

        let mut nota = Vec::new();
        if ctx.somente_leitura {
            nota.push(
                "arquivo de uma versão mais nova do monitorzinho — nada será gravado".to_string(),
            );
        }
        if !totais.ressalva().is_empty() {
            nota.push(totais.ressalva());
        }
        // **Todas** as fontes, e não só as que caíram. Uma tela que só fala quando dá
        // errado deixa quem olha sem saber se ela sequer tentou — que foi exatamente o
        // que aconteceu quando um preço não aparecia e não havia como descobrir por quê.
        if let Some((estado, _)) = crate::invest::modules::mercado::nota_fontes(ctx) {
            nota.push(estado);
        }
        if let Some(i) = self.confirmando_remocao
            && let Some(l) = linhas.get(i)
        {
            nota.push(format!(
                "remover {} de {}? Enter confirma, Esc desiste",
                l.posicao.ativo, l.posicao.fonte
            ));
        }

        let por_fonte: Vec<(String, String, Tone)> =
            carteira::agrupar(&linhas, |l| l.posicao.fonte.clone())
                .into_iter()
                .map(|(fonte, v)| (fonte, format!("R$ {}", calc::moeda(v)), Tone::Normal))
                .collect();

        let resumo = vec![
            (
                "mercado".to_string(),
                format!("R$ {}", calc::moeda(totais.mercado)),
                Tone::Destaque,
            ),
            (
                "custo".to_string(),
                format!("R$ {}", calc::moeda(totais.custo)),
                Tone::Dim,
            ),
            (
                "P&L".to_string(),
                match totais.pnl_pct() {
                    Some(p) => format!("R$ {} ({})", calc::moeda(totais.pnl), calc::pct(p)),
                    None => "—".into(),
                },
                match totais.pnl {
                    v if v > 0.0 => Tone::Bom,
                    v if v < 0.0 => Tone::Ruim,
                    _ => Tone::Dim,
                },
            ),
            (
                "no dia".to_string(),
                match (totais.dia == 0.0, totais.dia_pct()) {
                    (true, _) => "—".to_string(),
                    (false, Some(p)) => {
                        format!("R$ {} ({})", calc::moeda(totais.dia), calc::pct(p))
                    }
                    (false, None) => format!("R$ {}", calc::moeda(totais.dia)),
                },
                match totais.dia {
                    v if v > 0.0 => Tone::Bom,
                    v if v < 0.0 => Tone::Ruim,
                    _ => Tone::Dim,
                },
            ),
        ];

        Layout::rows(vec![
            (
                8,
                Layout::one(Pane::Table {
                    title: format!("Posições · {}", self.agrupamento.label()),
                    headers: CABECALHO.iter().map(|s| s.to_string()).collect(),
                    rows,
                    selected: Some(self.lista.selecionado),
                    query: self.lista.busca.clone(),
                    note: (!nota.is_empty()).then(|| (nota.join(" · "), Tone::Aviso)),
                }),
            ),
            (
                2,
                Layout::cols(vec![
                    (
                        1,
                        Layout::one(Pane::Facts {
                            title: "Total".into(),
                            rows: resumo,
                        }),
                    ),
                    (
                        1,
                        Layout::one(Pane::Facts {
                            title: "Por corretora".into(),
                            rows: por_fonte,
                        }),
                    ),
                ]),
            ),
        ])
    }

    fn key(&mut self, key: KeyEvent, ctx: &Ctx) -> Outcome {
        // O formulário vê as teclas primeiro: é um campo de texto, e toda letra é dele.
        if let Some(form) = &mut self.form {
            if form.tecla(key) {
                match ler_form(form) {
                    Ok(p) => {
                        let setor = form.valor("setor").trim().to_string();
                        let carteira = form.valor("carteira");
                        let ativo = p.ativo.clone();
                        self.form = None;
                        let mut edits = vec![Edit::UpsertPosicao(Box::new(p))];
                        if !setor.is_empty() {
                            edits.push(Edit::SetSetor(ativo.clone(), setor));
                        }
                        // Depois do upsert, e sobre **todas** as linhas do ativo: o
                        // vínculo é do papel, não desta linha. Ver `Edit::SetCarteiraDoAtivo`.
                        edits.push(Edit::SetCarteiraDoAtivo(
                            ativo,
                            match carteira.as_str() {
                                SEM_CARTEIRA | "" => None,
                                nome => Some(nome.to_string()),
                            },
                        ));
                        return Outcome::Editar(edits);
                    }
                    // Falhar com a caixa ainda aberta é a diferença entre corrigir e
                    // sair procurando.
                    Err(e) => form.erro = Some(e),
                }
            }
            return Outcome::Ok;
        }

        let linhas = self.ordenadas(ctx);
        let rows = self.linhas_tabela(ctx, &linhas);
        // A tabela tem linhas de grupo, que não são posições. O cursor anda por todas, e
        // é a conversão daqui que decide o que uma tecla age sobre.
        let indice_posicao = |vista: &Self, rows: &[Row]| -> Option<usize> {
            let idx = vista.lista.atual(rows)?;
            let ate_aqui = rows[..=idx]
                .iter()
                .filter(|r| r.depth > 0 || r.tone != Tone::Destaque)
                .count();
            (rows.get(idx)?.tone != Tone::Destaque).then(|| ate_aqui.saturating_sub(1))
        };

        if let Some(i) = self.confirmando_remocao {
            match key.code {
                KeyCode::Enter => {
                    self.confirmando_remocao = None;
                    if let Some(l) = linhas.get(i) {
                        return Outcome::Editar(vec![Edit::RemoverPosicao(l.posicao.key())]);
                    }
                }
                _ => self.confirmando_remocao = None,
            }
            return Outcome::Ok;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            // Ctrl e não uma letra pura: nesta tela toda letra é busca, e adicionar tem
            // que funcionar *enquanto* se procura — que é geralmente como se descobre que
            // a posição não existe.
            KeyCode::Char('a') if ctrl => {
                self.abrir_form(None, ctx);
                Outcome::Ok
            }
            // `Ctrl+U` de agrUpar, e não o `Ctrl+G` que era: esse virou a lista de marcas,
            // em toda tela do programa.
            KeyCode::Char('u') if ctrl => {
                self.agrupamento = self.agrupamento.proximo();
                self.lista.selecionado = 0;
                Outcome::Ok
            }
            KeyCode::Enter => {
                if let Some(i) = indice_posicao(self, &rows)
                    && let Some(l) = linhas.get(i)
                {
                    let p = l.posicao.clone();
                    self.abrir_form(Some(&p), ctx);
                }
                Outcome::Ok
            }
            KeyCode::Delete => {
                self.confirmando_remocao = indice_posicao(self, &rows);
                Outcome::Ok
            }
            _ => match self.lista.tecla(key, rows.len()) {
                true => Outcome::Ok,
                false => Outcome::Ignorada,
            },
        }
    }

    fn escape(&mut self) -> Escape {
        // Uma camada por vez: a caixa, depois a confirmação, depois a busca.
        if self.form.take().is_some() {
            return Escape::Consumido;
        }
        if self.confirmando_remocao.take().is_some() {
            return Escape::Consumido;
        }
        self.lista.escape()
    }

    fn hint(&self) -> String {
        match self.form.is_some() {
            true => hint(&["↑/↓ campo", "←/→ opções", "Enter gravar", "Esc cancelar"]),
            false => hint(&[
                "↑/↓ andar",
                "digite para buscar",
                "Enter editar",
                "Ctrl+A adicionar",
                "Ctrl+U agrupar",
                "Del remover",
                "Esc sair",
            ]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invest::model::{Market, Portfolio};
    use crate::invest::provider::MarketSnapshot;

    fn ctx_com<'a>(
        p: &'a Portfolio,
        m: &'a MarketSnapshot,
        pr: &'a std::sync::Arc<crate::invest::provider::ProviderSet>,
    ) -> Ctx<'a> {
        crate::invest::module::ctx_de_teste(p, m, pr)
    }

    fn form_preenchido() -> Formulario {
        Formulario::novo(
            "t",
            vec![
                Field::text("ativo", "B3/PETR4", ""),
                Field::choice("classe", "acao", vec!["acao".into()], ""),
                Field::text("quantidade", "300", ""),
                Field::text("preço médio", "32,10", ""),
                Field::choice("moeda", "BRL", vec!["BRL".into()], ""),
                Field::text("fonte", "corretora-x", ""),
                Field::text("conta", "", ""),
                Field::text("preço atual", "", ""),
            ],
        )
    }

    #[test]
    fn o_formulario_le_numero_escrito_por_gente() {
        let p = ler_form(&form_preenchido()).unwrap();
        assert_eq!(p.quantidade, 300.0);
        assert_eq!(p.preco_medio, Some(32.10));
        assert_eq!(p.ativo, AssetId::new(Market::B3, "PETR4"));
        assert_eq!(p.fonte, "corretora-x");
    }

    #[test]
    fn o_formulario_recusa_o_que_nao_pode_ser_gravado() {
        let mut f = form_preenchido();
        f.campos[0].value = "PETR4".into();
        assert!(
            ler_form(&f).is_err(),
            "sem mercado não dá para saber a quem perguntar"
        );

        let mut f = form_preenchido();
        f.campos[2].value = "0".into();
        assert!(ler_form(&f).is_err(), "quantidade zero não é posição");

        let mut f = form_preenchido();
        f.campos[3].value = "abc".into();
        assert!(ler_form(&f).is_err());
    }

    #[test]
    fn fonte_vazia_vira_manual_e_nunca_fica_em_branco() {
        let mut f = form_preenchido();
        f.campos[5].value = "  ".into();
        let p = ler_form(&f).unwrap();
        assert_eq!(p.fonte, "manual", "a fonte é metade da identidade da linha");
    }

    #[test]
    fn resumo_de_carteira_vazia_convida_a_comecar() {
        let p = Portfolio::default();
        let m = MarketSnapshot::default();
        let pr = crate::invest::module::providers_de_teste();
        let s = Posicoes.summary(&ctx_com(&p, &m, &pr));
        assert!(s.contains("nenhuma posição"));
    }

    #[test]
    fn agrupamento_cicla_e_volta() {
        let mut g = Agrupamento::Plano;
        for _ in 0..4 {
            g = g.proximo();
        }
        assert!(matches!(g, Agrupamento::Plano));
    }
}
