# 27 — Marcações na aba Invest: um papel seguido é seguido em toda parte

**Entregue em:** v0.39.0 · **Tipo:** recurso transversal
(`src/invest/marcas.rs`, `src/invest/module.rs`, os módulos da Invest, `main.rs`, `ui.rs`)

## O problema

`Ctrl+E` para marcar e `Ctrl+G` para gerenciar as marcas existiam desde a
[16](16-marcacoes.md), mas só nas tabelas do sistema. A aba Invest — que é onde
mais se quer seguir uma coisa — não tinha nem uma nem outra. Um papel aparece em
dez telas dela, e não havia como dizer «este aqui é o que eu estou olhando».

O gesto agora é o mesmo em **todas as abas**, e não por parecença: é o mesmo
código. Uma marca posta em Posições é gravada, listada, recolorida e apagada pelo
caminho de uma marca posta em Ports.

## As duas teclas são do programa, não do painel

`Ctrl+E` e `Ctrl+G` passaram a ser vistas **antes** do módulo aberto. Isso custou
quatro atalhos que já usavam essas letras, e eles se mudaram em vez de o padrão
ganhar exceções:

| Onde | Era | Virou |
| --- | --- | --- |
| Posições | `Ctrl+G` agrupar | **`Ctrl+U`** agrupar |
| Agenda | `Ctrl+E` corte de relevância | **`Ctrl+R`** relevância |
| Alocação | `Ctrl+E` alvos | **`Ctrl+A`** alvos |
| Índices e macro | `Ctrl+E` informar um valor | **`Ctrl+A`** informar um valor |

Na aba Ferramentas, `Ctrl+E` **deixou** de abrir o editor de execução — lá a
letra solta `e` continua fazendo isso. Uma tecla reservada que faz outra coisa
numa aba é pior do que uma que não faz nada.

## Um papel é um papel: dez telas, uma lista de marcas

Posições, Cotações, Câmbio, Cripto, cada carteira recomendada, Proventos,
Lançamentos, Alertas, Fundamentos, Risco, Gráfico, Heatmap, Alocação e
Rebalanceamento **dividem o mesmo id de marca** (`invest-ativo`). Seguir PETR4 em
Posições acende PETR4 em Cotações, no cartão da carteira recomendada em que ele
está e na moldura da célula dele no heatmap.

As outras quatro listas têm assunto próprio e id próprio:

| Lista | Seguir por |
| --- | --- |
| **Ativos (Invest)** — as catorze telas acima | `ativo` · `classe` · `setor` |
| **Agenda** | `evento` |
| **Notícias** | `assunto` (trecho do título, ou regex) |
| **Corretoras** | `corretora` |
| **Índices e macro** | `indicador` |

Os três tipos da lista de ativos convivem porque **cada um só existe onde a
coluna dele existe**. Em Posições a caixa oferece `ativo` e `classe`; em
Fundamentos, `ativo` e `setor`; em Cotações, só `ativo`. Ninguém escreveu isso em
lugar nenhum: a coluna é dita pelo **cabeçalho**, e não pelo número.

> Por que pelo cabeçalho. «Ativo» é a coluna 1 na tela de Proventos, a 0 na aba
> «por ativo» do mesmo módulo, e a 1 outra vez no cartão da home. Um número
> acertaria uma das três. Há teste que confere que toda coluna declarada existe
> mesmo nas tabelas que os módulos desenham — um «Ativo» renomeado para «Papel»
> não quebra compilação nenhuma, só faz o `Ctrl+E` daquela tela parar de fazer
> qualquer coisa, calado.

## O cartão da home também acende

Esta foi a parte que quase ficou pela metade. A maioria dos cartões da Invest
**não é tabela**: são painéis de fatos, de barras e uma grade. Marcar só as
tabelas acendia dois cartões dos catorze em que um papel aparece — e a home é
justamente onde se olha para achar o que se segue.

| Painel | Como a marca aparece |
| --- | --- |
| Tabela (Posições, Cotações, carteiras) | coluna **★** + a linha inteira na cor |
| Fatos (Proventos, Agenda, Notícias, Índices, Gráfico, Fundamentos, Câmbio, Cripto) | a linha inteira na cor, rótulo em negrito |
| Barras (Alocação, Corretoras) | rótulo e barra na cor |
| Grade (Heatmap) | **a moldura** da célula na cor — o miolo continua verde/vermelho, que é o dado |

