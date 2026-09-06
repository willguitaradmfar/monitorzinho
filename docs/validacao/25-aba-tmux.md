# 25 — Aba tmux: listar, entrar, e voltar

**Entregue em:** v0.35.0 · **Tipo:** recurso novo, aba própria
(`src/tmux/` novo, `src/action.rs` novo, `src/monitor/tmux.rs` novo, `app.rs`,
`ui.rs`, `main.rs`, `monitor/mod.rs`, `container/mod.rs`)

## O problema

O programa já sabia *ver* o tmux de longe: `monitor/ssh.rs` reconhece um reanexo
de tmux no `utmp` desde que existe, e a tabela de processos mostra o servidor
como qualquer outro processo. O que não havia era um lugar que falasse dele: o
que está aberto, onde, com o quê rodando dentro — e, sobretudo, um jeito de
**entrar**.

Entrar é a metade que muda a natureza da coisa, exatamente como foi com o shell
de container na v0.31.0. Listar é olhar; entrar é sair da frente.

## Por que aba, e não painel em Processos

As mesmas duas razões da aba Containers, e uma terceira.

A grade de Processos está cheia — sete painéis colocados à mão. A aba é a
unidade de custo: uma volta desta aba custa **3,5 ms** (`--bench`), contra os
~88 ms de Processos; enfiá-la lá dentro significaria nunca poder olhar sessões
sem pagar a varredura inteira do `/proc`.

A terceira: esta é a aba de onde se **sai do programa para trabalhar**. Uma
tabela que entrega o terminal não é um painel entre outros.

A aba só aparece onde o tmux está instalado — a busca é no `PATH` e é só
filesystem, sem criar processo nenhum para descobrir se existe um programa.

## O `Command`, e por que ele é aceitável aqui

Não havia um `std::process::Command` em nenhum lugar do programa antes disto, e
o `container/exec.rs` recusa o `docker exec` em voz alta. A conta é outra aqui,
por três razões:

* O tmux **não tem biblioteca nem protocolo de fio estável**. A interface de
  controle é o binário; não existe a alternativa que existia com o Docker.
* **O binário instalado é o assunto da aba.** Sem ele não há sessão, não há o
  que listar, e não há como criar uma. A dependência não é incidental.
* **O formato da resposta é nosso.** `-F '#{session_name}…'` é a linguagem de
  formatação documentada do tmux: o que volta tem os campos que pedimos, na
  ordem que pedimos. Não é raspar a saída de um comando; é pedir um registro.

Uma chamada por volta: `list-panes -a` dá os campos da janela e da sessão de
cada painel na mesma linha, e como toda sessão tem ao menos uma janela e toda
janela ao menos um painel, uma chamada reconstrói a árvore inteira. É por isso
que esta aba não tem as threads de fundo que a de Containers tem — um socket
local não trava do jeito que HTTP para um daemon trava.

## O separador é uma tabulação, e isso foi aprendido errando

A primeira tentativa usou `\x1f`, o separador de unidade do ASCII, no raciocínio
de que ninguém o digita. O tmux **escapou o separador junto com o resto** e cada
linha voltou com um campo só — `0\037$0\0371787957878…` em texto visível.

A regra real, conferida: o tmux escapa os caracteres de controle que aparecem
**nos valores** e não os que estão no texto do formato. Uma janela chamada com
uma tabulação dentro sai como `a\tb`, em quatro caracteres; a tabulação que nós
pusemos entre os campos sai como ela mesma. Isso torna a divisão **exata**, e
não só provável. Está num teste, não só num comentário.

## Entrar, estando já dentro do tmux

O caso difícil, e o caso comum: o monitorzinho rodando num painel do próprio
servidor. O tmux recusa anexar ali — o guarda dele exige `$TMUX` no ambiente
**e** que o terminal do cliente seja o de um painel do servidor — com «sessions
should be nested with care, unset $TMUX to force».

Nenhuma das duas saídas é obviamente a certa, então o menu oferece as duas,
nomeadas pelo que de fato fazem:

| | |
| --- | --- |
| **entrar aqui (aninhada)** | apaga o `$TMUX` do filho, que é o que a mensagem do tmux manda fazer. `prefix prefix d` desanexa e devolve a lista — o contrato pedido. |
| **trocar o tmux de fora para ela** | `switch-client`: o cliente externo troca de sessão e o monitorzinho continua onde está. Voltar é trocar de volta, não desanexar. |

Rodando fora do tmux não há o que aninhar, e o menu diz só **entrar**.

A sessão onde o monitorzinho vive fica com as duas entradas apagadas e o motivo
ao lado — e **matar sessão** também, porque matá-la mataria o programa. É o
mesmo idioma do `Action.blocked` da aba Containers: um item que se explica
ensina, um item ausente não ensina nada.

