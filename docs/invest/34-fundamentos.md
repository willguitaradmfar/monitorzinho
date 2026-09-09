# 34 — Fundamentos

**Grupo:** Análise · **Estado:** planejado · **Fase:** depois de um provedor com dado fundamentalista
**Depende de:** [02 — Provedores](02-dados-e-provedores.md)
**Precisa de:** `Need::Provedor("fundamentos")`
**Custo:** uma requisição por ativo, com cache longo — o dado muda por trimestre.

---

## 1. O que responde

**Se o preço faz sentido diante do que a empresa é.**

## 2. O problema, dito de frente

**Não há fonte sem chave para dado fundamentalista brasileiro.** Balanço, DRE, P/L, P/VP,
ROE, dívida líquida — nada disso é publicado numa API pública utilizável. O que existe:

| Caminho | Situação |
| --- | --- |
| CVM (dados abertos) | dados brutos em arquivo, oficiais e completos. Exige baixar e montar os indicadores por conta própria — é um projeto |
| provedores com chave grátis | cobrem parte, com limite baixo |
| raspagem de sites | o que a maioria faz, e o que este projeto não vai fazer |

Então este módulo **não é da v1**, e não porque falte tempo: porque a decisão de
[02 §1](02-dados-e-provedores.md#1-a-decisão-e-a-tensão-dentro-dela) — só fontes sem
chave — o exclui por construção.

## 3. As duas saídas, quando chegar a hora

**A curta:** ligar um provedor com chave. Cinco indicadores, cache de um dia, e pronto.

**A longa, e mais interessante:** os dados abertos da CVM. São oficiais, completos, sem
chave e sem limite — as mesmas cinco propriedades que fazem o BCB ser a melhor fonte da
aba ([26 §1](26-renda-fixa.md#1-por-que-este-é-o-módulo-com-melhor-relação-custovalor-da-lista-inteira)).
O custo é que vêm em arquivo grande e por trimestre, e os indicadores têm que ser
calculados aqui. Isso é um módulo de importação, não um provedor de cotação.

Vale registrar como a opção preferida: é o único caminho que dá dado fundamentalista com
as garantias que o resto da aba tem.

## 4. O que mostraria

| Indicador | Para que |
| --- | --- |
| P/L, P/VP | preço contra lucro e contra patrimônio |
| DY | quanto paga, sobre o preço de hoje — o par do YoC de [14](14-proventos.md) |
| ROE | quanto a empresa rende sobre o capital dela |
| Dívida líquida / EBITDA | quanto ela deve |
| Margem líquida | quanto sobra do que ela vende |
| Receita e lucro, série trimestral | a direção, que é mais informativa que o nível |

Para FII, um conjunto diferente: P/VP, vacância, cap rate, dividend yield.

## 5. Como ele apareceria na lista

Com `Need::Provedor("fundamentos")`, então a linha diz `precisa de fundamentos` e o módulo
abre numa tela que explica as duas saídas do §3. Isso é melhor que não listá-lo: a pessoa
descobre que a coisa existe e por que não está disponível.

## 6. Como validar, quando existir

* P/L conferido com fonte externa para cinco empresas.
* Uma empresa com lucro negativo: P/L não é desenhado (é uma divisão que não significa
  nada), e a tela diz por quê — em vez de mostrar um número negativo sem sentido.
* Dado com mais de um trimestre de idade: a data do balanço aparece sempre.
