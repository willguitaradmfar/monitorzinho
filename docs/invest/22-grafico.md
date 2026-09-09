# 22 — Gráfico

**Grupo:** Mercado · **Estado:** planejado · **Fase:** 3
**Depende de:** [02 — Provedores](02-dados-e-provedores.md)
**Precisa de:** `Need::Historico`
**Custo:** uma requisição de histórico por ativo/período, com cache. Ver §6.

---

## 1. O que responde

**Como este preço chegou até aqui.**

## 2. Linha, não candle — na v1

Um candle precisa de quatro valores por período e de meia dúzia de células de largura para
ser legível. Num terminal, uma tela de 120 colunas dá cerca de 110 candles, e o desenho
com blocos Unicode fica ambíguo em fonte estreita.

A v1 desenha **linha de fechamento**, com a `Sparkline`/`Chart` que o programa já usa, e
mostra máxima e mínima do período no cabeçalho. É o que cabe bem e é o que a base já sabe
desenhar.

O candle entra depois, como modo alternado com uma tecla, quando houver um desenho testado
em terminal estreito. Está fora da v1 por medida, não por preguiça.

## 3. Períodos

`←`/`→` andam entre: 1 dia, 5 dias, 1 mês, 6 meses, 1 ano, 5 anos.

Cada período pede uma granularidade diferente ao provedor (minuto, hora, dia, semana), e
nem todo provedor dá todas. Um período indisponível **não some do seletor**: ele aparece e
diz que a fonte não tem. Sumir faria parecer que o período não existe.

## 4. O cursor

`Ctrl+X` liga um cursor vertical que anda com ←/→ e mostra, no cabeçalho, a data e o valor
daquele ponto. Sem ele, ler um valor no meio da série é adivinhar pela altura.

Com o cursor ligado, ←/→ deixam de trocar o período e passam a andar no tempo. O rodapé
diz o que as setas estão fazendo agora — um mesmo par de teclas com dois significados
precisa dizer qual está valendo.

## 5. A tela

```
┌ PETR4 · 6 meses ────── 38,42  máx 41,80 (12/07)  mín 29,15 (03/04) ─┐
│                                                  ▂▄▆█▇▆▅            │
│                                    ▁▃▄▅▆▇█▇▅▄▃▄▅▆                   │
│              ▁▂▃▄▅▄▃▂▁▂▃▄▅▆▇█▇▆▅▄▃                                  │
│  ▃▄▅▄▃▂▁▂▃▄▅▆                                                       │
├──────────────────────────────────────────────────────────────────────┤
│ PM 32,10 ─────────────────────────────────────────  P&L +19,7%      │
└─ ←/→ período · Ctrl+X cursor · i indicadores · c comparar · Esc ────┘
```

**A linha do preço médio é desenhada por cima**, quando o ativo está na carteira. É o
único número que transforma um gráfico em uma decisão, e tê-lo no gráfico evita a conta de
cabeça que todo mundo faz olhando para essa tela.

## 6. Cache e custo

Uma série buscada fica em memória enquanto o módulo está aberto, por `(ativo, período)`.
Andar entre períodos já visitados não gera requisição. Trocar de ativo descarta o que não
é mais visível, com um teto de algumas séries.

A série **não** vai para a tabela `cotacao`: aquele cache guarda o último ponto de cada
ativo para a aba abrir com números, não séries inteiras — ver
[03 §4](03-armazenamento.md#4-o-cache-de-cotações).

## 7. Teclas

| Tecla | O que faz |
| --- | --- |
| ← / → | período — ou o tempo, com o cursor ligado |
| `Ctrl+X` | liga e desliga o cursor |
| `i` | abre [30 — Indicadores](30-indicadores.md) sobre esta série |
| `c` | abre [33 — Comparador](33-comparador.md) com este ativo |
| `Ctrl+T` | troca de ativo — abre uma busca sobre a watchlist |
| `Esc` | desliga o cursor, se ligado; depois sai |

Letras podem ser teclas puras aqui: não há busca digitada direto nesta tela. A busca de
ativo é uma caixa que se abre com `Ctrl+T` e aí sim engole as letras.

## 8. Erros e degradação

* Ativo em preço manual: `Pane::Empty` explicando que não há histórico porque a fonte é
  manual, e o que mudaria isso.
* Fonte sem aquele período: diz qual período ela tem.
* Série com buracos (feriado, pregão sem negócio): buracos desenhados como buracos.

## 9. Como validar

* Andar por todos os períodos e voltar: só a primeira visita a cada um gera requisição.
* Um ativo da carteira: a linha do PM está lá, e o P&L do rodapé bate com
  [Posições](10-posicoes.md).
* Cursor ligado: ←/→ andam no tempo e o rodapé diz isso.
* Ativo manual: mensagem clara, não gráfico vazio.
