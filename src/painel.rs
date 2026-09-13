//! O vocabulário com que uma tela é descrita, e que o `ui.rs` sabe desenhar.
//!
//! Quem desenha **não recebe um `Frame` do ratatui**: devolve uma destas descrições, e o
//! `ui.rs` a transforma em pixels. A razão é a mesma desde que isto nasceu na aba Invest:
//! um `render(&self, frame, area)` por tela é liberdade total para sair do padrão, e com
//! duas dezenas de telas o padrão vira duas dezenas de variações da mesma moldura. O
//! `Pane` é a garantia mecânica de que isso não acontece — uma tela desenha bem porque
//! não tem como desenhar diferente.
//!
//! Mora aqui, e não mais dentro da aba que o inventou, porque deixou de ser dela: a
//! inspeção de banco de dados desenha os mesmos painéis, e uma ferramenta que precisasse
//! importar `invest::module` para ter uma tabela estaria dizendo uma coisa falsa sobre o
//! programa.
//!
//! O preço é real: uma tela com uma necessidade visual que o enum não cobre fica
//! bloqueada até o enum crescer. Isso é intencional — crescer o vocabulário é uma decisão
//! consciente com um caso concreto na mão, e não um efeito colateral de alguém com pressa.

/// A cor de uma linha ou célula. Nomes pelo que **significam**, não pela cor: o `ui.rs`
/// escolhe o RGB da paleta que já existe, e um módulo nunca menciona uma cor.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Tone {
    #[default]
    Normal,
    /// Secundário, ou informação que não é o assunto da linha.
    Dim,
    /// Alta, lucro, meta batida.
    Bom,
    /// Baixa, prejuízo.
    Ruim,
    /// Algo que precisa de atenção mas não é erro.
    Aviso,
    /// Destaque neutro — um cabeçalho de grupo, um total.
    Destaque,
}

#[derive(Clone, Debug, Default)]
pub struct Row {
    pub cells: Vec<String>,
    pub tone: Tone,
    /// Por célula, quando a linha tem tons diferentes dentro dela (a coluna de variação
    /// verde numa linha normal). Vazio usa `tone` para tudo.
    pub cell_tones: Vec<Tone>,
    /// Profundidade, para as tabelas que agrupam. 0 é linha rasa.
    pub depth: usize,
    /// O **fim** de uma célula com tom próprio: qual célula, que sufixo, e em que tom.
    ///
    /// Existe para a origem e a idade do preço poderem avisar que o número é velho **sem
    /// pintar o número**. A alternativa era escrever a palavra «atrasado» ao lado, que
    /// ocupa espaço em toda linha para dizer o que a cor diz de relance.
    ///
    /// O sufixo é um pedaço do texto que **já está** na célula, e não um acréscimo: assim
    /// a largura da coluna continua saindo do conteúdo dela, sem ninguém precisar somar
    /// duas partes.
    pub rabicho: Option<(usize, String, Tone)>,
}

impl Row {
    pub fn new(cells: Vec<String>) -> Self {
        Self {
            cells,
            ..Default::default()
        }
    }

    pub fn tinted(cells: Vec<String>, tone: Tone) -> Self {
        Self {
            cells,
            tone,
            ..Default::default()
        }
    }

    pub fn with_cell_tones(mut self, tones: Vec<Tone>) -> Self {
        self.cell_tones = tones;
        self
    }

    /// Marca o fim de uma célula para ser pintado à parte — ver `rabicho`.
    pub fn com_rabicho(mut self, celula: usize, sufixo: impl Into<String>, tom: Tone) -> Self {
        self.rabicho = Some((celula, sufixo.into(), tom));
        self
    }

    pub fn at_depth(mut self, depth: usize) -> Self {
        self.depth = depth;
        self
    }

    /// O texto que a busca do módulo olha.
    pub fn matches(&self, needle: &str) -> bool {
        needle.is_empty() || self.cells.iter().any(|c| c.to_lowercase().contains(needle))
    }
}

/// Uma barra: rótulo, valor, e a marca do alvo quando existe.
#[derive(Clone, Debug)]
pub struct Bar {
    pub label: String,
    pub value: f64,
    pub target: Option<f64>,
    pub text: String,
    pub tone: Tone,
}

/// Uma célula do heatmap.
#[derive(Clone, Debug)]
pub struct Cell {
    pub label: String,
    pub sub: String,
    /// Uma terceira linha, mostrada **só quando a célula é alta o bastante**. É onde vai
    /// a grandeza que decidiu o tamanho — num treemap o tamanho é metade da informação, e
    /// as caixas grandes têm lugar de sobra para dizer quanto valem.
    pub detalhe: String,
    /// A **área** que ela pede, relativa às outras. Não é largura: o desenho reparte o
    /// retângulo por área, e um peso duas vezes maior ocupa duas vezes mais tela.
    pub weight: f64,
    pub tone: Tone,
}

