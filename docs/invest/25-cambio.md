# 25 — Câmbio

**Grupo:** Mercado · **Estado:** planejado · **Fase:** 2
**Depende de:** [02 — Provedores](02-dados-e-provedores.md)
**Precisa de:** nada
**Custo:** uma requisição leve por volta, compartilhada com todo o resto da aba.

---

## 1. Por que este módulo é crítico

O patrimônio é consolidado em **BRL**. Todo ativo que não é brasileiro passa por aqui
antes de virar um número na tela de [Posições](10-posicoes.md), [Alocação](11-alocacao.md)
e [Patrimônio](13-patrimonio.md).

**Um câmbio errado erra o patrimônio inteiro, silenciosamente.** É o único número da aba
com essa propriedade, e por isso este módulo tem regras que os outros não têm.

## 2. Duas cotações, de propósito

| Cotação | Fonte | Para que serve |
| --- | --- | --- |
| **mercado** | AwesomeAPI | converter o que está na tela, agora |
| **PTAX** | BCB | o número oficial do dia — o que vale para [imposto](50-imposto-de-renda.md) |

As duas são buscadas sempre, mesmo quando a primeira responde. Não são redundância: são
dois números certos para duas perguntas diferentes, e usar um no lugar do outro erra o
DARF.

A tela mostra as duas, lado a lado, nomeadas. Nunca uma "cotação do dólar" sem sobrenome.

## 3. A regra do câmbio ausente

Se não há câmbio, um valor em moeda estrangeira **não é convertido com um número velho
sem aviso**, e **não é tratado como se fosse real**.

O que acontece:

1. A linha mostra o valor na moeda original.
2. Fica de fora do total consolidado.
3. O total diz quantas linhas ficaram de fora e por quê.

Isso é a aplicação, no lugar onde mais importa, da regra geral de
[10 §8](10-posicoes.md#8-erros-e-degradação): nunca inventar. Um total que
silenciosamente exclui posições é ruim; um total que silenciosamente inclui posições
convertidas por um câmbio de três dias atrás é pior, porque parece exato.

Um câmbio de até 24 h **é** usado, com a idade visível. O corte é o mesmo do cache em
[03 §5](03-armazenamento.md#5-invest-cachejson).

## 4. A tela

```
┌ Câmbio ──────────────────────────────────── atualizado há 3 s ─┐
│ Par        Mercado    Var%      PTAX (05/09)   Fonte           │
│ USD/BRL     5,4210   +0,31%          5,4187   awesomeapi       │
│ EUR/BRL     5,9840   +0,18%          5,9812   awesomeapi       │
│                                                                 │
│ USD/BRL · 30 dias    ▂▃▄▃▄▅▆▅▄▅▆▇█▇▆▅▄▅▆▇                      │
├─────────────────────────────────────────────────────────────────┤
│ exposição em moeda estrangeira: R$ 21.108,41 (14,2% da carteira)│
└─ ↑/↓ · Ctrl+A par · Enter série · Esc sair ────────────────────┘
```

A linha de exposição é o que liga este módulo à carteira: quanto do patrimônio depende
deste número estar certo.

## 5. Teclas

| Tecla | O que faz |
| --- | --- |
| ↑ / ↓ | anda |
| `Enter` | série histórica do par |
| `Ctrl+A` | adiciona um par |
| `Esc` | sai |

## 6. Cadência

Compartilhada com [20 — Cotações](20-cotacoes.md): mesmo lote, mesma volta. O câmbio não
tem thread própria — ele é mais uma linha no retrato do `ProviderSet`.

Isso importa: se tivesse cadência própria, haveria um instante em que o preço de um ativo
em dólar já mudou e o câmbio ainda não, e o valor em BRL na tela seria de um par que nunca
existiu ao mesmo tempo. Buscando junto, o retrato é sempre coerente consigo mesmo.

## 7. Como validar

* Comparar com a PTAX publicada do dia: bate.
* Cortar a rede por uma hora: converte, com a idade visível.
* Cortar por dois dias: para de converter, e o total de [Posições](10-posicoes.md) diz
  quantas linhas ficaram de fora.
* Um ativo em USD: o BRL da tela é `preço × câmbio` do **mesmo** retrato.
