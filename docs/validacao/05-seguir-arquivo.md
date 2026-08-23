# 05 — Seguir arquivo

**Entregue em:** v0.15.0 · **Tipo:** ferramenta nova (`src/tools/tail.rs`)

## O que é

`tail -f` dentro do visualizador de log que as outras execuções já usam. O motivo
de ser uma ferramenta e não "abra outro terminal" é o visualizador: busca
enquanto digita, pulo entre resultados, esconder o que não casa, ler como hex, e
milhares de linhas ainda lá uma hora depois. `tail -f | grep` te dá **uma** dessas
e tira o resto.

| Campo | Para quê |
| --- | --- |
| Arquivo | caminho; precisa existir e ser legível — se não for, o erro aparece no formulário, não numa thread que morre calada |
| Começar do | `fim do arquivo` mostra as últimas 200 linhas e segue daí; `começo` lê tudo antes de seguir |
| Só linhas contendo | filtro **na origem** — a linha que não casa nunca entra no log (diferente da busca do visualizador, que filtra o que já entrou) |

Uma linha do arquivo é **uma** linha do log, com o horário em que chegou. Não é
tratada como "pedaço de tráfego" — isso renderia duas linhas por linha e cortaria
a tela útil pela metade.

**Rotação é tratada.** O arquivo é reconhecido por identidade (dispositivo e
inode, não pelo nome), então um `logrotate` que renomeia e cria outro é percebido:
o log diz `o arquivo foi rotacionado — seguindo o novo desde o começo` e continua.

## Como testar

### 1. Abrir com contexto, não com tela vazia

```sh
for i in $(seq 1 500); do echo "linha $i de teste"; done > /tmp/tail-teste.log
```
Crie a execução apontando para `/tmp/tail-teste.log`, começar do `fim`.

**Esperado:** abre já com as **últimas 200 linhas** (de `linha 301` a `linha
500`), não com o arquivo inteiro nem em branco. A coluna Resultado diz
`200 linhas`.

Conferência: `tail -n 200 /tmp/tail-teste.log | head -1` tem que ser a primeira
linha mostrada.

### 2. Ao vivo

```sh
echo "NOVA linha ao vivo A" >> /tmp/tail-teste.log
```
**Esperado:** aparece em menos de meio segundo, com carimbo de tempo diferente
das linhas iniciais (que vêm todas com `00:00.000`, pois já estavam lá).

### 3. Rotação — o teste que separa isto de um `cat` num laço

```sh
mv /tmp/tail-teste.log /tmp/tail-teste.log.1
echo "depois da rotacao: linha 1" > /tmp/tail-teste.log
```
**Esperado:** a linha `o arquivo foi rotacionado — seguindo o novo desde o começo`
e, em seguida, as linhas do arquivo novo. Se a saída simplesmente parar, é bug —
esse é o modo de falha clássico de quem segue pelo descritor antigo.

### 4. Filtro na origem

Edite a execução (`e`) e ponha `ERRO` em **Só linhas contendo**.
```sh
echo "info: tudo bem" >> /tmp/tail-teste.log
echo "ERRO: falhou algo" >> /tmp/tail-teste.log
```
**Esperado:** só a linha com `ERRO` entra. O título da execução passa a mostrar
`· contendo «ERRO»`. A busca do visualizador continua funcionando **por cima**
disso — são coisas diferentes de propósito.

### 5. Começar do começo

Crie outra apontando ao mesmo arquivo, com **Começar do** = `começo do arquivo`.
**Esperado:** lê o arquivo inteiro (conferir `wc -l`) e depois segue.

### 6. Arquivo grande

```sh
yes "linha de um arquivo enorme" | head -2000000 > /tmp/grande.log   # ~56 MB
```
**Esperado:** abrir do `fim` é instantâneo. A posição é achada andando para trás
em blocos de 64 KB contando quebras de linha — não lendo o arquivo todo. Se
demorar segundos, a busca da cauda regrediu.

### 7. Erros que têm que aparecer no formulário

- caminho inexistente → `não consegui abrir …`
- diretório em vez de arquivo → `… é um diretório`
- arquivo sem permissão (`/etc/shadow` como usuário comum) → erro de permissão

Nenhum deles pode criar uma execução que fica "rodando" sem fazer nada.

### 8. Persistência e remoção

Feche e reabra o app: a execução volta seguindo. `Del` remove com confirmação e o
descritor é liberado (`ls -l /proc/$(pgrep monitorzinho)/fd | grep tail-teste`
não deve mostrar nada).

## Como saber que falhou

- Duas linhas de log para cada linha do arquivo (voltou a tratar como tráfego)
- Saída parar depois de uma rotação
- Abrir do `fim` num arquivo grande demorando mais que um piscar
- Linha partida ao meio no log (leitura de linha incompleta sem rebobinar)
- Filtro de origem deixando passar linha que não casa
