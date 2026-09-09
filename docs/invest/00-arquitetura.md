# 00 — Arquitetura da aba Invest

**Estado:** planejado · **Fase:** 1
**Toca:** `src/app.rs`, `src/ui.rs`, `src/main.rs` (tudo aditivo) e `src/invest/` (novo)

Este documento é o contrato. Todo módulo em `docs/invest/` assume o que está aqui, e
nenhum módulo pode contrariá-lo — se um precisar, o desenho está errado e é este arquivo
que muda primeiro.

---

## 1. O que a aba é

Um terminal de mercado dentro do monitorzinho: a carteira, as cotações, o risco e as
contas que se faz em cima disso.

É a primeira aba que **não fala sobre a máquina em que o programa roda**. Todas as
outras respondem "o que está acontecendo aqui"; esta responde "o que está acontecendo
com o meu dinheiro". Essa diferença não é filosófica — ela decide três coisas:

* **A fonte de dados é a rede**, não o `/proc`. Uma leitura pode demorar segundos,
  falhar, ou ser recusada por cota. Nenhum monitor daqui tem esse problema.
* **O dado é do usuário**, não da máquina. Ele tem que sobreviver a reinstalação, e um
  arquivo corrompido não pode custar a carteira.
* **A aba tem estado próprio**, e caro. As outras amostram e esquecem.

## 2. Onde ela fica, e por que sempre aparece

Última na barra: `Visão Geral · Processos · Containers · tmux · Ferramentas · Invest`.

Última porque é a mais distante do assunto do programa — quem abriu o monitorzinho para
ver por que o servidor está lento passa por ela e não por cima dela.

**Sempre aparece**, ao contrário de Containers e tmux, que só existem onde há o que
mostrar. A regra daquelas duas é "uma aba permanentemente vazia é ruído": sem engine não
há container para listar, e a aba não teria conteúdo nem caminho para ganhar um. Aqui é o
contrário — a aba nunca está vazia, porque o catálogo de módulos *é* o conteúdo inicial, e
é exatamente por ele que se começa a usar a coisa. Uma aba condicional que só aparecesse
depois de configurada seria uma aba que ninguém descobre.

`Tab::ALL` passa de `[Tab; 5]` para `[Tab; 6]`. `App::tabs()` não ganha filtro nenhum.

## 3. Custo zero fora de foco — as regras duras

O comentário do próprio `Tab` já diz: *"cada aba é amostrada só enquanto é a aba ativa,
então sair de uma para de gastar recurso com ela"*. A aba Invest não afrouxa isso; ela
aperta, porque o recurso que ela gasta não é só CPU local.

Quatro regras, em ordem de importância:

1. **`App::new()` não constrói nada da Invest.** O campo é `invest: Option<Box<InvestState>>`
   e nasce `None`. Abrir o monitorzinho para olhar CPU não pode ler arquivo de carteira,
   nem resolver DNS, nem alocar um cache de cotação.

2. **Entrar na aba constrói o estado; sair não o destrói.** A construção é: ler a
   carteira e a ordem dos módulos do banco do perfil, montar o registro de módulos. São
   algumas consultas em tabelas pequenas e alocação — dezenas de microssegundos, não
   milissegundos. Nada de rede acontece aqui.

3. **Nenhuma thread de rede nasce ao entrar na aba.** Ela nasce quando um módulo que
   precisa dela é **aberto**, e morre quando o último módulo que a usava é fechado. Isto
   é mais rígido do que o `container::Store` faz — ele desacelera para `IDLE_REFRESH`
   (20 s) em vez de parar — e a diferença é deliberada:

   > Ler um socket local de graça a cada 20 segundos é aceitável. Bater numa API de
   > mercado a cada 20 segundos quando ninguém está olhando gasta cota de chamadas que
   > alguém tem em quantidade finita, e num provedor pago gasta dinheiro. A cota é um
   > recurso do usuário, e o programa não pode gastá-la em segundo plano por conta própria.

   A exceção — a única — são os **alertas** (módulo 42) e as **widgets favoritas** (§7.3),
   que existem justamente para rodar com a aba fechada. Ambas são explicitamente ligadas
   pelo usuário, uma a uma, e ambas dizem na tela o que estão consumindo.

