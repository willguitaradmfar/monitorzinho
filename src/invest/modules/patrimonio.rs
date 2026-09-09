//! Patrimônio: quanto eu tinha, quanto tenho, e o que dessa diferença foi mercado.
//!
//! Um patrimônio que subiu dez mil no mês não diz nada sozinho. Pode ser dez mil de
//! aporte com o mercado parado, ou dois mil de aporte com oito mil de valorização, ou
//! quinze mil de aporte com cinco mil de perda. As três são situações completamente
//! diferentes, e a curva sozinha mostra as três iguais.
//!
//! Por isso a curva é **sempre decomposta**, e por isso a variação de mercado é o resto.

use crossterm::event::{KeyCode, KeyEvent};

use crate::invest::calc;
use crate::invest::carteira;
use crate::invest::model::AssetId;
use crate::invest::module::{
    Bar, Ctx, Escape, Group, InvestModule, Layout, ModuleView, Need, Outcome, Pane, Tone,
};
use crate::invest::modules::comum::hint;
use crate::invest::provider::Span;
use crate::invest::serie::{self, Ponto};
use crate::invest::tempo::{self, Data};

pub struct Patrimonio;

const PERIODOS: [(&str, u32); 4] = [
    ("1 mês", 30),
    ("3 meses", 92),
    ("12 meses", 365),
    ("tudo", 36500),
];

impl InvestModule for Patrimonio {
    fn id(&self) -> &'static str {
        "patrimonio"
    }
    fn name(&self) -> &'static str {
        "Patrimônio"
    }
    fn description(&self) -> &'static str {
        "A curva do total, decomposta em aporte, mercado e proventos"
    }
    fn destaque(&self) -> u8 {
        9
    }
    fn group(&self) -> Group {
        Group::Carteira
    }
    fn needs(&self) -> &'static [Need] {
        &[Need::Posicoes]
    }
    fn keywords(&self) -> &'static str {
        "evolução curva histórico crescimento aporte valorização"
    }

    fn summary(&self, ctx: &Ctx) -> String {
        // O resumo não lê o arquivo: `summary` roda para todos os módulos a cada tique
        // da aba, e abrir arquivo aqui transformaria a lista no lugar mais caro do
        // programa. Ele diz o que dá para dizer sem I/O.
        match ctx.portfolio.posicoes.is_empty() {
            true => "sem posições — a curva começa quando houver".into(),
            false => "abra para ver a curva e a decomposição".into(),
        }
    }

    fn widget(&self, ctx: &Ctx) -> Option<Pane> {
        if ctx.portfolio.posicoes.is_empty() {
            return None;
        }
        // A curva mora em arquivo, e `widget` não lê arquivo — roda para todo módulo a
        // cada volta da aba. O que cabe aqui é a foto de hoje, que já está em memória.
        let linhas = carteira::linhas(ctx.portfolio, ctx.market, ctx.agora);
        let t = carteira::totais(&linhas);
        let mut rows = vec![(
            "patrimônio".into(),
            format!("R$ {}", calc::moeda(t.mercado)),
            Tone::Destaque,
        )];
        if t.custo != 0.0 {
            rows.push((
                "custo informado".into(),
                format!("R$ {}", calc::moeda(t.custo)),
                Tone::Normal,
            ));
        }
        // O rótulo diz **sobre quantas** posições o P&L fala. Com dezenove das vinte e uma
        // linhas sem preço médio informado, um «P&L» solto seria lido como o da carteira
        // inteira — e ele é o de duas.
        let com_custo = ctx.portfolio.posicoes.len().saturating_sub(t.sem_custo);
        rows.push((
            match t.sem_custo {
                0 => "P&L".into(),
                _ => format!("P&L de {com_custo} de {}", ctx.portfolio.posicoes.len()),
            },
            match t.pnl_pct() {
                Some(p) => format!("R$ {} ({})", calc::moeda(t.pnl), calc::pct(p)),
                None => format!("R$ {}", calc::moeda(t.pnl)),
            },
            crate::invest::modules::heatmap::tom(t.pnl_pct()),
        ));
        if t.dia != 0.0 {
            rows.push((
                "no dia".into(),
                // Reais **e** porcentagem: «R$ 10 mil» não diz se o dia foi bom sem se
                // saber sobre quanto, e a porcentagem sozinha não diz o tamanho.
                match t.dia_pct() {
                    Some(p) => format!("R$ {} ({})", calc::moeda(t.dia), calc::pct(p)),
                    None => format!("R$ {}", calc::moeda(t.dia)),
                },
                crate::invest::modules::heatmap::tom(Some(t.dia)),
            ));
        }
        if t.sem_custo > 0 {
            rows.push((
                "sem preço médio".into(),
                format!(
                    "{} — fora do P&L",
                    calc::plural(t.sem_custo, "posição", "posições")
                ),
                Tone::Dim,
            ));
        }
        // O rumo, pelas duas fontes — a mesma coisa que a tela cheia faz.
        //
        // Antes o cartão olhava só a série do patrimônio, e ela **nunca** vai ter pontos
        // em agosto: ela começou a existir esta semana. O resultado era «sem série no
        // período» para sempre, num painel que existe justamente para não precisar entrar
        // no módulo. Agora ele reconstrói do histórico de preço, como a tela cheia — e o
        // cache é o mesmo, então não custa uma busca a mais.
        let hoje_ = Data::de_epoch(ctx.agora, tempo::BRT_OFFSET);
        for (nome, de, ate) in periodos(&hoje_) {
            let (texto, tom) = match serie::rumo(ctx.patrimonio, &de, &ate) {
                Some(r) if r.completo() => (
                    format!(
                        "{} · {}",
                        r.pct().map(calc::pct).unwrap_or_else(|| "—".into()),
                        reais(r.total)
                    ),
                    crate::invest::modules::heatmap::tom(r.pct()),
                ),
                _ => match das_posicoes(ctx, &de, &ate) {
                    Ok(r) => (
                        format!("{} · {}", calc::pct(r.pct), reais(r.reais)),
                        crate::invest::modules::heatmap::tom(Some(r.pct)),
                    ),
                    Err(e) => (e, Tone::Dim),
                },
            };
            rows.push((rotulo(nome, &de, &ate), texto, tom));
        }

        Some(Pane::Facts {
            title: String::new(),
            rows,
        })
    }

    fn open(&self, _ctx: &Ctx, _alvo: Option<&AssetId>) -> Box<dyn ModuleView> {
        Box::new(Vista {
            pontos: serie::load(),
            periodo: 2,
            percentual: false,
        })
    }
}

