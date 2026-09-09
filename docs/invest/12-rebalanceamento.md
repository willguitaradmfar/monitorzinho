# 12 — Rebalanceamento

**Grupo:** Carteira · **Estado:** planejado · **Fase:** 4
**Depende de:** [11 — Alocação](11-alocacao.md)
**Precisa de:** `Need::Posicoes`, `Need::Cotacao`, alvos definidos
**Custo:** aritmética sobre dezenas de linhas. Sem I/O.

---

## 1. O que responde

**Quanto comprar e quanto vender para voltar ao alvo — e, se eu vou aportar, onde
colocar o dinheiro.**

## 2. Os dois modos, e por que são dois

| Modo | Pergunta | Como resolve |
| --- | --- | --- |
| **Aporte** | tenho R$ 3.000 para pôr; onde? | só compra. Distribui o aporte nas fatias mais abaixo do alvo |
| **Ajuste** | quero voltar ao alvo agora | compra e vende |

O modo de aporte é o padrão, e é o que se usa quase sempre. A razão é fiscal e não
estética: no Brasil vender realiza ganho e pode gerar imposto, enquanto comprar não gera
nada. Um rebalanceamento que sai vendendo por padrão é um rebalanceamento que custa
dinheiro sem avisar.

O modo de ajuste **mostra o imposto estimado** de cada venda que ele sugere, vindo de
[50](50-imposto-de-renda.md), com a mesma ressalva de estimativa que aquele módulo carrega.

## 3. A tela

```
┌ Rebalanceamento · aporte de R$ 3.000,00 ───────────────────────────┐
│ Classe        Real    Alvo   Depois    Aplicar                     │
│ renda fixa   23,5%   30,0%   25,4%   R$ 1.850,00                   │
│ FII          18,1%   20,0%   18,8%   R$   750,00                   │
│ ação         44,2%   40,0%   43,3%   R$   400,00                   │
│ cripto       14,2%   10,0%   13,9%          —                      │
├────────────────────────────────────────────────────────────────────┤
│ desvio 8,3 → 5,1 p.p.  ·  nada vendido  ·  R$ 0,00 de imposto      │
└─ ↑/↓ · +/− aporte · m modo · Enter detalhar em ativos · Esc sair ──┘
```

Duas coisas que a tela sempre mostra e que a maioria das ferramentas esconde:

* **`Depois`** — onde a alocação fica de fato. Um aporte pequeno numa carteira desbalanceada
  não conserta nada, e ver "23,5% → 25,4%" é mais honesto que ver a instrução sozinha.
* **O desvio antes e depois.** Se o aporte quase não mexe no desvio, isso aparece.

## 4. Como distribui

Em ordem de "mais abaixo do alvo, em reais e não em pontos percentuais". Uma classe 5 p.p.
abaixo numa carteira de R$ 500 mil precisa de mais dinheiro que uma classe 5 p.p. abaixo
numa de R$ 50 mil, e distribuir por pontos percentuais ignora isso.

O aporte acaba antes de fechar todos os buracos, quase sempre. Aí ele para, e as classes
que não receberam mostram `—` em vez de zero: `—` é "não coube nesta rodada", `0,00` seria
"a conta deu zero".

## 5. Descer para ativos

`Enter` numa classe mostra **quais ativos** comprar dentro dela. Isso precisa de um critério
que o programa não tem — o programa não sabe se você prefere reforçar o que já tem ou
entrar em coisa nova. Então na v1 ele oferece o único critério que é derivável dos dados e
não é palpite: **proporcional ao peso atual dentro da classe**, mantendo a composição
interna. Fica escrito na tela que é isso que ele está fazendo.

## 6. Teclas

| Tecla | O que faz |
| --- | --- |
| `+` / `−` | aumenta e diminui o aporte |
| dígitos | digita o valor do aporte direto |
| `m` | alterna aporte ↔ ajuste |
| ↑ / ↓ | anda |
| `Enter` | desce para os ativos daquela classe |
| `Esc` | sai |

Aqui as letras **não** são busca — a tela é uma lista curta de classes, sem nada a
procurar. Por isso `m` pode ser letra pura, ao contrário de [Posições](10-posicoes.md#6-teclas).
A regra geral: onde há busca digitada direto, ação é `Ctrl+`; onde não há, letra basta.

## 7. Erros e degradação

* Sem alvos: `Pane::Empty` mandando para [Alocação](11-alocacao.md) definir.
* Alvos que não somam 100%: recusa a calcular, e diz por quê. Aqui, diferente de
  [Alocação](11-alocacao.md), não dá para só mostrar — a conta depende da soma fechar.
* Ativo sem preço: fica fora, e a tela diz quantos, porque o total muda a distribuição toda.

## 8. Fica de fora na v1

* Lote mínimo e fracionário da B3 — exige regra por ativo que nenhuma fonte sem chave dá.
  Os valores saem em reais, não em quantidade de papéis.
* Custo de corretagem na conta.
* Otimização com restrição (não vender X, não passar de Y). O critério é o simples e é dito.

## 9. Como validar

* Aporte de zero: nada a fazer, e a tela diz isso em vez de mostrar uma tabela de zeros.
* Aporte gigante: todas as classes chegam ao alvo, e `Depois` mostra os alvos exatos.
* Modo ajuste numa carteira já no alvo: nenhuma operação sugerida.
* A soma do que ele manda aplicar é exatamente o aporte, ao centavo.
