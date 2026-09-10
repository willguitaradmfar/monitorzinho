//! O contrato de um módulo, e o vocabulário com que ele desenha.
//!
//! Um módulo **não recebe um `Frame` do ratatui**. Ele devolve uma descrição do que quer
//! na tela, e o `ui.rs` desenha.
//!
//! A razão é o pedido: «sem sair do padrão do monitorzinho». Um `render(&self, frame,
//! area)` por módulo é liberdade total para sair do padrão, e com duas dezenas de módulos
//! o padrão vira duas dezenas de variações da mesma moldura. O `Pane` é a garantia
//! mecânica de que isso não acontece: um módulo desenha bem porque não tem como desenhar
//! diferente. E o `ui.rs` continua sendo o único lugar do programa que conhece o ratatui.
//!
//! O preço é real: um módulo com uma necessidade visual que o enum não cobre fica
//! bloqueado até o enum crescer. Isso é intencional — crescer o vocabulário é uma decisão
//! consciente com um caso concreto na mão, e não um efeito colateral de alguém com pressa.

use crossterm::event::KeyEvent;

use crate::invest::model::{
    Alerta, AssetId, Carteira, Lancamento, Portfolio, PosKey, Position, Provento,
};
use crate::invest::provider::MarketSnapshot;

/// Em que grupo o módulo aparece na lista.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Group {
    Carteira,
    Mercado,
    Analise,
    Informacao,
    Operacao,
}

impl Group {
    /// A ordem em que alguém constrói o uso: primeiro o que é meu, depois o que está
    /// acontecendo, depois o que isso quer dizer.
    pub const ALL: [Group; 5] = [
        Group::Carteira,
        Group::Mercado,
        Group::Analise,
        Group::Informacao,
        Group::Operacao,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            Group::Carteira => "carteira",
            Group::Mercado => "mercado",
            Group::Analise => "análise",
            Group::Informacao => "informação",
            Group::Operacao => "operação",
        }
    }
}

/// Uma pré-condição de um módulo, para a lista poder dizer o que falta em vez de deixar
/// alguém abrir uma tela vazia sem explicação.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Need {
    Posicoes,
    Cotacao,
    Cambio,
    Historico,
    /// Precisa de uma fonte que este build não tem. A linha diz qual.
    Provedor(&'static str),
}

/// O que a coluna de estado da lista mostra. Vazio é o bom estado — o silêncio.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Estado {
    Pronto,
    Falta(String),
}

/// A leitura-só do estado da aba que a lista e os módulos compartilham.
pub struct Ctx<'a> {
    pub portfolio: &'a Portfolio,
    pub market: &'a MarketSnapshot,
    /// Para pedir série histórica, que é a única coisa que um módulo busca por conta
    /// própria — e mesmo assim numa thread, nunca no caminho do desenho.
    pub providers: &'a std::sync::Arc<crate::invest::provider::ProviderSet>,
    /// A curva do patrimônio, um ponto por dia. Vem do estado da aba porque o cartão da
    /// home também a usa, e um cartão não pode ler arquivo — `widget` roda para todo
    /// módulo a cada volta.
    pub patrimonio: &'a [crate::invest::serie::Ponto],
    /// As séries históricas já buscadas, compartilhadas por todos os módulos que as usam.
    /// Quem pede com `get` dispara a busca; quem só quer o que há usa `peek`.
    pub historico: &'a crate::invest::historico::Cache,
    /// Os fundamentos das empresas, pelo mesmo desenho.
    pub fundamentos: &'a crate::invest::fundamento::Cache,
    /// Os proventos que as empresas anunciaram. **Sugestões**: quem confirma é quem
    /// investe, porque o valor recebido depende da quantidade que se tinha na data-com e
    /// nenhuma fonte de mercado sabe isso.
    pub anunciados: &'a crate::invest::provento::Cache,
    /// As manchetes já lidas. A tela cheia pede as de verdade, com `get`, que dispara a
    /// busca; a home lê com `ja_tem`, que não dispara nada.
    pub noticias: &'a crate::invest::noticias::Cache,
    /// O calendário econômico já buscado. Quem está numa tela cheia pede o de verdade,
    /// com `get`, que dispara a busca; a home lê com `ja_tem`, que não dispara nada.
    pub calendario: &'a crate::invest::calendario::Cache,
    /// Os disparos de alerta desde que a aba abriu, do mais recente para o mais antigo.
    /// Vêm do estado da aba e não do módulo, porque acontecem também com ele fechado.
    pub disparos: &'a [(u64, String)],
    /// Há quanto tempo cada fonte externa respondeu — ver `InvestModule::buscado_em`.
    ///
    /// Um `&'static` no meio de referências emprestadas porque o registro é global: quem
    /// carimba são as threads de busca, e elas não têm o estado da aba na mão.
    pub buscas: &'static crate::invest::store::Buscas,
    pub agora: u64,
    /// Gravar está proibido — o arquivo é de uma versão futura. Um módulo que edita tem
    /// que dizer isso na tela em vez de aceitar a edição e perdê-la.
    pub somente_leitura: bool,
}

