# 27 — Cripto

**Grupo:** Mercado · **Estado:** planejado · **Fase:** 2
**Depende de:** [02 — Provedores](02-dados-e-provedores.md)
**Precisa de:** nada
**Custo:** uma requisição em lote por volta. É a única fonte com tempo real de graça.

---

## 1. Por que este módulo é diferente de todos os outros

É o **único mercado da aba com dado profissional, ao vivo, gratuito e sem chave**. A
Binance dá preço, volume, profundidade e histórico por API pública.

Consequência prática: tudo que os outros módulos de mercado **não** conseguem fazer na v1
por falta de fonte, este consegue. Ele é também onde se testa de verdade a arquitetura de
provedores, porque é o único que exercita o caminho completo.

## 2. O que ele mostra além do preço

Preço e variação já estão em [20 — Cotações](20-cotacoes.md). Este módulo existe para o
que é específico de cripto:

| Coisa | O que responde |
| --- | --- |
| Par em BRL vs. via USDT | se o preço é direto ou tem duas conversões embutidas |
| Volume 24 h | liquidez — se dá para sair da posição |
| Dominância do BTC | se o movimento é do bitcoin ou do mercado |
| Funding rate | o custo de manter posição alavancada; um termômetro de posicionamento |
| Máxima/mínima 24 h | a amplitude do dia, que em cripto é o número que assusta |

Funding e dominância são de mercados/endpoints diferentes do de preço à vista. Se algum
deles não estiver disponível sem chave, a linha some com uma nota — nunca é estimada.

## 3. BRL direto, e por que isso importa aqui

O patrimônio é em BRL. A Binance tem pares em BRL diretos (`BTCBRL`), e usá-los evita a
dupla conversão `BTC → USDT → BRL`, que carrega dois spreads e o erro do câmbio.

Onde o par em BRL não existe ou é ilíquido, converte-se via USDT e o câmbio de
[25](25-cambio.md) — e **a tela diz que foi convertido**, com o caminho:
`BTC → USDT → BRL`. Um preço com duas conversões escondidas é um preço em que se confia
demais.

## 4. A tela

```
┌ Cripto ───────────────────────────────── binance · há 2 s ─┐
│ Par         Último        24h      Máx24h    Mín24h   Vol  │
│ BTCBRL    512.340,00   +2,14%   516.800   498.210   1,2 B  │
│ ETHBRL     18.442,00   +1,08%    18.690    18.010   340 M  │
│ SOLUSDT       184,20   −0,42%    ...       ...             │
│              (via USDT → BRL: R$ 998,55)                    │
├─────────────────────────────────────────────────────────────┤
│ dominância BTC 54,1%  ·  funding BTC perp +0,0081% / 8h    │
│ sua posição: R$ 21.108,41 · 14,2% da carteira              │
└─ ↑/↓ · Enter gráfico · Ctrl+A par · Esc sair ──────────────┘
```

## 5. Cadência

2 s enquanto o módulo está aberto — o mercado nunca fecha, então não há o modo de 60 s dos
outros. É o único provedor da aba em que `is_open()` devolve sempre `true`.

O limite da Binance é por **peso** e a resposta diz quanto já se gastou, no cabeçalho
`X-MBX-USED-WEIGHT-1m`. O limitador lê isso em vez de adivinhar — ver
[02 §5](02-dados-e-provedores.md#5-cota-e-recuo). É a única fonte da v1 que se auto-reporta,
e por isso a mais fácil de respeitar sem chutar.

## 6. Teclas

| Tecla | O que faz |
| --- | --- |
| ↑ / ↓ | anda |
| letras | busca |
| `Enter` | [22 — Gráfico](22-grafico.md) |
| `Ctrl+A` | adiciona par |
| `Esc` | limpa busca; depois sai |

## 7. Fica de fora na v1

* Book e trades ao vivo — precisam de WebSocket, que é fase própria. Ver
  [28](28-book.md) e [02 §7](02-dados-e-provedores.md#7-websocket--a-fase-que-não-é-esta).
* Dado on-chain (hashrate, saldo de exchange, taxas de rede). Outras fontes, outro escopo.
* Saldo em carteira própria por endereço. Isso é ler blockchain, e é um projeto.

## 8. Como validar

* Preço confere com a Binance no navegador, no mesmo instante.
* Um par sem BRL direto: a tela mostra o caminho da conversão.
* Deixar rodando uma hora: o peso usado nunca chega perto do teto, e o cabeçalho confirma.
* Fechar o módulo: as requisições param na hora.
