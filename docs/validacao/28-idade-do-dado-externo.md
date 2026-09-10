# 28 — A idade de todo dado que veio de fora

**Entregue em:** v0.40.0 · **Tipo:** recurso transversal + correção
(`src/invest/store.rs`, `src/db/migracoes/0002_busca.sql`, `src/invest/tempo.rs`,
os módulos da Invest, `ui.rs`)

## O problema

Nada na tela dizia a **defasagem**. Um preço em cache de ontem, uma manchete guardada na
semana passada e um calendário de duas semanas atrás apareciam com exatamente a mesma
cara de dado fresco de um número que tinha acabado de chegar. Não havia como olhar para
um painel e responder «isto ainda vale?».

Havia idade em quatro lugares e faltava no resto — e os quatro eram os casos fáceis. O
caso difícil passava batido: um preço de **mercado** que parou de chegar continua na tela
com a fonte certa ao lado, e nada dizia que ele é de ontem.

## Duas idades diferentes, e as duas importam

| | O que responde | Onde aparece |
| --- | --- | --- |
| **Idade do dado** | de quando é **este** número | na própria linha: `62,52 kinvo atrasado·há 16 h` |
| **Idade da consulta** | há quanto tempo o programa **falou com a fonte** | na borda de baixo do painel, à direita: `agora`, `há 3 min`, `nunca buscado` |

São perguntas distintas e a distinção é o ponto. Com a bolsa fechada, o último negócio da
PRIO3 foi há dezesseis horas — o preço é de dezesseis horas atrás, e está certo. Mas a
busca aconteceu agora, e é isso que diz que o programa não está travado. Ver as duas
juntas é o que separa «o mercado está parado» de «o programa parou».

## O registro de buscas

Uma tabela nova, `busca`, com uma linha por fonte externa: `cotacao`, `noticia`,
`agenda`, `anunciado`, `historico`, `fundamento`.

É uma tabela e não uma coluna em cada uma das outras porque a pergunta não é sobre um
dado, é sobre uma **consulta**: «há quanto tempo não falo com o feed» tem resposta mesmo
quando a busca voltou sem nada novo — e é justamente aí que ela importa.

**Só o sucesso carimba.** Uma busca que falhou não deixou o dado mais novo, e mover o
carimbo nela faria a tela dizer «agora» sobre um número de ontem, que é exatamente o
engano que o registro existe para desfazer.

O carimbo é gravado, então a idade sobrevive a fechar e reabrir: um programa que passou a
noite desligado abre dizendo «há 9 h», e não «agora».

## O que cada módulo declara

`InvestModule::fonte_externa()`, um nome só. Um módulo que lê duas fontes declara a que é
o **assunto** dele — Fundamentos vive dos fundamentos e só usa a cotação para comparar, e
é a idade dos fundamentos que responde «isto ainda vale?».

| Fonte | Módulos |
| --- | --- |
| `cotacao` | Posições, Cotações, Câmbio, Cripto, carteiras, Alocação, Rebalanceamento, Corretoras, Heatmap, Índices e macro, Patrimônio, Alertas |
| `historico` | Gráfico, Indicadores, Risco |
| `fundamento` | Fundamentos |
| `anunciado` | Proventos |
| `noticia` | Notícias |
| `agenda` | Agenda |
| — | Book, Importação, Lançamentos: não consultam nada de fora |

O selo é escrito pela moldura e não por cada módulo. Fica cinza e vira **amarelo depois
de uma hora**: a idade é uma ressalva, não o assunto, e um número de mercado atrasado
alguns minutos ainda é um número de mercado.

> A nota da esquerda **encolhe** para dar lugar ao selo, com reticências. Ela carrega o
> total da carteira e a porcentagem de cada carteira recomendada; o selo carrega cinco
> caracteres de ressalva. Sobrepor os dois comia o fim daquele número, e foi o que a
> primeira versão disto fez.

## A correção: 90% das notícias estavam sem data