/// Uma mudança na carteira, devolvida pelo módulo e aplicada pelo app.
///
/// Os módulos não escrevem no `Portfolio` diretamente. Isso mantém uma coisa só: quem
/// grava, quando grava, e o que fazer quando gravar é proibido — e evita que cada módulo
/// tenha o seu jeito de salvar.
#[derive(Clone, Debug)]
pub enum Edit {
    UpsertPosicao(Box<Position>),
    RemoverPosicao(PosKey),
    /// Preço informado de um ativo. É de natureza diferente do preço médio: aquele é
    /// custo e é imutável; este é valor de mercado que por acaso foi digitado.
    PrecoManual(AssetId, f64),
    /// O preço médio de um ativo, aplicado a **todas as posições dele**, em qualquer
    /// corretora.
    ///
    /// Existe porque as duas metades da informação vêm de lugares diferentes: o extrato
    /// da B3 diz quanto se tem e onde, e não diz o custo; a planilha de quem investe diz o
    /// custo, e não sabe de corretora. Substituir uma fonte com a planilha duplicaria a
    /// carteira inteira; esta operação escreve só a coluna que falta.
    ///
    /// Um preço médio é **por unidade**, então ele vale igual nas duas corretoras que
    /// tenham o mesmo papel — não há o que ratear.
    PrecoMedio(AssetId, f64),
    AddWatch(AssetId),
    RemoveWatch(AssetId),
    SetAlvos(std::collections::BTreeMap<String, f64>),
    AddProvento(Box<Provento>),
    RemoverProvento(usize),
    AddLancamento(Box<Lancamento>),
    RemoverLancamento(usize),
    SetAlertas(Vec<Alerta>),
    /// Cria ou substitui uma carteira recomendada, pelo nome.
    ///
    /// Uma operação só porque a carteira é uma coisa só: mexer num alvo e gravar a lista
    /// inteira mantém a tela e o arquivo sempre de acordo, sem um caminho em que metade
    /// da mudança foi aplicada.
    SetCarteira(Box<Carteira>),
    /// Apaga uma carteira recomendada, e desmarca os ativos que apontavam para ela.
    RemoverCarteira(String),
    /// Diz de qual carteira um ativo é. `None` o desmarca.
    ///
    /// Aplica-se a **todas** as linhas daquele ativo, em qualquer corretora: o vínculo é
    /// do papel, não da posição, e duas linhas discordando fariam o mesmo dinheiro contar
    /// em duas carteiras.
    SetCarteiraDoAtivo(AssetId, Option<String>),
    SetSetor(AssetId, String),
    /// O mapeamento de colunas aprendido para uma fonte — o que faz a segunda
    /// importação daquela corretora não perguntar nada.
    SetMapeamento(String, std::collections::BTreeMap<String, String>),
    AddFeed(String),
    RemoveFeed(String),
    /// Substitui as posições de uma fonte inteira — a importação. É uma operação só
    /// porque ela **tem** que ser atômica: metade de uma importação aplicada é uma
    /// carteira que não corresponde a nada.
    SubstituirFonte {
        fonte: String,
        posicoes: Vec<Position>,
    },
}

