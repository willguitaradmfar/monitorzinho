# 23 — Heatmap

**Grupo:** Mercado · **Estado:** planejado · **Fase:** 4
**Depende de:** [20 — Cotações](20-cotacoes.md)
**Precisa de:** `Need::Cotacao`
**Custo:** zero próprio — lê o retrato de [20](20-cotacoes.md).

---

## 1. O que responde

**O mercado inteiro num olhar: o que subiu, o que caiu, e o tamanho de cada coisa.**

## 2. Por que isso funciona bem num terminal

Um heatmap é uma grade de retângulos coloridos com um rótulo dentro. É exatamente o que
uma grade de células de texto é. Não há nada a aproximar aqui — diferente do candle
([22 §2](22-grafico.md#2-linha-não-candle--na-v1)), que é uma forma que o terminal não tem.

`Pane::Grid` é o painel criado para isto.

## 3. As duas dimensões

| Dimensão | O que é | Como aparece |
| --- | --- | --- |
| Variação | quanto subiu ou caiu | a **cor** da célula |
| Tamanho | peso na carteira, ou volume | a **largura** da célula |

Duas fontes possíveis de tamanho, com `Ctrl+W`:

* **peso na carteira** — "onde meu dinheiro está sendo machucado hoje";
* **volume negociado** — "onde o mercado está prestando atenção".

Sem tamanho, todas as células têm a mesma largura, e a grade vira uma tabela colorida —
ainda útil, e é o padrão quando não há de onde tirar peso.

## 4. A escala de cor

Divergente e simétrica em torno de zero, com cinco degraus para cada lado, a partir de
`palette::GREEN` e `palette::RED`. Os limiares são **fixos e escritos na legenda** —
±0,5%, ±1%, ±2%, ±5% — e não relativos ao dia.

Escala relativa ao dia é o erro comum: num dia calmo ela pinta de vermelho forte uma queda
de 0,3%, e num crash pinta de amarelo uma queda de 6%. A cor tem que significar a mesma
coisa todo dia, senão ela não ensina nada.

Preço `manual` ou `não-oficial` fica **cinza**, sem cor de variação — mesma regra de
[20 §4](20-cotacoes.md#4-as-cores).

## 5. A tela

```
┌ Heatmap · carteira · por peso ──────────────────────────────────┐
│ ┌──────────────┬────────┬──────────┬─────┬────────────────────┐ │
│ │    BTCBRL    │ HGLG11 │  PETR4   │BBAS3│      TESOURO       │ │
│ │    +2,14%    │ manual │  manual  │ mnl │      +0,04%        │ │
│ └──────────────┴────────┴──────────┴─────┴────────────────────┘ │
├─────────────────────────────────────────────────────────────────┤
│ −5%  ▓▓ ▒▒ ░░  0  ░░ ▒▒ ▓▓  +5%   ·  cinza = preço não ao vivo  │
└─ ↑/↓/←/→ · Enter gráfico · g agrupar · Ctrl+W tamanho · Esc ────┘
```

Com `g`: sem agrupamento, por classe, por setor. Agrupado, cada grupo é um bloco com
título e as células dentro.

**A legenda é permanente.** Um mapa de cores sem legenda é um mapa que cada pessoa lê de
um jeito.

## 6. Teclas

| Tecla | O que faz |
| --- | --- |
| setas | anda pelas células |
| `Enter` | abre [22 — Gráfico](22-grafico.md) |
| `g` | agrupamento |
| `Ctrl+W` | fonte do tamanho: peso ou volume |
| `f` | alterna entre carteira e watchlist |
| `Esc` | sai |

## 7. Fica de fora na v1

* Índice inteiro (todas as ações do IBOV). Precisa da composição do índice, que não há
  sem chave. Fica no que se possui e no que se acompanha.
* Treemap com áreas proporcionais de verdade (retângulos aninhados). Larguras proporcionais
  numa grade regular são o que cabe bem num terminal.

## 8. Como validar

* Terminal estreito: as células encolhem até um mínimo legível e depois quebram em linhas.
* Um ativo manual: cinza, e a legenda explica.
* Um dia calmo e um dia de queda forte: a mesma queda percentual tem a mesma cor nos dois.
