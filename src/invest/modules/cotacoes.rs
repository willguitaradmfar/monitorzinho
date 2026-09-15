//! Cotações: a watchlist. O que eu acompanho, esteja ou não na carteira.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::invest::calc;
use crate::invest::carteira;
use crate::invest::historico::Estado;
use crate::invest::model::{AssetId, Moeda};
use crate::invest::module::{
    Ctx, Edit, Escape, Field, Group, InvestModule, Layout, Marcavel, ModuleView, Need, Outcome,
    Pane, Row, Tone,
};
use crate::invest::modules::comum::{Formulario, Lista, hint, sem_preco};
use crate::invest::modules::heatmap::tom;
use crate::invest::modules::mercado;
use crate::invest::provider::{Candle, Span};
use crate::invest::tempo::{self, Data};

pub struct Cotacoes;

impl InvestModule for Cotacoes {
    fn id(&self) -> &'static str {
        "cotacoes"
    }
    fn name(&self) -> &'static str {
        "Cotações"
    }
    fn description(&self) -> &'static str {
        "A watchlist: último, variação, volume — o que se acompanha e o que se tem"
    }
    fn destaque(&self) -> u8 {
        7
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
    fn needs(&self) -> &'static [Need] {
        &[Need::Cotacao]
    }
    fn keywords(&self) -> &'static str {
        "watchlist preço ticker acompanhar mercado"
    }

    fn summary(&self, ctx: &Ctx) -> String {
        let total = ativos(ctx).len();
        if total == 0 {
            return "nada acompanhado ainda".into();
        }
        let vivos = ativos(ctx)
            .iter()
            .filter(|a| ctx.market.quote(a).is_some_and(|q| q.grade.ao_vivo()))
            .count();
        format!("{total} acompanhados · {vivos} ao vivo")
    }

    fn widget(&self, ctx: &Ctx) -> Option<Pane> {
        let mut ativos = ativos(ctx);
        if ativos.is_empty() {
            return None;
        }
        ordenar(ctx, &mut ativos);
        // Já ordenados por `ativos` — a mesma ordem da tela cheia, para entrar no módulo
        // não embaralhar a lista que se acabou de ler.
        let linhas: Vec<(&AssetId, Option<f64>, String)> = ativos
            .iter()
            .map(|a| {
                let q = ctx.market.quote(a);
                let variacao = q.and_then(|q| q.variacao().filter(|_| q.grade.ao_vivo()));
                let texto = q
                    .map(|q| calc::preco(q.preco))
                    .unwrap_or_else(|| sem_preco(ctx));
                (a, variacao, texto)
            })
            .collect();
        Some(Pane::Table {
            title: String::new(),
            // Três colunas, e não duas: com «48,09 +2,08%» num campo só, o preço de um
            // ativo e a variação de outro nunca ficam alinhados, e a coluna que se quer
            // varrer com o olho é justamente a da porcentagem.
            headers: vec!["Ativo".into(), "Último".into(), "Var%".into()],
            rows: linhas
                .iter()
                .take(10)
                .map(|(a, variacao, preco)| {
                    Row::new(vec![
                        a.short().to_string(),
                        format!(
                            "{}{preco}",
                            mercado::seta_de(a, "preco", ctx.market.quote(a).map(|q| q.preco))
                        ),
                        variacao.map(calc::pct).unwrap_or_else(|| "—".into()),
                    ])
                    .with_cell_tones(vec![
                        Tone::Normal,
                        Tone::Normal,
                        crate::invest::modules::heatmap::tom(*variacao),
                    ])
                })
                .collect(),
            selected: None,
            query: String::new(),
            // O rodapé soma o que a lista mostra papel a papel: quanto a carteira andou
            // hoje. Sem ele, dez linhas de variação não dizem se o dia foi bom.
            note: variacao_do_dia(ctx),
        })
    }

    fn open(&self, ctx: &Ctx, alvo: Option<&AssetId>) -> Box<dyn ModuleView> {
        let mut ordem = ativos(ctx);
        ordenar(ctx, &mut ordem);
        // Quem pediu um ativo já entra no gráfico dele, com o cursor na linha certa — é
        // para onde o `Esc` volta, e deixá-lo no topo faria a tela de trás falar de outro
        // papel que não o que se estava vendo.
        let selecionado = alvo
            .and_then(|a| ordem.iter().position(|x| x == a))
            .unwrap_or(0);
        Box::new(Vista {
            lista: Lista {
                selecionado,
                busca: String::new(),
            },
            form: None,
            aberto: alvo.cloned().map(Aberto::novo),
            ordem,
        })
    }
}