4. **`sample()` nunca faz I/O de rede.** Nem no monitor da lista, nem em módulo nenhum.
   O `tick()` do app é síncrono e roda no laço de desenho; uma chamada HTTP ali congela a
   interface pelo tempo do timeout. Toda leitura de rede acontece em thread de fundo e a
   UI lê um retrato sob mutex, exatamente como `container::store::Store::read`.

### O que isso custa, em número

`--bench` mede o custo de uma amostragem de cada aba, que é o que se paga ao apertar Tab.
A aba Invest **não entra no `--bench`**, e isso é intencional: o comando existe para
medir leitura local — `/proc`, cgroup, socket do tmux — e um `--bench` que abrisse conexão
com a Binance seria um comando de diagnóstico que precisa de internet para responder.
O custo de amostragem da Invest é uma ordenação de vetor de dezenas de itens; se algum dia
deixar de ser, é sinal de que a regra 4 foi quebrada.

## 4. O ciclo de vida, ponta a ponta

```
programa abre          → invest: None                              (custo: zero)
Tab até Invest         → switch_tab marca pending_sample           (custo: zero)
primeiro desenho       → mostra a lista com o que já tem
tick seguinte          → InvestState::load(): dois arquivos         (~dezenas de µs)
tecla «1»              → lista em tela cheia, busca ativa
digita, Enter          → módulo abre: aí sim nascem threads         (custo: o do módulo)
Esc                    → caixa: «sair do módulo?»
Enter                  → volta para a lista, módulo suspenso
Tab para outra aba     → threads do módulo param                    (volta a custo zero)
```

Sair da aba com um módulo aberto **não fecha o módulo** — ele fica suspenso, com o estado
intacto, e volta ao vivo quando a aba voltar. O que morre são as threads de rede. Isso
mantém a promessa de custo zero sem punir quem foi olhar o CPU por dez segundos.

## 5. A navegação

### 5.1 A tela principal da aba

Uma grade de widgets, no padrão que a aba Visão Geral já usa:

```
┌─ fita (opcional, uma linha) ───────────────────────────────────┐
├─ Módulos ─────────────────────────────────────────────── [1] ──┤
│ ▸ Posições              carteira    12 ativos · R$ 148.302,10  │
│   Cotações              mercado     8 acompanhados             │
│   Renda fixa            mercado     CDI 10,40% · IPCA 4,12%    │
│   Alocação              carteira    5 classes · desvio 3,1 p.p.│
│   …                                                            │
├────────────────────────┬───────────────────────────────────────┤
│  (favorita) [2]        │  (favorita) [3]                       │
└────────────────────────┴───────────────────────────────────────┘
```

A widget **Módulos** é a primeira e ocupa a metade de cima. As de baixo são as favoritas
— vazias na v1, por decisão explícita: elas existem para o que o usuário quiser monitorar
sem entrar em módulo nenhum, e essa escolha só faz sentido depois que houver módulos para
escolher. O espaço fica reservado no layout desde já, mostrando o que é.

### 5.2 A lista é uma `TableMonitor`, e isso não é detalhe de implementação

Fazer a lista de módulos ser uma `TableMonitor` comum ganha, **de graça e sem uma linha
de código de interface nova**, tudo que o usuário pediu:

| O que se pediu | O que já existe |
| --- | --- |
| widget no padrão do monitorzinho | `render_table_panel`, com a mesma moldura e o mesmo badge |
| atalho por número | `shortcut_targets()` → `Tab::Invest` cai no ramo de `tables_on` |
| tela cheia no atalho | `activate_shortcut` → `Focus::Table(TableFocus)` |
| buscar digitando | `search_push` — toda letra numa tabela em tela cheia já é busca |
| entrar com Enter | `open_row()` |

Nenhuma dessas cinco coisas precisa ser escrita. É a razão de a lista ser uma tabela e
não uma tela própria: uma tela própria significaria reimplementar busca, seleção,
paginação e moldura, e significaria que elas se comportariam *quase* como no resto do
programa.

Detalhes: `mark_kinds()` devolve `&[]` (marcar um módulo não quer dizer nada — favoritar
é outra coisa, §7.3), `tree()` é `false`, e `TableRow::key` carrega o `InvestModule::id`,
que é como o Enter sabe o que abrir. O campo `pid` fica em zero — não há processo — e
nada o lê nesta tabela.

