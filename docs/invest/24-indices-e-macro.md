# 24 — Índices e macro

**Grupo:** Mercado · **Estado:** planejado · **Fase:** 3
**Depende de:** [02 — Provedores](02-dados-e-provedores.md)
**Precisa de:** `Need::Cotacao`
**Custo:** entra no lote de [20](20-cotacoes.md). Sem requisição própria.

---

## 1. O que responde

**O que está acontecendo com o pano de fundo — o mercado, o dólar, o juro, o medo.**

## 2. O que entra

| Indicador | Fonte v1 | Grade |
| --- | --- | --- |
| CDI, Selic, IPCA, IGP-M | BCB/SGS | `Fechamento` |
| USD/BRL, EUR/BRL | AwesomeAPI + PTAX | `AoVivo` |
| BTC, ETH, dominância | Binance | `AoVivo` |
| **IBOV** | **Kinvo** (aberta, sem chave) | `Atrasado` |
| S&P 500, Nasdaq, DXY, VIX | sem fonte sem chave | `Manual` |
| Treasury 10y, DI futuro | sem fonte sem chave | `Manual` |

O **Ibovespa saiu da lista dos informados**: a API aberta da Kinvo o serve, sem chave e em
lote ([02 §3.5](02-dados-e-provedores.md)). E ele não saiu porque alguém editou esta
tabela — saiu porque o módulo deixou de decidir por mercado e passou a perguntar
`ProviderSet::quem_busca`. Uma fonte nova amanhã move outro indicador sozinha.

O resto da tabela continua manual, e isso é a consequência direta da decisão de
[02 §1](02-dados-e-provedores.md#1-a-decisão-e-a-tensão-dentro-dela). É também o módulo em
que essa consequência dói mais, porque índice é justamente o número que ninguém quer
digitar.

**Isso está escrito aqui de propósito.** Este é o módulo com maior chance de motivar a
ligação de um provedor não-oficial ou com chave, e a decisão de fazer isso deve ser tomada
olhando o que se ganha e o que se aceita, não por atrito acumulado.

## 3. A tela

Blocos por família, cada um um `Pane::Facts` com mini-gráfico onde há série:

```
┌ Índices e macro ─────────────────────────────────────────────────┐
│ Juro e inflação          Câmbio               Bolsa              │
│ Selic    15,00%          USD/BRL  5,4210      IBOV   142.318 mnl │
│ CDI      10,40%  ▃▄▄▅▅   EUR/BRL  5,9840      S&P      6.284 mnl │
│ IPCA 12m  4,12%  ▅▄▄▃▃   DXY        — mnl     VIX       14,2 mnl │
│                                                                  │
│ Cripto                   Renda fixa                              │
│ BTC   512.340  +2,14%    Tesouro IPCA+ 2030   6,12% a.a.  mnl    │
│ dominância 54,1%                                                 │
└─ ↑/↓ · Enter série · Ctrl+E editar manuais · Esc sair ──────────┘
```

`mnl` marca o que é informado. Nunca colorido, nunca com variação calculada — mesma regra
de [20 §4](20-cotacoes.md#4-as-cores).

## 4. Teclas

| Tecla | O que faz |
| --- | --- |
| ↑ / ↓ | anda pelos indicadores |
| `Enter` | abre a série histórica daquele indicador |
| `Ctrl+E` | edita os valores manuais |
| `Esc` | sai |

## 5. Erros e degradação

* SGS fora do ar: os últimos valores ficam com a data ao lado. Uma série do BCB
  desatualizada por um dia é normal e não é erro.
* Indicador manual nunca preenchido: mostra `—` e um convite a preencher. Não some.

## 6. Como validar

* CDI e Selic conferem com o site do BCB, no mesmo dia.
* Um indicador manual editado sobrevive ao reinício.
* Sem rede: tudo fica, com data, e nada vira zero.
