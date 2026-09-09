# 20 — Cotações

**Grupo:** Mercado · **Estado:** planejado · **Fase:** 2
**Depende de:** [02 — Provedores](02-dados-e-provedores.md)
**Precisa de:** `Need::Cotacao`
**Custo:** uma requisição em lote por volta, por provedor. Ver §5.

A watchlist: o que eu acompanho, esteja ou não na carteira.

---

## 1. O que responde

**O que os preços que me interessam estão fazendo agora.**

## 2. A watchlist é separada da carteira, e as duas se encontram aqui

A lista de acompanhados vive em `invest.json` (`"watchlist"`) e é independente das
posições. O que se acompanha e o que se possui são conjuntos diferentes — acompanha-se o
que se pensa em comprar, e o índice que serve de referência, e o dólar.

A tela mostra os dois conjuntos, marcados: um ativo que está na carteira ganha um sinal e
mostra o P&L junto; um que só é acompanhado mostra só o mercado. Toda posição entra
automaticamente, sem precisar ser adicionada duas vezes.

## 3. A tela

```
┌ Cotações ──────────────────────── 14 ativos · atualizado há 2 s ─┐
│  Ativo         Último       Var%      Dia      Volume    Gráfico │
│ ●BTCBRL      512.340,00    +2,14%   +10.740     1,2 B   ▂▃▅▄▆█▇  │
│ ●PETR4            38,42    manual·3d      —         —      —     │
│  IBOV        142.318,00    −0,42%     −601     8,4 B   ▇▆▅▆▄▃▂   │
│ ●HGLG11          161,05    manual·3d      —         —      —     │
│  USD/BRL           5,4210  +0,31%   +0,0168        —   ▃▄▅▄▅▆▇   │
├──────────────────────────────────────────────────────────────────┤
│ binance ok · awesomeapi ok · bcb ok · 7 ativos em preço manual   │
└─ ↑/↓ · Enter gráfico · Ctrl+A add · Del remover · Esc sair ──────┘
```

`●` marca o que está na carteira.

O mini-gráfico é a `Sparkline` que o programa já usa nos painéis da Visão Geral, com os
últimos pontos que o provedor deu. Um ativo em preço manual **não tem** mini-gráfico, e
mostra `—` em vez de uma linha reta: uma linha reta pareceria um preço estável, quando na
verdade é um preço não observado.

O rodapé é o estado dos provedores. É a linha que responde "por que este número não mexe" —
ver [02 §5](02-dados-e-provedores.md#5-cota-e-recuo).

## 4. As cores

Verde para alta, vermelho para baixa, cinza para sem variação — as três da paleta que o
programa já tem (`palette::GREEN`, `palette::RED`, `palette::DIM`). Nada de intensidade
variável por magnitude: a cor diz o sinal, o número diz o tamanho.

Um preço `manual` ou `não-oficial` **nunca é colorido**, mesmo tendo variação calculável
entre dois valores informados. Colorir daria a ele a mesma autoridade visual de um preço
ao vivo.

## 5. Cadência e custo

Uma requisição em lote por provedor por volta, nunca uma por ativo — a razão está em
[02 §2](02-dados-e-provedores.md#2-o-trait-provider): quase toda API cobra por requisição
e não por símbolo.

| Situação | Frequência |
| --- | --- |
| módulo aberto, algum mercado aberto | 2 s, ou o que a cota permitir |
| módulo aberto, todos fechados | 60 s |
| módulo fechado | nada |

## 6. Teclas

| Tecla | O que faz |
| --- | --- |
| ↑ / ↓ / `PgUp` / `PgDn` | anda |
| letras | busca |
| `Enter` | abre [22 — Gráfico](22-grafico.md) neste ativo |
| `Ctrl+A` | adiciona à watchlist |
| `Del` | remove da watchlist — nunca remove posição, e a confirmação diz isso |
| `Ctrl+S` | cicla a ordenação: manual, variação, volume, alfabética |
| `Esc` | limpa busca; depois sai |

`Del` numa linha que é posição remove só o acompanhamento explícito; a linha continua
aparecendo porque toda posição entra automaticamente. A confirmação explica isso, senão
parece que o `Del` não funcionou.

## 7. Erros e degradação

* Provedor caiu: os preços ficam, a idade cresce visivelmente, e o rodapé nomeia quem caiu.
* Cota estourada: o rodapé diz que recuou e por quanto tempo.
* Símbolo que nenhum provedor cobre: entra na lista com `sem provedor` no lugar do preço.
  Não é recusado na hora de adicionar — a fonte pode passar a existir.

## 8. Fica de fora na v1

* Book e times & trades — ver [28](28-book.md).
* Alerta na linha — é [42](42-alertas.md), e é acessível daqui com `Ctrl+P`.
* Ordenação persistida por sessão.

## 9. Como validar

* Adicionar um par de cripto: preço em segundos, mini-gráfico aparecendo.
* Cortar a rede: a idade cresce, nada vira zero, e o rodapé diz que o provedor caiu.
* Contar as requisições com `tcpdump` com 20 ativos: uma por provedor por volta. Não 20.
* Fechar o módulo: as requisições param. Nenhuma sobra.
