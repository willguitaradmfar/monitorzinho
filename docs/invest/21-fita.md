# 21 — Fita

**Grupo:** Mercado · **Estado:** planejado · **Fase:** 2
**Depende de:** [20 — Cotações](20-cotacoes.md)
**Precisa de:** `Need::Cotacao`
**Custo:** zero próprio — lê o mesmo retrato que [20](20-cotacoes.md).

A faixa de uma linha no topo da aba Invest.

---

## 1. O que responde

**O mínimo que eu quero ver sem entrar em nada.**

## 2. O que ela é, exatamente

Uma linha, no topo da aba, acima da widget de módulos:

```
IBOV 142.318 −0,42% · USD/BRL 5,4210 +0,31% · BTC 512.340 +2,14% · CDI 10,40% · S&P 6.284 +0,18%
```

Não rola nem anima. Um terminal não é um telão de corretora, e texto que se move é texto
que não se lê — além de forçar um redesenho constante, que é exatamente o que o laço
principal foi desenhado para evitar (ele só redesenha quando algo muda). Se não couber na
largura, corta com `…` e a lista é reordenável.

## 3. Por que é módulo se não tem tela

Porque tem configuração — o que aparece nela — e porque essa configuração precisa de um
lugar. Abrir a Fita abre o editor da lista.

É o primeiro caso da aba de um módulo cuja tela cheia é só de configuração. Isso é
aceitável e é a exceção; um segundo caso desses seria sinal de que falta uma tela de
preferências na aba.

## 4. Onde ela vive no layout

`render_invest_tab` reserva `Constraint::Length(1)` no topo quando a fita tem conteúdo, e
zero quando não tem. Vazia, ela não ocupa linha nenhuma — nada de faixa em branco.

**A fita só aparece na tela principal da aba Invest.** Não aparece nas outras abas, e não
aparece dentro de um módulo: um módulo é tela cheia, e uma faixa persistente por cima dele
seria o programa se recusando a sair da frente.

## 5. O que ela custa quando a aba está fora de foco

Nada, na v1. Ela lê o retrato que [20](20-cotacoes.md) mantém, e esse retrato só é
alimentado enquanto a aba está visível — regra 3 de [00 §3](00-arquitetura.md#3-custo-zero-fora-de-foco--as-regras-duras).

Quando as widgets favoritas existirem, a fita será a primeira candidata a rodar com a aba
fechada, e aí ela vira uma escolha explícita do usuário, com o custo dito.

## 6. Teclas (na tela de configuração)

| Tecla | O que faz |
| --- | --- |
| ↑ / ↓ | anda |
| `Ctrl+A` | adiciona um ativo |
| `Del` | remove |
| `Ctrl+↑` / `Ctrl+↓` | reordena — a ordem é o que decide quem sobrevive ao corte |
| `Esc` | sai |

## 7. Como validar

* Fita vazia: a tela principal não tem linha em branco no topo.
* Terminal estreito: corta com `…`, sem quebrar o layout.
* Entrar num módulo: a fita some.
* Preço mudando: só a fita repinta, não a aba inteira.
