# 29 — Heatmap: a área passa a ser fiel, e passa a poder medir outra coisa

**Entregue em:** v0.41.0 · **Tipo:** correção + recurso
(`src/ui.rs` — `treemap`, `src/invest/modules/heatmap.rs`, `src/invest/calc.rs`)

## O problema: o tamanho mentia

A grade era feita de fileiras com o mesmo **número** de células, e dentro de cada fileira
só a **largura** seguia o peso. Isso errava de três jeitos ao mesmo tempo:

* cada fileira tinha `n` células e não `n` de peso, então uma fileira com dois papéis
  grandes ocupava a mesma altura que uma com cinco pequenos;
* a altura era fixa em quatro linhas, então a **área** não seguia o peso entre fileiras;
* havia um piso de oito colunas por célula, que dava aos menores mais espaço do que o
  peso pedia.

O resultado prático: uma posição de 0,5% aparecia do tamanho de uma de 3% — o oposto do
que um heatmap existe para mostrar.

## A correção: um treemap de verdade

`ui::treemap` reparte o retângulo pelo algoritmo *squarified* de Bruls, Huizing e van
Wijk: a **área** de cada peça é proporcional ao peso, e as fileiras crescem enquanto a
pior razão de aspecto delas melhora — é o que evita as tiras compridas e finas de um
treemap ingênuo.

Duas decisões que o terminal impõe:

**Nada de piso de tamanho.** Quem não cabe legível **sai** do mapa, do menor para o
maior, até todos os que ficam caberem. Um piso é justamente o que fazia a grade antiga
mentir; melhor uma caixa a menos do que uma caixa do tamanho errado.

**Os limites são arredondados a partir das coordenadas acumuladas**, e não do tamanho de
cada peça. É o que faz o fim de uma peça ser o começo exato da seguinte — há teste que
percorre célula por célula e exige que cada uma da área seja coberta exatamente uma vez.

## O recurso: o tamanho mede o que se pedir

`Ctrl+W` anda por cinco modos, e o **título do painel** diz qual está ligado.

| | Tamanho | O que responde |
| --- | --- | --- |
| 1 | **posição em R$** (padrão) | onde está o meu dinheiro |
| 2 | **lucro total (contra o PM)** | de onde veio o meu lucro |
| 3 | **lucro do mês** | o que andou desde o dia 1º |
| 4 | **lucro da semana** | o que andou desde segunda |
| 5 | **ajuste à carteira alvo** | o que falta comprar ou vender |

**A cor e a linha do meio são sempre a variação do dia**, em todos os modos. É a regra que
faz o mapa continuar legível ao trocar de modo: quem aprendeu a ler as cores não reaprende
nada, e a única pergunta que muda é «tamanho de quê». A terceira linha — a grandeza que
decidiu o tamanho — aparece só nas caixas altas o bastante para ela.

Três detalhes que caem fora do óbvio:

* **O lucro do mês e o da semana medem contra o último fechamento *antes* do começo da
  janela**, e não contra o primeiro ponto dentro dela: o lucro do mês é contra onde o
  papel fechou o mês passado. A semana começa na **segunda**, que é quando a bolsa abre.
* **O modo «ajuste» mostra papéis que não se tem.** Um papel recomendado e ainda não
  comprado é justamente o que tem o maior ajuste; deixá-lo de fora esconderia a maior
  coisa a fazer. Por isso ele lista 27 caixas onde os outros listam 21.
* **Quem não tem o número não entra com zero.** Sem preço médio não há lucro total, sem
  série não há lucro do mês, e fora de toda carteira recomendada não há ajuste. A legenda
  diz quantos ficaram de fora, em vez de desenhar caixas invisíveis.

## Números cortados nunca

`calc::moeda_curta` — `747,9 mil`, `2,0 mi`, `930`. Existe por um perigo concreto:
`R$ 30.291,00` cortado na largura da caixa dava `R$ 30.29`, que não é um número truncado,
é **outro número**, e se lê como trinta reais. Uma grandeza abreviada diz menos; uma
cortada mente. E quando nem a forma curta cabe, a linha **some** em vez de encolher.

## Duas coisas do desenho

**O cursor é a moldura, grossa** (`┏━━┓`), e não o fundo invertido. Inverter o fundo de
uma caixa que ocupa meia tela pinta meia tela, e o que era para ser um cursor vira um
bloco de cor que parece defeito.

**O texto é centrado na vertical.** Numa caixa de vinte linhas, três linhas encostadas no
topo deixam a caixa parecendo vazia.

## Como testar

### 1. A área segue o valor

Abra o Heatmap (`9` na aba Invest). **Esperado:** a maior posição com a maior caixa, e uma
posição com metade do valor com **metade da área** — não metade da largura. Compare com a
coluna *Mercado* de Posições.

O mapa cobre o painel inteiro: sem faixa vazia embaixo, sem buraco entre caixas, sem
borda desenhada por cima de outra.

### 2. Os cinco modos

`Ctrl+W` cinco vezes, voltando ao começo. **Esperado:** o título mudando a cada vez, e o
conjunto de caixas mudando junto — «ajuste» com mais caixas que os outros (inclui o que
ainda não se tem), «lucro do mês» com menos (exclui quem não tem série).

**A cor não muda entre os modos.** Um papel que caiu hoje é vermelho nos cinco.

### 3. Nenhum número cortado

Estreite o terminal até as caixas ficarem pequenas. **Esperado:** a terceira linha some
inteira quando não cabe. Nenhuma caixa pode mostrar `R$ 30.29` de um valor de R$ 30.291.

### 4. O cursor

Setas andam entre as caixas. **Esperado:** a caixa sob o cursor com a **moldura grossa**,
o conteúdo dela legível como antes, e nenhum bloco de fundo invertido.

### 5. Uma tela estreita

Reduza a janela. **Esperado:** as menores saem do mapa uma a uma, e as que ficam
continuam proporcionais entre si — nunca todas do mesmo tamanho.

## Como saber que falhou

- Buraco ou sobreposição entre caixas (o arredondamento voltou a ser por peça)
- Faixa vazia no fim do painel
- Uma posição pequena com caixa de posição grande (voltou o piso de tamanho)
- Número cortado numa caixa estreita
- A cor mudando ao trocar de modo
- Fundo invertido cobrindo a caixa sob o cursor
- «lucro do mês» preso em «buscando» depois de a série chegar
