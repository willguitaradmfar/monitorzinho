# 26 — Renda fixa

**Grupo:** Mercado · **Estado:** planejado · **Fase:** 2
**Depende de:** [02 — Provedores](02-dados-e-provedores.md)
**Precisa de:** nada
**Custo:** uma requisição por série por dia. É a fonte mais barata da aba.

---

## 1. Por que este é o módulo com melhor relação custo/valor da lista inteira

A API do Banco Central (SGS) é **pública, gratuita, sem chave, sem cadastro, estável e
oficial**. Não há nenhuma outra fonte nesta aba com essas cinco propriedades ao mesmo tempo.

E o dado que ela dá é o que ancora quase toda decisão de investimento no Brasil: se o CDI
está em 10,4%, tudo o mais é comparado com isso.

Construir este módulo é barato e não depende de decisão nenhuma que ainda esteja em aberto.

## 2. As séries

Selic (meta e diária), CDI, IPCA, IGP-M, e a poupança.

**Os códigos de série do SGS têm que ser conferidos no catálogo, um por um, na
implementação**, e fixados em constantes com o nome escrito ao lado. Escrever de memória
um código de série econômica é o tipo de erro que não aparece: o número chega, é plausível,
e é de outra coisa. Um teste que confere ordem de grandeza (um CDI entre 0% e 30%) pega o
caso grosseiro; o caso sutil só o catálogo pega.

## 3. Acumulado, e a armadilha dele

O SGS dá a série diária. O que se lê é o **acumulado**, e acumular juro é multiplicativo,
não aditivo:

```
acumulado = ∏(1 + taxa_do_dia) − 1
```

Somar taxas diárias é o erro clássico, e num ano de CDI alto ele erra por vários décimos
de ponto percentual. A tela mostra 12 meses, no ano, e no mês, e os três são produtos.

O IPCA é mensal e não diário, e "IPCA 12 meses" também é produto dos doze últimos meses,
não soma.

## 4. A tela

```
┌ Renda fixa ─────────────────────────────── BCB · dados de 05/09 ─┐
│ Indicador      Hoje      No mês    No ano   12 meses             │
│ Selic (meta)  15,00%          —        —          —              │
│ CDI            0,0556%    0,84%    7,12%     10,40%   ▃▄▄▅▅▅▆▆   │
│ IPCA               —      0,31%    3,08%      4,12%   ▅▅▄▄▃▃▃▂   │
│ IGP-M              —     −0,12%    1,94%      2,88%              │
│ Poupança           —      0,52%    4,41%      6,17%              │
├──────────────────────────────────────────────────────────────────┤
│ juro real (CDI − IPCA, 12m): 6,03%                               │
│ sua renda fixa: R$ 34.851,00 · 23,5% da carteira                 │
└─ ↑/↓ · Enter série · c calculadora · Esc sair ──────────────────┘
```

O **juro real** está na tela e não escondido: é a única linha que responde se o dinheiro
está de fato crescendo, e calculá-la de cabeça a partir das outras é o que todo mundo faz.

## 5. A calculadora

`c` abre uma caixa que responde a pergunta prática: *"quanto rende X, a Y% do CDI, por Z
meses, líquido de imposto?"*

Ela aplica a tabela regressiva de IR de renda fixa (22,5% até 180 dias, 20% até 360, 17,5%
até 720, 15% acima) e o IOF dos primeiros 30 dias. São regras fixas em lei, não dados de
mercado, e por isso podem ser codificadas com segurança — ao contrário de quase tudo o
mais nesta aba.

Ela usa o CDI **corrente** projetado para a frente, o que é uma suposição, e diz que é.

## 6. Teclas

| Tecla | O que faz |
| --- | --- |
| ↑ / ↓ | anda |
| `Enter` | série histórica do indicador |
| `c` | calculadora |
| `Esc` | fecha a calculadora, se aberta; depois sai |

## 7. Cadência

Uma vez por dia por série. Estes números mudam uma vez por dia (ou por mês, no IPCA), e
pedi-los a cada dois segundos é gastar cota alheia para receber a mesma resposta.

A primeira volta ao abrir o módulo busca o que estiver mais velho que um dia; o resto vem
do cache.

## 8. Fica de fora na v1

* Curva de juros (DI futuro por vencimento). A fonte é a B3 e não é pública.
* Preços do Tesouro Direto por título. A fonte existe em arquivo aberto e é candidata
  natural para a v2 — fica registrado aqui como o próximo passo óbvio deste módulo.
* Marcação a mercado de título privado.

## 9. Como validar

* CDI de 12 meses confere com o publicado, com uma casa decimal.
* Acumular à mão trinta dias da série diária: bate com o "no mês" da tela.
* Sem rede: mostra os últimos valores com a data. Não vira zero.
