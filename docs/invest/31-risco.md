# 31 — Risco

**Grupo:** Análise · **Estado:** planejado · **Fase:** 3
**Depende de:** [13 — Patrimônio](13-patrimonio.md), [22 — Gráfico](22-grafico.md)
**Precisa de:** `Need::Posicoes`, `Need::Historico`
**Custo:** aritmética sobre séries já carregadas. Sem I/O próprio.

---

## 1. O que responde

**O quanto essa carteira balança, o quanto ela já caiu, e se o retorno pagou por isso.**

## 2. As medidas

| Medida | O que é | Cuidado |
| --- | --- | --- |
| Volatilidade | desvio padrão dos retornos, anualizado | anualizar é `× √252`, não `× 252` |
| Drawdown máximo | a maior queda do topo até o fundo | sobre a curva de [13](13-patrimonio.md), não sobre preço |
| Drawdown atual | quanto abaixo do topo histórico se está agora | o número que dói |
| Sharpe | retorno acima do CDI, dividido pela volatilidade | o "sem risco" é o CDI de [26](26-renda-fixa.md), não zero |
| Beta | sensibilidade a um índice | precisa de índice com série; na v1 quase sempre falta |
| VaR histórico 95% | a perda que só é superada em 5% dos dias | histórico, não paramétrico — ver §3 |

## 3. VaR histórico, e por que não o paramétrico

O VaR paramétrico supõe que os retornos são normais. Não são: eles têm cauda gorda, e é
exatamente na cauda que o VaR vive. Supor normalidade num número que existe para medir o
extremo é errar onde importa.

O histórico é o percentil 5 dos retornos observados. É mais simples de calcular, mais
simples de explicar, e não supõe nada. O preço é precisar de série suficiente — e a tela
diz quantos dias tem e a partir de quantos o número começa a valer.

## 4. A honestidade obrigatória deste módulo

Este é o módulo com maior chance de produzir um número bonito e errado. Três regras:

1. **Toda medida mostra o tamanho da amostra.** "Sharpe 1,42" não quer dizer nada; "Sharpe
   1,42 · 68 dias" já avisa que é pouco.
2. **Abaixo de um mínimo, a medida não aparece.** Sharpe com 20 dias de série é ruído com
   duas casas decimais. Aparece "faltam N dias", que é informação verdadeira.
3. **Nenhuma medida é interpretada pelo programa.** Nada de "risco alto" ou de semáforo. O
   programa mostra números e o que eles são; classificar é o trabalho de quem investe.

## 5. A tela

```
┌ Risco ────────────────────────── 12 meses · 248 pregões ─┐
│ Volatilidade (a.a.)     18,4%                            │
│ Drawdown máximo        −22,1%   (14/03 a 02/05)          │
│ Drawdown atual          −4,2%                            │
│ Sharpe                   0,84   (CDI 10,40% a.a.)        │
│ Beta vs IBOV                —   (índice sem série)       │
│ VaR 95% (1 dia)        −1,92%   ≈ R$ 2.847,40            │
├───────────────────────────────────────────────────────────┤
│ curva e afundamento                                       │
│  ▁▂▃▄▅▆▇█▇▆▅▄▅▆▇█▇█                                       │
│  ▔▔▔▔▔▔▔▔▔▔▂▄▆▃▁▔▔▔▔  ← drawdown                          │
├───────────────────────────────────────────────────────────┤
│ maior contribuição para a volatilidade: BTCBRL (61%)      │
└─ ←/→ período · a por ativo · Esc sair ───────────────────┘
```

O VaR aparece em percentual **e em reais**. Um VaR percentual é abstrato; "R$ 2.847,40" é a
frase que a pessoa entende.

A linha de contribuição para a volatilidade é o que transforma o módulo em ação: saber que
a carteira balança 18% é uma coisa, saber que 61% disso vem de um ativo é outra.

## 6. Teclas

| Tecla | O que faz |
| --- | --- |
| ← / → | período (3m, 6m, 12m, tudo) |
| `a` | alterna entre carteira e por ativo |
| `Enter` | abre o ativo selecionado no [gráfico](22-grafico.md) |
| `Esc` | sai |

## 7. Erros e degradação

* Série curta: cada medida diz quantos dias faltam. Nenhuma é estimada.
* Sem índice com série: beta mostra `—` e diz por quê. Na v1 esse é o caso normal, porque
  IBOV é manual — ver [24 §2](24-indices-e-macro.md#2-o-que-entra).
* Ativo sem histórico: fica de fora, e a tela diz quanto do patrimônio ficou de fora. Um
  risco calculado sobre 60% da carteira e apresentado como o risco da carteira seria a
  falha mais grave que este módulo poderia ter.

## 8. Fica de fora na v1

* Fronteira eficiente e otimização de carteira. É recomendação, e este programa não
  recomenda.
* Stress test por cenário. Precisa de correlações estimadas, que precisam de mais série
  do que se terá no começo.
* TWR e MWR. Entram junto com [15 — Lançamentos](15-lancamentos.md) em uso pleno.

## 9. Como validar

* Volatilidade de um ativo conhecido bate com fonte externa, na mesma janela.
* Drawdown máximo confere com a maior queda visível na curva.
* Carteira com um ativo sem histórico: a tela diz que N% ficou de fora.
* Série de 20 dias: nenhuma medida aparece; todas dizem quantos dias faltam.
