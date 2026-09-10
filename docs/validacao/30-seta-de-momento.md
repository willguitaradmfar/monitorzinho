# 30 — A seta entre uma leitura e a seguinte, e as colunas que estavam vazias

**Entregue em:** v0.42.0 · **Tipo:** recurso + correção
(`src/invest/provider.rs`, `src/invest/providers/kinvo.rs`, `src/ui.rs`, os módulos de mercado)

## O sinalizador de momento

A variação do dia responde «como está o papel hoje». Ela não responde «está andando
agora, e para que lado» — e essa é outra pergunta, que a tela não tinha como responder:
entre uma volta do polling e a seguinte, um número mudava e nada dizia que tinha mudado.

Agora todo preço, porcentagem e valor de posição carrega uma **seta** do último
movimento: `▲ 62,52`, `▼ +1,14%`, `▼ R$ 1.988.188,13`.

**Colada no número, e não numa coluna própria.** Uma coluna separa a seta do valor que
ela qualifica e obriga o olho a ir e voltar; a seta é parte do número.

**Com cor própria, apesar de colada.** Um papel pode estar em alta no dia e caindo neste
minuto — PRNR3 apareceu `▼ 18,17` com `+2,66%` ao lado — e é exatamente esse desencontro
que a seta existe para mostrar. Se ela herdasse a cor da célula diria a coisa errada. Quem
monta a linha escreve `▲ 62,52` e não sabe de cor nenhuma; quem pinta é `ui::com_seta`,
no único lugar do programa que decide cor.

**Preço parado não apaga a seta.** Ela fica até o próximo movimento. Uma seta que aparece
por uma volta e some não chega a ser lida, e a pergunta é «para que lado foi o último
movimento», não «mexeu exatamente nesta volta».

### Onde ela aparece

**Em todo número que anda com o preço, e não só num por linha.** A primeira versão punha
uma seta por linha e isso deixava metade da tela sem resposta: quem olha o P&L não olha o
preço ao lado dele.

**E cada número tem a própria direção.** A segunda versão guardava a direção do *preço* e
repetia a mesma seta em toda coluna — estava errado e era visível: a máxima do dia pode
subir numa volta em que o preço caiu, e a seta dizia o contrário. Agora a memória é por
grandeza (`invest::momento`), não por papel.

| Módulo | Em quê |
| --- | --- |
| Patrimônio | no total da carteira **e no «no dia»**, cartão e tela |
| Posições | no valor da posição, no preço atual, no **Mercado** e no **P&L** |
| Cotações · Cripto · Câmbio | no preço, na **Var%**, na **Máx 24h** e na **Mín 24h** |
| Carteiras recomendadas | no **Hoje**, no **Valor** e no **Preço / teto** |
| Índices e macro | no valor do indicador |
| Heatmap | na variação, dentro de cada caixa |

Uma linha cujo preço não se move — Tesouro, CDB, qualquer coisa informada à mão — não
ganha seta nenhuma, e é assim que tem que ser: ela não andou.

O tique do **patrimônio** não é o de nenhum papel: é a soma das posições, e por isso ele
é medido no estado da aba, que é quem tem a carteira e o mercado na mão ao mesmo tempo —
o retrato de mercado não conhece a carteira.

## A correção: Máx 24h, Mín 24h e Volume vazias

Não faltava dado. Faltava quem o pusesse.

Só a brapi e o Yahoo preenchiam esses três campos, e quem serve a B3 aqui é a Kinvo — que
publica **a série intradiária de cinco minutos** e nenhum dos campos prontos. Resultado:
as três colunas em branco em quase toda linha.

* **Máx e Mín agora saem da própria série.** O máximo dos fechamentos de cinco minutos é o
  máximo do dia. Uma pavio que suba e volte dentro da mesma barra não entra — é por baixo,
  e é honesto; a alternativa era a coluna continuar vazia.
* **Volume a Kinvo não publica em endereço nenhum.** Conferido nos dois intervalos que a
  API aceita. Para esse, os acessórios passaram a ser **herdados de quem os tiver**: o
  preço continua vindo da melhor fonte, mas volume, máxima e mínima que outra fonte
  respondeu na mesma volta preenchem o buraco.
* **A herança não atravessa o pregão.** O volume é acumulado do dia; carregá-lo para o
  dia seguinte mostraria o giro de ontem como o de hoje.

## Uma coluna nova quebra o que ninguém compila

