# 50 — Imposto de renda

**Grupo:** Operação · **Estado:** planejado · **Fase:** 4
**Depende de:** [15 — Lançamentos](15-lancamentos.md), [25 — Câmbio](25-cambio.md)
**Precisa de:** `Need::Posicoes`
**Custo:** aritmética. Sem I/O próprio.

---

## 1. O que responde

**Quanto eu devo este mês, e por quê.**

## 2. A limitação, dita antes de tudo

Apuração de imposto precisa de **custo por operação**. O preço médio é informado e não
derivado de operações ([03 §1.1](03-armazenamento.md#11-o-preço-médio-é-informado-nunca-calculado)),
então o que este módulo consegue calcular é:

```
ganho estimado = valor da venda − (preço médio informado × quantidade vendida)
```

Isso é o número certo **se** o preço médio informado for o preço médio real de todo o lote
vendido. É quase sempre verdade e é exatamente como a maioria das corretoras calcula. Mas
não é apuração completa, e as diferenças são reais:

* Não há histórico de lotes, então não há FIFO de verdade.
* Não há como separar automaticamente day trade de operação normal — as alíquotas são
  diferentes (20% contra 15%) e a compensação de prejuízo é separada.
* Eventos corporativos que alteram custo (bonificação, cisão) só entram se forem informados.

**Então este módulo se apresenta como estimativa, na tela, permanentemente.** Não numa nota
de rodapé — no título. Um módulo de imposto que parece exato e não é produz um DARF errado,
e isso tem consequência fora do programa.

## 3. O caminho para a apuração completa

Está aberto e não muda nenhuma decisão já tomada: quando [15 — Lançamentos](15-lancamentos.md)
tiver histórico completo de operações, este módulo passa a ter uma segunda fonte de custo
— **em paralelo, nunca sobrescrevendo o preço médio informado.**

A tela então mostraria os dois números lado a lado, com a origem de cada um. Onde eles
divergem, a divergência é informação, e resolvê-la é decisão de quem declara.

## 4. As regras que se pode codificar com segurança

Diferente de quase tudo nesta aba, aqui há regras fixas em lei, e elas podem ser escritas
com confiança:

| Regra | Valor |
| --- | --- |
| Ações — alíquota normal | 15% sobre o ganho |
| Ações — day trade | 20%, com apuração separada |
| Ações — isenção | vendas até R$ 20.000 no mês, para operação normal |
| FII | 20%, **sem** isenção de R$ 20 mil |
| Cripto | 15%, com isenção até R$ 35.000 vendidos no mês |
| Renda fixa | tabela regressiva — ver [26 §5](26-renda-fixa.md#5-a-calculadora) |
| Prejuízo | compensável dentro da mesma categoria, sem prazo |
| DARF | recolhimento até o último dia útil do mês seguinte; mínimo de R$ 10,00 |

Cada uma dessas linhas vira uma constante nomeada com a regra escrita ao lado, e cada uma
tem teste. **Alíquotas e limites mudam por lei**, e um número mágico no meio do código é o
que faz uma mudança de lei virar um bug silencioso.

Ativo no exterior tem regra própria e usa a **PTAX**, não a cotação de mercado — que é a
razão de [25](25-cambio.md) buscar as duas.

## 5. A tela

```
┌ Imposto · estimativa ─────────────────────── setembro/2026 ─┐
│ Categoria        Vendas      Ganho   Isento   Base   Imposto│
│ ações (normal)  18.400,00  2.140,00     sim      —        — │
│ ações (day)          0,00       —        —       —        — │
│ FII              6.200,00    840,00     não   840,00  168,00│
│ cripto               0,00       —        —       —        — │
├──────────────────────────────────────────────────────────────┤
│ prejuízo acumulado: ações R$ 1.204,00 · FII R$ 0,00         │
│ DARF de setembro: R$ 168,00 · vence 31/10                   │
├──────────────────────────────────────────────────────────────┤
│ ESTIMATIVA. Calculada sobre o preço médio informado, sem     │
│ histórico de lotes. Confira antes de recolher.               │
└─ ←/→ mês · Enter detalhar · Esc sair ───────────────────────┘
```

A linha de vendas de ações mostra `18.400,00` e `isento: sim` porque está abaixo dos
R$ 20 mil — e mostrar a venda mesmo isenta importa, porque é o número que decide a isenção
e é o que a pessoa precisa acompanhar durante o mês para não estourar sem perceber.

O aviso do rodapé é permanente e não é dispensável.

## 6. Teclas

| Tecla | O que faz |
| --- | --- |
| ← / → | mês |
| `Enter` | detalha as operações da categoria |
| `Ctrl+E` | edita o prejuízo acumulado de abertura |
| `Esc` | sai |

`Ctrl+E` existe porque quase todo mundo já tem prejuízo acumulado de antes de começar a
usar isto, e sem poder informá-lo o módulo erra desde o primeiro mês.

## 7. Fica de fora na v1

* Gerar o DARF ou o arquivo da declaração. O programa calcula e mostra; recolher é fora.
* Detecção automática de day trade. Sem carimbo de hora nas operações, não dá — e chutar
  aqui muda a alíquota.
* Come-cotas de fundos.
* Bem e direito para a declaração anual (a posição em 31/12 ao custo). É derivável e é
  candidato natural para logo depois da v1.

## 8. Como validar

* Venda de ação abaixo de R$ 20 mil no mês: isento, imposto zero, e a venda aparece.
* Venda de FII de R$ 6.200 com ganho de R$ 840: R$ 168,00. Sem isenção.
* Prejuízo acumulado maior que o ganho: base zero, e o prejuízo diminui do valor certo.
* Imposto abaixo de R$ 10: acumula para o mês seguinte em vez de gerar DARF.
* Alíquota mudada numa constante: um teste falha, apontando a linha.
