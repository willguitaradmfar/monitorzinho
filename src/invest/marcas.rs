//! As marcas da aba Invest: a ponte entre `monitor::mark` e o vocabulário de desenho.
//!
//! Marcar uma linha é o mesmo gesto em toda parte do programa — `Ctrl+E` sobre a linha,
//! `Ctrl+G` para a lista do que está marcado —, e isso não é uma coincidência de teclado:
//! é literalmente o mesmo código. O que muda de uma aba para a outra é só de onde a linha
//! vem. Uma `TableMonitor` amostra `TableRow`s; um módulo da Invest devolve um `Pane` com
//! `Row`s. Este arquivo é a tradução entre os dois, e é toda a diferença que existe.
//!
//! A tradução é rasa de propósito: uma `Row` vira uma `TableRow` só com as células e a
//! profundidade, porque é só disso que uma marca precisa para decidir se é sobre a linha.
//! Nada aqui sabe o que é um ticker.

use crate::invest::module::{Marcavel, Pane, Row, TipoDeMarca};
use crate::monitor::TableRow;
use crate::monitor::mark::{MarkColor, MarkKind, Marks};

/// A lista de ativos da aba Invest — **uma só**, embora ela apareça em dez telas.
///
/// Posições, Cotações, Câmbio, Cripto, cada carteira recomendada, Proventos, Lançamentos,
/// Alertas, Fundamentos e Risco não são dez listas de coisas diferentes: são dez recortes
/// da mesma lista de papéis. Um id por tela deixaria a marca valer só onde foi feita, e
/// «seguir PETR4» que não segue PETR4 na tela ao lado não é seguir coisa nenhuma.
///
/// Os três tipos convivem porque cada um só existe onde a coluna dele existe — `tipos`
/// resolve pelo cabeçalho e descarta o resto. Em Posições a caixa oferece ativo e classe,
/// em Fundamentos ativo e setor, em Cotações só ativo, e ninguém precisou dizer isso.
pub static ATIVOS: Marcavel = Marcavel {
    tabela: "invest-ativo",
    nome: "Ativos (Invest)",
    tipos: &[
        TipoDeMarca {
            nome: "ativo",
            coluna: "Ativo",
            numerico: false,
            ajuda: "o ticker — vale em toda tela da aba que lista papéis",
        },
        TipoDeMarca {
            nome: "classe",
            coluna: "Classe",
            numerico: false,
            ajuda: "ação, FII, cripto — a classe inteira de uma vez",
        },
        TipoDeMarca {
            nome: "setor",
            coluna: "Setor",
            numerico: false,
            ajuda: "o setor inteiro de uma vez",
        },
    ],
};

/// Os tipos de marca de uma tela, com a coluna já resolvida contra os cabeçalhos dela.
///
/// Um tipo cuja coluna esta tabela não tem cai fora: numa tela onde não há a coluna
/// «Classe» não há como uma marca por classe casar, e oferecê-la seria oferecer uma que
/// nunca acende.
pub fn tipos(alvo: &Marcavel, headers: &[String]) -> Vec<MarkKind> {
    alvo.tipos
        .iter()
        .filter_map(|tipo| {
            let coluna = headers.iter().position(|h| h == tipo.coluna)?;
            Some(MarkKind {
                name: tipo.nome,
                column: coluna,
                numeric: tipo.numerico,
                help: tipo.ajuda,
            })
        })
        .collect()
}

/// Os mesmos tipos sem tabela nenhuma na frente — para a caixa aberta a partir da lista
/// de marcas, onde não há linha para casar e o que se edita é só o nome do tipo, o valor
/// e a cor. A coluna vai como 0 porque ninguém a lê nesse caminho.
pub fn tipos_soltos(alvo: &Marcavel) -> Vec<MarkKind> {
    alvo.tipos
        .iter()
        .map(|tipo| MarkKind {
            name: tipo.nome,
            column: 0,
            numeric: tipo.numerico,
            help: tipo.ajuda,
        })
        .collect()
}

/// A linha do módulo vista como a linha de uma tabela, que é o que uma marca sabe ler.
///
/// Sem pid: uma linha de módulo não é um processo, e a marca não olha para ele. A
/// profundidade vai junto porque é o que dá o recuo de um agrupamento — e porque uma
/// tradução que deixa um campo para trás é uma tradução que mente.
fn como_tabela(row: &Row) -> TableRow {
    TableRow {
        depth: row.depth,
        ..TableRow::leaf(row.cells.clone(), 0)
    }
}

