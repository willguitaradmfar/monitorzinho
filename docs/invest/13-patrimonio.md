# 13 — Patrimônio

**Grupo:** Carteira · **Estado:** planejado · **Fase:** 3
**Depende de:** [10 — Posições](10-posicoes.md)
**Precisa de:** `Need::Posicoes`, `Need::Cotacao`
**Custo:** um ponto novo por dia. A série vive em disco.

---


## O rumo: semana e mês

Duas linhas respondem «para onde isto está indo», e elas vêm de **duas fontes diferentes**
— a tela diz qual está sendo usada, porque as duas respondem perguntas diferentes.

Os períodos são **fechados, de calendário**: a semana passada é de **segunda a sexta**, o
mês passado vai do **dia 1 ao último**. Uma janela móvel de sete ou trinta dias dá um
número diferente a cada dia e nunca fecha — não é o que se entende por «a performance da
semana passada». As datas aparecem no rótulo (`semana 31/08–04/09`) para não haver dúvida
de qual período é.

* **Da série do patrimônio** (`serie::rumo`), quando ela cobre o período. É a resposta
  certa: ela sabe de aporte, retirada e provento, e separa o que foi mercado do que foi
  dinheiro novo entrando. «O patrimônio subiu R$ 10 mil» é verdade e pode ser só um
  aporte; «o mercado deu R$ 2 mil» é o que responde se as escolhas estão indo bem.
* **Do histórico de preço das posições de hoje**, enquanto a série não existe. Responde a
  pergunta vizinha: quanto estes papéis andaram. Ela **ignora o que foi comprado no meio
  do caminho**, e por isso a linha se nomeia — «as posições de hoje», nunca «patrimônio».
  Confundir as duas seria o erro caro deste módulo.

Um período que a série cobre pela metade **não é rotulado como o inteiro**: cinco dias de
série não são «o mês passado», e `Rumo::cobre`/`Rumo::pedido` existem para a tela poder
dizer isso. A folga de quatro dias em `Rumo::completo` é o fim de semana prolongado: exigir
um ponto por dia recusaria toda semana, porque sábado, domingo e feriado não têm pregão.

O cartão da home mostra só a versão da série: reconstruir do histórico custa uma busca por
ativo, e a home não vai à rede.

## 1. O que responde

**Quanto eu tinha, quanto tenho, e o que dessa diferença foi mercado e o que foi aporte.**

## 2. A distinção que dá sentido ao módulo

Um patrimônio que subiu R$ 10 mil no mês não diz nada sozinho. Pode ser R$ 10 mil de
aporte com o mercado parado, ou R$ 2 mil de aporte com R$ 8 mil de valorização, ou
R$ 15 mil de aporte com R$ 5 mil de perda. As três são situações completamente diferentes
e a curva sozinha mostra as três iguais.

Então a curva é **sempre decomposta**:

```
patrimônio(t) = patrimônio(t−1) + aporte + retirada + variação de mercado + proventos
```

Aporte e retirada vêm de [Lançamentos](15-lancamentos.md) quando existem, ou são
informados. Proventos vêm de [14](14-proventos.md). A variação de mercado é o resto —
**e é o resto de propósito**: é o único termo que não se informa, e fechá-lo por diferença
garante que a decomposição some exatamente ao que aconteceu.

## 3. A série

Um ponto **por dia**, gravado no primeiro tick da aba em cada dia novo. Não por tick: um
patrimônio amostrado a cada dois segundos é trezentos pontos de ruído intradiário para uma
grandeza que se lê em meses.

A série vive em `invest-patrimonio.json` (arquivo próprio — é a única coisa da aba que só
cresce), com `{ data, total_brl, aporte, retirada, proventos }` por dia.

**A série é histórica e não se recalcula.** O patrimônio de 12 de março foi o que foi, com
os preços daquele dia; recalcular com o preço de hoje daria outro número e apagaria o
registro. Isso é a mesma família de decisão do preço médio informado.

Um dia sem o programa aberto não tem ponto. A curva interpola visualmente entre os pontos
que existem e **marca as lacunas**, em vez de fingir continuidade.

## 4. A tela

```
┌ Patrimônio ─────────────────────────── R$ 148.302,10 · 12 meses ─┐
│                                                        ▁▃▅▆█    │
│                                          ▁▂▃▄▅▄▅▆▇█▇▆▇█         │
│                     ▁▂▃▄▃▄▅▆▇█▇▆▅▄▅▆▇█                          │
│  ▁▂▃▄▅▆▇█▇▆▅▆▇█                                                 │
├──────────────────────────────────────────────────────────────────┤
│ 12 meses      aportes  R$ 42.000,00                             │
│               mercado  R$ 18.884,30   (+14,6%)                  │
│             proventos  R$  3.418,00                             │
│                 total  R$ 64.302,30                             │
└─ ←/→ período · d decompor · Esc sair ───────────────────────────┘
```

`Pane::Chart` grande com `Pane::Facts` embaixo. A decomposição é sempre visível — não é
uma visão alternativa que se precisa procurar.

Períodos com ←/→: mês, 3 meses, 12 meses, tudo.

## 5. Teclas

| Tecla | O que faz |
| --- | --- |
| ← / → | período |
| `d` | alterna a decomposição entre valores absolutos e percentuais |
| `Esc` | sai |

## 6. Erros e degradação

* Menos de dois pontos: `Pane::Empty` dizendo que a curva começa a existir a partir do
  segundo dia de uso. É honesto e é temporário.
* Lacunas na série: desenhadas como lacunas, com quantos dias faltam no rodapé.
* Sem lançamentos: aporte e retirada ficam em zero e a variação de mercado absorve tudo.
  **A tela avisa que a decomposição está incompleta** — senão um aporte de R$ 10 mil
  aparece como valorização de R$ 10 mil, que é a mentira mais cara que este módulo poderia
  contar.

## 7. Fica de fora na v1

* Reconstruir o passado a partir de lançamentos + histórico de preços. É possível e é
  caro, e a série só começa quando o módulo começa.
* TWR e MWR (taxas ponderadas por tempo e por dinheiro). Ficam em [31 — Risco](31-risco.md).

## 8. Como validar

* Rodar dois dias seguidos: dois pontos, e o segundo com a decomposição do dia.
* Registrar um aporte e conferir: a variação de mercado **não** o inclui.
* Ficar uma semana sem abrir: a lacuna aparece como lacuna, não como linha reta.
* Somar aporte + mercado + proventos do período: bate exatamente com a diferença das
  pontas da curva.