/// O que o módulo fez com a tecla.
pub enum Outcome {
    /// Não era dele. O app pode tratar.
    Ignorada,
    /// Consumida, sem mudar nada que precise ser gravado.
    Ok,
    /// Consumida, e isto tem que ser aplicado e gravado.
    Editar(Vec<Edit>),
    /// Pede para abrir outro módulo, **levando junto o ativo que estava sob o cursor**.
    ///
    /// O alvo não é enfeite: sem ele, «Enter no PETR4 em Cotações» abria o Gráfico no
    /// primeiro ativo com série que existisse — sempre o mesmo, nunca o escolhido. Um
    /// atalho que ignora a seleção é pior que atalho nenhum, porque parece funcionar.
    Abrir {
        modulo: &'static str,
        /// `None` quando a transição não é sobre um ativo — «r» da Alocação para o
        /// Rebalanceamento fala da carteira inteira.
        alvo: Option<AssetId>,
    },
}

/// Se o `Esc` foi consumido dentro do módulo. `Nao` é o que faz o app perguntar se sai.
#[derive(PartialEq, Eq, Debug)]
pub enum Escape {
    /// Havia uma caixa aberta, ou uma busca digitada, e ela foi desfeita.
    Consumido,
    /// Não havia o que desfazer.
    Nao,
}

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
    /// Quanto espaço ela pede, relativo às outras. `1.0` é a fatia média.
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

/// Que marcas uma tela aceita: o nome com que elas são gravadas, e sobre o que podem ser.
///
/// É o mesmo par que uma `TableMonitor` devolve em `id()` e `mark_kinds()`, e de propósito:
/// uma marca posta numa lista da Invest é gravada, listada e apagada pelo mesmíssimo
/// caminho de uma posta na lista de processos. O `Ctrl+E` não tem duas implementações.
#[derive(Clone, Copy)]
pub struct Marcavel {
    /// Identificador estável, no mesmo espaço de nomes de `TableMonitor::id` — uma marca
    /// gravada com ele sobrevive a renomear o módulo.
    ///
    /// **Duas telas podem dividir o mesmo id, e dez dividem.** Um papel é um papel:
    /// segui-lo em Posições e não o ver seguido em Cotações, na carteira recomendada e
    /// nos Proventos dele seria a marca valendo só no lugar onde foi feita — que é o
    /// oposto do que se pede a ela. Ver `marcas::ATIVOS`.
    pub tabela: &'static str,
    /// Como a lista de marcas chama esta tabela. Não é o nome do módulo porque as telas
    /// que dividem um id dividem também o nome — a mesma marca não pode se apresentar de
    /// um jeito conforme a tela em que nasceu.
    pub nome: &'static str,
    pub tipos: &'static [TipoDeMarca],
}

/// Uma coisa que uma marca da Invest pode ser sobre, e **em que coluna** ela está.
///
/// A coluna é dita pelo cabeçalho e não pelo número, que é a única diferença real para o
/// `MarkKind` das tabelas do sistema — e a diferença existe porque a mesma lista aparece
/// em mais de uma forma. «Ativo» é a coluna 1 na tela de Proventos, a 0 na aba «por
/// ativo» do mesmo módulo, e a 1 outra vez no cartão da home. Um número seria a coluna
/// certa numa das três e a errada nas outras duas — e uma marca que casa em algumas telas
/// e não em outras é pior do que uma marca que não existe.
pub struct TipoDeMarca {
    pub nome: &'static str,
    /// O texto do cabeçalho da coluna que carrega o assunto. Onde a tabela não tem essa
    /// coluna, o tipo simplesmente não vale nela.
    pub coluna: &'static str,
    pub numerico: bool,
    pub ajuda: &'static str,
}

/// Um módulo da aba Invest.
///
/// A diferença para uma `TableMonitor` é o custo: uma tabela amostra e devolve linhas toda
/// vez que se olha para ela, enquanto um módulo tem estado, threads e uma tela própria —
/// e por isso só existe entre `open` e o momento em que é fechado.
pub trait InvestModule: Send + Sync {
    /// Chave estável, gravada no arquivo do MRU — nunca muda depois de publicada, mesma
    /// regra de `Tool::id` e `Monitor::id`.
    fn id(&self) -> &'static str;
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn group(&self) -> Group;

