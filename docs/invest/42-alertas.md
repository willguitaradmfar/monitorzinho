# 42 — Alertas

**Grupo:** Informação · **Estado:** planejado · **Fase:** 4
**Depende de:** [20 — Cotações](20-cotacoes.md)
**Precisa de:** `Need::Cotacao`
**Custo:** o único módulo autorizado a rodar com a aba fechada. Ver §4.

---

## 1. O que responde

**Me avise quando acontecer o que eu estou esperando.**

## 2. Por que este módulo se parece com uma ferramenta

Porque é uma. A aba Ferramentas já tem exatamente esta forma: coisas que o usuário cria,
que rodam em segundo plano, que sobrevivem à troca de aba, que voltam vivas depois de
reiniciar o programa, e que se ligam e desligam com espaço.

Um alerta é uma `Execution` com outro nome. E isso não é uma analogia — é uma instrução de
implementação: reaproveitar `tools::Execution`, `Recorder`, `EventLog` e
`tools::persist::ExecutionSpec` em vez de escrever um segundo mecanismo com a mesma forma
e comportamento levemente diferente.

## 3. As regras

| Tipo | Dispara quando | Parâmetros |
| --- | --- | --- |
| Preço | cruza um valor | ativo, acima/abaixo, valor |
| Variação | varia mais que X% | ativo, percentual, janela (dia, hora) |
| Indicador | RSI, média, cruzamento | ativo, indicador, condição — precisa de [30](30-indicadores.md) |
| Alocação | uma classe passa de X p.p. do alvo | classe, desvio |
| Câmbio | USD/BRL cruza um valor | par, valor |

Cada regra dispara **uma vez** e fica armada de novo só depois de a condição deixar de
valer. Um alerta de "BTC acima de 500 mil" que dispara a cada volta enquanto o preço fica
acima é um alerta que se aprende a ignorar em dez minutos.

## 4. Rodar com a aba fechada — a exceção, e as condições dela

Um alerta que só funciona com a aba Invest na frente é inútil. Então este é o módulo que
justifica a exceção da regra 3 de
[00 §3](00-arquitetura.md#3-custo-zero-fora-de-foco--as-regras-duras).

As condições, todas obrigatórias:

1. **Cada alerta é ligado explicitamente**, um a um. Nada roda em segundo plano por padrão.
2. **A tela diz o custo.** A lista de alertas mostra quantas requisições por minuto o
   conjunto ligado está gerando, e para quais provedores.
3. **A cadência de fundo é mais lenta**: 60 s em vez de 2 s. Um alerta de preço não precisa
   da mesma resolução de uma tela que se está olhando.
4. **Os ativos são agrupados numa requisição só**, como em [20 §5](20-cotacoes.md#5-cadência-e-custo).
   Dez alertas sobre dez ativos do mesmo provedor são uma requisição, não dez.
5. **Nada de alerta sobre fonte manual.** Um preço que só muda quando o usuário o digita
   não tem o que disparar, e o formulário recusa isso na hora — com a caixa aberta, como
   `Tool::start` faz.

## 5. Como o alerta aparece

Dentro do programa, sempre. Três lugares, do mais discreto ao mais visível:

* **Na barra de abas**, um contador ao lado de `Invest` — o mesmo lugar onde uma aba diria
  que tem novidade.
* **Na lista de módulos**, a linha de Alertas com o disparo mais recente no resumo.
* **No módulo**, o log completo, com o histórico.

O que ele **não** faz: não interrompe a tela, não abre caixa por cima do que se está
fazendo, e não emite som. Um monitor que sequestra a tela é um monitor que se fecha.

Notificação do sistema operacional fica de fora da v1 — é dependência nova e é uma decisão
de outra natureza.

## 6. A tela

```
┌ Alertas ─────────────────── 6 regras · 4 ligadas · 1/min ─┐
│  Estado  Regra                          Último disparo    │
│  ●       BTCBRL acima de 520.000              —           │
│  ●       PETR4 varia mais de 3% no dia   ontem 11:04      │
│  ○       USD/BRL abaixo de 5,20          12/08 09:31      │
│  ●       cripto passa de 15% da carteira      —           │
├────────────────────────────────────────────────────────────┤
│ 14:22  PETR4 variou −3,2% no dia (38,42 → 37,19)          │
│ 11:04  PETR4 variou +3,4% no dia                          │
├────────────────────────────────────────────────────────────┤
│ ligadas: 4 · 1 requisição/min para binance, awesomeapi     │
└─ espaço ligar/desligar · Ctrl+A add · Del · Enter log ────┘
```

`●` ligado, `○` desligado — e espaço alterna, que é o mesmo gesto da aba Ferramentas.

## 7. Persistência

a tabela `alerta`, com a mesma ideia de `ExecutionSpec`: qual regra, com que valor, e se
está ligada. Desligado continua desligado depois de reiniciar — a mesma decisão que
`tools::persist` documenta, e pela mesma razão: voltar fazendo o que alguém desligou é o
oposto do que foi pedido.

## 8. Como validar

* Criar um alerta de preço já satisfeito: dispara uma vez, e não fica repetindo.
* Desligar, reiniciar o programa: continua desligado.
* Quatro alertas em quatro ativos do mesmo provedor: uma requisição por volta, confirmada
  com `tcpdump`.
* Sair da aba com alertas ligados: eles continuam. Com todos desligados: nenhuma requisição
  sai, nenhuma thread fica.
* Tentar criar alerta sobre um ativo manual: recusado na caixa, com a explicação.