/// A watchlist mais toda posição — quem tem um ativo o acompanha por definição, e pedir
/// para adicioná-lo duas vezes seria trabalho sem sentido.
fn ativos(ctx: &Ctx) -> Vec<AssetId> {
    let mut v: Vec<AssetId> = Vec::new();
    for p in &ctx.portfolio.posicoes {
        if !v.contains(&p.ativo) {
            v.push(p.ativo.clone());
        }
    }
    for a in &ctx.portfolio.watchlist {
        if !v.contains(a) {
            v.push(a.clone());
        }
    }
    v
}

/// Quanto a carteira andou hoje, em reais e em porcentagem.
///
/// Só isso: patrimônio e P&L são a resposta do cartão de Patrimônio, e repeti-los aqui
/// fazia a linha estourar a largura e ser cortada no meio. O que falta a uma lista de
/// cotações é o total do movimento que ela está mostrando papel a papel.
fn variacao_do_dia(ctx: &Ctx) -> Option<(String, Tone)> {
    let linhas = carteira::linhas(ctx.portfolio, ctx.market, ctx.agora);
    let t = carteira::totais(&linhas);
    if t.dia == 0.0 {
        return None;
    }
    let texto = match t.dia_pct() {
        Some(p) => format!("no dia R$ {} ({})", calc::moeda(t.dia), calc::pct(p)),
        None => format!("no dia R$ {}", calc::moeda(t.dia)),
    };
    // Verde ou vermelho, pelo sinal. Numa tela de mercado a cor é a primeira coisa que o
    // olho pega, e um número de queda em amarelo se lê como um de alta.
    Some((texto, crate::invest::modules::heatmap::tom(Some(t.dia))))
}

