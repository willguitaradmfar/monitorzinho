//! Notícias: o que está sendo dito sobre o que eu tenho.
//!
//! A tela é um log que cresce, com busca digitada direto e rolagem — que é exatamente o
//! `ToolMonitorFocus` da aba Ferramentas. Reaproveitar aquele desenho, em vez de inventar
//! outro, é o que mantém a promessa de não sair do padrão.
//!
//! `Enter` **não abre navegador**: o monitorzinho é uma interface de terminal, e abrir um
//! processo gráfico a partir dela é sair da frente sem ter sido pedido. Ele mostra o
//! resumo e a URL para copiar.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::invest::calc;
use crate::invest::model::AssetId;
use crate::invest::module::{
    Ctx, Edit, Escape, Field, Group, InvestModule, Layout, Marcavel, ModuleView, Outcome, Pane,
    Row, TipoDeMarca, Tone,
};
use crate::invest::modules::comum::{Formulario, Lista, hint};
use crate::invest::rss::{self, Item};
use crate::invest::tempo;

pub struct Noticias;

/// O que se pode seguir nesta lista. O id da tabela nunca muda depois de publicado — é
/// com ele que as marcas já gravadas se reconhecem.
static MARCAS: Marcavel = Marcavel {
    tabela: "invest-noticias",
    nome: "Notícias",
    tipos: &[TipoDeMarca {
        nome: "assunto",
        coluna: "Título",
        numerico: false,
        ajuda: "uma palavra do título, ou uma expressão regular",
    }],
};

