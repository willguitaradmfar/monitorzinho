# 23 — Aba Containers: ver, e também fazer

**Entregue em:** v0.31.0 (correção do shell em v0.31.1) · **Tipo:** recurso novo, aba própria
(`src/container/` novo, `src/monitor/containers.rs` novo, `app.rs`, `ui.rs`,
`monitor/mod.rs`, `tools/tail.rs`)

## O problema

O programa já sabia que containers existiam — o painel de conexões atravessa
namespaces desde a v0.16.0, e `netns.rs` já nomeava containers lendo o cgroup e o
arquivo de estado da engine. O que não havia era um lugar que **falasse** deles: o
que está no ar, quanto consome, por onde se chega, e o que dá para fazer.

Fazer é a metade que muda a natureza da coisa. Parar, reiniciar, remover, abrir um
shell — nada disso existe em arquivo. Foi preciso falar com a engine, o que o
projeto vinha evitando de propósito.

## Por que aba, e não mais um painel em Processos

Duas razões, as duas medidas.

**A grade de Processos está cheia.** São sete painéis colocados à mão, indexados
por posição, com um comentário explicando por que cada par está onde está. Um
oitavo não é acrescentar: é desmanchar um argumento já feito.

**A aba é a unidade de custo.** Amostrar Processos custa ~78 ms por tick
(`--bench`); os painéis de container custam **0,1 ms**, porque quem fala com a
engine são threads de fundo e a aba só copia o retrato delas. Containers dentro de
Processos significaria nunca poder olhar containers sem pagar a varredura inteira
do `/proc`. Uma aba em que dá para *ficar* num nó ocupado é o ponto.

A aba só aparece onde há o que mostrar: uma engine que responde, ou cgroups de
container legíveis. Uma quarta aba permanentemente vazia num laptop é ruído.

## Genérico por engine, desde a primeira linha

`ContainerEngine` é um trait; Docker é `docker.rs`, a primeira implementação.
Nada acima de `src/container/` menciona Docker — os painéis falam `Container`,
`Volume`, `Image`, `Network`. Os ids que vão para o `marks.json` são
`containers`/`volumes`/`images`/`networks`: trocar de engine não pode invalidar as
marcas de quem já usa.

O ponto central é `actions()`, no mesmo idioma de `Tool::params()` e de
`tools::offers_for`: **a engine declara o que sabe fazer, a UI monta o menu com o
que voltar**. Nenhuma tela conhece uma operação pelo nome, então uma engine que
não saiba pausar simplesmente não oferece pausar.

A metade que **mede** já era agnóstica antes disto existir: cgroup, PSI e
`/proc/<pid>/net/dev` são do kernel, e `container_id` já reconhecia
`docker-<id>.scope`, `libpod-<id>` e `crio-<id>`.

## O que fica onde

```
┌───────────────────────────────────────────────────────┐
│ Containers                     (largura toda, 1/2 alt) │
├───────────────────────────┬───────────────────────────┤
│ Volumes                   │ Imagens                    │
├───────────────────────────┼───────────────────────────┤
│ Redes                     │ Resumo                     │
└───────────────────────────┴───────────────────────────┘
```

**Containers**: nome, imagem, estado (com saúde), binds de portas publicadas,
CPU%, memória contra o teto quando há um, e rede. Todos os estados. Em tela cheia
vira árvore por projeto do compose.

**Volumes**: quem monta, quanto ocupa, de quando é. A borda diz o total, quantos
estão órfãos e **há quanto tempo os tamanhos foram medidos** — a medição custa
quase um segundo de daemon, então acontece em thread própria e o painel diz a
idade dela em vez de apresentar um número de um minuto atrás como atual.

**Imagens**: tamanho, idade, quem usa. Cruzado por **id**, não por nome.

**Redes**: driver, sub-rede, quem está conectado.

**Resumo**: quantidades, e qual engine respondeu. `Enter` na linha da engine é
onde se aponta para outro endereço.

## O menu do `Enter`

Numa tabela de container, `Enter` abre **o que dá para fazer** com a linha — ver
os detalhes é uma das entradas, não a tela inteira. Uma entrada que não dá para
executar agora fica na lista, apagada, com o motivo ao lado
(`remover — está em execução, pare antes`). Um item que se explica ensina; um item
ausente não ensina nada.

Três níveis de atrito, pelo que a operação custa:

| | |
| --- | --- |
| `·` | executa direto — iniciar, parar, reiniciar, pausar |
| `!` | caixa dizendo o que se perde, `Enter` confirma — matar, remover container/imagem/rede |
| `‼` | **digitar o nome** — remover volume, qualquer limpeza |

Uma ação leva segundos (parar espera o processo sair sozinho), então roda em
thread e a linha diz `parando…` enquanto isso. O erro que volta é o da engine, sem
tradução: quem sabe por que falhou é ela.

**Não** se oferece remover à força um container em execução. A engine consegue;
«pare antes» é mais honesto que uma tecla que derruba o que está servindo tráfego
sem ter dito que era isso.

## Logs e shell

**Logs** abrem no seguidor de arquivo que já existia — busca, filtro, hex, horas
de rolagem e sobrevivência a rotação, tudo já escrito. O arquivo que a engine
escreve é um documento JSON por linha, então o seguidor aprendeu a desembrulhar
isso (e a tirar as cores ANSI que ele não pinta), o que qualquer log em JSON por
linha ganha de graça.

