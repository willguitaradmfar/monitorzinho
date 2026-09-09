# 14 — Proventos

**Grupo:** Carteira · **Estado:** planejado · **Fase:** 4
**Depende de:** [10 — Posições](10-posicoes.md)
**Precisa de:** `Need::Posicoes`
**Custo:** nenhum I/O próprio na v1 — os proventos são informados ou importados.

---

## 1. O que responde

**Quanto a carteira me paga, quando paga, e o quanto isso rende sobre o que eu de fato
gastei.**

## 2. Yield on cost, e por que é o número que interessa

O *dividend yield* que se lê em qualquer lugar é sobre o preço de hoje. O número que
importa para quem já é dono é sobre o **preço médio** — o que se pagou.

```
yield on cost = proventos dos últimos 12 meses ÷ (quantidade × preço médio)
```

Uma posição comprada a R$ 20 que hoje vale R$ 40 e paga R$ 2 por ano tem yield de 5% para
quem for comprar agora, e de 10% para quem já tem. São dois números certos respondendo
duas perguntas diferentes, e este módulo mostra os dois lado a lado, nomeados.

O yield on cost depende do preço médio informado, e por isso herda a mesma ressalva: se o
PM veio de uma importação parcial, o número herda o erro. A tela mostra a origem do PM.

## 3. Os dados

Na v1, proventos são **informados ou importados**. Nenhuma fonte sem chave dá histórico de
proventos confiável para B3 e US — é a mesma parede de [02 §1](02-dados-e-provedores.md#1-a-decisão-e-a-tensão-dentro-dela).

Entram por:
* extrato da corretora, via [51 — Importação](51-importacao.md) — o caminho principal;
* digitados, um a um;
* como lançamento do tipo provento em [15](15-lancamentos.md).

Cada provento: `(ativo, tipo, valor por unidade, quantidade na data, data-com, data-ex,
data de pagamento, imposto retido)`.

`tipo` é dividendo, JCP, rendimento de FII, ou amortização — e a distinção é fiscal, não
cosmética: dividendo é isento, JCP tem 15% retido na fonte, rendimento de FII é isento com
condições, e amortização é devolução de capital que **reduz o preço médio**. Essa última é
a única coisa no programa inteiro autorizada a mexer no PM, e mesmo assim ela não mexe: ela
**avisa** que o PM deveria ser reduzido e por quanto, e quem edita é o usuário.

## 4. A tela

```
┌ Proventos ────────────────── 12 meses: R$ 3.418,00 · YoC 2,7% ─┐
│ Recebidos                                                       │
│ Data       Ativo    Tipo        Bruto     Retido      Líquido  │
│ 05/09/26   HGLG11   rendimento  132,00      0,00       132,00  │
│ 02/09/26   PETR4    JCP         210,00     31,50       178,50  │
│ …                                                               │
├─────────────────────────────────────────────────────────────────┤
│ Agendados                                                       │
│ 15/09/26   BBAS3    dividendo    98,00      0,00        98,00  │
├─────────────────────────────────────────────────────────────────┤
│ por mês  ▂▃▅▂▇▃▄▆▃█▄▅                                          │
│ HGLG11  YoC 8,4% · yield atual 8,1%   ·   PETR4  YoC 14,2% …   │
└─ ↑/↓ · t alternar recebido/agendado/ativo · Esc sair ──────────┘
```

Três visões com `t`: por data (o padrão), por ativo (com YoC de cada), e o calendário do
que está por vir.

## 5. Teclas

| Tecla | O que faz |
| --- | --- |
| ↑ / ↓ | anda |
| letras | busca |
| `Ctrl+T` | alterna a visão — `Ctrl` porque há busca digitada direto nesta tela |
| `Ctrl+A` | adiciona provento |
| `Enter` | edita |
| `Del` | remove, com confirmação destrutiva |
| `Esc` | limpa busca; depois sai |

## 6. Erros e degradação

* Sem provento nenhum: `Pane::Empty` explicando os três caminhos de entrada.
* Provento de um ativo que não está mais na carteira: **aparece mesmo assim**. Recebido é
  recebido, e sumir com ele por causa de uma venda apagaria o histórico.
* Quantidade na data desconhecida: o valor total é aceito direto, e o YoC daquele ativo
  fica marcado como incompleto em vez de estimado.

## 7. Fica de fora na v1

* Busca automática de proventos anunciados. Precisa de fonte que não temos.
* Reinvestimento automático no cálculo. Um provento reinvestido é um aporte, e entra como
  aporte em [13](13-patrimonio.md).
* Ajuste automático de PM por amortização — avisa, não faz. Ver §3.

## 8. Como validar

* JCP de R$ 210 com 15%: líquido de R$ 178,50, e o retido aparece separado.
* Vender um ativo: os proventos recebidos dele continuam na lista.
* YoC de uma posição sem PM informado: marcado como incompleto, não estimado.