    fn needs(&self) -> &'static [Need] {
        &[]
    }

    /// De que fonte externa esta tela vive, para a moldura dizer há quanto tempo ela foi
    /// consultada. `None` numa tela que só faz conta sobre o que já está em disco.
    ///
    /// Um dos nomes de `store::fonte`. Um módulo que lê duas fontes declara a que é o
    /// **assunto** dele — Fundamentos vive dos fundamentos e só usa a cotação para
    /// comparar, e é a idade dos fundamentos que responde «isto ainda vale?».
    ///
    /// Existe porque a defasagem era invisível: um preço em cache, uma manchete guardada
    /// e um calendário de duas semanas atrás apareciam com exatamente a mesma cara de
    /// dado fresco, e não havia nada na tela que dissesse o contrário.
    fn fonte_externa(&self) -> Option<&'static str> {
        None
    }

    /// Que marcas a lista deste módulo aceita — `None` quando não é uma lista de coisas
    /// que se acompanha.
    ///
    /// Vale para os dois lugares em que o módulo aparece: o cartão da home e a tela
    /// aberta. É de propósito que seja **uma** declaração para os dois — marcar um papel
    /// dentro de Posições e não vê-lo marcado no cartão de Posições seria a marca dizendo
    /// duas coisas diferentes sobre a mesma linha.
    fn marcavel(&self) -> Option<Marcavel> {
        None
    }

    /// A coluna de resumo da linha na lista.
    ///
    /// **Sem I/O.** Roda a cada amostragem da aba, para todos os módulos, inclusive os que
    /// nunca foram abertos — um `summary()` que abrisse socket transformaria a lista no
    /// lugar mais caro do programa.
    fn summary(&self, ctx: &Ctx) -> String;

    /// Quanto lugar este módulo merece na home, de 0 a 5.
    ///
    /// **Declarado, e não medido.** A primeira versão ordenava a grade pelo número de
    /// linhas que o cartão tinha naquele instante — e como esse número cresce quando a
    /// cotação chega ou quando um cache enche, os cartões trocavam de lugar sozinhos. Uma
    /// home que se rearranja obriga a reler a tela antes de cada tecla, e a tecla de um
    /// módulo deixa de ser decorável.
    ///
    /// Este número não muda em tempo de execução. É ele que decide **a ordem e a altura**
    /// do cartão, e as duas ficam paradas onde estão.
    fn destaque(&self) -> u8 {
        1
    }

    /// O painel compacto deste módulo na tela principal da aba.
    ///
    /// É o que transforma a home numa mesa de trabalho em vez de um índice: em vez de uma
    /// lista de nomes que exige entrar para ver qualquer coisa, cada módulo mostra o que
    /// tem a dizer e a tecla de atalho entra nele.
    ///
    /// **Nunca bloqueia**: isto roda para todo módulo visível a cada volta da aba. Ele
    /// pode *disparar* uma busca em segundo plano — é o que enche o painel sem obrigar a
    /// entrar em cada módulo —, mas jamais espera por ela: o que desenha é o que o cache
    /// já tem, e o resto aparece na volta seguinte.
    ///
    /// O que ele **não** dispara é busca cara por ativo. Reconstruir a curva do patrimônio
    /// custa uma chamada por papel da carteira, e isso é coisa de módulo aberto.
    ///
    /// `None` num módulo sem nada a mostrar de fora; ele ainda aparece, com o resumo.
    fn widget(&self, _ctx: &Ctx) -> Option<Pane> {
        None
    }

    /// Os cartões que este módulo põe na home.
    ///
    /// Quase todo módulo põe **um**, com o nome dele — é o que o padrão faz. Carteiras
    /// recomendadas põe **um por carteira**: cada uma é uma coisa que se acompanha por si,
    /// e uma linha «3 carteiras» num painel não responde nada sobre nenhuma delas.
    ///
    /// Vale a mesma regra de `widget`: nunca bloqueia.
    fn cartazes(&self, ctx: &Ctx) -> Vec<Cartaz> {
        vec![Cartaz {
            titulo: self.name().to_string(),
            sub: None,
            pane: self.widget(ctx),
        }]
    }

    /// Abre **numa coisa específica** de dentro do módulo — a carteira do cartão que foi
    /// apertado, por exemplo.
    ///
    /// O padrão ignora o `sub` e abre como sempre: só quem põe mais de um cartão precisa
    /// saber em qual deles a tecla foi.
    fn open_em(&self, ctx: &Ctx, _sub: Option<&str>) -> Box<dyn ModuleView> {
        self.open(ctx, None)
    }

    /// Abre. É aqui, e só aqui, que qualquer custo nasce.
    ///
    /// `alvo` é o ativo em que o módulo deve abrir, quando quem pediu tinha um sob o
    /// cursor. A maioria dos módulos o ignora; os que desenham **um** ativo — Gráfico,
    /// Indicadores, Comparador — existem para respeitá-lo.
    fn open(&self, ctx: &Ctx, alvo: Option<&AssetId>) -> Box<dyn ModuleView>;

    /// Palavras que a busca da lista deve encontrar além do nome e do resumo — «RSI»
    /// achando Indicadores, «DARF» achando Imposto.
    fn keywords(&self) -> &'static str {
        ""
    }
}