/// Da maior alta para a maior queda, e quem ainda não tem preço no fim.
///
/// **Aqui e não em cada tela.** O cartão da home ordenava assim e a tela cheia não, então
/// entrar no módulo embaralhava a lista que se acabou de ler. Uma ordenação escrita duas
/// vezes é uma que diverge.
///
/// Um traço não é uma queda: sem cotação, o ativo vai para o fim em vez de disputar o
/// fundo da lista com quem de fato caiu.
pub fn ordenar(ctx: &Ctx, ativos: &mut [AssetId]) {
    let variacao = |a: &AssetId| {
        ctx.market
            .quote(a)
            .and_then(|q| q.variacao().filter(|_| q.grade.ao_vivo()))
    };
    ativos.sort_by(|a, b| match (variacao(a), variacao(b)) {
        (Some(x), Some(y)) => y.total_cmp(&x),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
}

struct Vista {
    lista: Lista,
    form: Option<Formulario>,
    /// O ativo aberto, quando há um. `None` é a lista.
    aberto: Option<Aberto>,
    /// A ordem **congelada no instante em que o módulo abriu**.
    ///
    /// A ordenação é por variação do dia, e ela muda a cada cotação nova. Reordenar a
    /// tela cheia ao vivo faria a lista andar debaixo de quem está lendo — e como `Del`
    /// apaga o ativo sob o cursor, apagaria o errado. É a mesma disciplina de
    /// `TableFocus`, que congela a forma da lista ao entrar.
    ///
    /// O cartão da home continua reordenando a cada volta: lá não há cursor nem tecla
    /// sobre a linha, e o que se quer é ver o que subiu agora.
    ordem: Vec<AssetId>,
}

impl Vista {
    /// Os ativos na ordem congelada, sem o que saiu e com o que entrou depois no fim.
    ///
    /// Um ativo adicionado com `Ctrl+A` tem que aparecer sem fechar o módulo; ele entra
    /// no fim, e não no meio, porque o meio é onde o cursor está.
    fn ativos(&self, ctx: &Ctx) -> Vec<AssetId> {
        let atuais = ativos(ctx);
        let mut v: Vec<AssetId> = self
            .ordem
            .iter()
            .filter(|a| atuais.contains(a))
            .cloned()
            .collect();
        for a in atuais {
            if !v.contains(&a) {
                v.push(a);
            }
        }
        v
    }
}

/// As linhas da tabela, na mesma ordem de `ativos` — é o que permite resolver o cursor
/// filtrado para o ativo certo. Ver `Lista::atual`.
fn linhas(ctx: &Ctx, ativos: &[AssetId]) -> Vec<Row> {
    ativos
        .iter()
        .map(|a| {
            mercado::linha(
                a,
                ctx.market.quote(a),
                mercado::na_carteira(ctx, a),
                ctx.agora,
                ctx,
            )
        })
        .collect()
}

impl ModuleView for Vista {
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

        if let Some(aberto) = &self.aberto {
            return detalhe(ctx, aberto);
        }

        let ativos = self.ativos(ctx);
        if ativos.is_empty() {
            return Layout::one(Pane::Empty {
                title: "Cotações".into(),
                note: "Nada acompanhado ainda.\n\n\
                       Ctrl+A adiciona um ativo. Toda posição da carteira entra aqui \
                       sozinha, então isto é para o que você ainda não tem.\n\n\
                       Sem chave de API, o que responde ao vivo é cripto (BINANCE/BTCBRL), \
                       câmbio (FX/USDBRL) e as séries do Banco Central (BCB/CDI)."
                    .into(),
            });
        }

        let rows = linhas(ctx, &ativos);

        Layout::one(Pane::Table {
            title: format!("Cotações · {}", calc::plural(rows.len(), "ativo", "ativos")),
            headers: mercado::CABECALHO.iter().map(|s| s.to_string()).collect(),
            rows,
            selected: Some(self.lista.selecionado),
            query: self.lista.busca.clone(),
            note: mercado::nota_fontes(ctx),
        })
    }

    fn key(&mut self, key: KeyEvent, ctx: &Ctx) -> Outcome {
        if let Some(form) = &mut self.form {
            if form.tecla(key) {
                return match AssetId::parse(&form.valor("ativo")) {
                    Ok(a) => {
                        self.form = None;
                        Outcome::Editar(vec![Edit::AddWatch(a)])
                    }
                    Err(e) => {
                        form.erro = Some(e);
                        Outcome::Ok
                    }
                };
            }
            return Outcome::Ok;
        }

        let ativos = self.ativos(ctx);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        // Com um ativo aberto, as teclas são outras — e nenhuma cai na lista atrás: uma
        // letra ali viraria busca numa tabela que não está na tela, e o `Del` apagaria da
        // watchlist um ativo que ninguém está vendo.
        if let Some(aberto) = &mut self.aberto {
            return match key.code {
                // ←/→ trocam a janela do gráfico; ↑/↓ trocam de ativo sem sair, seguindo a
                // mesma ordem congelada da lista — é o que faz varrer a watchlist gráfico
                // a gráfico ser um gesto só.
                KeyCode::Left | KeyCode::Right => {
                    aberto.virar();
                    Outcome::Ok
                }
                KeyCode::Up | KeyCode::Down => {
                    let delta = match key.code {
                        KeyCode::Up => -1,
                        _ => 1,
                    };
                    self.lista
                        .mover(delta, self.lista.visiveis(&linhas(ctx, &ativos)).len());
                    if let Some(a) = self
                        .lista
                        .atual(&linhas(ctx, &ativos))
                        .and_then(|i| ativos.get(i))
                    {
                        aberto.ativo = a.clone();
                    }
                    Outcome::Ok
                }
                _ => Outcome::Ignorada,
            };
        }

        match key.code {
            KeyCode::Char('a') if ctrl => {
                self.form = Some(Formulario::novo(
                    "Acompanhar um ativo",
                    vec![Field::text(
                        "ativo",
                        "",
                        "BINANCE/BTCBRL, FX/USDBRL, BCB/CDI, B3/PETR4, US/AAPL",
                    )],
                ));
                Outcome::Ok
            }
            KeyCode::Delete => {
                // Só sai da watchlist. Uma posição continua aparecendo, porque ela entra
                // sozinha — e a confirmação diz isso, senão parece que a tecla não funcionou.
                match self
                    .lista
                    .atual(&linhas(ctx, &ativos))
                    .and_then(|i| ativos.get(i))
                {
                    Some(a) if ctx.portfolio.watchlist.contains(a) => {
                        Outcome::Editar(vec![Edit::RemoveWatch(a.clone())])
                    }
                    _ => Outcome::Ok,
                }
            }
            // O ativo sob o cursor — pelo índice **visível**, que com busca ativa não é o
            // mesmo da lista original. Sem isto, o Enter abriria o gráfico sempre da
            // primeira linha, qualquer que fosse a escolhida.
            KeyCode::Enter => {
                if let Some(a) = self
                    .lista
                    .atual(&linhas(ctx, &ativos))
                    .and_then(|i| ativos.get(i))
                {
                    self.aberto = Some(Aberto::novo(a.clone()));
                }
                Outcome::Ok
            }
            _ => match self.lista.tecla(key, ativos.len()) {
                true => Outcome::Ok,
                false => Outcome::Ignorada,
            },
        }
    }

    fn escape(&mut self) -> Escape {
        // O gráfico fecha para a lista, e só o segundo `Esc` sai do módulo: quem entrou
        // num papel quer voltar para a linha dele, não para a aba.
        if self.form.take().is_some() || self.aberto.take().is_some() {
            return Escape::Consumido;
        }
        self.lista.escape()
    }

    fn hint(&self) -> String {
        if self.aberto.is_some() {
            return hint(&[
                "←/→ período",
                "↑/↓ trocar de ativo",
                "Esc volta para a lista",
            ]);
        }
        hint(&[
            "↑/↓ andar",
            "digite para buscar",
            "Ctrl+A acompanhar",
            "Del tirar",
            "Enter gráfico e posição",
            "Esc sair",
        ])
    }
}

/// Um ativo aberto: o gráfico de preço dele e o que a carteira tem dele.
///
/// Vive dentro de Cotações e não num módulo à parte porque o assunto é a linha sob o
/// cursor — e um Enter que saísse do módulo perderia a busca e a posição da lista para as
/// quais ele teria que voltar.
struct Aberto {
    ativo: AssetId,
    span: Span,
}

impl Aberto {
    fn novo(ativo: AssetId) -> Aberto {
        // Trinta dias: é a janela em que a forma do preço ainda se lê numa linha de
        // terminal, e a que toda fonte devolve sem paginar.
        Aberto {
            ativo,
            span: Span::Mes,
        }
    }

    fn janela(&self) -> &'static str {
        match self.span {
            Span::Mes => "30 dias",
            Span::Ano => "1 ano",
        }
    }

    /// As duas janelas que os provedores sabem responder — ver `Span`.
    fn virar(&mut self) {
        self.span = match self.span {
            Span::Mes => Span::Ano,
            Span::Ano => Span::Mes,
        };
    }
}