Não era o feed. Era o leitor.

Quatro dos cinco endereços do investing.com carimbam `2026-09-10 12:08:07` — **espaço no
lugar do `T`, mês em número, sem fuso**. O leitor de ISO exigia o `T` literal, e o leitor
tolerante procurava um nome de mês em inglês. Nenhum dos dois pegava essa forma, e o item
ficava sem instante:

```
215 de 240 linhas gravadas · formato '9999-99-99 99:99:99'
 25 de 240 linhas gravadas · formato 'Sep 99, 9999 99:99 GMT'
```

Sem data, o item vai para o fim da lista — que é onde nenhuma notícia de agora deveria
estar — e a coluna «Quando» escrevia «sem data».

Três coisas mudaram em `epoch_de_iso`:

* **O separador pode ser espaço**, e a hora pode faltar (`2026-08-10` vale meia-noite).
* **Sem fuso escrito, vale UTC.** Não é chute: o item mais novo desses feeds fica sempre
  alguns minutos atrás do relógio UTC, e nunca três horas à frente dele, que é o que
  aconteceria se o carimbo fosse horário de Brasília.
* **O fuso escrito é descontado** em vez de ignorado. Ele era jogado fora — `Z` e
  `+03:00` davam o mesmo número — com a justificativa de que ordenar notícias não precisa
  de tanto. Precisa: três horas de erro põem a notícia da manhã depois da da tarde.

**E as já gravadas se curam sozinhas.** O texto cru continua no banco, então uma linha com
o instante nulo é relida na abertura. Quem tinha 215 notícias sem data abre a versão nova
com zero, sem esperar o feed republicar nada.

## O que o selo denunciou na primeira hora de uso

Proventos dizia **«nunca buscado»**, e estava certo. O cache de anúncios é semeado do
disco na abertura, `get` só busca o que ainda não é chave do mapa — e depois da primeira
execução todo ativo da carteira já era chave. Nenhum era rebuscado. Nem naquela execução,
nem em nenhuma futura: **quem tivesse rodado o programa uma vez nunca mais veria um
anúncio novo**, e nada na tela dizia isso.

A semente é dado, não recibo: ela diz o que se sabe, não quando se soube. Agora `get`
pergunta ao registro de buscas, e uma lista com mais de um dia — ou sem registro nenhum,
que é o caso de toda instalação que sobe para esta versão — é rebuscada uma vez por
execução.

É o primeiro uso concreto do selo, e vale registrar por isso: a defasagem invisível
escondia um cache congelado havia semanas.

## O que a falta de internet revelou

O selo **renovava sem rede**. Com a internet caída ele dizia «agora» em todos os painéis,
que é exatamente o contrário do que ele existe para fazer.

O culpado não era o carimbo, era quem o encostava. A cotação era carimbada por qualquer
provedor que devolvesse `Ok` com pelo menos um preço — e o provedor `informado` não faz
I/O nenhum, cobre todo ativo, roda de dois em dois segundos e sempre devolve `Ok`. Com a
rede caída ele carimbava a cotação como recém-buscada a cada duas voltas de relógio.

Agora só conta quem **foi à rede**: `Provider::remoto()`. E um feed que respondeu sem
nenhum item legível também deixou de contar — ele falou com a fonte, mas não trouxe nada
mais novo do que já havia, e para «este dado é de quando?» é isso que importa.

Medido rodando o programa dentro de um *network namespace* sem rota nenhuma
(`unshare -rn`), com os carimbos semeados duas horas atrás:

```
antes:   agora ┘   agora ┘   agora ┘   agora ┘   agora ┘   agora ┘
depois:  há 2 h ┘  há 2 h ┘  há 2 h ┘  há 2 h ┘  há 2 h ┘  há 2 h ┘
```

Os preços continuam na tela nos dois casos — vindos do cache e do informado, que é como
tem que ser. O que muda é a tela parar de dizer que eles acabaram de chegar.