## A entrega do terminal é mais simples que a do shell

`container/exec.rs` tem 160 linhas de relay porque o shell está do outro lado de
um socket. Aqui o tmux é um processo filho que **herda o terminal de verdade**:
sem relay, sem thread, e o `SIGWINCH` do redimensionamento vai direto para quem
está desenhando.

O que precisou de atenção foi o modo bruto. O tmux configura o terminal do jeito
dele e restaura o que encontrou ao sair; entregá-lo já em modo bruto o faria
restaurar o modo bruto, e a interface voltaria para um terminal que ela acha que
configurou e não configurou. Então `open_attach` desliga antes e religa depois —
ao contrário do `open_shell`, que o mantém ligado de propósito.

## Criar: duas coisas que o tmux não confere

Medidas antes de escrever o formulário, rodando o `tmux` na mão:

1. **`-c /caminho/que/não/existe` funciona.** A sessão é criada em outro lugar e
   o comando sai com zero. Quem pediu nunca fica sabendo. → o caminho é conferido
   na caixa, antes de criar.
2. **`.` e `:` no nome são reescritos para `_`, calado, também saindo com zero.**
   `deploy.web` vira `deploy_web`, e anexar depois pelo nome digitado erraria a
   sessão. → a criação usa `-P -F '#{session_name}'` e usa **o nome que voltou**;
   a linha de resultado avisa quando os dois diferem. Renomear lê de volta pelo
   mesmo motivo.

É a mesma regra do `apply_endpoint`: falhar enquanto a caixa ainda está aberta é
a diferença entre corrigir e sair procurando.

A tecla é **Ctrl+N** e não `n`: numa tabela em tela cheia toda letra é entrada de
busca, e criar tem que funcionar *enquanto* se procura — que é geralmente como
se descobre que a sessão não existe. Mesma razão do Ctrl+E que marca.

## Os três níveis dividem as mesmas colunas

A primeira versão da árvore tinha as colunas dizendo coisas diferentes conforme a
profundidade da linha: «Anexada» valia `sim`/`não` numa sessão, `ativa` numa
janela e `81×60` num painel. Uma coluna que muda de assunto ao descer é uma
coluna que não dá para ler de cima para baixo.

Agora cada uma responde a mesma pergunta em todo nível e fica **vazia** onde o
nível não tem resposta — uma janela não tem janelas dentro, um painel não tem
painéis dentro, e só a sessão é datada pelo tmux. Vazio diz «esta pergunta não se
faz aqui»; um zero diria «a resposta é nenhum», que é outra coisa. O tamanho do
painel saiu da tabela e foi para o detalhe, que é onde ele sempre foi um fato
sobre um painel só.

A árvore abre **até o painel**, nos dois tamanhos — `TableMonitor::expand_all`,
novo. O padrão das outras árvores daqui é abrir só o primeiro nível, porque a de
processos tem centenas de folhas; esta tem três níveis rasos e o de baixo é o
único que tem um processo. Deixá-lo fechado esconderia o que se veio ver. Pelo
mesmo motivo o painel compacto também mostra a árvore em vez de achatá-la, com
teto de **30 sessões** — o corte é por sessão e não por linha, porque metade de
uma árvore é um painel pendurado numa janela cujo pai não está na tela.

As duas colunas de texto são **medidas sobre o conteúdo** (`ui::fitted`) em vez de
receberem uma fatia fixa da sobra. São a única tabela daqui em que isso importa:
o nome soma o recuo de até três níveis e o caminho tanto pode ser `/tmp` quanto
ter oitenta caracteres, e proporções fixas davam um nome com setenta colunas de
vazio ao lado de um caminho cortado. A medida é sobre **todas** as linhas e não
só as visíveis — uma largura que mudasse ao abrir e fechar um nó faria a tabela
dançar debaixo de quem está lendo.

## Matar em qualquer nível, e a recusa exata

Sessão, janela e painel têm menu. O painel entrou porque é o único nível em que
existe de fato um processo — parar na janela deixaria a única linha que aponta
para um processo sendo a única sem menu. `kill-pane` aponta pelo id do tmux
(`%12`) e não pela posição: matar um painel renumera os que sobram, e um alvo
posicional guardado antes disso aponta para outro.

A recusa é **exata**, não ampla. A sessão onde o monitorzinho vive não pode ser
morta; mas as *outras* janelas dela podem, e só a que segura o nosso painel é
recusada — decidido pelo `$TMUX_PANE`, que o próprio tmux exporta, e não pelo
nome da sessão. Bloquear pela sessão inteira teria sido mais fácil e teria
proibido coisas legítimas.