/// O gráfico em cima, ocupando a largura, e os números embaixo em colunas.
///
/// Nessa ordem porque é a pergunta em ordem: primeiro como o preço chegou até aqui,
/// depois o que isso significa para quem tem o papel. E lado a lado embaixo porque o
/// gráfico é o que precisa de largura; dois blocos de fatos curtos empilhados jogariam o
/// segundo para fora da tela.
fn detalhe(ctx: &Ctx, aberto: &Aberto) -> Layout {
    let estado = ctx.historico.get(ctx.providers, &aberto.ativo, aberto.span);
    let velas: &[Candle] = match &estado {
        Estado::Pronta(v) => v,
        _ => &[],
    };
    let posicao = posicao_de(ctx, &aberto.ativo);
    let pm = posicao.as_ref().and_then(|p| p.preco_medio);

    let mut colunas = vec![
        (
            1,
            Layout::one(painel_carteira(&aberto.ativo, posicao.as_ref())),
        ),
        (1, Layout::one(painel_cotacao(ctx, aberto, velas))),
    ];
    // A terceira coluna só existe quando há o que pôr nela: uma posição em duas
    // corretoras é a única em que a soma esconde alguma coisa.
    if let Some(p) = posicao.as_ref().filter(|p| p.contas.len() > 1) {
        colunas.push((1, Layout::one(tabela_contas(p))));
    }

    Layout::rows(vec![
        (3, Layout::one(grafico(aberto, &estado, velas, pm))),
        (2, Layout::cols(colunas)),
    ])
}