/// Um campo de formulário.
#[derive(Clone, Debug)]
pub struct Field {
    pub label: String,
    pub value: String,
    pub help: String,
    /// Quando é uma escolha entre valores fixos, andados com ←/→ em vez de digitados.
    pub options: Vec<String>,
}

impl Field {
    pub fn text(label: &str, value: impl Into<String>, help: &str) -> Self {
        Self {
            label: label.to_string(),
            value: value.into(),
            help: help.to_string(),
            options: Vec::new(),
        }
    }

    pub fn choice(label: &str, value: impl Into<String>, options: Vec<String>, help: &str) -> Self {
        Self {
            label: label.to_string(),
            value: value.into(),
            help: help.to_string(),
            options,
        }
    }
}

/// O que o `ui.rs` sabe desenhar. Um módulo só consegue pedir coisas desta lista.
pub enum Pane {
    Table {
        title: String,
        headers: Vec<String>,
        rows: Vec<Row>,
        selected: Option<usize>,
        query: String,
        /// Nota no rodapé do painel: o estado das fontes, o que ficou de fora do total,
        /// quanto a carteira andou hoje.
        ///
        /// Com o tom junto, porque uma nota que carrega um número **precisa** dizer o
        /// sinal dele: «no dia R$ -1.757» em amarelo se lê igual a «+R$ 1.757», e a cor é
        /// a primeira coisa que o olho pega numa tela de mercado.
        note: Option<(String, Tone)>,
    },
    Chart {
        title: String,
        series: Vec<f64>,
        format: fn(f64) -> String,
        /// Linhas horizontais de referência: o preço médio, o 30 e o 70 do RSI.
        marks: Vec<(f64, String)>,
        /// O que escrever no eixo do tempo. Vazio deixa o eixo sem rótulo — é o caso de
        /// uma série de preço, em que o que importa é a forma e não a data exata.
        ///
        /// Numa série de poucos pontos, ao contrário, o rótulo é metade da informação:
        /// «02/09 a 08/09» diz de que semana se está falando.
        x_labels: Vec<String>,
        /// A ressalva do rodapé: de onde a série veio, o que ficou de fora. Fora do
        /// título porque o título já carrega o nome e o último valor — e um título longo
        /// é um título cortado, que come justamente o valor.
        note: Option<String>,
    },
    Facts {
        title: String,
        rows: Vec<(String, String, Tone)>,
    },
    Bars {
        title: String,
        rows: Vec<Bar>,
        /// O que 100% da barra representa. `None` usa o maior valor da lista.
        full: Option<f64>,
    },
    Grid {
        title: String,
        cells: Vec<Cell>,
        selected: Option<usize>,
        legend: String,
    },
    Text {
        title: String,
        lines: Vec<(String, Tone)>,
        scroll: u16,
    },
    /// A tela de «isto ainda não dá para mostrar, e aqui está o porquê». Nunca uma tela
    /// vazia sem explicação: a lista deixa entrar em qualquer módulo justamente para que
    /// ele possa ensinar o que falta.
    Empty { title: String, note: String },
    Form {
        title: String,
        fields: Vec<Field>,
        selected: usize,
        error: Option<String>,
        hint: String,
    },
}

impl Pane {
    /// Dá um título ao painel se ele ainda não tiver um.
    ///
    /// Os painéis de `widget()` nascem sem título de propósito: quem os desenha é a grade
    /// da home, e lá o título é o nome do módulo. O mesmo painel dentro do módulo aberto
    /// já vem titulado, e este método não o sobrescreve.
    pub fn intitular(&mut self, titulo: &str) {
        let alvo = match self {
            Pane::Table { title, .. }
            | Pane::Chart { title, .. }
            | Pane::Facts { title, .. }
            | Pane::Bars { title, .. }
            | Pane::Grid { title, .. }
            | Pane::Text { title, .. }
            | Pane::Empty { title, .. }
            | Pane::Form { title, .. } => title,
        };
        if alvo.is_empty() {
            *alvo = titulo.to_string();
        }
    }
}

/// Como os painéis se dividem na tela. Os pesos são relativos, como `Constraint::Fill`.
pub enum Layout {
    Rows(Vec<(u16, Layout)>),
    Cols(Vec<(u16, Layout)>),
    Leaf(Box<Pane>),
}

impl Layout {
    pub fn one(pane: Pane) -> Layout {
        Layout::Leaf(Box::new(pane))
    }

    pub fn rows(partes: Vec<(u16, Layout)>) -> Layout {
        Layout::Rows(partes)
    }

    pub fn cols(partes: Vec<(u16, Layout)>) -> Layout {
        Layout::Cols(partes)
    }