/// Quem sabe de que cor pintar cada linha de um painel.
///
/// Vive fora do `Pane` de propósito. O vocabulário de desenho não conhece marcas — um
/// módulo devolve linhas cruas, do mesmo jeito que uma `TableMonitor` devolve `TableRow`s
/// cruas — e enfiar um campo de cor em cada variante do enum obrigaria vinte módulos a
/// escrever `marca: None` para uma coisa de que nenhum deles sabe. O pintor é consultado
/// na hora de desenhar e some depois.
pub struct Pintor<'a> {
    marks: &'a Marks,
    /// `None` quando a tela não aceita marca nenhuma — e aí o pintor não pinta nada.
    alvo: Option<Marcavel>,
}

impl<'a> Pintor<'a> {
    pub fn novo(marks: &'a Marks, alvo: Option<Marcavel>) -> Self {
        Self { marks, alvo }
    }

    /// Um pintor que não pinta. Para os painéis desenhados fora de qualquer módulo.
    #[cfg(test)]
    pub fn nenhum(marks: &'a Marks) -> Self {
        Self { marks, alvo: None }
    }

    /// A cor da linha de uma tabela, com a coluna de cada tipo resolvida contra os
    /// cabeçalhos **desta** tabela.
    pub fn tabela(&self, headers: &[String], row: &Row) -> Option<MarkColor> {
        let alvo = self.alvo.as_ref()?;
        let tipos = tipos(alvo, headers);
        self.marks
            .hit(alvo.tabela, &tipos, &como_tabela(row))
            .map(|hit| hit.color)
    }

    /// A cor de uma linha que não tem colunas nomeadas — um fato, uma barra, uma célula
    /// do heatmap, uma linha de texto.
    ///
    /// A maioria dos cartões da home não é tabela: são fatos e barras. Se a marca só
    /// valesse em tabela, seguir um papel acenderia dois cartões dos dez em que ele
    /// aparece — e a home é justamente onde se olha para achar o que se segue.
    ///
    /// Recebe **todos os pedaços** da linha e não só o primeiro, porque o assunto não
    /// mora sempre no mesmo lugar: no cartão de Posições o rótulo é o papel, mas no de
    /// Notícias o rótulo é «há 6 h» e a manchete é o valor, e no de Agenda o rótulo é a
    /// data e o evento é o valor. Testar só o rótulo deixava esses dois sem destaque —
    /// e eram justamente os dois em que o rótulo não é o assunto.
    ///
    /// Não há coluna a resolver aqui: cada pedaço é testado como se fosse a única célula,
    /// e o primeiro que casa dá a cor.
    pub fn rotulo(&self, partes: &[&str]) -> Option<MarkColor> {
        let alvo = self.alvo.as_ref()?;
        let tipos = tipos_soltos(alvo);
        partes.iter().find_map(|texto| {
            let linha = TableRow::leaf(vec![(*texto).to_string()], 0);
            self.marks
                .hit(alvo.tabela, &tipos, &linha)
                .map(|hit| hit.color)
        })
    }
}