/// Um cartão que um módulo põe na home.
pub struct Cartaz {
    pub titulo: String,
    /// Que coisa dentro do módulo este cartão representa — o nome de uma carteira. `None`
    /// no caso comum, em que o cartão é o módulo inteiro.
    pub sub: Option<String>,
    pub pane: Option<Pane>,
}

/// A tela viva de um módulo.
pub trait ModuleView: Send {
    fn title(&self) -> String;

    /// Chamado a cada tick, só enquanto o módulo está na tela. Lê retratos que as threads
    /// publicaram; **nunca faz I/O**.
    fn tick(&mut self, _ctx: &Ctx) {}

    fn layout(&self, ctx: &Ctx) -> Layout;

    fn key(&mut self, key: KeyEvent, ctx: &Ctx) -> Outcome;

    /// Se o módulo consumiu o `Esc` internamente. `Esc` desfaz uma camada por vez, que é
    /// a regra de todo o programa: primeiro a caixa aberta, depois a busca digitada, e só
    /// então a pergunta de saída.
    fn escape(&mut self) -> Escape {
        Escape::Nao
    }

    /// A linha de teclas do rodapé, no formato das outras telas.
    fn hint(&self) -> String;

    /// Se este módulo precisa que a busca de cotações esteja rodando. Um módulo que só
    /// faz conta sobre a carteira não acorda thread nenhuma.
    fn quer_mercado(&self) -> bool {
        false
    }

    /// Ativos que este módulo quer buscar **enquanto está aberto**, além do que a carteira
    /// já pede.
    ///
    /// É como as séries do Banco Central chegam à tela de Renda Fixa sem ninguém precisar
    /// pô-las na watchlist — e é o que garante que elas parem de ser buscadas no instante
    /// em que o módulo fecha.
    fn ativos(&self) -> Vec<AssetId> {
        Vec::new()
    }
}

#[cfg(test)]
#[cfg(test)]
static ANUNCIADOS_DE_TESTE: std::sync::OnceLock<crate::invest::provento::Cache> =
    std::sync::OnceLock::new();

#[cfg(test)]
static HISTORICO_DE_TESTE: std::sync::OnceLock<crate::invest::historico::Cache> =
    std::sync::OnceLock::new();

#[cfg(test)]
static FUNDAMENTOS_DE_TESTE: std::sync::OnceLock<crate::invest::fundamento::Cache> =
    std::sync::OnceLock::new();

#[cfg(test)]
static NOTICIAS_DE_TESTE: std::sync::OnceLock<crate::invest::noticias::Cache> =
    std::sync::OnceLock::new();

#[cfg(test)]
static CALENDARIO_DE_TESTE: std::sync::OnceLock<crate::invest::calendario::Cache> =
    std::sync::OnceLock::new();