### 5.3 A ordem é o último acessado no topo

A lista é ordenada por **quando cada módulo foi aberto pela última vez**, do mais
recente para o mais antigo. Módulos nunca abertos vêm depois, na ordem de registro.

Isso é escrito em `modulo_mru` (§8) no momento em que o módulo abre, não quando
fecha: abrir é o gesto que expressa interesse, e um módulo que ficou aberto por engano
por dois segundos ainda foi o último que a pessoa quis ver.

**O ponto que precisa ficar registrado:** uma lista que se reordena sozinha normalmente
briga com o idioma do monitorzinho, onde `3` é sempre o mesmo painel. Aqui não briga,
porque o atalho `1` é **a widget da lista**, não uma linha dela. Dentro da lista a
seleção é por seta e por busca, nunca por número. Nenhuma tecla muda de significado
quando a ordem muda.

### 5.4 Entrar no módulo

`Enter` numa linha da tabela da lista abre o módulo. O caminho é o `App::open_row()`, que
já bifurca por tabela — hoje trata a linha da engine no resumo de containers e o
`actions_on_enter()` das tabelas de container. Ganha um terceiro ramo, pela mesma porta.

O módulo abre em **tela cheia**, ocupando a área inteira abaixo da barra de abas. Não é
um painel grande: é uma tela, do jeito que o menu de operações e a tela de texto já são.

`Focus` ganha uma variante:

```rust
/// Um módulo da aba Invest rodando em tela cheia. Boxed pela mesma razão do detalhe e
/// do menu de operações: carrega inteira a tabela de onde veio.
Module(Box<ModuleFocus>),

pub struct ModuleFocus {
    /// Qual módulo, por `InvestModule::id` — nunca por índice, que a ordem MRU muda.
    pub module: String,
    /// A tela viva do módulo: o estado dele, e quem manda nas threads dele.
    pub view: Box<dyn ModuleView>,
    /// A lista de onde veio, devolvida intacta no Esc: mesma seleção, mesma busca.
    pub parent: TableFocus,
}
```

O `parent: TableFocus` é o mesmo padrão de `DetailFocus` e de `ActionMenu`. Sair do
módulo devolve a lista **exatamente como estava** — com a busca ainda digitada e o cursor
na mesma linha. Quem entrou procurando "corr" e abriu Correlação volta com "corr" na
caixa, e não no topo de uma lista que ainda por cima reordenou porque o módulo que ele
acabou de abrir subiu para o primeiro lugar.

### 5.5 Sair do módulo: a pergunta

`Esc` dentro de um módulo **não sai**. Ele abre uma caixa perguntando.

Esta é a única confirmação do programa que não é sobre algo destrutivo, e a razão está
no pedido: um módulo é uma tela em que se fica, com trabalho acumulado nela — um filtro
montado, um gráfico no timeframe certo, uma simulação em andamento — e `Esc` é uma tecla
que a mão aperta sozinha ao voltar de qualquer outra coisa. Perder isso por um reflexo é
um custo real, mesmo que nada seja apagado.

A mecânica reaproveita `App::pending` / `Pending` / `confirm_key`, que já tem exatamente
o comportamento necessário: fica acima de toda tela, **toma todas as teclas** para que
nada embaixo aja sobre a tecla que responde, `Enter` faz, `Esc` cancela, e qualquer outra
tecla é ignorada em vez de contar como resposta.

Uma diferença de aparência é obrigatória. A caixa de confirmação hoje é vermelha porque
toda pergunta que ela faz é sobre perda irreversível. Sair de um módulo não é isso, e uma
caixa vermelha para uma pergunta reversível ensina a pessoa a ignorar caixas vermelhas —
que é o custo mais caro que este desenho poderia gerar. Então:

* `Pending` ganha um campo de severidade (`Severity::Destructive` | `Severity::Advisory`).
* `render_confirm` escolhe a cor a partir dele: vermelho como hoje, ou amarelo.
* Todo `Pending` que existe hoje é `Destructive`, então **nenhuma caixa atual muda**.
* `PendingAction` ganha `LeaveModule`.

A caixa diz o que está acontecendo agora, e não uma pergunta vazia:

```
  ┌ Sair de Correlação? ─────────────────────────────────┐
  │                                                      │
  │   • 3 séries carregadas serão descartadas            │
  │   • a matriz de 90 dias precisa ser recalculada      │
  │   • o filtro «FII» digitado será perdido             │
  │                                                      │
  │   Enter sai. Esc fica.                               │
  └──────────────────────────────────────────────────────┘
```

Essas linhas vêm de `ModuleView::leaving()`, que cada módulo escreve em seus próprios
termos. Um módulo sem nada a perder devolve uma linha só — "nada em andamento" —, e a
caixa ainda aparece, porque foi pedida assim e porque uma confirmação que às vezes não
aparece é pior que uma que sempre aparece.

### 5.6 A ordem em que o Esc é atendido

`Esc` desfaz **uma camada por vez**, que é a regra de todo o programa. Dentro de um
módulo, na ordem:

1. Se o módulo tem uma caixa própria aberta (um formulário, um seletor) → fecha a caixa.
2. Se o módulo tem busca digitada → limpa a busca.
3. Só então → abre a pergunta de saída.

Isso vem do módulo, não do app: `ModuleView::escape()` devolve `Handled::Yes` quando
consumiu o `Esc` internamente, e `Handled::No` quando não tinha o que desfazer — e aí o
app pergunta. É o mesmo desenho de `Focus::Table(tf) if !tf.query.is_empty()`, que hoje
limpa a busca antes de sair da tela cheia.

### 5.7 Teclas, e quem fica com elas

Enquanto um módulo está aberto, **o módulo recebe toda tecla**, com três exceções que o
app nunca entrega:

| Tecla | Quem trata | Por quê |
| --- | --- | --- |
| `Ctrl+C` `Ctrl+C` | o app | é a única saída do programa e não pode ser sequestrável |
| `Esc` | o módulo primeiro, depois o app | ver §5.6 |
| `Tab` / `Shift+Tab` | ninguém | trocar de aba com um módulo aberto seria sair pela porta errada |

`Tab` já é bloqueado sem uma linha de código nova: o `main.rs` só troca de aba com
`matches!(app.focus, Focus::None)`, e um módulo aberto não é `Focus::None`.

Todo o resto — letras, setas, `PgUp`/`PgDn`, `Enter`, `Delete`, combinações com `Ctrl` —
vai para `ModuleView::key()`. Um módulo pode ter busca digitada como as tabelas têm, e
por isso não pode haver letra reservada pelo app.

## 6. O registro de módulos

Espelha `tools::all_tools()`, pelo mesmo motivo que aquele existe: uma lista num lugar
só, e um módulo novo é uma linha nela.

```rust
/// Um módulo da aba Invest.
///
/// A diferença para uma `TableMonitor` é o custo: uma tabela amostra e devolve linhas
/// toda vez que se olha para ela, enquanto um módulo tem estado, threads e uma tela
/// própria — e por isso só existe entre `open` e o momento em que é fechado.
pub trait InvestModule: Send + Sync {
    /// Chave estável, gravada no arquivo do MRU e nos favoritos — nunca muda depois de
    /// publicada, mesma regra de `Tool::id` e `Monitor::id`.
    fn id(&self) -> &'static str;
    fn name(&self) -> &'static str;
    /// Uma linha, mostrada ao lado do nome na lista.
    fn description(&self) -> &'static str;
    fn group(&self) -> Group;

    /// O que este módulo precisa para dizer alguma coisa. A lista decide o que a linha
    /// mostra na coluna de estado, e é o que evita abrir um módulo que só vai
    /// apresentar uma tela vazia sem explicar por quê.
    fn needs(&self) -> &'static [Need];

    /// A coluna de resumo da linha na lista — «12 ativos · R$ 148.302,10».
    ///
    /// Lida do que já está em memória, **sem I/O**: esta função roda a cada amostragem
    /// da aba, para todos os módulos, inclusive os que nunca foram abertos.
    fn summary(&self, ctx: &InvestCtx) -> String;

    /// Abre. É **aqui**, e só aqui, que qualquer custo nasce: thread, cache, série
    /// histórica, assinatura de provedor.
    fn open(&self, ctx: &InvestCtx) -> Box<dyn ModuleView>;
}

/// Em que grupo o módulo aparece na lista.
pub enum Group { Carteira, Mercado, Analise, Informacao, Operacao }

/// Uma pré-condição de um módulo, para a lista poder dizer o que falta.
pub enum Need {
    /// Precisa de posições cadastradas.
    Posicoes,
    /// Precisa de cotação para os ativos da carteira.
    Cotacao,
    /// Precisa de câmbio — todo módulo que consolida em BRL precisa.
    Cambio,
    /// Precisa de série histórica de preço, que nem toda fonte dá.
    Historico,
    /// Precisa de um provedor que este build não tem. A linha diz qual.
    Provedor(&'static str),
}

pub fn all_modules() -> Vec<Box<dyn InvestModule>> { /* … */ }
```