## Como testar

### 1. A idade da consulta em toda parte

Abra a aba Invest. **Esperado:** todo cartão que vive de fonte externa com um selo na
borda de baixo à direita — `agora` logo depois da primeira volta. Patrimônio, Posições,
Cotações, Índices, cada carteira, Agenda, Notícias, Heatmap, Alocação, Corretoras,
Proventos, Câmbio, Cripto, Gráfico, Fundamentos. Book, Importação e Lançamentos **sem**
selo: não consultam nada.

Entre em qualquer um deles. O selo aparece na borda do painel principal, uma vez só — e
não em cada painel da tela.

### 2. A idade sobrevive a fechar o programa

Feche, espere alguns minutos, reabra e vá para a aba Invest **sem esperar a primeira
volta**. **Esperado:** `há 3 min`, não `agora`.

```sh
sqlite3 ~/.local/share/monitorzinho/db/padrao.db "SELECT chave, em FROM busca"
```

### 3. A idade do dado, na linha

Em Posições, a coluna *Atual*: `565,08 manual·há 17 h`, `62,52 kinvo atrasado·há 16 h`.
**Toda** linha diz de quando é o preço, e não só as informadas à mão — era o caso que
faltava.

Com a bolsa fechada, essas horas todas são normais e o selo do painel continua dizendo
`agora`: o mercado é que está parado, não a busca.

### 4. Sem rede nenhuma, o relógio para

O teste que pega a regressão inteira, e não precisa esperar a internet cair:

```sh
unshare -rn env XDG_DATA_HOME=/tmp/sandbox monitorzinho
```

**Esperado:** os selos parados na última busca boa — «há 2 h», «há 1 d» — e **nunca**
«agora». Os preços continuam aparecendo, vindos do cache e do informado; é a idade que
tem que dizer a verdade sobre eles.

### 5. Uma fonte que caiu não mente

Derrube a rede e espere. **Esperado:** o selo **para de avançar** e amarela na primeira
hora; ele não volta para `agora` a cada tentativa falha. O rodapé de Cotações continua
dizendo qual fonte caiu e por quê.

### 6. Proventos rebusca quando envelhece

Abra Proventos. **Esperado:** o selo sai de «nunca buscado» e vira uma idade de verdade
em alguns segundos, e a tabela `busca` ganha a linha `anunciado`. Feche e reabra dentro
do mesmo dia: **não** rebusca (o selo mostra a idade acumulada). Mais de 24 h depois,
rebusca uma vez.

### 7. Notícias com data

Abra Notícias. **Esperado:** nenhuma linha com «sem data», e a coluna *Quando* em ordem
decrescente de verdade.

```sh
sqlite3 ~/.local/share/monitorzinho/db/padrao.db \
  "SELECT count(*) FROM noticia WHERE em IS NULL"
```

Isto vale já na **primeira abertura**, antes de qualquer busca — as linhas antigas são
relidas do texto cru.

Há teste ao vivo contra os cinco endereços:
`cargo test rss::tests::ao_vivo -- --ignored`.

## Como saber que falhou

- Selo dizendo `agora` numa fonte que está caindo (o carimbo está sendo posto no erro)
- Selo sumindo depois de reabrir o programa (o registro não está sendo gravado)
- A nota do rodapé cortada pelo selo em vez de encolher com `…`
- Selo repetido em cada painel da mesma tela
- Preço sem idade ao lado em qualquer módulo
- «sem data» voltando em Notícias
- Uma notícia da manhã acima de uma da tarde (o fuso voltou a ser ignorado)
- Selo dizendo «agora» com a rede caída (voltou a carimbar sem ter ido à rede)
- Proventos preso em «nunca buscado» depois de aberto (o cache voltou a congelar)
- O cartão de Gráfico ou de Fundamentos dizendo «nunca buscado» sobre preços ao vivo — o
  cartão deles é uma lista de papéis com o **preço** ao lado, e a idade dali é a do preço