/// Um contexto de teste: carteira e mercado emprestados, e um conjunto de provedores
/// vazio. Vive aqui para que cada módulo não invente o seu.
#[cfg(test)]
pub fn ctx_de_teste<'a>(
    portfolio: &'a Portfolio,
    market: &'a MarketSnapshot,
    providers: &'a std::sync::Arc<crate::invest::provider::ProviderSet>,
) -> Ctx<'a> {
    Ctx {
        buscas: crate::invest::store::buscas(),
        patrimonio: &[],
        noticias: NOTICIAS_DE_TESTE.get_or_init(Default::default),
        historico: HISTORICO_DE_TESTE.get_or_init(Default::default),
        fundamentos: FUNDAMENTOS_DE_TESTE.get_or_init(Default::default),
        anunciados: ANUNCIADOS_DE_TESTE.get_or_init(Default::default),
        calendario: CALENDARIO_DE_TESTE.get_or_init(Default::default),
        portfolio,
        market,
        providers,
        disparos: &[],
        agora: 0,
        somente_leitura: false,
    }
}

/// O conjunto que `ctx_de_teste` empresta.
///
/// São os provedores **de verdade**, e não uma lista vazia: `tem_historico` é uma
/// pergunta a eles, e com o conjunto vazio nenhum ativo teria série — o que faria os
/// testes dos módulos de gráfico passarem por acidente. Construir um provedor não faz
/// I/O nenhum, e `ProviderSet::new` não cria thread: quem cria é `start`.
#[cfg(test)]
pub fn providers_de_teste() -> std::sync::Arc<crate::invest::provider::ProviderSet> {
    let manuais = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
    crate::invest::provider::ProviderSet::new(
        crate::invest::providers::todos(std::sync::Arc::clone(&manuais)),
        manuais,
        &Default::default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busca_de_linha_e_por_qualquer_celula() {
        let r = Row::new(vec!["PETR4".into(), "ação".into(), "38,42".into()]);
        assert!(r.matches("petr"));
        assert!(r.matches("ação"));
        assert!(r.matches("38"));
        assert!(!r.matches("vale"));
        assert!(r.matches(""), "busca vazia casa com tudo");
    }

    #[test]
    fn grupos_estao_na_ordem_de_uso() {
        assert_eq!(Group::ALL[0], Group::Carteira);
        assert_eq!(Group::ALL[1], Group::Mercado);
        assert_eq!(Group::ALL.len(), 5);
    }
}

#[cfg(test)]
mod alvo_tests {
    use super::*;
    use crate::invest::model::{Market, Portfolio};
    use crate::invest::modules;
    use crate::invest::provider::MarketSnapshot;

    /// Fixa o contrato que faltava: um módulo que desenha **um** ativo tem que abrir no
    /// que lhe foi pedido.
    ///
    /// Sem isto, «Enter no PETR4» em Cotações abria o gráfico no primeiro ativo com série
    /// que existisse — na prática, sempre o mesmo. Um atalho que ignora a seleção é pior
    /// que atalho nenhum, porque parece funcionar.
    #[test]
    fn os_modulos_de_um_ativo_abrem_no_alvo() {
        let mut portfolio = Portfolio::default();
        // Uma watchlist em que o «primeiro com série» é outro, para o teste distinguir
        // «abriu no alvo» de «abriu no padrão».
        portfolio
            .watchlist
            .push(AssetId::new(Market::Binance, "BTCBRL"));
        portfolio.watchlist.push(AssetId::new(Market::Fx, "USDBRL"));
        let market = MarketSnapshot::default();
        let providers = providers_de_teste();
        let ctx = ctx_de_teste(&portfolio, &market, &providers);

        let alvo = AssetId::new(Market::Fx, "USDBRL");
        for id in ["grafico", "indicadores"] {
            let m = modules::todos()
                .into_iter()
                .find(|m| m.id() == id)
                .expect("o módulo tem que estar registrado");
            let padrao = m.open(&ctx, None).title();
            let com_alvo = m.open(&ctx, Some(&alvo)).title();
            assert!(
                com_alvo.contains("USDBRL"),
                "{id} ignorou o alvo: abriu «{com_alvo}»"
            );
            assert_ne!(padrao, com_alvo, "{id} abriu igual com e sem alvo");
        }
    }

    #[test]
    fn sem_alvo_o_modulo_escolhe_por_conta_propria() {
        // O alvo é uma preferência, não uma exigência: abrir o Gráfico pela lista de
        // módulos não passa alvo nenhum, e ele ainda tem que mostrar alguma coisa.
        let mut portfolio = Portfolio::default();
        portfolio
            .watchlist
            .push(AssetId::new(Market::Binance, "BTCBRL"));
        let market = MarketSnapshot::default();
        let providers = providers_de_teste();
        let ctx = ctx_de_teste(&portfolio, &market, &providers);
        let m = modules::todos()
            .into_iter()
            .find(|m| m.id() == "grafico")
            .unwrap();
        assert!(m.open(&ctx, None).title().contains("BTCBRL"));
    }
}

#[cfg(test)]
mod largura_tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;

    use super::{Row, Tone};

    /// Desenha uma tabela de módulo e devolve o que apareceu, linha a linha.
    fn desenhar(largura: u16, headers: &[&str], rows: Vec<Row>) -> Vec<String> {
        let pane = super::Pane::Table {
            title: "t".into(),
            headers: headers.iter().map(|h| h.to_string()).collect(),
            rows,
            selected: Some(0),
            query: String::new(),
            note: None,
        };
        let mut terminal = Terminal::new(TestBackend::new(largura, 8)).unwrap();
        terminal
            .draw(|f| crate::ui::render_pane_teste(f, Rect::new(0, 0, largura, 8), &pane))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..8)
            .map(|y| {
                (0..largura)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn a_tabela_ocupa_a_largura_e_nao_se_encolhe_num_canto() {
        // O sintoma relatado: a tabela aparecia espremida à direita com a tela vazia à
        // esquerda. Com todas as colunas em `Length` e sobra de espaço, o layout não tinha
        // onde pôr o resto.
        let linhas = desenhar(
            120,
            &["Ativo", "Último", "Var%"],
            vec![
                Row::new(vec!["PETR4".into(), "48,09".into(), "+2,08%".into()]),
                Row::new(vec!["VALE3".into(), "79,02".into(), "-0,40%".into()]),
            ],
        );
        let corpo = linhas.join("\n");
        assert!(
            corpo.contains("PETR4"),
            "a linha tem que aparecer:\n{corpo}"
        );
        // A primeira coluna começa à esquerda, e não empurrada para o meio da tela.
        let linha_petr = linhas.iter().find(|l| l.contains("PETR4")).unwrap();
        let comeco = linha_petr.find("PETR4").unwrap();
        assert!(
            comeco < 10,
            "a tabela começou na coluna {comeco}, espremida à direita:\n«{linha_petr}»"
        );
    }

    #[test]
    fn conteudo_largo_nao_faz_as_colunas_seguintes_sumirem() {
        // O outro sintoma: valores que piscavam e sumiam. Uma célula longa fazia a soma
        // das larguras passar da tela, e as colunas depois dela ficavam com zero.
        let linhas = desenhar(
            80,
            &["Ativo", "Último", "P&L"],
            vec![
                Row::new(vec![
                    "PETR4".into(),
                    "48,09 yahoo não-oficial muito longo mesmo".into(),
                    "1.896,00".into(),
                ]),
                Row::new(vec!["VALE3".into(), "79,02".into(), "2.673,00".into()]),
            ],
        );
        let corpo = linhas.join("\n");
        assert!(
            corpo.contains("1.896") || corpo.contains("2.673"),
            "a última coluna sumiu quando a do meio cresceu:\n{corpo}"
        );
    }

    #[test]
    fn tabela_de_muitas_colunas_cabe_numa_tela_estreita() {
        let linhas = desenhar(
            60,
            &[
                "Ativo", "Classe", "Qtd", "PM", "Atual", "Mercado", "P&L", "Peso",
            ],
            vec![Row::tinted(
                vec![
                    "PETR4".into(),
                    "ação".into(),
                    "300,00".into(),
                    "32,10".into(),
                    "48,09 brapi".into(),
                    "14.427,00".into(),
                    "4.797,00".into(),
                    "26,4%".into(),
                ],
                Tone::Normal,
            )],
        );
        assert!(
            linhas.join("\n").contains("PETR4"),
            "numa tela estreita a tabela sumiu por inteiro"
        );
    }
}