`InvestCtx` é a leitura-só do estado da aba que a lista e os módulos compartilham:
posições, câmbio corrente, retrato de cotações, e o acesso aos provedores. Ele é
passado por referência e nunca clonado.

## 7. Como um módulo desenha

### 7.1 O módulo não desenha

`ModuleView` **não recebe um `Frame` do ratatui**. Ele devolve uma descrição do que quer
na tela, e o `ui.rs` desenha.

```rust
pub trait ModuleView: Send {
    fn title(&self) -> String;
    /// Chamado a cada tick, só enquanto o módulo está na tela. Lê retratos que as
    /// threads publicaram; nunca faz I/O.
    fn tick(&mut self, ctx: &InvestCtx);
    /// O que desenhar agora.
    fn layout(&self, ctx: &InvestCtx) -> Layout;
    fn key(&mut self, key: KeyEvent, ctx: &InvestCtx) -> Handled;
    /// `Yes` se o módulo consumiu o Esc internamente — ver §5.6.
    fn escape(&mut self) -> Handled;
    /// O que se perde ao sair, uma consequência por linha — ver §5.5.
    fn leaving(&self) -> Vec<String>;
    /// A linha de teclas do rodapé, no formato das outras telas.
    fn hint(&self) -> String;
}

/// O vocabulário que o `ui.rs` sabe desenhar. Um módulo não pode inventar um visual
/// novo porque não tem como: ele só consegue pedir coisas desta lista.
pub enum Pane {
    Table  { title: String, headers: Vec<String>, rows: Vec<TableRow>, selected: Option<usize>, query: String },
    Chart  { title: String, series: Vec<f64>, format: fn(f64) -> String, limit: Option<f64> },
    Facts  { title: String, rows: Vec<(String, String)> },
    Text   { title: String, lines: Vec<String>, scroll: u16 },
    Bars   { title: String, rows: Vec<(String, f64, Option<f64>)> },
    Grid   { title: String, cells: Vec<Cell>, cols: usize },
    Empty  { title: String, note: String },
}

pub enum Layout {
    Rows(Vec<(u16, Layout)>),
    Cols(Vec<(u16, Layout)>),
    Leaf(Pane),
}
```

**Por que assim, e não entregando o `Frame`:** o pedido foi "sem sair do padrão do
monitorzinho". Um `render(&self, frame, area)` por módulo é liberdade total para sair do
padrão, e com vinte e cinco módulos o padrão acaba em vinte e cinco variações da mesma
moldura. Este enum é a garantia mecânica de que isso não acontece: um módulo desenha bem
porque não tem como desenhar diferente. Também mantém `ui.rs` como o único lugar do
programa que conhece o ratatui, que é como está hoje.

O preço é real e vale registrar: um módulo com uma necessidade visual legítima que o
enum não cobre **fica bloqueado até o enum crescer**. Isso é intencional — crescer o
vocabulário é uma decisão consciente, com um caso concreto na mão, e não um efeito
colateral de alguém com pressa.

### 7.2 O painel compacto

Um módulo não desenha widget na tela principal da aba. Quem desenha lá é a lista (§5.2) e
as favoritas (§7.3). A coluna de resumo da lista, vinda de `InvestModule::summary()`, é o
quanto um módulo fechado consegue dizer.

### 7.3 Favoritas — reservado, não construído

Na v1 as widgets abaixo da lista **ficam vazias**, mostrando o que virão a ser. A decisão
é do usuário e é explícita: só depois de haver módulos é que faz sentido escolher quais
merecem ficar na tela.

Fica reservado desde já: um módulo poderá oferecer um painel compacto
(`InvestModule::widget()`), o usuário liga um por um, o arquivo guarda a escolha, e ligar
uma favorita é o que autoriza aquele provedor a rodar com a aba fechada — a exceção da
regra 3 do §3. Nada disso é escrito agora.