    /// A primeira `Pane::Table` do arranjo, na ordem em que ela é desenhada.
    ///
    /// É a tabela que as marcas pintam e sobre a qual o `Ctrl+E` age. «A primeira» e não
    /// «a que tem o cursor» porque um módulo com duas listas tem uma que é o assunto e
    /// outra que é apoio — e a de cima é sempre a primeira. Um módulo com o formulário
    /// aberto devolve `None` daqui sozinho: o formulário **substitui** o arranjo, então
    /// não há tabela, não há linha, e o `Ctrl+E` não tem o que marcar.
    pub fn primeira_tabela(&self) -> Option<&Pane> {
        match self {
            Layout::Leaf(pane) => matches!(**pane, Pane::Table { .. }).then_some(&**pane),
            Layout::Rows(partes) | Layout::Cols(partes) => {
                partes.iter().find_map(|(_, filho)| filho.primeira_tabela())
            }
        }
    }
}

impl Pane {
    /// O painel como texto puro, para quem quer **copiar** em vez de olhar.
    ///
    /// Sem moldura, sem cor e sem corte: uma tabela vira colunas alinhadas com espaço, um
    /// texto vira as linhas dele. É o mesmo conteúdo da tela, no formato que sobrevive a
    /// ser colado num chamado — que é onde um achado de banco de dados costuma terminar.
    pub fn como_texto(&self) -> Vec<String> {
        match self {
            Pane::Table {
                title,
                headers,
                rows,
                note,
                ..
            } => {
                let mut linhas = vec![title.clone()];
                // As larguras saem do conteúdo, não de um palpite: a coluna de query é a
                // única larga, e dar a todas a largura dela seria uma tabela de espaços.
                let colunas = headers
                    .len()
                    .max(rows.iter().map(|r| r.cells.len()).max().unwrap_or(0));
                let mut larguras = vec![0usize; colunas];
                for (i, header) in headers.iter().enumerate() {
                    larguras[i] = header.chars().count();
                }
                for row in rows {
                    for (i, cell) in row.cells.iter().enumerate() {
                        larguras[i] = larguras[i].max(cell.chars().count());
                    }
                }
                let alinhar = |celulas: &[String]| -> String {
                    let mut linha = String::new();
                    for (i, celula) in celulas.iter().enumerate() {
                        // A última coluna não é preenchida: espaço no fim da linha é
                        // sujeira que aparece na hora de colar.
                        if i + 1 == celulas.len() {
                            linha.push_str(celula);
                        } else {
                            linha.push_str(&format!("{celula:<largura$}  ", largura = larguras[i]));
                        }
                    }
                    linha.trim_end().to_string()
                };
                if !headers.is_empty() {
                    linhas.push(alinhar(headers));
                    linhas.push("-".repeat(larguras.iter().sum::<usize>() + 2 * colunas));
                }
                for row in rows {
                    linhas.push(alinhar(&row.cells));
                }
                if let Some((nota, _)) = note {
                    linhas.push(nota.clone());
                }
                linhas
            }
            Pane::Facts { title, rows } => {
                let largura = rows
                    .iter()
                    .map(|(rotulo, _, _)| rotulo.chars().count())
                    .max()
                    .unwrap_or(0);
                let mut linhas = vec![title.clone()];
                for (rotulo, valor, _) in rows {
                    linhas.push(
                        format!("{rotulo:<largura$}  {valor}")
                            .trim_end()
                            .to_string(),
                    );
                }
                linhas
            }
            Pane::Text { title, lines, .. } => {
                let mut linhas = vec![title.clone()];
                linhas.extend(lines.iter().map(|(texto, _)| texto.trim_end().to_string()));
                linhas
            }
            Pane::Bars { title, rows, .. } => {
                let mut linhas = vec![title.clone()];
                linhas.extend(
                    rows.iter()
                        .map(|bar| format!("{}  {}", bar.label, bar.text)),
                );
                linhas
            }
            Pane::Grid { title, cells, .. } => {
                let mut linhas = vec![title.clone()];
                linhas.extend(
                    cells
                        .iter()
                        .map(|cell| format!("{}  {}  {}", cell.label, cell.sub, cell.detalhe)),
                );
                linhas
            }
            Pane::Chart { title, note, .. } => {
                let mut linhas = vec![title.clone()];
                linhas.extend(note.clone());
                linhas
            }
            Pane::Empty { title, note } => vec![title.clone(), note.clone()],
            Pane::Form { title, fields, .. } => {
                let mut linhas = vec![title.clone()];
                linhas.extend(
                    fields
                        .iter()
                        .map(|field| format!("{}: {}", field.label, field.value)),
                );
                linhas
            }
        }
    }
}

impl Layout {
    /// O arranjo inteiro como texto, na ordem em que é desenhado.
    pub fn como_texto(&self) -> Vec<String> {
        match self {
            Layout::Leaf(pane) => pane.como_texto(),
            Layout::Rows(partes) | Layout::Cols(partes) => partes
                .iter()
                .flat_map(|(_, filho)| filho.como_texto())
                .collect(),
        }
    }
}
