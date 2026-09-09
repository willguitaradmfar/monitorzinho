# 16 — Corretoras

**Grupo:** Carteira · **Estado:** planejado · **Fase:** 4
**Depende de:** [10 — Posições](10-posicoes.md)
**Precisa de:** `Need::Posicoes`
**Custo:** agregação sobre dezenas de linhas. Sem I/O.

---

## 1. O que responde

**O mesmo patrimônio, visto por onde ele está guardado.**

## 2. Por que é módulo e não uma dimensão de [Alocação](11-alocacao.md)

Porque a pergunta é outra. Alocação pergunta "estou bem distribuído"; aqui a pergunta é
operacional: onde eu entro para mexer nisso, de onde veio cada importação, e o que está
desatualizado.

Três coisas que só existem nesta visão:

* **A idade dos dados por fonte.** "corretora-x: importada há 3 dias" é a informação que
  decide se o P&L da tela vale alguma coisa. Ela é por fonte, e some quando os números são
  consolidados por ativo.
* **Concentração de custódia.** 80% do patrimônio numa corretora só é um risco de natureza
  diferente de 80% numa classe só, e nenhuma outra tela mostra isso.
* **O que reimportar.** A lista de fontes é a lista do que precisa ser atualizado.

## 3. A tela

```
┌ Corretoras ──────────────────────────── R$ 148.302,10 em 3 fontes ─┐
│ Fonte          Conta      Ativos    Valor        Peso   Importada  │
│ corretora-x    12345-6        7    92.140,10    62,1%   há 3 d     │
│ corretora-y    98765-4        4    56.162,00    37,9%   há 3 d     │
│ manual         —              1       ...        ...    hoje       │
├────────────────────────────────────────────────────────────────────┤
│ maior concentração: corretora-x com 62,1%                          │
└─ ↑/↓ · Enter ver posições · i reimportar · Esc sair ──────────────┘
```

`Enter` abre as posições daquela fonte. `Ctrl+I` leva para
[51 — Importação](51-importacao.md) já com a fonte escolhida — que é o gesto que essa tela
existe para encurtar.

## 4. Erros e degradação

* Uma fonte cuja última importação passou de 30 dias fica marcada. Não é erro: é que o
  P&L dela está sendo calculado sobre quantidades de um mês atrás.
* Uma fonte sem nenhuma posição (tudo vendido) continua listada com zero, até ser removida
  à mão. Sumir sozinha esconderia que aquela conta existe.

## 5. Fica de fora na v1

* Conexão direta com corretora. Não há API pública para isso e, se houvesse, seria
  credencial — que este desenho não guarda.
* Saldo em conta / dinheiro parado. Entra quando [Lançamentos](15-lancamentos.md) tiver
  aporte e retirada em uso de verdade.

## 6. Como validar

* Somar o valor das fontes: bate com o total de [Posições](10-posicoes.md), ao centavo.
* Reimportar uma fonte: só a idade dela muda.
* Uma posição digitada à mão: aparece na fonte `manual`, e não some junto com uma
  reimportação.