/// A linha de fechamento da janela, com o preço médio desenhado por cima.
///
/// Sem série não é uma tela vazia: cada motivo diz o que é, porque «a fonte não tem
/// histórico deste ativo» e «a busca falhou» pedem coisas diferentes de quem lê — e o
/// resumo da carteira continua embaixo nos dois casos.
fn grafico(aberto: &Aberto, estado: &Estado, velas: &[Candle], pm: Option<f64>) -> Pane {
    let titulo = format!("{} · {}", aberto.ativo.short(), aberto.janela());
    if velas.is_empty() {
        return Pane::Empty {
            title: titulo,
            note: match estado {
                Estado::Buscando => "Buscando a série…".into(),
                Estado::SemFonte => format!(
                    "{} não tem série histórica.\n\n\
                     Preço informado não tem gráfico — ele é um número digitado, não uma \
                     leitura de mercado. Têm série os pares da Binance, o câmbio e as \
                     séries do Banco Central.",
                    aberto.ativo.short()
                ),
                _ => "A busca da série não respondeu. ←/→ tenta a outra janela.".into(),
            },
        };
    }

    let serie: Vec<f64> = velas.iter().map(|c| c.fechamento).collect();
    // Um rótulo por ponto: o desenho escolhe quantos cabem e descarta o resto — ver
    // `x_labels`.
    let x_labels: Vec<String> = velas
        .iter()
        .map(|c| {
            let d = Data::de_epoch(c.em, tempo::BRT_OFFSET);
            format!("{:02}/{:02}", d.dia, d.mes)
        })
        .collect();
    // O preço médio é o único traço que transforma um gráfico numa decisão: abaixo dele a
    // posição está no prejuízo, acima não.
    let marks = match pm.filter(|p| *p > 0.0) {
        Some(pm) => vec![(pm, "seu PM".to_string())],
        None => Vec::new(),
    };

    Pane::Chart {
        title: titulo,
        series: serie,
        format: calc::preco,
        marks,
        x_labels,
        note: Some(format!(
            "{} · ←/→ troca o período",
            calc::plural(velas.len(), "fechamento", "fechamentos")
        )),
    }
}

/// O que a carteira tem deste ativo — ou o convite a não ter.
fn painel_carteira(ativo: &AssetId, posicao: Option<&NaCarteira>) -> Pane {
    let Some(p) = posicao else {
        return Pane::Empty {
            title: "Na carteira".into(),
            note: format!(
                "{} não está na carteira — é um ativo só acompanhado.\n\n\
                 Com uma posição, aparece aqui a quantidade, o preço médio, quanto vale \
                 hoje e o P&L.",
                ativo.short()
            ),
        };
    };

    let mut rows = vec![
        (
            "quantidade".to_string(),
            calc::preco(p.quantidade),
            Tone::Normal,
        ),
        (
            "preço médio".to_string(),
            // Um traço não, uma frase: sem preço médio informado não há custo nem P&L, e
            // o painel inteiro abaixo fica com traços que precisam desta explicação.
            p.preco_medio
                .map(calc::preco)
                .unwrap_or_else(|| "não informado".into()),
            match p.preco_medio {
                Some(_) => Tone::Normal,
                None => Tone::Dim,
            },
        ),
        (
            "custo".to_string(),
            match p.custo > 0.0 {
                true => reais(p.custo),
                false => "—".into(),
            },
            Tone::Dim,
        ),
        (
            "vale hoje".to_string(),
            p.mercado.map(reais).unwrap_or_else(|| "—".into()),
            Tone::Normal,
        ),
        (
            "P&L".to_string(),
            match (p.pnl, p.pnl_pct()) {
                (Some(v), Some(pct)) => format!("{} ({})", reais(v), calc::pct(pct)),
                (Some(v), None) => reais(v),
                _ => "—".into(),
            },
            tom(p.pnl),
        ),
        (
            "no dia".to_string(),
            p.dia.map(reais).unwrap_or_else(|| "—".into()),
            tom(p.dia),
        ),
        (
            "peso na carteira".to_string(),
            p.peso.map(calc::pct_simples).unwrap_or_else(|| "—".into()),
            Tone::Dim,
        ),
    ];
    // As ressalvas por último, e só quando existem — cada uma explica um traço acima em
    // vez de deixá-lo passar por «zero».
    if p.contas.len() > 1 {
        rows.push((
            "origens".to_string(),
            format!("{} posições somadas", p.contas.len()),
            Tone::Dim,
        ));
    }
    if p.sem_preco > 0 {
        rows.push((
            "fora do total".to_string(),
            format!("{} sem preço ou sem câmbio", p.sem_preco),
            Tone::Aviso,
        ));
    }

    Pane::Facts {
        title: "Na carteira".into(),
        rows,
    }
}

