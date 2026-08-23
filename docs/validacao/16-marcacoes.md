# 16 — Marcações: seguir uma linha numa lista que se reordena

**Entregue em:** v0.24.0 · **Tipo:** recurso novo, transversal
(`src/monitor/mark.rs`, todas as tabelas, `app.rs`, `ui.rs`)

## O problema

Toda tabela aqui reordena a cada tick — e está certo: o processo mais pesado tem
que estar no topo. O efeito colateral é que seguir **uma** linha é impossível.
Você acha a conexão que interessa, olha para o lado, e ela mudou de lugar.

Uma marcação prende a linha **visualmente**: ela continua onde o ranking a puser,
e passa a usar uma **★** amarela onde quer que caia.

## Como marcar

Numa tabela em tela cheia, **`Ctrl+E`** sobre a linha. A caixa abre **já
preenchida com o valor da linha sob o cursor** — é ali que a resposta quase sempre
está. `Ctrl+E` de novo numa linha marcada **remove** a marcação.

> Não é `Ctrl+M`: no terminal, `Ctrl+M` é o mesmo byte que `Enter`. Também não é
> uma letra solta, porque numa tabela em tela cheia toda letra é busca — e marcar
> precisa funcionar **enquanto** se busca, já que achar a linha é como se chega
> nela.

## O que uma marca pode ser, por tabela

O tipo muda porque o assunto muda — e é esse o ponto. Uma porta é um número, um
processo é uma linha de comando, uma sessão é uma pessoa.

| Tabela | Seguir por |
| --- | --- |
| **Ports** | `porta` (número exato) · `processo` |
| **Connections** | `porta` (qualquer uma das duas pontas) · `endereço` · `processo` |
| **Top CPU / Top Memory** | `comando` — **e a opção de incluir a árvore** |
| **SSH Sessions** | `usuário` · `origem` · `comando` |
| **Interfaces** | `interface` |
| **System Info** | nenhuma — não há linha a seguir numa lista de fatos |

**Número é número.** Marcar a porta `443` não pega a `4433`, mesmo que `4433`
contenha `443` — a comparação é feita sobre cada número da célula, não sobre o
texto. Numa conexão, isso quer dizer que qualquer das duas portas serve.

**Texto é trecho — ou regex, quando parece regex.** Digitar `postgres` não exige
saber o que é expressão regular; digitar `^ssh(d)?$` não é tratado literalmente.
A decisão é pela presença de `^ $ * + ? [ ] ( ) |`.

**Árvore.** Nas tabelas de processo, `↑/↓` na caixa alterna *Incluir a árvore*.
Com ela ligada, o processo marcado **e todos os descendentes** ganham a estrela —
um build que importa é o build mais tudo que ele lançou.

## Como testar

### 1. Marcar uma porta e ver a estrela sobreviver

1. Aba Processos → `1` (Ports) → digite `5432` para achar
2. `Ctrl+E` → a caixa abre com `Seguir por ◂ porta ▸` e `Valor 5432`
3. `Enter`

**Esperado:** a linha ganha **★** e fica amarela. Sai da busca (`Esc`) e a estrela
continua lá, onde quer que a linha tenha ido parar.

```sh
cat ~/.local/share/monitorzinho/marks.json
```
```json
[ { "table": "ports", "kind": "porta", "value": "5432", "subtree": false } ]
```

**Feche e reabra o app:** a estrela tem que voltar, inclusive **no painel
compacto** da grade, não só em tela cheia.

### 2. Número não é substring

Marque a porta `443` numa máquina que também tenha algo em `4433`.
**Esperado:** só a `443` estrelada. Se as duas acenderem, a comparação voltou a
ser textual — há teste automatizado disso (`cargo test mark`).

### 3. Árvore

1. `3` (Top CPU) → ache um processo com filhos (`rootlesskit`, `dockerd`, um `make`)
2. `Ctrl+E` → tipo `comando`, *Incluir a árvore* em `sim` → `Enter`
3. `→` para expandir o nó

**Esperado:** o processo e **cada descendente** com estrela. Na validação:
`rootlesskit` mais os dois filhos dele.

### 4. Regex

Marque em Top CPU com valor `^(nginx|postgres)` — só esses dois, e não um
`postgres-exporter`… (que casa com `postgres` como trecho, mas não com o `^`
seguido de fim). Teste os dois valores e compare.

### 5. Remoção

`Ctrl+E` sobre uma linha marcada: a estrela some, e o `marks.json` perde a
entrada. Se várias marcas casarem com a mesma linha, todas saem — é o que
"pare de seguir isto" quer dizer.

### 6. Onde a tecla não é oferecida

Em **System Info**, o rodapé **não** mostra `Ctrl+E marcar ★`, e a tecla não faz
nada. Uma lista de fatos sobre a máquina não tem linha a seguir.

### 7. As colunas não dançam

A coluna da estrela existe sempre, marcada ou não. Marcar algo **não pode**
deslocar a tabela para o lado sob o leitor.

## Como saber que falhou

- Estrela sumindo depois de um tick (as marcas não estão sendo reaplicadas)
- Marca não voltando depois de reabrir o app
- `443` acendendo `4433`
- Árvore marcada sem os filhos, ou com filhos de outro processo
- Tabela deslocando ao marcar
- `Ctrl+E` abrindo o detalhe (é `Enter` disfarçado — voltou para `Ctrl+M`)
