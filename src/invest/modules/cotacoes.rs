//! Cotações: a watchlist. O que eu acompanho, esteja ou não na carteira.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::invest::calc;
use crate::invest::carteira;
use crate::invest::model::AssetId;
use crate::invest::module::{
    Ctx, Edit, Escape, Field, Group, InvestModule, Layout, Marcavel, ModuleView, Need, Outcome,
    Pane, Row, Tone,
};
use crate::invest::modules::comum::{Formulario, Lista, hint, sem_preco};
use crate::invest::modules::mercado;

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

    fn open(&self, ctx: &Ctx, _alvo: Option<&AssetId>) -> Box<dyn ModuleView> {
        let mut ordem = ativos(ctx);
        ordenar(ctx, &mut ordem);
        Box::new(Vista {
            lista: Lista::default(),
            form: None,
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
    fn title(&self) -> String {
        "Cotações".into()
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
            // mesmo da lista original. Sem isto, o Enter abria o gráfico sempre no
            // primeiro ativo com série, qualquer que fosse a linha escolhida.
            KeyCode::Enter => Outcome::Abrir {
                modulo: "grafico",
                alvo: self
                    .lista
                    .atual(&linhas(ctx, &ativos))
                    .and_then(|i| ativos.get(i))
                    .cloned(),
            },
            _ => match self.lista.tecla(key, ativos.len()) {
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
        hint(&[
            "↑/↓ andar",
            "digite para buscar",
            "Ctrl+A acompanhar",
            "Del tirar",
            "Enter gráfico",
            "Esc sair",
        ])
    }
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