/// A linha sob o cursor de uma tabela de módulo, já como a tabela que a marca lê.
///
/// A busca **filtra** nas listas da Invest, então `selected` é um índice na lista
/// filtrada e não na original. Indexar as linhas com ele direto pegaria outra linha
/// sempre que houvesse busca ativa — e marcar é justamente o que se faz depois de achar
/// a linha procurando por ela.
pub fn linha_sob_cursor(pane: &Pane) -> Option<TableRow> {
    let Pane::Table {
        rows,
        selected,
        query,
        ..
    } = pane
    else {
        return None;
    };
    let needle = query.to_lowercase();
    rows.iter()
        .filter(|r| r.matches(&needle))
        .nth((*selected)?)
        .map(como_tabela)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invest::module::TipoDeMarca;
    use crate::monitor::mark::{Mark, MarkColor};

    static TIPOS: &[TipoDeMarca] = &[TipoDeMarca {
        nome: "ativo",
        coluna: "Ativo",
        numerico: false,
        ajuda: "o ticker",
    }];

    fn tabela(linhas: &[&str], selected: Option<usize>, query: &str) -> Pane {
        Pane::Table {
            title: "t".into(),
            headers: vec!["Ativo".into()],
            rows: linhas
                .iter()
                .map(|t| Row::new(vec![t.to_string()]))
                .collect(),
            selected,
            query: query.into(),
            note: None,
        }
    }

    fn com_marca(valor: &str, cor: MarkColor) -> Marks {
        let mut marks = Marks::default();
        marks.add(Mark {
            table: "invest-x".into(),
            kind: "ativo".into(),
            value: valor.into(),
            subtree: false,
            color: cor,
        });
        marks
    }

    #[test]
    fn pinta_so_a_linha_que_a_marca_segue() {
        let marks = com_marca("PETR4", MarkColor::Verde);
        let pintor = Pintor::novo(
            &marks,
            Some(Marcavel {
                tabela: "invest-x",
                nome: "X",
                tipos: TIPOS,
            }),
        );
        let cabecalhos = vec!["Ativo".to_string()];
        assert_eq!(
            pintor.tabela(&cabecalhos, &Row::new(vec!["PETR4".into()])),
            Some(MarkColor::Verde)
        );
        assert_eq!(
            pintor.tabela(&cabecalhos, &Row::new(vec!["VALE3".into()])),
            None
        );
    }

    #[test]
    fn o_rotulo_de_um_fato_ou_de_uma_barra_tambem_e_pintado() {
        // A maioria dos cartões da home é painel de fatos ou de barras, não tabela. Sem
        // isto, seguir um papel acenderia dois cartões dos dez em que ele aparece.
        let marks = com_marca("PETR4", MarkColor::Ciano);
        let pintor = Pintor::novo(
            &marks,
            Some(Marcavel {
                tabela: "invest-x",
                nome: "X",
                tipos: TIPOS,
            }),
        );
        assert_eq!(pintor.rotulo(&["PETR4"]), Some(MarkColor::Ciano));
        assert_eq!(pintor.rotulo(&["VALE3"]), None);
        // E o assunto não mora sempre no rótulo: no cartão de Notícias ele está no
        // valor, atrás de um «há 6 h».
        assert_eq!(
            pintor.rotulo(&["há 6 h", "PETR4 sobe com o petróleo"]),
            Some(MarkColor::Ciano)
        );
    }

    #[test]
    fn a_marca_de_outra_tabela_nao_pinta_esta() {
        let marks = com_marca("PETR4", MarkColor::Verde);
        let pintor = Pintor::novo(
            &marks,
            Some(Marcavel {
                tabela: "invest-y",
                nome: "Y",
                tipos: TIPOS,
            }),
        );
        assert_eq!(
            pintor.tabela(&["Ativo".to_string()], &Row::new(vec!["PETR4".into()])),
            None
        );
        assert_eq!(pintor.rotulo(&["PETR4"]), None);
    }

    #[test]
    fn uma_tela_que_nao_aceita_marca_nao_pinta_nada() {
        let marks = com_marca("PETR4", MarkColor::Verde);
        let pintor = Pintor::nenhum(&marks);
        assert_eq!(
            pintor.tabela(&["Ativo".to_string()], &Row::new(vec!["PETR4".into()])),
            None
        );
        assert_eq!(pintor.rotulo(&["PETR4"]), None);
    }

    #[test]
    fn a_coluna_e_achada_pelo_cabecalho_e_nao_pelo_numero() {
        // A mesma declaração serve à tela aberta e ao cartão da home, que mostram a
        // coluna «Ativo» em posições diferentes. É o caso que o número errava.
        let alvo = Marcavel {
            tabela: "invest-x",
            nome: "X",
            tipos: TIPOS,
        };
        let cabecalhos = vec!["Data".to_string(), "Ativo".to_string()];
        let resolvidos = tipos(&alvo, &cabecalhos);
        assert_eq!(resolvidos.len(), 1);
        assert_eq!(resolvidos[0].column, 1);
    }

    #[test]
    fn tipo_sem_coluna_na_tabela_cai_fora() {
        let alvo = Marcavel {
            tabela: "invest-x",
            nome: "X",
            tipos: TIPOS,
        };
        assert!(tipos(&alvo, &["Data".to_string()]).is_empty());
    }

    #[test]
    fn sem_cursor_nao_ha_linha() {
        assert!(linha_sob_cursor(&tabela(&["PETR4"], None, "")).is_none());
        assert!(linha_sob_cursor(&tabela(&[], Some(0), "")).is_none());
    }
}