## 8. O que fica em disco

No banco do perfil — `~/.local/share/monitorzinho/db/<perfil>.db` —, junto de tudo o que
o resto do programa guarda. Ver [docs/banco.md](../banco.md) para o arquivo, os perfis e
as migrations; aqui só as tabelas desta aba.

| Tabela | O que é | Escrita quando |
| --- | --- | --- |
| `posicao` + `watchlist`, `alvo`, `setor`, `feed`, `mapeamento` | a carteira e o resto do que é do usuário | ao editar, e a cada `SAVE_EVERY_N_TICKS` |
| `provento`, `lancamento`, `alerta` | histórico e regras | idem |
| `carteira`, `carteira_alvo` | as carteiras recomendadas | idem |
| `modulo_mru` | id do módulo → epoch da última abertura | ao abrir um módulo |
| `patrimonio` | a série diária, um ponto por dia | ao fechar a aba, e ao sair |
| `cotacao` | último retrato bom de cada série de mercado | ao fechar a aba, e a cada 60 s |
| `agenda`, `noticia`, `anunciado` | o que a home mostra e a home não busca | quando a busca volta |

`cotacao` existe por uma razão só: **abrir a aba mostrando números, e não traços**. A
primeira volta de rede leva de centenas de milissegundos a segundos; sem cache, a aba abre
vazia toda vez. O cache é sempre desenhado com a idade ao lado ("há 4 min"), nunca
apresentado como atual — a mesma regra que o retrato de tamanhos dos containers já segue
com `measured_at`.

Uma linha ilegível custa aquela linha e nunca o programa abrir, que é o comportamento de
`tools::persist::load` e de `history::load_all`. Com uma diferença que a carteira exige: a
gravação é uma **transação**, e não onze gravações que podem parar no meio. Um histórico de
gráfico truncado por queda de energia é um gráfico feio; uma carteira truncada é o registro
do patrimônio da pessoa.

O detalhamento está em [03 — Armazenamento](03-armazenamento.md).

## 9. Cadência

O tick global do app continua sendo o de sempre — `TICK_RATE` de 2 s, esticado por
`interval_for` quando a amostragem custa caro. A aba Invest **não estica nada**, porque
sua amostragem é uma ordenação de vetor.

As threads de rede têm cadência própria, negociada com o provedor e não com o tick:

| Situação | Frequência |
| --- | --- |
| módulo aberto, mercado aberto | o mais rápido que a cota do provedor permitir, no piso de 2 s |
| módulo aberto, mercado fechado | 60 s — o preço não muda, e insistir é gastar cota à toa |
| aba visível, nenhum módulo aberto | só o que as favoritas pedirem (v1: nada) |
| aba fora de foco | parado, salvo alertas e favoritas ligadas |

"Mercado fechado" é decidido pelo provedor, não por relógio local: a Binance nunca fecha,
a B3 fecha às 18h de Brasília, e a NYSE tem feriado próprio. `Provider::is_open()`.

## 10. Contrato de não-impacto

O que existe hoje não pode mudar de comportamento. O que a aba Invest toca, exaustivamente:

### `src/app.rs`
| Mudança | Risco |
| --- | --- |
| `Tab` ganha `Invest`; `ALL: [Tab; 5]` → `[Tab; 6]`; braço em `title()` | nenhum — o compilador acha todo `match` |
| `App` ganha `invest: Option<Box<InvestState>>`, iniciado em `None` | nenhum |
| `tick()` ganha `Tab::Invest => self.refresh_invest()` | nenhum — as outras abas não passam por aí |
| `shortcut_targets()` ganha `Tab::Invest` no braço de `tables_on` | nenhum — os atalhos são por aba, e cada aba já numera do 1 |
| `Focus` ganha `Module(Box<ModuleFocus>)` | nenhum — variante nova, `match`es checados pelo compilador |
| `PendingAction` ganha `LeaveModule` | nenhum |
| `Pending` ganha `severity` | **todos os usos atuais passam `Destructive` e ficam idênticos** |
| `open_row()` ganha um ramo para a tabela da lista | baixo — mesmo formato dos dois ramos que já existem ali |
| `bench()` **não muda** | ver §3 |

