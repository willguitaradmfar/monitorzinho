//! Book e negócios — listado e adiado, e as duas coisas de propósito.
//!
//! Ele está aqui por duas razões de projeto:
//!
//! 1. **É o único módulo que exige infraestrutura que não existe.** Book por polling não é
//!    book: é um retrato de dois em dois segundos de uma coisa que muda dezenas de vezes
//!    por segundo. Ele precisa de WebSocket, e é ele que justifica construí-lo.
//! 2. **Ele delimita o que a aba não é** — a fronteira entre terminal de acompanhamento e
//!    terminal de operação. Deixá-lo explícito e adiado é mais honesto do que deixá-lo de
//!    fora e responder de improviso quando alguém perguntar.

use crossterm::event::KeyEvent;

use crate::invest::model::AssetId;
use crate::invest::module::{
    Ctx, Escape, Group, InvestModule, Layout, ModuleView, Need, Outcome, Pane,
};
use crate::invest::modules::comum::hint;

pub struct Book;

impl InvestModule for Book {
    fn id(&self) -> &'static str {
        "book"
    }
    fn name(&self) -> &'static str {
        "Book e negócios"
    }
    fn description(&self) -> &'static str {
        "Profundidade e times & trades — precisa de WebSocket, que ainda não existe"
    }
    fn destaque(&self) -> u8 {
        1
    }
    fn group(&self) -> Group {
        Group::Mercado
    }
    fn needs(&self) -> &'static [Need] {
        &[Need::Provedor("WebSocket")]
    }
    fn keywords(&self) -> &'static str {
        "profundidade ofertas times trades livro nível 2 spread"
    }

    fn summary(&self, _ctx: &Ctx) -> String {
        "precisa de WebSocket — fase própria".into()
    }

    fn widget(&self, _ctx: &Ctx) -> Option<Pane> {
        // Este é o único módulo cujo cartão **não** tem o que mostrar, e o motivo é
        // concreto: book por polling não é book. Dizer isso no painel é melhor que uma
        // moldura vazia que parece um cartão quebrado.
        None
    }

    fn open(&self, _ctx: &Ctx, _alvo: Option<&AssetId>) -> Box<dyn ModuleView> {
        Box::new(Vista)
    }
}

struct Vista;

impl ModuleView for Vista {
    fn title(&self) -> String {
        "Book e negócios".into()
    }

    fn layout(&self, _ctx: &Ctx) -> Layout {
        Layout::one(Pane::Empty {
            title: "Book e negócios".into(),
            note: "Este módulo ainda não existe, e o motivo é concreto.\n\n\
                   Book por polling não é book: seria um retrato de dois em dois segundos \n\
                   de uma coisa que muda dezenas de vezes por segundo. Ele precisa de \n\
                   WebSocket, e não há um no projeto — o que falta é o enquadramento do \n\
                   RFC 6455 (cabeçalho de tamanho variável, mascaramento do lado cliente, \n\
                   fragmentação, ping/pong) e a reconexão com recuo.\n\n\
                   Onde ele seria possível, quando existir: **só cripto**. A profundidade \n\
                   da Binance é pública; o dado de nível 2 da B3 é vendido, e o dos EUA \n\
                   também. Isso não é uma limitação a contornar — é o que o mercado dá de \n\
                   graça e o que não dá.\n\n\
                   E quando existir, ele será o único módulo que **nunca** poderá ser uma \n\
                   widget favorita: um stream de book manda milhares de mensagens por \n\
                   minuto, e deixá-lo vivo em segundo plano seria a violação mais cara \n\
                   possível da regra de custo zero fora de foco."
                .into(),
        })
    }

    fn key(&mut self, _key: KeyEvent, _ctx: &Ctx) -> Outcome {
        Outcome::Ignorada
    }

    fn escape(&mut self) -> Escape {
        Escape::Nao
    }

    fn hint(&self) -> String {
        hint(&["Esc sair"])
    }
}