E a marca é testada contra **todos os pedaços** da linha, não só o rótulo: no
cartão de Notícias o rótulo é «há 6 h» e a manchete é o valor, e no de Agenda o
rótulo é a data e o evento é o valor. Testar só o rótulo deixava esses dois sem
destaque — eram justamente os dois em que o rótulo não é o assunto.

## A coluna da estrela aparece só quando há estrela

Ao contrário das tabelas do sistema, onde ela é fixa. Lá a lista se reordena
sozinha embaixo de quem lê, e um deslocamento lateral no meio disso é ruído. Aqui
a lista está parada, e dois caracteres cobrados de todo painel — inclusive dos
cartões estreitos da home — por uma coluna quase sempre vazia sairiam mais caro
que o pulo de uma vez só quando a primeira marca nasce.

## Como testar

### 1. Marcar em Posições e ver acender em Cotações

1. Aba Invest → `2` (Posições) → `↓` até um papel
2. `Ctrl+E` → a caixa diz **«Marcar em Ativos (Invest)»**, com o ticker já preenchido
3. `Enter` → `Esc`

**Esperado:** na home, o mesmo papel com **★** e na cor **nos dois cartões**,
Posições e Cotações. E na moldura da célula dele no Heatmap, se ele estiver entre
os doze maiores.

```sh
sqlite3 ~/.local/share/monitorzinho/db/padrao.db \
  "SELECT tabela, tipo, valor, cor FROM marca WHERE tabela LIKE 'invest%'"
```
```
invest-ativo|ativo|PRIO3|amarelo
```

**Feche e reabra o app:** tudo tem que voltar.

### 2. Marcar enquanto se busca

Em Posições, digite `itub` (a lista filtra) e então `Ctrl+E`. **Esperado:** a
caixa preenchida com o papel **visível**, e não com o que estaria naquela posição
sem a busca. É o erro que a busca torna possível e que só aparece com busca.

### 3. Uma classe inteira

`Ctrl+E` em Posições → `→` no campo *Seguir por* até `classe`. O valor se
**repreenche** com a classe da linha sob o cursor. Troque para `ETF` → `Enter`.

**Esperado:** as posições ETF acesas em Posições, e a barra **ETF** do cartão de
Alocação na mesma cor.

### 4. Agenda e Notícias

Entre na Agenda (`Ctrl+E` sobre um evento, valor `COPOM`) e nas Notícias
(`Ctrl+E` sobre uma manchete). **Esperado:** as linhas correspondentes **do
cartão da home** na cor da marca — é o caso em que o assunto está no valor e não
no rótulo.

### 5. Onde a tecla não é oferecida

No **Heatmap** o rodapé **não** mostra `Ctrl+E marcar ★`, e a tecla não faz nada:
o mapa mostra marcas mas não tem linha para criar uma. No **Patrimônio** também
não — não é lista de papéis.

### 6. As teclas que se mudaram

- Posições: `Ctrl+U` cicla o agrupamento (era `Ctrl+G`)
- Agenda: `Ctrl+R` cicla o corte de relevância (era `Ctrl+E`)
- Alocação: `Ctrl+A` abre os alvos (era `Ctrl+E`)
- Índices: `Ctrl+A` informa um valor (era `Ctrl+E`)
- Ferramentas: `e` edita a execução; **`Ctrl+E` não faz nada**

### 7. `Ctrl+G` de qualquer lugar

Da home da Invest, de dentro de um módulo, da aba Ferramentas, do wizard. A lista
sobe por cima do que estiver na tela, e `Esc` devolve exatamente onde se estava.

## Como saber que falhou

- Marca acendendo em Posições e **não** em Cotações (os ids se separaram de novo)
- Cartão da home sem destaque com a tela cheia destacando (o pintor não chegou no cartão)
- Agenda ou Notícias sem destaque (voltou a testar só o rótulo)
- `Ctrl+E` num módulo agrupando, filtrando ou abrindo formulário (um atalho antigo voltou)
- A caixa dizendo «Marcar em Posições» em vez de «Marcar em Ativos (Invest)»
- Marcar com busca ativa pegando a linha errada
- Heatmap com o miolo da célula pintado da cor da marca — a variação tem que continuar verde/vermelha
