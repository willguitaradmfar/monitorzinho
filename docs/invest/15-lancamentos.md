# 15 — Lançamentos

**Grupo:** Carteira · **Estado:** planejado · **Fase:** 4
**Depende de:** [10 — Posições](10-posicoes.md)
**Precisa de:** nada
**Custo:** leitura de arquivo próprio. Sem rede.

---

## 1. O que responde

**O que aconteceu na carteira ao longo do tempo.**

E, com igual importância, o que ele **não** responde: qual é o preço médio.

## 2. A regra que define o módulo

Este módulo é **histórico opcional e nunca escreve no preço médio**. Está em
[03 §1.1](03-armazenamento.md#11-o-preço-médio-é-informado-nunca-calculado) e é a coisa
mais importante deste arquivo.

O motivo é o uso real: a carteira vem de várias origens, cada uma já traz o PM dela, e o
histórico que produziu esse PM em geral não vem junto. Um módulo de lançamentos com
histórico parcial que recalculasse o PM produziria um número errado e apagaria o certo.

Então a relação entre os dois módulos é de mão única:

```
Lançamentos  ──lê──▶  Posições        (para saber que ativos existem)
Lançamentos  ──✗──▶  preço médio      (nunca)
```

## 3. Para que serve, então

Quatro coisas, todas reais:

1. **Ver o que aconteceu.** Quando comprei, a que preço, quanto paguei de corretagem.
2. **Alimentar a decomposição do [Patrimônio](13-patrimonio.md).** Sem lançamentos, um
   aporte aparece como valorização — a mentira mais cara daquele módulo.
3. **Alimentar os [Proventos](14-proventos.md)**, que são um tipo de lançamento.
4. **Ser o caminho para o dia em que o [imposto](50-imposto-de-renda.md) precisar de custo
   por operação.** Aí o histórico completo passa a existir como caminho paralelo — e
   continua sem escrever no PM.

## 4. Os tipos

| Tipo | Campos além dos comuns | Efeito |
| --- | --- | --- |
| compra | quantidade, preço, taxas | aporte em [13](13-patrimonio.md) |
| venda | quantidade, preço, taxas | retirada em [13](13-patrimonio.md), e uma estimativa de ganho |
| provento | ver [14](14-proventos.md) | proventos em [13](13-patrimonio.md) |
| aporte | valor | dinheiro que entrou na conta, sem comprar nada |
| retirada | valor | dinheiro que saiu |
| ajuste | descrição livre | split, bonificação, o que a vida trouxer |

`ajuste` existe porque eventos corporativos não têm fonte sem chave, e a alternativa a um
tipo genérico seria a pessoa não ter como registrar que houve um desdobramento.

## 5. A tela

Uma tabela em ordem cronológica inversa, com totais no rodapé. Nada de especial —
deliberadamente: é um livro-caixa, e um livro-caixa que tenta ser interessante é um
livro-caixa difícil de conferir.

```
┌ Lançamentos ─────────────────────── 148 registros · 2024–2026 ─┐
│ Data       Tipo      Ativo    Qtd     Preço      Taxas   Total │
│ 05/09/26   provento  HGLG11     —         —       0,00  132,00 │
│ 28/08/26   compra    PETR4     100     38,10      4,90 3.814,90│
│ 20/08/26   aporte      —         —         —          — 3.000,00│
│ …                                                              │
├────────────────────────────────────────────────────────────────┤
│ 2026:  aportes 42.000,00 · retiradas 0,00 · proventos 3.418,00 │
└─ ↑/↓ · Ctrl+A adicionar · Enter editar · Del remover · Esc ────┘
```

Um aviso fixo no rodapé, e ele não sai nunca:

> Preço médio não é calculado daqui. Ele é informado em Posições.

Isso não é redundante com a documentação. É a tela onde a expectativa contrária nasce, e
é onde o aviso precisa estar.

## 6. Teclas

| Tecla | O que faz |
| --- | --- |
| ↑ / ↓ / `PgUp` / `PgDn` | anda |
| letras | busca |
| `Ctrl+A` | adiciona |
| `Enter` | edita |
| `Del` | remove, com confirmação destrutiva |
| `Ctrl+F` | filtra por tipo |
| `Esc` | limpa busca; depois sai |

## 7. Armazenamento

Tabela própria, `lancamento`, com as mesmas garantias de [03](03-armazenamento.md):
gravação em transação, no banco do perfil. É um registro que só cresce, e ele é uma tabela
separada de `posicao` — o que antes custava um arquivo grande reescrito a cada edição hoje
não custa nada, porque o lançamento e a posição não se tocam.

E, sendo tabela, o histórico passou a responder a pergunta que ele existe para responder:

```sql
SELECT strftime('%Y-%m', em, 'unixepoch') AS mes, sum(valor)
FROM lancamento WHERE tipo = 'aporte' GROUP BY mes;
```

## 8. Fica de fora na v1

* Importar nota de corretagem em PDF. Ver [51](51-importacao.md) — CSV e OFX primeiro.
* Conferência cruzada entre lançamentos e posições ("seus lançamentos dizem 300 ações, sua
  posição diz 400"). É útil, é fácil de fazer errado com histórico parcial, e vira aviso e
  não correção quando entrar.

## 9. Como validar

* Adicionar uma compra: o PM da posição correspondente **não muda**. Este é o teste.
* Registrar um aporte e abrir [Patrimônio](13-patrimonio.md): aparece como aporte, não
  como valorização.
* Apagar o arquivo de lançamentos: as posições continuam intactas.