### `src/ui.rs`
| Mudança | Risco |
| --- | --- |
| `render_screen` ganha `Tab::Invest => render_invest_tab(...)` | nenhum |
| `render_screen` ganha o braço de `Focus::Module` | nenhum |
| rodapé de teclas ganha o braço da aba | nenhum |
| `render_confirm` escolhe a cor pela severidade | **nenhum — hoje toda caixa é `Destructive` e continua vermelha** |
| `render_invest_tab` e os desenhistas de `Pane` são função nova | nenhum |

### `src/main.rs`
| Mudança | Risco |
| --- | --- |
| `mod invest;` | nenhum |
| braços de tecla para `Focus::Module` | **atenção**: têm que ficar depois das caixas que já veem tecla primeiro (`confirm_open`, `mark_editor_open`, …), pela mesma razão que aqueles vieram primeiro |

### `src/monitor/mod.rs`
| Mudança | Risco |
| --- | --- |
| `all_table_monitors()` registra o monitor da lista | **atenção**: o construtor não pode fazer I/O; ele é chamado em `App::new()`, no arranque de toda sessão, inclusive de quem nunca vai abrir a aba |

Aquele último ponto é o único lugar do plano onde é fácil quebrar a regra 1 do §3 sem
perceber. O monitor da lista é uma struct vazia; ele lê o estado por `InvestCtx` na hora
de amostrar, e amostrar só acontece com a aba na frente.

### `Cargo.toml`
Nenhuma dependência nova na fase 1. O que a aba precisa já está: `rustls` +
`rustls-native-certs` para TLS, `serde`/`serde_json` para o JSON das APIs e dos arquivos.

O que **não** está e será preciso encarar quando chegar a hora: enquadramento WebSocket
(RFC 6455) para preço no tick, e um decodificador de `Content-Encoding: gzip`, que várias
APIs mandam por padrão. Os dois são fase própria; nenhum módulo da fase 1 ou 2 depende
deles.

## 11. Rede: o que falta na base hoje

`tools/http.rs` faz requisição HTTP com TLS, mede as quatro fases e **descarta o corpo**
— ele é uma sonda de latência, não um cliente. `container/http.rs` fala HTTP sobre socket
Unix com o daemon. Nenhum dos dois serve para "pegue este JSON".

Então `invest/feed.rs` é código novo: GET com TLS, redirecionamento, `Content-Length` e
`chunked`, teto de corpo, timeout, e o JSON de volta. Ele reaproveita a montagem de
configuração de TLS de `tools/tls.rs` em vez de montar outra.

O detalhamento está em [02 — Dados e provedores](02-dados-e-provedores.md).

## 12. Fases

| Fase | O que entra | O que passa a existir |
| --- | --- | --- |
| **1 — Fundação** | 00, 01, 02, 03, [10](10-posicoes.md), [51](51-importacao.md) | a aba, a lista, a navegação, a carteira importável com P&L |
| **2 — Mercado grátis** | [20](20-cotacoes.md), [25](25-cambio.md), [26](26-renda-fixa.md), [27](27-cripto.md), [21](21-fita.md) | cotação viva onde ela é grátis, e o BRL como âncora |
| **3 — Leitura** | [22](22-grafico.md), [11](11-alocacao.md), [13](13-patrimonio.md), [24](24-indices-e-macro.md), [31](31-risco.md) | o gráfico, a alocação, a curva e o risco |
| **4 — Diante** | o resto, na ordem em que doer | |

O corte entre 2 e 3 é onde acaba o que se consegue sem chave e sem fonte que quebra.

## 13. O que este desenho deliberadamente não faz

* **Não executa ordem.** Não há corretora ligada, não há botão de comprar. Isto é um
  terminal de leitura, e a distância entre ler e mandar ordem é a distância entre um erro
  de leitura e um erro de dinheiro.
* **Não recalcula preço médio.** É informado, sempre. Ver
  [10 — Posições](10-posicoes.md) e [03 — Armazenamento](03-armazenamento.md).
* **Não promete tempo real onde não tem.** Todo preço aparece com origem e idade.
* **Não guarda credencial.** Nenhum provedor da v1 tem chave. Quando tiver, o desenho de
  onde ela mora entra em [02](02-dados-e-provedores.md), e não aqui.