A primeira tentativa pôs a seta numa coluna própria, e ela desalinhou a linha de cabeçalho
de grupo de Posições — que é montada à mão, com células vazias contadas na unha. O total
do grupo foi parar debaixo de «Atual». Nada estourou; o número só apareceu no rótulo
errado.

Ficaram dois testes disso, e eles sobrevivem à volta atrás:

* um genérico, que percorre todo módulo e exige que **toda linha tenha uma célula por
  coluna**, na tela e no cartão;
* um específico de Posições, que faz isso nos **quatro agrupamentos** — é o agrupado que
  monta a linha à mão, e o plano não passa por ela.

E os índices de coluna dos testes de `mercado` viraram constantes com nome, porque um
número solto num teste passa a conferir a célula errada, calado, quando uma coluna nasce
no meio.

## Como testar

### 0. Cada coluna tem a própria seta

Cotações, com o pregão andando. **Esperado:** linhas em que o preço tem `▼` e a *Máx 24h*
não tem seta nenhuma — o preço caiu, a máxima do dia ficou onde estava. Se as quatro
colunas de uma linha mostram sempre a mesma seta, a memória voltou a ser por papel.

### 0.1. Ela está em todo número que anda

Posições, com o pregão aberto: a mesma linha com seta no preço, no Mercado **e** no P&L.
Carteiras: no Hoje, no Valor e no Preço. Patrimônio: no total e no «no dia».

### 1. A seta aparece e discorda do dia

Aba Invest, deixe uma volta ou duas passarem com o pregão aberto. **Esperado:** setas
verdes e vermelhas ao lado dos números, e pelo menos uma linha com seta **vermelha** ao
lado de uma variação **verde** (ou o contrário). Se todas as setas concordarem com o sinal
do dia, a cor está sendo herdada da célula.

### 2. Ela fica

Um papel que não mexeu em três voltas mantém a seta da última mudança. Ela não pisca nem
some.

### 3. O patrimônio tem a sua

O cartão de Patrimônio com `▲` ou `▼` no total, e ele pode discordar do «no dia» logo
abaixo — a carteira pode ter subido hoje e recuado neste minuto.

### 4. Máx e Mín preenchidas

Cotações, com o pregão aberto: as colunas *Máx 24h* e *Mín 24h* preenchidas nas linhas
servidas pela Kinvo, e o preço atual entre as duas.

*Volume* fica vazio onde nenhuma fonte o deu — é o que se sabe, e é diferente de zero.

### 5. Nenhuma coluna desalinhada

Em Posições, `Ctrl+U` pelos quatro agrupamentos. **Esperado:** o total de cada grupo
debaixo de «Mercado», e o peso debaixo de «Peso».

## «Atrasado» virou cor

A palavra ocupava espaço em toda linha — `48,09 kinvo atrasado` — para dizer o que a cor
diz de relance. Saiu. No lugar dela, **a origem e a idade saem em amarelo** quando o preço
não é ao vivo, e em cinza quando é:

```
▼ 18,41 kinvo·há 16 min      ← amarelo: a B3 vem com atraso
393.917,00 binance·há 37 s   ← cinza: ao vivo
```

O número em si **não** é pintado: um preço amarelo se leria como um alerta sobre o preço,
e o que está velho é a leitura, não o valor. É o que `Row::rabicho` existe para permitir —
o fim de uma célula com tom próprio, sem virar outra coluna.

## O primeiro minuto sai em segundos

`há 12 s`, `há 47 s`. A faixa inteira abaixo de um minuto dizia «agora», e é justamente
nela que se quer saber se a busca está viva — uma volta leva segundos. Acima de um minuto
a unidade sobe e não se escreve «há 4 min 12 s»: o selo mora na borda de um painel, e ali
a precisão que importa é a de ordem de grandeza.

## Como saber que falhou

- Seta sempre da mesma cor que a variação do dia (herdou a cor da célula)
- Seta piscando e sumindo na volta seguinte
- Seta numa coluna separada em vez de colada no número
- Máx menor que Mín, ou o preço fora do intervalo entre as duas
- Volume de ontem aparecendo hoje
- Todas as colunas de uma linha com a mesma seta (a memória voltou a ser por papel)
- A palavra «atrasado» de volta na coluna do preço
- Origem e idade em amarelo num preço ao vivo, ou em cinza num atrasado
- «agora» cobrindo os primeiros cinquenta e nove segundos
- Total de grupo debaixo do rótulo errado em Posições