## Ampliar relê a árvore

A tela cheia congela a forma da lista de propósito — ela não pode reordenar
debaixo de quem está lendo. Só que congelar um retrato de até dois segundos atrás
significa carregar essa defasagem pelo tempo todo em que a tela ficar aberta: uma
janela aberta por fora não aparecia até sair e voltar. Reparado ao vivo, e
corrigido onde custa uma vez: a tecla que amplia relê o tmux antes de amostrar,
3,5 ms medidos. Nenhuma outra aba precisa disso — as threads do `Store` já mantêm
o retrato dos containers fresco sozinhas.

## O que fica onde

```
┌───────────────────────────────────────────────────────┐
│ Sessões                        (largura toda, 1/2 alt) │
├─────────────────────────────────┬─────────────────────┤
│ Janelas                         │ Resumo               │
└─────────────────────────────────┴─────────────────────┘
```

**Sessões**: janelas, painéis, se há alguém anexado, idade, pasta base, e o
comando do painel ativo — que é o que reconhece uma sessão sem entrar nela. Em
tela cheia vira árvore sessão → janela → painel, na mesma máquina de árvore que
agrupa containers por projeto do compose.

**Janelas**: todas as janelas de todas as sessões, planas. A árvore responde «o
que tem dentro desta sessão»; esta responde a pergunta oposta, a que se faz
quando não se lembra onde uma coisa ficou rodando.

**Resumo**: versão, socket, contagens, e se o monitorzinho está dentro do tmux.
A última linha não é curiosidade: é o que explica por que o menu de uma sessão
oferece duas maneiras de entrar em vez de uma.

## O que o menu deixou de pertencer a containers

`Action`, `Gravity` e `ActionKey` moravam em `container/mod.rs`, porque
containers foram a primeira coisa deste programa sobre a qual havia o que
*fazer*. Saíram para `src/action.rs` quando a segunda apareceu. `container`
reexporta os três, então a metade que fala com a engine continua lendo
`container::Action` como sempre leu — nenhuma linha de `docker.rs` mudou.

`ActionMenu` passou a guardar um `Target`, que é container ou tmux. Um enum só,
e não um menu por assunto: dois menus seriam dois despachos fazendo a mesma
coisa com nomes diferentes.

## O que deliberadamente **não** foi feito

**`Del` não mata nada aqui.** O `kill_selected` genérico mata por pid e
sub-árvore, e nem sessão nem janela têm um pid — quem tem é o servidor.
`danger()` devolve `None` para estas tabelas, então a tecla não faz nada e o
rodapé não a anuncia. Matar está no menu, nos três níveis, com confirmação que
diz o que se perde: quantas janelas e painéis, quantos clientes desconectados, e
se aquele painel era o último da janela (ou a janela a última da sessão).

**A lista não se atualiza sozinha em tela cheia.** Uma tabela ampliada tem a
forma congelada de propósito — ela não pode reordenar debaixo de quem está lendo
—, então uma sessão criada por fora só aparece ao sair e voltar. O que muda a
forma por ação nossa (criar, matar, renomear) reamostra na hora.

## Como foi validado

Rodado dentro de uma sessão de tmux de teste, dirigido por `send-keys` e lido por
`capture-pane`:

* a aba aparece na barra, com a árvore correta de 5 sessões / 8 painéis;
* `Enter` na sessão do próprio monitorzinho mostra as três entradas bloqueadas
  com os três motivos distintos;
* `Ctrl+N` → nome `mz.target`, pasta `/nao/existe` → a caixa fica aberta dizendo
  «`/nao/existe`: No such file or directory»;
* corrigida para `/tmp` → sessão criada como `mz_target` (o ponto reescrito) e o
  terminal entregue a ela, aninhada;
* `prefix d` → de volta na lista, com o cursor na linha nova;
* renomear `mz_t2` → `mz.renomeada` → a linha vira `mz_renomeada` no lugar e o
  rodapé diz «`mz.renomeada` virou `mz_renomeada` — o tmux não aceita `.` nem
  `:`»;
* matar sessão → confirmação com as consequências, linha some no mesmo quadro;
* `Enter` num painel → menu com «matar painel»; confirmado, `%43` some da árvore,
  as contagens da janela e da sessão caem juntas, e a árvore continua aberta;
* menu da nossa própria janela e do nosso próprio painel → bloqueados, cada um
  com o seu motivo; a janela irmã da mesma sessão continua matável;
* busca em tela cheia conferida com `orki`, `logs`, `42`, `var`, `mz_alvo` e
  `CLAUDE` — o cursor cai na linha certa nos seis, incluindo maiúsculas, um id de
  painel e um pedaço de caminho.
