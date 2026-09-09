# 10 — Posições

**Grupo:** Carteira · **Estado:** planejado · **Fase:** 1
**Depende de:** [03 — Armazenamento](03-armazenamento.md), [02 — Provedores](02-dados-e-provedores.md)
**Precisa de:** nada — é o módulo que cria a pré-condição de todos os outros
**Custo:** uma leitura do retrato de cotações por tick. Sem I/O próprio.

O módulo central. Tudo em `docs/invest/` ou lê daqui, ou existe para alimentar isto.

---

## 1. O que responde

**O que eu tenho, quanto custou, quanto vale agora, e quanto disso é lucro.**

## 2. A regra que define o módulo

O preço médio é **informado**, nunca calculado. Está detalhado em
[03 §1.1](03-armazenamento.md#11-o-preço-médio-é-informado-nunca-calculado) e é a razão de
este módulo ser o dono do dado em vez de uma vista sobre lançamentos.

O que o programa calcula, e só isso:

| Coluna | Conta |
| --- | --- |
| Valor de mercado | `qtd × preço_atual`, convertido para BRL |
| Custo | `qtd × preço_médio`, convertido para BRL |
| P&L aberto | `mercado − custo` |
| P&L % | `p&l / custo` |
| P&L do dia | `qtd × (preço_atual − fechamento_anterior)` |
| Peso | `mercado / total_da_carteira` |

`preço_médio` não aparece nessa tabela, e é o ponto.

## 3. Os dados

Vêm de `invest.json` (posições) e do retrato de cotações do `ProviderSet`. Nada mais.

A identidade de uma linha é `(fonte, conta, ativo)` — o mesmo ativo em duas corretoras dá
duas linhas, e as duas ficam visíveis. Consolidar apagaria de onde veio o número.

### Preço atual, e de onde ele veio

Cada linha mostra a **origem** e a **idade** do preço, vindas de `Provider::grade()`:

| O que a linha mostra | O que significa |
| --- | --- |
| *(nada)* | ao vivo, agora |
| `15 min` | fonte oficial, com atraso conhecido |
| `fech. 05/09` | fechamento do dia |
| `manual · há 3 d` | informado por você, e há quanto tempo |
| `não-oficial` | fonte raspada, sem contrato |

Na v1 toda ação da B3 e dos EUA é `manual` — ver
[02 §1](02-dados-e-provedores.md#1-a-decisão-e-a-tensão-dentro-dela). Isso não é uma
limitação escondida: está escrito em cada linha.

## 4. A tela

```
┌ Posições ───────────────────── 12 ativos · R$ 148.302,10 · +R$ 21.884,30 (+17,3%) ┐
│ Ativo      Classe  Qtd     PM       Atual              Mercado      P&L      Peso │
│ PETR4      ação    300     32,10    38,42 manual·3d   11.526,00  +1.896,00   7,8% │
│ BTC        cripto  0,0412  418.200  512.340,00        21.108,41  +3.877,37  14,2% │
│ HGLG11     FII     120     158,90   161,05 manual·3d  19.326,00    +258,00  13,0% │
│ …                                                                                 │
├───────────────────────────────────────────────────────────────────────────────────┤
│ corretora-x  R$ 92.140,10   ·   corretora-y  R$ 56.162,00                          │
└─ ↑/↓ andar · Enter editar · a adicionar · Del remover · / agrupar · Esc sair ──────┘
```

`Layout::Rows` de um `Pane::Table` grande e um `Pane::Facts` curto com os totais por
corretora.

### Agrupamento

`/` cicla o agrupamento: **plano** (padrão) → por classe → por corretora → por moeda. Vira
árvore quando agrupado, e a tela cheia de tabela já sabe desenhar árvore, abrir e fechar
nó com ←/→, e semear os pais abertos.

## 5. Editar

Um formulário do módulo (`ModuleView` com caixa própria), no molde do `SessionEditor` e do
`MarkEditor`: campos com ↑/↓, `Enter` grava, `Esc` fecha a caixa antes de fechar o módulo.

Campos: ativo, classe, quantidade, preço médio, moeda, fonte, conta, preço manual.

**A validação acontece com o formulário ainda aberto**, que é a regra de `Tool::start` e do
`SessionEditor`: um formulário que aceita e falha depois é um formulário que perde o que
foi digitado.

O que se confere: quantidade e preços são números positivos, o ativo tem mercado e símbolo
(`B3/PETR4`, não `PETR4`), e a moeda existe.

## 6. Teclas

| Tecla | O que faz |
| --- | --- |
| ↑ / ↓ / `PgUp` / `PgDn` | anda |
| letras | busca, digitada direto |
| `Enter` | edita a linha |
| `a` | adiciona posição — mas ver a nota abaixo |
| `Del` | remove a linha, com confirmação **destrutiva** (vermelha) |
| `/` | cicla o agrupamento |
| ← / → | abre e fecha nó, quando agrupado |
| `Esc` | limpa a busca; depois pergunta se sai |

**Nota sobre `a`:** neste módulo toda letra é busca, então adicionar tem que ser
`Ctrl+A` — pela mesmíssima razão que marcar é `Ctrl+E` e criar sessão de tmux é `Ctrl+N`:
numa tela com busca digitada direto, a letra pertence à busca, e a ação tem que funcionar
*enquanto* se procura, que é geralmente como se descobre que a posição não existe.

## 7. Cadência e custo

`tick()` lê o retrato de cotações e recalcula as colunas derivadas. É aritmética sobre
dezenas de linhas: microssegundos.

Nenhuma requisição sai daqui. Quem busca preço é a thread do `ProviderSet`, acordada
porque este módulo está aberto e declarou `Need::Cotacao`.

## 8. Erros e degradação

| Situação | O que a tela faz |
| --- | --- |
| sem posição nenhuma | `Pane::Empty` explicando as duas formas de começar: importar ([51](51-importacao.md)) ou `Ctrl+A` |
| sem preço para um ativo | mostra `—` na coluna de preço, e a linha não entra no total. **O total diz quantas linhas ficaram de fora** |
| provedor caiu | preços ficam, envelhecem visivelmente, e o rodapé diz qual fonte caiu |
| câmbio indisponível | linhas em BRL seguem normais; as outras mostram valor na moeda original e ficam fora do total, que avisa |

A regra dos dois últimos: **nunca inventar**. Um total que silenciosamente exclui três
ativos é pior que um total ausente, porque parece completo.

## 9. Fica de fora na v1

* Cálculo de PM a partir de operações — por decisão, não por escopo.
* Posição vendida / alavancagem — quantidade negativa é recusada na v1.
* Split e bonificação automáticos: exigem histórico de eventos corporativos, que nenhuma
  fonte sem chave dá. Ajusta-se editando a linha, e [15](15-lancamentos.md) registra.

## 10. Como validar

* Importar duas corretoras com PETR4: duas linhas, PMs diferentes, ambas visíveis.
* Editar o PM à mão, fechar e reabrir o programa: o número está lá.
* Cortar a rede: preços envelhecem na tela, nada vira zero, nada trava.
* Um ativo sem cotação: `—` na coluna, e o total diz que uma linha ficou de fora.
* `Ctrl+A` com busca ativa: a caixa abre e a busca não come a tecla.