impl InvestModule for Noticias {
    fn id(&self) -> &'static str {
        "noticias"
    }
    fn name(&self) -> &'static str {
        "Notícias"
    }
    fn description(&self) -> &'static str {
        "Feeds RSS, com o que toca a sua carteira marcado"
    }
    fn destaque(&self) -> u8 {
        4
    }
    fn group(&self) -> Group {
        Group::Informacao
    }

    fn fonte_externa(&self) -> Option<&'static str> {
        Some(crate::invest::store::fonte::NOTICIA)
    }

    fn marcavel(&self) -> Option<Marcavel> {
        Some(MARCAS)
    }
    fn keywords(&self) -> &'static str {
        "rss feed jornal manchete mercado imprensa"
    }

    fn summary(&self, ctx: &Ctx) -> String {
        match ctx.portfolio.feeds.len() {
            0 => "nenhum feed — adicione um RSS".into(),
            n => format!("{n} feed(s)"),
        }
    }

    fn widget(&self, ctx: &Ctx) -> Option<Pane> {
        if ctx.portfolio.feeds.is_empty() {
            return None;
        }
        // **As manchetes, não os endereços.** O cartão listava de onde as notícias viriam,
        // que é o que se tem, e não o que elas disseram, que é o que se quer ler.
        //
        // `get` dispara a busca em segundo plano e devolve o que já há: o cartão não
        // espera por ela, e é o que enche o painel sem obrigar a entrar no módulo. O
        // intervalo é de cinco minutos, e o cache é o mesmo que o módulo aberto usa.
        let itens = ctx.noticias.get(&ctx.portfolio.feeds, ctx.agora);
        if itens.is_empty() {
            return None;
        }
        // Ordem única entre os feeds, do mais recente para o mais antigo: um item de
        // ontem não pode ficar acima de um de agora só por ter vindo de outro endereço.
        let ativos = ctx.portfolio.ativos_de_interesse();
        Some(Pane::Facts {
            title: String::new(),
            rows: crate::invest::noticias::recentes(&itens)
                .into_iter()
                .take(10)
                .map(|i| {
                    let (_, item) = &itens[i];
                    (
                        quando(item, ctx.agora),
                        item.titulo.clone(),
                        match marca(item, &ativos).is_some() {
                            // Marcado é o que toca a carteira — o motivo de a lista existir.
                            true => Tone::Destaque,
                            false => Tone::Normal,
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
            so_marcados: false,
            aberto: None,
            aba: Aba::Itens,
            confirmando: None,
            editando: None,
        })
    }
}

/// Os índices dos itens que a tela mostra, na ordem em que ela os mostra: do mais novo
/// para o mais velho, e só os marcados quando o filtro está ligado.
///
/// Existe porque a tela mostra uma ordem e o vetor guarda outra: sem este mapeamento, o
/// `Enter` abriria a notícia errada — e com o filtro ligado, uma bem distante.
/// O que a tela está mostrando.
#[derive(PartialEq, Eq)]
enum Aba {
    Itens,
    /// A lista de endereços, para gerenciar. Sem ela, o único jeito de tirar um feed era
    /// um atalho escondido que apagava sempre o primeiro.
    Feeds,
}

struct Vista {
    lista: Lista,
    form: Option<Formulario>,
    so_marcados: bool,
    /// O item aberto, quando há um.
    aberto: Option<usize>,
    aba: Aba,
    /// O feed cuja remoção espera confirmação.
    confirmando: Option<usize>,
    /// O endereço que está sendo trocado, quando a caixa é de edição. Editar é remover e
    /// adicionar — mas numa operação só, para a lista não piscar nem perder o lugar.
    editando: Option<String>,
}

/// Quando a notícia saiu, do jeito que se lê: as de hoje em tempo relativo, as mais
/// antigas com data e hora. Um item sem data legível diz isso, em vez de aparecer com uma
/// data qualquer.
fn quando(item: &Item, agora: u64) -> String {
    let Some(em) = item.em else {
        return "sem data".to_string();
    };
    let idade = agora.saturating_sub(em);
    match idade < 86400 {
        true => tempo::idade(idade),
        false => tempo::dia_e_hora(em, tempo::BRT_OFFSET),
    }
}

/// Se um item menciona algum ativo da carteira ou da watchlist.
///
/// A busca é pelo símbolo com um mínimo de cuidado: `VALE3` é seguro, `VALE` casaria com
/// meia notícia do país. Por isso só símbolos de quatro caracteres ou mais entram.
pub fn marca(item: &Item, ativos: &[AssetId]) -> Option<String> {
    let texto = crate::format::fold(&format!("{} {}", item.titulo, item.resumo));
    ativos
        .iter()
        .find(|a| a.symbol.len() >= 4 && texto.contains(&crate::format::fold(&a.symbol)))
        .map(|a| a.short().to_string())
}

impl Vista {
    /// A lista de endereços: o que está configurado, o estado de cada um, e as teclas
    /// para mexer. Sem esta tela, tirar um feed dependia de um atalho escondido que
    /// apagava sempre o primeiro da lista.
    fn layout_feeds(&self, ctx: &Ctx) -> Layout {
        let erros = ctx.noticias.erros();
        let itens = ctx.noticias.ja_tem();

        let rows: Vec<Row> = ctx
            .portfolio
            .feeds
            .iter()
            .map(|url| {
                // O nome do host é o que identifica um feed para quem olha; a URL inteira
                // é o que ele precisa para editar.
                let host = url.split('/').nth(2).unwrap_or(url).replace("www.", "");
                let quantos = itens.iter().filter(|(f, _)| *f == host).count();
                let erro = erros.iter().find(|e| e.starts_with(&host));
                Row::new(vec![
                    host.clone(),
                    match erro {
                        Some(e) => e.split_once(": ").map(|(_, m)| m).unwrap_or(e).to_string(),
                        None => calc::plural(quantos, "item", "itens"),
                    },
                    url.clone(),
                ])
                .with_cell_tones(vec![
                    Tone::Normal,
                    match erro {
                        Some(_) => Tone::Ruim,
                        None => Tone::Dim,
                    },
                    Tone::Dim,
                ])
            })
            .collect();

        let nota = match self.confirmando.and_then(|i| ctx.portfolio.feeds.get(i)) {
            Some(url) => format!("remover {url}? Enter confirma, Esc desiste"),
            None => "Ctrl+A adiciona · Enter edita · Del remove".to_string(),
        };

        Layout::one(Pane::Table {
            title: format!(
                "Feeds · {}",
                calc::plural(ctx.portfolio.feeds.len(), "endereço", "endereços")
            ),
            headers: vec!["Fonte".into(), "Estado".into(), "Endereço".into()],
            rows,
            selected: Some(self.lista.selecionado),
            query: self.lista.busca.clone(),
            note: Some((nota, Tone::Aviso)),
        })
    }

    /// Os índices dos itens que a tela mostra, **na ordem em que ela os mostra**: por
    /// data, a mais recente acima.
    ///
    /// Antes era a ordem de chegada das threads invertida — que muda a cada volta, mistura
    /// os feeds e põe uma notícia de ontem acima de uma de agora. Um item sem data
    /// legível vai para o fim, em vez de fingir uma posição no topo.
    fn visiveis(&self, itens: &[(String, Item)], ativos: &[AssetId]) -> Vec<usize> {
        let mut v: Vec<usize> = (0..itens.len())
            .filter(|&i| !self.so_marcados || marca(&itens[i].1, ativos).is_some())
            .collect();
        v.sort_by_key(|&i| std::cmp::Reverse(itens[i].1.em.unwrap_or(0)));
        v
    }
}

impl ModuleView for Vista {
    fn tick(&mut self, ctx: &Ctx) {
        // A busca vive no estado da aba, não aqui — ver `invest::noticias::Cache`. O
        // módulo aberto só a mantém acordada; o cartão da home faz o mesmo pedido, e o
        // intervalo do cache garante que os dois juntos não busquem duas vezes.
        ctx.noticias.get(&ctx.portfolio.feeds, ctx.agora);
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
        if self.aba == Aba::Feeds {
            return self.layout_feeds(ctx);
        }
        if ctx.portfolio.feeds.is_empty() {
            return Layout::one(Pane::Empty {
                title: "Notícias".into(),
                note: "Nenhum feed ainda.\n\n\
                       Ctrl+A adiciona um endereço RSS, e Tab abre a lista deles para \n\
                       editar e remover. RSS é a única fonte de informação desta aba sem \n\
                       chave, sem cota e sem termo de uso hostil — as mesmas garantias do \n\
                       Banco Central.\n\n\
                       Um item que menciona um ativo da sua carteira aparece marcado com ●, \n\
                       e Ctrl+F mostra só esses."
                    .into(),
            });
        }

        let ativos = ctx.portfolio.ativos_de_interesse();
        let itens = ctx.noticias.ja_tem();

        if let Some(i) = self.aberto
            && let Some((fonte, item)) = itens.get(i)
        {
            return Layout::one(Pane::Text {
                title: item.titulo.clone(),
                lines: vec![
                    (format!("  {fonte} · {}", item.data), Tone::Dim),
                    (String::new(), Tone::Normal),
                    (format!("  {}", rss::sem_html(&item.resumo)), Tone::Normal),
                    (String::new(), Tone::Normal),
                    (format!("  {}", item.link), Tone::Destaque),
                    (String::new(), Tone::Normal),
                    (
                        "  O programa não abre navegador — a URL está aí para copiar.".into(),
                        Tone::Dim,
                    ),
                ],
                scroll: 0,
            });
        }

        let visiveis = self.visiveis(&itens, &ativos);
        let mut rows: Vec<Row> = Vec::new();
        for &i in &visiveis {
            let (fonte, item) = &itens[i];
            let marcado = marca(item, &ativos);
            rows.push(
                Row::new(vec![
                    match &marcado {
                        Some(_) => "●".to_string(),
                        None => " ".to_string(),
                    },
                    // A hora **tem** que aparecer: sem ela a coluna mostrava só «Tue, 08
                    // Sep 2026» em toda linha, e uma lista ordenada por data parecia
                    // desordenada porque nada distinguia uma linha da outra.
                    quando(item, ctx.agora),
                    fonte.clone(),
                    item.titulo.clone(),
                ])
                .with_cell_tones(vec![
                    Tone::Bom,
                    Tone::Dim,
                    Tone::Dim,
                    match marcado.is_some() {
                        true => Tone::Normal,
                        false => Tone::Dim,
                    },
                ]),
            );
        }

        let erros = ctx.noticias.erros();
        let nota = match (rows.is_empty(), erros.is_empty()) {
            (true, true) => Some("buscando…".to_string()),
            (_, false) => Some(erros.join(" · ")),
            _ => None,
        };

        Layout::one(Pane::Table {
            title: format!(
                "Notícias · {}{}",
                calc::plural(rows.len(), "item", "itens"),
                match self.so_marcados {
                    true => " · só a sua carteira",
                    false => "",
                }
            ),
            headers: vec![
                String::new(),
                "Quando".into(),
                "Fonte".into(),
                "Título".into(),
            ],
            rows,
            selected: Some(self.lista.selecionado),
            query: self.lista.busca.clone(),
            note: nota.map(|n| (n, Tone::Aviso)),
        })
    }

    fn key(&mut self, key: KeyEvent, ctx: &Ctx) -> Outcome {
        if let Some(form) = &mut self.form {
            if form.tecla(key) {
                let url = form.valor("endereço");
                if !url.starts_with("http") {
                    form.erro = Some("o endereço tem que começar com http:// ou https://".into());
                    return Outcome::Ok;
                }
                self.form = None;
                ctx.noticias.invalidar();
                // Trocar um endereço é uma edição só: sair e entrar deixaria a lista
                // piscar e o cursor no lugar errado.
                let mut edits = Vec::new();
                if let Some(antigo) = self.editando.take() {
                    edits.push(Edit::RemoveFeed(antigo));
                }
                edits.push(Edit::AddFeed(url));
                return Outcome::Editar(edits);
            }
            return Outcome::Ok;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        // A tela de feeds tem as próprias teclas: as duas listas não são a mesma coisa e
        // não podem responder igual ao Enter e ao Del.
        if self.aba == Aba::Feeds {
            let feeds = ctx.portfolio.feeds.clone();
            let rows: Vec<Row> = feeds.iter().map(|u| Row::new(vec![u.clone()])).collect();
            let atual = self.lista.atual(&rows);
            if let Some(i) = self.confirmando.take() {
                return match (key.code, feeds.get(i)) {
                    (KeyCode::Enter, Some(url)) => {
                        Outcome::Editar(vec![Edit::RemoveFeed(url.clone())])
                    }
                    _ => Outcome::Ok,
                };
            }
            return match key.code {
                KeyCode::Char('a') if ctrl => {
                    self.form = Some(Formulario::novo(
                        "Adicionar feed",
                        vec![Field::text(
                            "endereço",
                            "https://",
                            "O endereço RSS ou Atom",
                        )],
                    ));
                    Outcome::Ok
                }
                // Editar é substituir: o formulário abre com o endereço atual, e gravar
                // troca um pelo outro numa edição só.
                KeyCode::Enter => {
                    if let Some(url) = atual.and_then(|i| feeds.get(i)) {
                        self.editando = Some(url.clone());
                        self.form = Some(Formulario::novo(
                            "Editar feed",
                            vec![Field::text(
                                "endereço",
                                url.clone(),
                                "O endereço RSS ou Atom",
                            )],
                        ));
                    }
                    Outcome::Ok
                }
                KeyCode::Delete => {
                    self.confirmando = atual;
                    Outcome::Ok
                }
                KeyCode::Tab => {
                    self.aba = Aba::Itens;
                    self.lista = Lista::default();
                    Outcome::Ok
                }
                _ => match self.lista.tecla(key, feeds.len()) {
                    true => Outcome::Ok,
                    false => Outcome::Ignorada,
                },
            };
        }

        match key.code {
            KeyCode::Tab => {
                self.aba = Aba::Feeds;
                self.lista = Lista::default();
                Outcome::Ok
            }
            KeyCode::Char('a') if ctrl => {
                self.form = Some(Formulario::novo(
                    "Adicionar feed",
                    vec![Field::text(
                        "endereço",
                        "https://",
                        "O endereço RSS ou Atom",
                    )],
                ));
                Outcome::Ok
            }
            KeyCode::Char('f') if ctrl => {
                self.so_marcados = !self.so_marcados;
                self.lista.selecionado = 0;
                Outcome::Ok
            }
            KeyCode::Enter => {
                // Três ordens diferentes em jogo: o vetor, a tela (invertida e filtrada) e
                // a busca. O `Enter` resolve as três antes de abrir.
                let itens = ctx.noticias.ja_tem();
                let ativos = ctx.portfolio.ativos_de_interesse();
                let visiveis = self.visiveis(&itens, &ativos);
                let rows: Vec<Row> = visiveis
                    .iter()
                    .map(|&i| Row::new(vec![itens[i].1.titulo.clone()]))
                    .collect();
                self.aberto = self
                    .lista
                    .atual(&rows)
                    .and_then(|v| visiveis.get(v))
                    .copied();
                Outcome::Ok
            }
            _ => {
                let total = ctx.noticias.ja_tem().len();
                match self.lista.tecla(key, total) {
                    true => Outcome::Ok,
                    false => Outcome::Ignorada,
                }
            }
        }
    }

    fn escape(&mut self) -> Escape {
        // Uma camada por vez: a caixa, a confirmação, o item aberto, a aba de feeds, a
        // busca — e só então a pergunta de saída.
        if self.form.take().is_some() || self.confirmando.take().is_some() {
            self.editando = None;
            return Escape::Consumido;
        }
        if self.aberto.take().is_some() {
            return Escape::Consumido;
        }
        if self.aba == Aba::Feeds {
            self.aba = Aba::Itens;
            self.lista = Lista::default();
            return Escape::Consumido;
        }
        self.lista.escape()
    }

    fn hint(&self) -> String {
        match self.aba {
            Aba::Feeds => hint(&[
                "↑/↓ andar",
                "Ctrl+A adicionar",
                "Enter editar",
                "Del remover",
                "Tab volta às notícias",
                "Esc sair",
            ]),
            Aba::Itens => hint(&[
                "↑/↓ andar",
                "Enter abrir",
                "Ctrl+F só a carteira",
                "Tab gerenciar feeds",
                "Esc sair",
            ]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invest::model::Market;

    fn item(titulo: &str) -> Item {
        Item {
            titulo: titulo.into(),
            link: String::new(),
            data: String::new(),
            resumo: String::new(),
            em: None,
        }
    }

    #[test]
    fn simbolo_curto_nao_marca_meia_noticia() {
        // «VALE» casaria com «vale a pena»; o mínimo de quatro caracteres é o que evita.
        let ativos = vec![AssetId::new(Market::B3, "VALE3")];
        assert!(marca(&item("Vale a pena investir?"), &ativos).is_none());
        assert_eq!(
            marca(&item("VALE3 sobe 3%"), &ativos).as_deref(),
            Some("VALE3")
        );
    }

    #[test]
    fn a_marca_ignora_caixa_e_acento() {
        let ativos = vec![AssetId::new(Market::B3, "PETR4")];
        assert!(marca(&item("petr4 aprova dividendos"), &ativos).is_some());
    }

    #[test]
    fn a_lista_ordena_por_data_com_a_mais_recente_acima() {
        // Antes a ordem era a de chegada das threads invertida: mudava a cada volta e
        // punha uma notícia de ontem acima de uma de agora.
        let com_data = |t: &str, em: Option<u64>| {
            (
                "fonte".to_string(),
                Item {
                    titulo: t.into(),
                    link: String::new(),
                    data: String::new(),
                    resumo: String::new(),
                    em,
                },
            )
        };
        // Inseridos fora de ordem, de propósito.
        let itens = vec![
            com_data("velha", Some(100)),
            com_data("nova", Some(300)),
            com_data("sem data", None),
            com_data("média", Some(200)),
        ];
        let v = Vista {
            lista: Lista::default(),
            form: None,
            so_marcados: false,
            aberto: None,
            aba: Aba::Itens,
            confirmando: None,
            editando: None,
        };
        let ordem: Vec<&str> = v
            .visiveis(&itens, &[])
            .into_iter()
            .map(|i| itens[i].1.titulo.as_str())
            .collect();
        assert_eq!(ordem, vec!["nova", "média", "velha", "sem data"]);
    }

    #[test]
    fn item_sem_relacao_nao_marca() {
        let ativos = vec![AssetId::new(Market::B3, "PETR4")];
        assert!(marca(&item("Fed sinaliza cautela"), &ativos).is_none());
    }
}
