# 28 — Book e negócios

**Grupo:** Mercado · **Estado:** planejado · **Fase:** depois do WebSocket
**Depende de:** [02 §7 — WebSocket](02-dados-e-provedores.md#7-websocket--a-fase-que-não-é-esta)
**Precisa de:** `Need::Provedor("book")`
**Custo:** o mais alto da aba — um stream contínuo por ativo.

---

## 1. O que responde

**Quem está querendo comprar e vender agora, a que preço, e o que acabou de ser
negociado.**

## 2. Por que ele está listado se não vai ser construído tão cedo

Por duas razões, e as duas são de projeto e não de escopo:

1. **Ele é o único módulo que exige infraestrutura que não existe.** Book por polling não
   é book: é um retrato de dois em dois segundos de uma coisa que muda dezenas de vezes por
   segundo. Ele precisa de WebSocket, e é o módulo que justifica construir o WebSocket.
2. **Ele delimita o que a aba não é.** É a fronteira entre "terminal de acompanhamento" e
   "terminal de operação". Deixá-lo explícito e adiado é mais honesto do que deixá-lo de
   fora e responder de improviso quando alguém perguntar.

## 3. Onde ele é possível

| Mercado | Book sem chave? |
| --- | --- |
| Cripto (Binance) | **sim** — profundidade e trades são públicos, via WebSocket |
| B3 | não. Dado de nível 2 é vendido |
| US | não, na prática |

Ou seja: quando existir, ele existe **só para cripto**. Isso não é uma limitação a
contornar; é o que o mercado vende de graça e o que não vende.

## 4. A tela, quando existir

```
┌ BTCBRL · book ───────────────────────── binance · stream ─┐
│      Compra              │              Venda             │
│  qtd      preço          │    preço       qtd             │
│ 0,842  512.310  ████     │  512.350  ███  0,610           │
│ 1,204  512.290  ██████   │  512.380  ██   0,402           │
│ …                        │  …                             │
├───────────────────────────────────────────────────────────┤
│ Negócios                                                  │
│ 14:32:07  512.340   0,0120  C                             │
│ 14:32:07  512.330   0,0044  V                             │
├───────────────────────────────────────────────────────────┤
│ spread R$ 40,00 (0,008%)  ·  desequilíbrio +18% compra    │
└─ Ctrl+D profundidade · Esc sair ─────────────────────────┘
```

Os negócios são um log que cresce, e o programa já tem exatamente essa tela: o
`ToolMonitorFocus` da aba Ferramentas, com busca digitada direto, rolagem, e `End` para
voltar a acompanhar a ponta. Reaproveitar aquele desenho, em vez de inventar outro, é o
que mantém a promessa de não sair do padrão.

## 5. O que precisa existir antes

1. Enquadramento WebSocket (RFC 6455) — ver [02 §7](02-dados-e-provedores.md#7-websocket--a-fase-que-não-é-esta).
2. Reconexão com recuo, porque um stream cai.
3. Um `Pane` novo para o book, com as duas metades e as barras de profundidade. É um caso
   legítimo de crescer o vocabulário de desenho, com o caso concreto na mão — que é
   exatamente como [00 §7.1](00-arquitetura.md#71-o-módulo-não-desenha) diz que ele deve
   crescer.

## 6. O custo, e por que ele é diferente

Este é o único módulo em que fechar a tela **tem** que matar o stream imediatamente, e não
só parar de desenhar. Um stream de book manda milhares de mensagens por minuto; deixá-lo
vivo em segundo plano seria a violação mais cara possível da regra 3 de
[00 §3](00-arquitetura.md#3-custo-zero-fora-de-foco--as-regras-duras).

Por isso, e só por isso, este módulo **nunca poderá ser uma widget favorita**.

## 7. Como validar, quando existir

* Fechar o módulo: o socket fecha. Confere com `ss`, não com boa vontade.
* Derrubar a rede: reconecta com recuo, e a tela diz que está reconectando.
* Deixar rodando dez minutos: a memória não cresce — o log tem teto, como o das ferramentas.