/// O retrato do mercado agora, e o que a janela do gráfico diz.
fn painel_cotacao(ctx: &Ctx, aberto: &Aberto, velas: &[Candle]) -> Pane {
    let mut rows = Vec::new();
    match ctx.market.quote(&aberto.ativo) {
        Some(q) => {
            rows.push(("último".to_string(), calc::preco(q.preco), Tone::Normal));
            rows.push((
                "fonte".to_string(),
                format!("{}·{}", q.fonte, tempo::idade(q.idade(ctx.agora))),
                // Amarelo é «este número não é de agora» — a mesma regra da tabela.
                match q.grade.ao_vivo() {
                    true => Tone::Dim,
                    false => Tone::Aviso,
                },
            ));
            let variacao = q.variacao().filter(|_| q.grade.ao_vivo());
            rows.push((
                "variação do dia".to_string(),
                variacao.map(calc::pct).unwrap_or_else(|| "—".into()),
                tom(variacao),
            ));
            if let Some(v) = q.max24 {
                rows.push(("máx 24h".to_string(), calc::preco(v), Tone::Dim));
            }
            if let Some(v) = q.min24 {
                rows.push(("mín 24h".to_string(), calc::preco(v), Tone::Dim));
            }
            if let Some(v) = q.volume.filter(|v| *v > 0.0) {
                rows.push(("volume".to_string(), calc::moeda(v), Tone::Dim));
            }
        }
        None => rows.push(("último".to_string(), sem_preco(ctx), Tone::Dim)),
    }

    // O que a série responde e a cotação não: quanto andou na janela que está desenhada,
    // e entre que preços.
    if let (Some(primeiro), Some(ultimo)) = (velas.first(), velas.last())
        && primeiro.fechamento > 0.0
    {
        let retorno = (ultimo.fechamento / primeiro.fechamento - 1.0) * 100.0;
        rows.push((
            format!("no período ({})", aberto.janela()),
            calc::pct(retorno),
            tom(Some(retorno)),
        ));
        let (mut min, mut max) = (f64::INFINITY, f64::NEG_INFINITY);
        for c in velas {
            min = min.min(c.fechamento);
            max = max.max(c.fechamento);
        }
        rows.push((
            "faixa do período".to_string(),
            format!("{} a {}", calc::preco(min), calc::preco(max)),
            Tone::Dim,
        ));
    }

    Pane::Facts {
        title: "Cotação".into(),
        rows,
    }
}

/// De onde vem a posição, quando ela vem de mais de um lugar.
///
/// A soma acima responde «quanto eu tenho»; esta tabela responde «onde está» — que é a
/// pergunta de quem vai vender.
fn tabela_contas(posicao: &NaCarteira) -> Pane {
    Pane::Table {
        title: "Onde está".into(),
        headers: vec!["Origem".into(), "Qtd".into(), "Vale hoje".into()],
        rows: posicao
            .contas
            .iter()
            .map(|(origem, qtd, valor)| {
                Row::new(vec![
                    origem.clone(),
                    calc::preco(*qtd),
                    valor.map(reais).unwrap_or_else(|| "—".into()),
                ])
            })
            .collect(),
        selected: None,
        query: String::new(),
        note: None,
    }
}

fn reais(valor: f64) -> String {
    format!("R$ {}", calc::moeda(valor))
}

/// As posições de um ativo, somadas — com o que a soma teve que deixar de fora.
struct NaCarteira {
    quantidade: f64,
    /// Na moeda da posição, que é a do gráfico. **Não** sai do custo em reais: aquele já
    /// passou pelo câmbio, e um PM em reais desenhado sobre uma série em dólar seria uma
    /// linha no lugar errado. `None` também quando as posições estão em moedas
    /// diferentes — uma média entre reais e dólares não é um preço.
    preco_medio: Option<f64>,
    custo: f64,
    /// `None` quando nenhuma linha chegou a BRL: falta preço, ou falta câmbio.
    mercado: Option<f64>,
    pnl: Option<f64>,
    dia: Option<f64>,
    /// Quanto este ativo pesa no patrimônio inteiro.
    peso: Option<f64>,
    /// Origem, quantidade e valor de cada posição — uma por corretora ou importação.
    contas: Vec<(String, f64, Option<f64>)>,
    sem_preco: usize,
}

impl NaCarteira {
    /// O P&L em porcentagem, sobre o custo que se conhece. Mesma regra de
    /// `carteira::Totais::pnl_pct`: sem custo não há porcentagem, em vez de uma sobre zero.
    fn pnl_pct(&self) -> Option<f64> {
        let pnl = self.pnl?;
        (self.custo > 0.0).then(|| pnl / self.custo * 100.0)
    }
}