struct Vista {
    pontos: Vec<Ponto>,
    periodo: usize,
    percentual: bool,
}

/// Os dois períodos que respondem «para onde isto está indo».
///
/// **Fechados, de calendário.** A semana passada é de segunda a sexta e o mês passado é do
/// dia 1 ao último — coisas que já aconteceram e não mudam mais. Uma janela móvel de sete
/// ou trinta dias dá um número diferente a cada dia e nunca fecha, e não é o que se
/// entende por «a performance da semana passada».
fn periodos(hoje: &Data) -> [(&'static str, Data, Data); 2] {
    let (s1, s2) = tempo::semana_anterior(hoje);
    let (m1, m2) = tempo::mes_passado(hoje);
    [("semana", s1, s2), ("mês", m1, m2)]
}

/// `+R$ 77.000` — com o sinal, porque um número de reais sem sinal ao lado de uma
/// porcentagem com sinal se lê como se fosse sempre positivo.
fn reais(v: f64) -> String {
    match v < 0.0 {
        true => format!("-R$ {}", calc::moeda(-v)),
        false => format!("+R$ {}", calc::moeda(v)),
    }
}

/// `semana 31/08–04/09` — o rótulo com o período dentro.
///
/// As datas ficam no rótulo e não no valor porque a dúvida que elas tiram é «de quando a
/// quando», que é uma pergunta sobre o nome da linha. Curto porque esta é a coluna da
/// esquerda de uma tabela de dois campos: um rótulo longo empurra todos os números.
fn rotulo(nome: &str, de: &Data, ate: &Data) -> String {
    format!(
        "{nome} {:02}/{:02}–{:02}/{:02}",
        de.dia, de.mes, ate.dia, ate.mes
    )
}

/// A série reconstruída, com a ressalva de como ela foi montada.
struct Reconstrucao {
    valores: Vec<f64>,
    /// Quantas posições entraram com série de verdade.
    com_serie: usize,
    /// Quantas entraram paradas no valor de hoje, por não terem cotação histórica.
    parados: usize,
}

impl Reconstrucao {
    /// A ressalva do rodapé. Vazia quando toda a carteira tem série — aí não há o que
    /// ressalvar.
    fn ressalva(&self) -> Option<String> {
        (self.parados > 0).then(|| {
            format!(
                "hoje é o valor ao vivo · {} com série · {} sem cotação, parados",
                self.com_serie, self.parados
            )
        })
    }
}

/// O quanto as posições de hoje andaram num período, reconstruído do histórico de preço.
struct Reconstruido {
    reais: f64,
    pct: f64,
    /// Quantos ativos entraram na conta. Uma carteira em que só metade tem série responde
    /// por metade, e a tela diz isso — o número sozinho pareceria falar da carteira toda.
    ativos: usize,
}

/// Quanto valiam **as posições de hoje** em cada uma das datas pedidas.
///
/// Duas regras que fazem a série significar alguma coisa:
///
/// * **O mesmo conjunto de ativos em todas as datas.** A primeira versão somava dia a
///   dia o que estivesse pronto naquele instante, e um papel cuja série ainda não
///   tinha chegado era pulado calado — o total daquele dia caía um milhão e o gráfico
///   virava um degrau que não aconteceu.
/// * **Quem não tem cotação histórica entra parado no valor de hoje.** CDB, Tesouro e
///   crowdfunding não têm série que se busque, e descartá-los fazia o gráfico do
///   patrimônio mostrar R$ 919 mil de uma carteira de R$ 1,99 milhão. Parado é a
///   afirmação certa: não é que valessem zero, é que não se sabe o que mudou — e o
///   rodapé diz quantos estão assim.
fn reconstruir(ctx: &Ctx, datas: &[Data]) -> Result<Reconstrucao, String> {
    use crate::invest::historico::Estado;
    if datas.is_empty() {
        return Err("sem período".into());
    }
    let alvos: Vec<u64> = datas
        .iter()
        .map(|d| d.epoch_inicio(tempo::BRT_OFFSET) + 86_399)
        .collect();
    // O valor de hoje de cada linha, já em BRL e já com câmbio e preço informado
    // resolvidos — é ele que serve de piso para quem não tem série.
    let linhas = carteira::linhas(ctx.portfolio, ctx.market, ctx.agora);

    let mut buscando = 0usize;
    let mut parados = 0usize;
    let mut com_serie = 0usize;
    let mut valores = vec![0.0f64; datas.len()];

    for l in &linhas {
        let hoje = l.mercado_brl.unwrap_or(0.0);
        // Um ano de série cobre seis meses de gráfico e qualquer período passado sem
        // uma segunda busca.
        let velas = match ctx
            .historico
            .get(ctx.providers, &l.posicao.ativo, Span::Ano)
        {
            Estado::Pronta(v) if !v.is_empty() => Some(v),
            Estado::Buscando => {
                buscando += 1;
                None
            }
            _ => None,
        };
        // Só serve a série que cobre **todas** as datas: uma que começa no meio faria
        // o ativo aparecer do nada no gráfico.
        let cobre = velas
            .as_ref()
            .is_some_and(|v| alvos.iter().all(|a| v.iter().any(|c| c.em <= *a)));
        match (velas, cobre) {
            (Some(velas), true) => {
                com_serie += 1;
                for (i, a) in alvos.iter().enumerate() {
                    // O último pregão **até** a data, sem interpolar: um dia sem
                    // negócio não vira um preço inventado.
                    if let Some(c) = velas.iter().rfind(|c| c.em <= *a) {
                        valores[i] += l.posicao.quantidade * c.fechamento;
                    }
                }
            }
            _ => {
                parados += 1;
                for v in valores.iter_mut() {
                    *v += hoje;
                }
            }
        }
    }

    if buscando > 0 {
        return Err(format!("buscando o histórico de {buscando}…"));
    }
    if com_serie == 0 {
        return Err("nenhuma posição tem histórico para reconstruir".into());
    }
    Ok(Reconstrucao {
        valores,
        com_serie,
        parados,
    })
}

/// Quanto as posições de hoje andaram na janela, pelo histórico de preço delas.
///
/// Não é a variação do patrimônio: um aporte feito no meio da janela aparece aqui como
/// se o papel tivesse subido. Por isso a frase que sai daqui **se nomeia**.
fn das_posicoes(ctx: &Ctx, de: &Data, ate: &Data) -> Result<Reconstruido, String> {
    let r = reconstruir(ctx, &[*de, *ate])?;
    let (antes, depois) = (r.valores[0], r.valores[1]);
    let ativos = r.com_serie;
    if antes <= 0.0 {
        return Err("sem histórico para reconstruir".into());
    }
    Ok(Reconstruido {
        reais: depois - antes,
        pct: (depois / antes - 1.0) * 100.0,
        ativos,
    })
}

impl Vista {
    /// O rumo da semana e do mês, em linhas prontas para a tela.
    ///
    /// **Duas fontes, e a tela diz qual está sendo usada.** A série do patrimônio é a
    /// resposta certa: ela sabe de aporte, de retirada e de provento, e separa o que foi
    /// mercado do que foi dinheiro novo entrando. Ela só não existe nos primeiros dias.
    ///
    /// Enquanto isso, o histórico de preço das posições de **hoje** responde a pergunta
    /// vizinha: quanto estes papéis andaram. É outra pergunta — ela ignora o que foi
    /// comprado no meio do caminho —, e por isso a linha diz «posições de hoje» em vez de
    /// «patrimônio». Chamar as duas de a mesma coisa seria o erro caro deste módulo.
    fn rumo(&self, ctx: &Ctx) -> Vec<(String, String, Tone)> {
        let hoje = Data::de_epoch(ctx.agora, tempo::BRT_OFFSET);
        let mut linhas = Vec::new();
        for (nome, de, ate) in periodos(&hoje) {
            match serie::rumo(&self.pontos, &de, &ate) {
                // A série cobre o período: a resposta completa, com o mercado separado.
                Some(r) if r.completo() => linhas.push((
                    rotulo(nome, &de, &ate),
                    format!(
                        "{} · {} · mercado {}",
                        r.pct().map(calc::pct).unwrap_or_else(|| "—".into()),
                        reais(r.total),
                        reais(r.mercado)
                    ),
                    crate::invest::modules::heatmap::tom(r.pct()),
                )),
                // Sem série que cubra o período, vale o histórico de preço das posições de
                // hoje — outra pergunta, e por isso o rótulo muda junto.
                _ => {
                    let rotulo = format!("{} · posições", rotulo(nome, &de, &ate));
                    match das_posicoes(ctx, &de, &ate) {
                        Ok(r) => linhas.push((
                            rotulo,
                            format!(
                                "{} · {} ({} com série)",
                                calc::pct(r.pct),
                                reais(r.reais),
                                r.ativos
                            ),
                            crate::invest::modules::heatmap::tom(Some(r.pct)),
                        )),
                        Err(e) => linhas.push((rotulo, e, Tone::Dim)),
                    }
                }
            }
        }
        linhas
    }

    /// O total de patrimônio no fim de cada um dos últimos seis meses.
    fn mensal(&self, ctx: &Ctx) -> Pane {
        let hoje = Data::de_epoch(ctx.agora, tempo::BRT_OFFSET);
        let mut meses: Vec<(String, Data)> = Vec::new();
        let mut m = hoje;
        for _ in 0..6 {
            // O fim de cada mês; o do mês corrente é hoje, que ainda não fechou.
            let fim = match (m.ano, m.mes) == (hoje.ano, hoje.mes) {
                true => hoje,
                false => {
                    tempo::data_de_dias(tempo::dias_de(m.ano, m.mes, 1) - 1 + 32).mes_anterior()
                }
            };
            meses.push((format!("{}/{}", tempo::nome_mes(m.mes), m.ano % 100), fim));
            m = m.mes_anterior();
        }
        meses.reverse();
        self.grafico(ctx, "Patrimônio por mês · 6 meses", meses)
    }

    /// O total de patrimônio dia a dia, nos últimos sete dias.
    fn diario(&self, ctx: &Ctx) -> Pane {
        let hoje = Data::de_epoch(ctx.agora, tempo::BRT_OFFSET);
        let base = tempo::dias_de(hoje.ano, hoje.mes, hoje.dia);
        let dias: Vec<(String, Data)> = (0..7)
            .rev()
            .map(|i| {
                let d = tempo::data_de_dias(base - i);
                (format!("{:02}/{:02}", d.dia, d.mes), d)
            })
            .collect();
        self.grafico(ctx, "Patrimônio dia a dia · 7 dias", dias)
    }

    /// Um dos dois gráficos: uma coluna por período, o tempo correndo para a direita.
    ///
    /// `Pane::Chart` e não barras: numa lista de barras o tempo corre **para baixo**, e um
    /// gráfico de série temporal se lê da esquerda para a direita. O painel de gráfico já
    /// põe a base no mínimo da janela e escreve mín/máx no rodapé — sem isso, um
    /// patrimônio que varia dois por cento desenhado a partir do zero seria uma faixa
    /// reta, e a forma é a única coisa que este gráfico tem a dizer.
    fn grafico(&self, ctx: &Ctx, titulo: &str, pontos: Vec<(String, Data)>) -> Pane {
        let (rotulos, datas): (Vec<String>, Vec<Data>) = pontos.into_iter().unzip();
        // A série do patrimônio manda quando cobre todas as datas: ela é o valor de
        // verdade, com as quantidades que se tinha em cada dia.
        let da_serie: Option<Vec<f64>> = datas
            .iter()
            .map(|d| {
                self.pontos
                    .iter()
                    .rfind(|p| p.data() == Some(*d))
                    .map(|p| p.total_brl)
            })
            .collect();

        let (mut valores, nota) = match da_serie {
            Some(v) => (v, None),
            None => match reconstruir(ctx, &datas) {
                Ok(r) => {
                    let nota = r.ressalva();
                    (r.valores, nota)
                }
                Err(e) => {
                    return Pane::Empty {
                        title: titulo.into(),
                        note: format!(
                            "{e}.\n\n\
                             O gráfico sai da série do patrimônio, que ganha um ponto por \n\
                             dia, ou do histórico de preço das posições."
                        ),
                    };
                }
            },
        };

        // **A ponta direita é o patrimônio de agora — e só ela.**
        //
        // O histórico de preço termina no último pregão fechado, então o último ponto
        // reconstruído era o fechamento de dias atrás enquanto o painel ao lado mostrava o
        // valor ao vivo. Dois números diferentes para «hoje» na mesma tela.
        //
        // A primeira correção reescalava a **curva inteira** pela razão entre os dois, e
        // isso era pior: o valor de uma terça-feira passada mudava porque a PETR4 andou
        // hoje. Medido, a forma ficava igual e o nível deslizava alguns milhares a cada
        // abertura. O passado é fato e não se mexe; só o ponto de hoje é substituído.
        let hoje = Data::de_epoch(ctx.agora, tempo::BRT_OFFSET);
        if datas.last() == Some(&hoje)
            && let Some(ultimo) = valores.last_mut()
        {
            let agora =
                carteira::totais(&carteira::linhas(ctx.portfolio, ctx.market, ctx.agora)).mercado;
            if agora > 0.0 {
                *ultimo = agora;
            }
        }

        Pane::Chart {
            title: titulo.into(),
            series: valores,
            format: |v| format!("R$ {}", calc::moeda(v)),
            marks: Vec::new(),
            x_labels: rotulos,
            note: nota,
        }
    }

    /// A composição de hoje, para o dia em que ainda não há curva.
    ///
    /// Não é um consolo: é a decomposição que o módulo passa a mostrar **junto** com a
    /// curva depois. O que falta no primeiro dia é a série, não o retrato.
    /// O painel de cima: quanto se tem agora, e como isso está repartido.
    ///
    /// **Uma linha só, usada pelas duas telas.** Antes ela existia apenas no primeiro dia
    /// de uso; quando a série ganhava o segundo ponto a tela trocava a resposta de «quanto
    /// eu tenho» por uma curva, e o número que se vem ver saía do alto. A curva do período
    /// não some — ela é o gráfico dia a dia logo abaixo, com eixo e escala.
    fn painel_de_valores(&self, ctx: &Ctx) -> Layout {
        let linhas = carteira::linhas(ctx.portfolio, ctx.market, ctx.agora);
        let t = carteira::totais(&linhas);

        let mut fatos = vec![(
            "patrimônio".into(),
            format!("R$ {}", calc::moeda(t.mercado)),
            Tone::Destaque,
        )];
        // O custo só entra quando **todas** as posições o informam. Somar o custo de
        // metade da carteira e chamar de custo daria um P&L inventado — que é a mesma
        // regra do preço médio informado, aplicada ao total.
        match t.sem_custo {
            0 => {
                fatos.push((
                    "custo informado".into(),
                    format!("R$ {}", calc::moeda(t.custo)),
                    Tone::Normal,
                ));
                fatos.push((
                    "P&L".into(),
                    match t.pnl_pct() {
                        Some(p) => format!("R$ {} ({})", calc::moeda(t.pnl), calc::pct(p)),
                        None => format!("R$ {}", calc::moeda(t.pnl)),
                    },
                    crate::invest::modules::heatmap::tom(t.pnl_pct()),
                ));
            }
            n => fatos.push((
                "P&L".into(),
                format!(
                    "não dá para dizer — {} sem preço médio informado",
                    calc::plural(n, "posição", "posições")
                ),
                Tone::Aviso,
            )),
        }
        if t.dia != 0.0 {
            fatos.push((
                "no dia".into(),
                // Reais **e** porcentagem: «R$ 10 mil» não diz se o dia foi bom sem se
                // saber sobre quanto, e a porcentagem sozinha não diz o tamanho.
                match t.dia_pct() {
                    Some(p) => format!("R$ {} ({})", calc::moeda(t.dia), calc::pct(p)),
                    None => format!("R$ {}", calc::moeda(t.dia)),
                },
                crate::invest::modules::heatmap::tom(Some(t.dia)),
            ));
        }
        fatos.push((
            "posições".into(),
            calc::plural(ctx.portfolio.posicoes.len(), "linha", "linhas"),
            Tone::Dim,
        ));
        // O rumo vem logo abaixo do retrato: é a pergunta seguinte a «quanto eu tenho».
        fatos.extend(self.rumo(ctx));

        let barras = |chave: fn(&carteira::Linha) -> String| -> Vec<Bar> {
            carteira::agrupar(&linhas, chave)
                .into_iter()
                .map(|(rotulo, valor)| Bar {
                    label: rotulo,
                    value: valor,
                    target: None,
                    text: format!("R$ {}", calc::moeda(valor)),
                    tone: Tone::Normal,
                })
                .collect()
        };

        Layout::cols(vec![
            (
                1,
                Layout::one(Pane::Facts {
                    title: "Hoje".into(),
                    rows: fatos,
                }),
            ),
            (
                1,
                Layout::one(Pane::Bars {
                    title: "Por classe".into(),
                    rows: barras(|l| l.posicao.classe.label().to_string()),
                    full: None,
                }),
            ),
            (
                1,
                Layout::one(Pane::Bars {
                    title: "Por corretora".into(),
                    rows: barras(|l| l.posicao.fonte.clone()),
                    full: None,
                }),
            ),
        ])
    }

    /// A tela do primeiro dia: o painel de valores, os gráficos, e o aviso de que a
    /// curva do período ainda não tem dois pontos.
    fn hoje(&self, ctx: &Ctx, pontos: usize) -> Layout {
        if ctx.portfolio.posicoes.is_empty() {
            return Layout::one(Pane::Empty {
                title: "Patrimônio".into(),
                note: "Sem posições, não há patrimônio a somar.\n\n\
                       Importe em Importação, ou adicione à mão em Posições."
                    .into(),
            });
        }
        Layout::rows(vec![
            (2, self.painel_de_valores(ctx)),
            (
                2,
                Layout::cols(vec![
                    (1, Layout::one(self.diario(ctx))),
                    (1, Layout::one(self.mensal(ctx))),
                ]),
            ),
            (
                1,
                Layout::one(Pane::Empty {
                    title: "A curva ainda não".into(),
                    note: format!(
                        "Há {} ponto(s) na série, e uma curva precisa de dois.\n\n\
                         Um ponto é gravado por dia, no primeiro tique da aba naquele dia \n\
                         — e um dia sem o programa aberto fica como lacuna, e não como \n\
                         linha reta que não aconteceu.",
                        pontos
                    ),
                }),
            ),
        ])
    }
}

impl ModuleView for Vista {
    fn title(&self) -> String {
        format!("Patrimônio · {}", PERIODOS[self.periodo].0)
    }

    fn quer_mercado(&self) -> bool {
        true
    }

    fn tick(&mut self, _ctx: &Ctx) {
        // A série é gravada pelo estado da aba, de minuto em minuto. Reler o arquivo aqui
        // seria I/O no caminho do desenho — e o ponto de hoje já está em memória lá.
    }

    fn layout(&self, ctx: &Ctx) -> Layout {
        let janela = serie::janela(&self.pontos, ctx.agora, PERIODOS[self.periodo].1);
        // Sem dois pontos não há curva — mas há a **foto de hoje**, que é dado de verdade
        // e já está em memória. Antes esta tela era só a explicação de por que ela estava
        // vazia, o que é o pior dos dois mundos: quem abre no primeiro dia não vê nem a
        // curva nem o patrimônio que ele acabou de importar.
        if janela.len() < 2 {
            return self.hoje(ctx, janela.len());
        }

        let decomposicao = serie::decompor(&janela);
        let lacunas = serie::lacunas(&janela);

        let mut fatos = Vec::new();
        if let Some(d) = &decomposicao {
            let base = janela.first().map(|p| p.total_brl).unwrap_or(0.0);
            let fmt = |v: f64| match self.percentual && base > 0.0 {
                true => calc::pct(v / base * 100.0),
                false => format!("R$ {}", calc::moeda(v)),
            };
            fatos.push(("aportes".into(), fmt(d.aportes), Tone::Normal));
            if d.retiradas > 0.0 {
                fatos.push(("retiradas".into(), fmt(-d.retiradas), Tone::Ruim));
            }
            fatos.push((
                "mercado".into(),
                fmt(d.mercado),
                match d.mercado >= 0.0 {
                    true => Tone::Bom,
                    false => Tone::Ruim,
                },
            ));
            fatos.push(("proventos".into(), fmt(d.proventos), Tone::Normal));
            fatos.push(("variação total".into(), fmt(d.total), Tone::Destaque));

            // Sem lançamentos não há como separar aporte de valorização — e apresentar
            // tudo como mercado seria a mentira mais cara deste módulo.
            if ctx.portfolio.lancamentos.is_empty() {
                fatos.push((
                    "decomposição incompleta".into(),
                    "sem lançamentos, um aporte aparece como valorização".into(),
                    Tone::Aviso,
                ));
            }
        }
        fatos.extend(self.rumo(ctx));
        if lacunas > 0 {
            fatos.push((
                "lacunas".into(),
                format!("{lacunas} dia(s) sem registro — desenhados como lacuna"),
                Tone::Dim,
            ));
        }

        // Os valores em cima, os gráficos embaixo — e **a mesma ordem do primeiro dia**.
        //
        // A curva do período ficava no alto e empurrava para o rodapé o número que se vem
        // ver. Ela também não fazia falta: o gráfico dia a dia logo abaixo desenha a mesma
        // coisa, com eixo rotulado e escala escrita, o que a curva não tinha.
        Layout::rows(vec![
            (2, self.painel_de_valores(ctx)),
            (
                3,
                Layout::cols(vec![
                    (1, Layout::one(self.diario(ctx))),
                    (1, Layout::one(self.mensal(ctx))),
                ]),
            ),
            (
                2,
                Layout::one(Pane::Facts {
                    title: format!("De onde veio · {} pontos", janela.len()),
                    rows: fatos,
                }),
            ),
        ])
    }

    fn key(&mut self, key: KeyEvent, _ctx: &Ctx) -> Outcome {
        match key.code {
            KeyCode::Left => {
                self.periodo = (self.periodo + PERIODOS.len() - 1) % PERIODOS.len();
                Outcome::Ok
            }
            KeyCode::Right => {
                self.periodo = (self.periodo + 1) % PERIODOS.len();
                Outcome::Ok
            }
            KeyCode::Char('d') => {
                self.percentual = !self.percentual;
                Outcome::Ok
            }
            _ => Outcome::Ignorada,
        }
    }

    fn escape(&mut self) -> Escape {
        Escape::Nao
    }

    fn hint(&self) -> String {
        hint(&["←/→ período", "d absoluto/percentual", "Esc sair"])
    }
}