**Um shell** dentro de um container em execução toma o terminal: sai da tela
alternativa — o que você digitou continua no histórico depois do `exit` — e o
devolve ao sair. Fala o protocolo de exec da engine direto, sem binário externo,
tentando `bash`, depois `sh`, depois `/bin/sh` — pelo caminho absoluto no fim, para
a imagem cujo `PATH` está vazio. Quando nenhum abre, uma tela diz o que a engine
respondeu para cada tentativa, em vez de voltar sozinha.

## O que custa, medido

25 s por aba, máquina com 8 containers e 3 execuções rodando:

| Aba | CPU |
| --- | --- |
| Visão Geral | 0,72 % |
| Processos | 6,88 % |
| Containers | 0,84 % |
| Ferramentas | 0,72 % |

A linha de base (0,72 %) são as execuções que já rodavam: **a aba nova custa
0,12 % de um núcleo** acima do que a máquina já pagava. A primeira versão custava
o triplo, porque as threads liam a engine a cada segundo mesmo com a aba fora da
tela — contrariando o princípio escrito no próprio `Tab`. Três correções:

1. As threads desaceleram para 20 s quando nenhum painel de container está na
   tela, e a troca de aba as acorda por condvar — chegar na aba não custa uma tela
   vazia.
2. Volumes, imagens e redes são relidos a cada dez voltas: custam 24 ms dos 36 ms
   de cada volta e mudam na escala de um deploy. Mesmo raciocínio do
   `netns::Watcher`.
3. A cadência dos containers é a mesma do resto do programa. O que precisa ser
   instantâneo já é: uma ação avisa a tela quando começa e quando termina.

`--bench` passou a medir **por aba** — um total somando todas as tabelas
responderia uma pergunta que ninguém faz.

## As armadilhas que só apareceram medindo

**`stats?stream=false` custa um segundo por container.** O daemon coleta *duas*
amostras para calcular o CPU% no seu lugar. Com `one-shot=true` são 7 ms e o delta
é nosso — como o painel de conexões já faz para throughput. Ler o cgroup custa
~1 ms para todos, e funciona sem root até para containers do root, porque os
arquivos de cgroup são legíveis por todos: **mais** do que o painel de conexões
enxerga.

**A memória tem que descontar o `inactive_file`.** Sem isso o número discorda das
ferramentas de container e parece errado — 23,9 MiB contra 20,8 MiB no mesmo
container, e a diferença é exatamente o cache que o kernel devolve sob pressão.

**A engine responde `-1` em «quantos containers usam esta imagem»** — que quer
dizer «não contei», não zero. E o nome da imagem no container
(`docker.io/chromedp/headless-shell:latest`) não é o texto da tag da imagem
(`chromedp/headless-shell:latest`). Casar essas strings exigiria conhecer as regras
de nome de todo registro que existe; o id é o mesmo dos dois lados.

**`101 UPGRADED` não quer dizer que o shell subiu.** Um comando que não existe no
container é aceito na criação — a engine não confere — e o *upgrade* também dá
certo: ela responde `101`, escreve «executable file not found in $PATH» no fluxo e
fecha. Quem olhar só o código de status entrega um fluxo já morto, a tela volta no
instante seguinte e a mensagem se perde na limpeza. Foi assim numa imagem sem
`bash` — a maioria das imagens enxutas, que é o que mais roda em produção.

E perguntar à inspeção **logo depois** do `101` também não resolve: o runtime
ainda não decidiu, ela responde «rodando», e a falha passa. A corrida se fecha
esperando a primeira palavra do fluxo — medido, chega em ~60 ms nos dois casos, e
nesse ponto o veredito já existe. O que o fluxo disse é a explicação boa, e é ela
que vai para a tela; o prompt lido junto é guardado para a sessão não abrir sem ele.

**Redimensionar antes de iniciar um exec trava.** A chamada só vale numa execução
já iniciada; pedida antes, a engine não responde nada e segura a conexão. O
tamanho inicial vai no `ConsoleSize` do próprio `create`.

**Ler até a conexão fechar é sorte, não protocolo.** Foi o que transformou aquela
chamada sem resposta em dez segundos de espera. O corpo agora é lido pelo que o
HTTP diz que ele mede — `Content-Length`, os pedaços de `chunked` até o
terminador, nada nos códigos sem corpo — e esperar o fechamento ficou só para a
resposta sem nenhuma dessas marcas.

**`SIGWINCH` interrompe a leitura com `EINTR`,** e o relay do shell tratava
qualquer erro de leitura como fim da sessão — redimensionar a janela fechava o
shell. `EINTR` quer dizer «tente de novo», que é como o `poll.rs` daqui já o trata.

## Segurança

`~/.docker/config.json` guarda credenciais de registry em texto claro. Só a chave
`currentContext` é lida; nada toca no bloco `auths`, nem em log, nem em tela.

## Como saber que falhou

- A aba aparecendo numa máquina sem container nenhum
- Memória discordando do que a engine mesma reporta (é o `inactive_file`)
- Imagem em uso aparecendo como «sem uso» (é o cruzamento por nome em vez de id)
- Tamanho de volume aparecendo como `0 B` em vez de `medindo…`
- Uma ação executando sem a caixa que devia parar antes — ou pedindo o nome
  digitado para algo reversível
- A tela congelando ao abrir um shell (é uma chamada esperando resposta que não vem)
- O shell fechando ao redimensionar a janela
- O shell «voltando rápido» sem dizer nada — é um fluxo morto entregue como bom, e
  o teste é abrir um shell numa imagem sem `bash` (uma Alpine qualquer)
- A aba Visão Geral ficando mais cara depois desta entrega (as threads deixaram de
  desacelerar fora de foco)