/// O que a carteira tem deste ativo. `None` quando não tem nenhuma posição nele.
fn posicao_de(ctx: &Ctx, ativo: &AssetId) -> Option<NaCarteira> {
    let todas = carteira::linhas(ctx.portfolio, ctx.market, ctx.agora);
    let totais = carteira::totais(&todas);
    let minhas: Vec<&carteira::Linha> =
        todas.iter().filter(|l| l.posicao.ativo == *ativo).collect();
    if minhas.is_empty() {
        return None;
    }

    let mut r = NaCarteira {
        quantidade: 0.0,
        preco_medio: None,
        custo: 0.0,
        mercado: None,
        pnl: None,
        dia: None,
        peso: None,
        contas: Vec::new(),
        sem_preco: 0,
    };
    let (mut mercado, mut com_mercado) = (0.0, false);
    let (mut pnl, mut com_pnl) = (0.0, false);
    let (mut dia, mut com_dia) = (0.0, false);
    // O preço médio consolidado é a média **ponderada pela quantidade** das posições que
    // informam um: comprar 10 a 20 e 90 a 30 dá 29, não 25.
    let (mut soma_pm, mut qtd_com_pm) = (0.0, 0.0);
    let moedas: Vec<Moeda> = minhas.iter().map(|l| l.posicao.moeda).collect();
    let moeda_unica = moedas.windows(2).all(|par| par[0] == par[1]);

    for l in &minhas {
        r.quantidade += l.posicao.quantidade;
        if let Some(v) = l.custo_brl {
            r.custo += v;
        }
        if let Some(pm) = l.posicao.preco_medio {
            soma_pm += pm * l.posicao.quantidade;
            qtd_com_pm += l.posicao.quantidade;
        }
        match l.mercado_brl {
            Some(v) => {
                mercado += v;
                com_mercado = true;
            }
            None => r.sem_preco += 1,
        }
        if let Some(v) = l.pnl_brl {
            pnl += v;
            com_pnl = true;
        }
        if let Some(v) = l.dia_brl {
            dia += v;
            com_dia = true;
        }
        // O nome que quem importou reconhece: a conta quando ela existe, senão a fonte
        // que trouxe a posição.
        let origem = match l.posicao.conta.trim().is_empty() {
            true => l.posicao.fonte.clone(),
            false => l.posicao.conta.clone(),
        };
        r.contas.push((origem, l.posicao.quantidade, l.mercado_brl));
    }

    r.preco_medio = (moeda_unica && qtd_com_pm > 0.0).then(|| soma_pm / qtd_com_pm);
    r.mercado = com_mercado.then_some(mercado);
    r.pnl = com_pnl.then_some(pnl);
    r.dia = com_dia.then_some(dia);
    r.peso = match (r.mercado, totais.mercado > 0.0) {
        (Some(v), true) => Some(v / totais.mercado * 100.0),
        _ => None,
    };
    Some(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invest::model::{Classe, Market, Moeda, Portfolio, Position};
    use crate::invest::provider::MarketSnapshot;

    fn ctx<'a>(
        p: &'a Portfolio,
        m: &'a MarketSnapshot,
        pr: &'a std::sync::Arc<crate::invest::provider::ProviderSet>,
    ) -> Ctx<'a> {
        crate::invest::module::ctx_de_teste(p, m, pr)
    }

    #[test]
    fn toda_posicao_entra_sem_precisar_ser_acompanhada() {
        let mut p = Portfolio::default();
        p.posicoes.push(Position {
            fonte: "t".into(),
            conta: String::new(),
            ativo: AssetId::new(Market::B3, "PETR4"),
            classe: Classe::Acao,
            quantidade: 1.0,
            preco_medio: Some(1.0),
            moeda: Moeda::Brl,
            preco_manual: None,
            preco_manual_em: None,
            atualizado_em: 0,
            carteira: None,
        });
        p.watchlist.push(AssetId::new(Market::Binance, "BTCBRL"));
        let m = MarketSnapshot::default();
        let pr = crate::invest::module::providers_de_teste();
        let a = ativos(&ctx(&p, &m, &pr));
        assert_eq!(a.len(), 2);
        assert!(a.iter().any(|x| x.symbol == "PETR4"));
    }

    /// Uma posição pronta para os testes, com o mínimo que a carteira exige.
    fn posicao(ativo: &AssetId, conta: &str, quantidade: f64, pm: Option<f64>) -> Position {
        Position {
            fonte: "t".into(),
            conta: conta.into(),
            ativo: ativo.clone(),
            classe: Classe::Acao,
            quantidade,
            preco_medio: pm,
            moeda: Moeda::Brl,
            preco_manual: None,
            preco_manual_em: None,
            atualizado_em: 0,
            carteira: None,
        }
    }

    fn vista(ordem: Vec<AssetId>) -> Vista {
        Vista {
            lista: Lista::default(),
            form: None,
            aberto: None,
            ordem,
        }
    }

    /// O Enter abria um módulo que não existe mais, e a aba caía no dashboard. Agora ele
    /// abre aqui dentro — e no ativo que está sob o cursor, não no primeiro da lista.
    #[test]
    fn enter_abre_o_ativo_sob_o_cursor() {
        let mut p = Portfolio::default();
        p.watchlist.push(AssetId::new(Market::B3, "PETR4"));
        p.watchlist.push(AssetId::new(Market::Binance, "BTCBRL"));
        let m = MarketSnapshot::default();
        let pr = crate::invest::module::providers_de_teste();
        let c = ctx(&p, &m, &pr);

        let mut v = vista(ativos(&c));
        // Com a busca ativa o cursor anda pela lista filtrada: o índice 0 visível é o
        // segundo ativo da lista original.
        v.lista.busca = "btc".into();
        v.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &c);
        assert_eq!(
            v.aberto.as_ref().map(|a| a.ativo.symbol.clone()),
            Some("BTCBRL".to_string())
        );
    }

    /// O primeiro `Esc` fecha o gráfico; só o segundo sai do módulo.
    #[test]
    fn esc_fecha_o_grafico_antes_de_sair() {
        let mut v = vista(Vec::new());
        v.aberto = Some(Aberto::novo(AssetId::new(Market::B3, "PETR4")));
        assert_eq!(v.escape(), Escape::Consumido);
        assert!(v.aberto.is_none());
        assert_eq!(v.escape(), Escape::Nao);
    }

    /// Duas compras do mesmo papel somam a quantidade e **ponderam** o preço médio: 10 a
    /// 20 e 90 a 30 dá 29, não 25.
    #[test]
    fn duas_posicoes_do_mesmo_ativo_somam_e_ponderam_o_pm() {
        let petr = AssetId::new(Market::B3, "PETR4");
        let mut p = Portfolio::default();
        p.posicoes
            .push(posicao(&petr, "corretora A", 10.0, Some(20.0)));
        p.posicoes
            .push(posicao(&petr, "corretora B", 90.0, Some(30.0)));
        let m = MarketSnapshot::default();
        let pr = crate::invest::module::providers_de_teste();

        let r = posicao_de(&ctx(&p, &m, &pr), &petr).expect("tem posição");
        assert_eq!(r.quantidade, 100.0);
        assert_eq!(r.preco_medio, Some(29.0));
        assert_eq!(r.contas.len(), 2);
        // Sem cotação nenhuma, o valor de hoje não existe — e não é zero.
        assert!(r.mercado.is_none());
        assert_eq!(r.sem_preco, 2);
    }

    /// Sem preço médio informado não há PM consolidado — e não um zero, que se leria como
    /// «custou nada».
    #[test]
    fn sem_preco_medio_informado_nao_ha_pm() {
        let btc = AssetId::new(Market::Binance, "BTCBRL");
        let mut p = Portfolio::default();
        p.posicoes.push(posicao(&btc, "", 1.0, None));
        let m = MarketSnapshot::default();
        let pr = crate::invest::module::providers_de_teste();
        let r = posicao_de(&ctx(&p, &m, &pr), &btc).expect("tem posição");
        assert!(r.preco_medio.is_none());
        assert!(r.pnl_pct().is_none());
    }

    /// Um ativo só acompanhado diz isso com todas as letras, em vez de um painel de
    /// traços que pareceria uma posição zerada.
    #[test]
    fn ativo_so_acompanhado_diz_que_nao_esta_na_carteira() {
        let pane = painel_carteira(&AssetId::new(Market::B3, "VALE3"), None);
        match pane {
            Pane::Empty { note, .. } => assert!(note.contains("não está na carteira")),
            _ => panic!("esperava a tela de ausência"),
        }
    }

    #[test]
    fn um_ativo_em_dois_lugares_aparece_uma_vez_so() {
        let mut p = Portfolio::default();
        let btc = AssetId::new(Market::Binance, "BTCBRL");
        p.posicoes.push(Position {
            fonte: "t".into(),
            conta: String::new(),
            ativo: btc.clone(),
            classe: Classe::Cripto,
            quantidade: 1.0,
            preco_medio: Some(1.0),
            moeda: Moeda::Brl,
            preco_manual: None,
            preco_manual_em: None,
            atualizado_em: 0,
            carteira: None,
        });
        p.watchlist.push(btc);
        let m = MarketSnapshot::default();
        let pr = crate::invest::module::providers_de_teste();
        assert_eq!(ativos(&ctx(&p, &m, &pr)).len(), 1);
    }
}
