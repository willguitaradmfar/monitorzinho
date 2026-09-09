# 32 — Correlação

**Grupo:** Análise · **Estado:** planejado · **Fase:** 4
**Depende de:** [31 — Risco](31-risco.md)
**Precisa de:** `Need::Posicoes`, `Need::Historico`
**Custo:** N² sobre séries já carregadas. Ver §5.

---

## 1. O que responde

**Quais dos meus ativos sobem e descem juntos — ou seja, onde a diversificação que eu
acho que tenho não existe.**

## 2. A matriz

Correlação de Pearson dos retornos diários, par a par, no período escolhido.

```
┌ Correlação · 12 meses ──────────────────── 8 ativos · 248 dias ─┐
│           PETR4  VALE3  BBAS3  HGLG11  BTC   ETH   IVVB  TESO   │
│  PETR4     1,00   0,71   0,58    0,12  0,08  0,11  0,34  −0,05  │
│  VALE3      ·     1,00   0,49    0,09  0,14  0,16  0,38  −0,02  │
│  BBAS3      ·      ·     1,00    0,21  0,04  0,07  0,29   0,01  │
│  HGLG11     ·      ·      ·      1,00  0,02  0,03  0,11   0,18  │
│  BTC        ·      ·      ·       ·    1,00  0,88  0,41  −0,03  │
│  ETH        ·      ·      ·       ·     ·    1,00  0,39  −0,01  │
├──────────────────────────────────────────────────────────────────┤
│ mais correlacionados: BTC–ETH 0,88 · PETR4–VALE3 0,71            │
│ correlação média da carteira: 0,27                               │
└─ ←/→ período · f fonte · Enter par no gráfico · Esc sair ───────┘
```

Só a metade de cima é desenhada — a matriz é simétrica, e desenhar as duas metades gasta
espaço para repetir a informação.

A escala de cor é divergente: azul para negativo, cinza para perto de zero, vermelho para
perto de 1 — porque correlação alta é o que se quer notar numa carteira, e vermelho é a cor
de "olhe para isto".

## 3. As duas linhas do rodapé

São o módulo inteiro, na prática:

* **O par mais correlacionado** é a resposta a "o que eu tenho em duplicata".
* **A correlação média** é um número só para acompanhar ao longo do tempo. Ela subindo
  significa que a carteira está ficando mais parecida consigo mesma.

## 4. A armadilha que a tela tem que evitar

Correlação **não é causalidade** e, mais importante aqui, **não é estável**. Duas coisas
com correlação de 0,1 em ano calmo vão para 0,8 num crash — que é exatamente quando a
diversificação deveria funcionar.

A tela mostra o período usado, sempre, e oferece períodos curtos e longos justamente para
que a instabilidade fique visível. O que ela **não** faz é apresentar um número como se
fosse uma propriedade fixa dos ativos.

## 5. Custo

N² pares, cada um sobre M dias. Com 30 ativos e 250 dias são 435 pares e cerca de 110 mil
multiplicações — irrelevante.

O que não é irrelevante é **carregar 30 séries históricas**, que é I/O de rede e é o que
`open()` dispara. Por isso este módulo tem um carregamento visível: a matriz aparece
parcial e vai preenchendo, em vez de esperar tudo para mostrar algo. Mesma filosofia de
[01 §5](01-lista-de-modulos.md#5-entrar).

Pares em que uma das séries falta ficam vazios, e não zerados. Zero é uma correlação
válida e significa "não andam juntos"; vazio significa "não sabemos".

## 6. Teclas

| Tecla | O que faz |
| --- | --- |
| ← / → | período |
| setas ↑↓ | anda pelas linhas |
| `f` | alterna entre carteira e watchlist |
| `Enter` | abre o par no [comparador](33-comparador.md) |
| `Esc` | sai |

## 7. Como validar

* Um ativo consigo mesmo: exatamente 1,00.
* BTC e ETH: correlação alta, como se espera. Se der baixa, o cálculo está errado.
* Um ativo sem série: coluna vazia, não zerada.
* Períodos diferentes dão números diferentes — e isso é o esperado, não um defeito.
