# 33 — Comparador

**Grupo:** Análise · **Estado:** planejado · **Fase:** 4
**Depende de:** [22 — Gráfico](22-grafico.md)
**Precisa de:** `Need::Historico`
**Custo:** as séries dos ativos comparados. Sem cálculo pesado.

---

## 1. O que responde

**Qual desses foi melhor, e por quanto.**

## 2. Normalizar a 100 é o módulo inteiro

Duas séries em escalas diferentes — uma ação a R$ 38 e um índice a 142 mil — não se
comparam no mesmo eixo: a de escala maior domina o desenho e a outra vira uma linha reta
no rodapé.

A saída é normalizar: todo ativo começa em 100 no início do período, e o gráfico passa a
mostrar **retorno relativo** em vez de preço. Aí as linhas são comparáveis por construção.

O eixo é rotulado como retorno percentual, não como preço, para que ninguém leia "112" como
reais.

## 3. A âncora

O ponto de partida é o **início do período**, e trocar o período reancorará tudo. Isso é
correto e é surpreendente na primeira vez: o mesmo par de ativos pode trocar de posição
quando se muda de 6 meses para 12, porque a pergunta mudou.

O rodapé sempre diz a data da âncora.

## 4. A comparação com o CDI

Uma linha do CDI acumulado é oferecida em qualquer comparação, com `Ctrl+B`. É a referência
que responde a pergunta que importa no Brasil — *"isso rendeu mais que deixar parado?"* —
e ela vem de graça de [26](26-renda-fixa.md).

## 5. A tela

```
┌ Comparador · 12 meses · base 100 em 08/09/25 ──────────────┐
│  PETR4  +19,7%   BTCBRL  +48,2%   CDI  +10,4%   IBOV  mnl  │
│                                          ╭─── BTCBRL       │
│                                    ▁▃▄▆█▇                  │
│                        ▁▂▃▄▅▆▇█▇▆▅▆                        │
│         ▁▂▃▄▄▅▅▆▆▇▇███  ╰── PETR4                          │
│  ───────────────────────────────────── CDI                 │
├─────────────────────────────────────────────────────────────┤
│ melhor: BTCBRL +48,2%  ·  pior: IBOV (sem série)           │
└─ ←/→ período · Ctrl+A add · Del tirar · Ctrl+B CDI · Esc ──┘
```

Até seis séries. Acima disso o gráfico em blocos Unicode fica ilegível, e a legenda não
cabe — a sétima é recusada com essa explicação, e não silenciosamente ignorada.

## 6. Teclas

| Tecla | O que faz |
| --- | --- |
| ← / → | período |
| `Ctrl+A` | adiciona um ativo à comparação |
| `Del` | tira o selecionado |
| `Ctrl+B` | liga e desliga a linha do CDI |
| `Esc` | sai |

## 7. Erros e degradação

* Ativo sem série (manual): entra na legenda marcado como `mnl`, sem linha. Some da
  comparação mas não da lista — senão parece que não foi adicionado.
* Séries de tamanhos diferentes: o período efetivo é a interseção, e o rodapé diz qual é.
  Comparar retornos sobre janelas diferentes é comparar coisas diferentes.

## 8. Como validar

* Comparar um ativo consigo mesmo: duas linhas idênticas.
* O retorno da legenda bate com `(fim / início − 1)` da série.
* Trocar o período: os retornos mudam, e a data da âncora acompanha.
* Adicionar um sétimo: recusa com explicação.
