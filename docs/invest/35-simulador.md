# 35 — Simulador

**Grupo:** Análise · **Estado:** planejado · **Fase:** 4
**Depende de:** [13 — Patrimônio](13-patrimonio.md), [26 — Renda fixa](26-renda-fixa.md)
**Precisa de:** `Need::Posicoes`
**Custo:** aritmética. Sem I/O.

---

## 1. O que responde

Duas perguntas separadas, que compartilham a mesma tela e a mesma advertência:

1. **Projeção:** se eu aportar X por mês a Y% ao ano, onde chego em Z anos?
2. **Backtest:** se eu tivesse feito isso nos últimos N anos, onde teria chegado?

## 2. A advertência, e por que ela é permanente na tela

Uma projeção é uma conta, não uma previsão. A conta está certa; a suposição de que o
retorno futuro é Y% é do usuário, e é a parte que decide o resultado.

Então a tela mostra **três cenários sempre**, nunca um só:

```
pessimista   CDI − 2 p.p.
base         o que você informou
otimista     o que você informou + 4 p.p.
```

Três linhas no gráfico e três números no rodapé. Um número só, por mais bem explicado que
esteja, é lido como promessa; três números lado a lado são lidos como faixa, que é o que
de fato são.

## 3. A projeção

```
valor(n) = patrimônio_atual × (1+r)ⁿ + aporte × [((1+r)ⁿ − 1) / r]
```

com `r` mensal e `n` em meses. Duas coisas que a tela explicita porque quase toda
calculadora esconde:

* **Se o retorno informado é real ou nominal.** Uma projeção nominal de 10% ao ano com
  IPCA de 4,5% cresce muito menos do que parece. O campo tem essa escolha, e a tela
  mostra o resultado nas duas formas.
* **Imposto na retirada.** A projeção bruta é o número grande; o líquido é o que se recebe.
  A conta usa a tabela regressiva de [26 §5](26-renda-fixa.md#5-a-calculadora) para a parte
  de renda fixa e 15% para a de renda variável, e diz que é uma aproximação grosseira.

## 4. O backtest

Simples de propósito: aporte fixo mensal num ativo ou numa combinação de ativos, sobre a
série histórica que os provedores derem.

**Não é um framework de estratégia.** Não há regra de entrada e saída, não há stop, não há
otimização de parâmetro. Um backtest de estratégia mal feito é pior que nenhum, porque
produz um número que parece evidência — e fazer um bem feito é um projeto próprio, com
custo de transação, escorregamento e ajuste para sobrevivência de amostra.

O que ele responde é a pergunta honesta e útil: *"aporte constante nisto, desde então, daria
o quê?"* — e ela se compara direto com a linha do CDI.

## 5. A tela

```
┌ Simulador · projeção ──────────────── 10 anos · R$ 2.000/mês ─┐
│                                                    ╭── otim.  │
│                                            ▁▃▄▆█▇▆╱           │
│                              ▁▂▃▄▅▆▇█▇▆▅▄▅╱  ╰── base         │
│               ▁▂▃▄▅▆▇█▇▆▅▄▃▄╱                ╰── pess.        │
├────────────────────────────────────────────────────────────────┤
│ pessimista  R$   412.800   ·  real R$ 268.400                 │
│ base        R$   561.200   ·  real R$ 364.900                 │
│ otimista    R$   784.100   ·  real R$ 509.800                 │
│ aportado    R$   240.000                                       │
├────────────────────────────────────────────────────────────────┤
│ conta, não previsão. o retorno futuro é sua suposição.        │
└─ ↑/↓ campo · m projeção/backtest · Esc sair ──────────────────┘
```

A linha **`aportado`** é obrigatória: ela separa o que veio de dinheiro colocado do que
veio de rendimento, que é a mesma decomposição que [13](13-patrimonio.md) faz com o passado.

O aviso do rodapé é fixo e não sai.

## 6. Teclas

| Tecla | O que faz |
| --- | --- |
| ↑ / ↓ | campo |
| dígitos | edita o campo |
| `m` | alterna projeção ↔ backtest |
| `Esc` | sai |

## 7. Fica de fora na v1

* Simulação de Monte Carlo. Daria uma distribuição em vez de três cenários, o que é
  melhor — e é fácil de apresentar de um jeito que parece mais certeza do que é.
* Estratégia com regras. Ver §4.
* Previdência, come-cotas, contribuição patronal.

## 8. Como validar

* Retorno zero: o valor final é exatamente `patrimônio + aporte × meses`.
* Uma taxa e um prazo conhecidos: bate com uma calculadora de juro composto, ao centavo.
* Backtest de aporte mensal em CDI: bate com o CDI acumulado do período.
