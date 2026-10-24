# Visão

Uma ferramenta da aba Ferramentas que guarda **uma pergunta a uma fonte de dados** e a
responde em forma de tabela. A fonte é um banco (PostgreSQL, MySQL, MongoDB, SQLite) ou
um endpoint HTTP que devolve JSON. A pergunta é um SQL, um comando do Mongo, ou — para o
HTTP — um caminho até o array e a lista de colunas que se quer ver dele.

Cada visão é uma execução como as outras: aparece na lista, sobrevive ao restart, abre
com `Enter`, e o resultado é o **tabelão** da resposta. Ela existe para o caso em que a
pergunta é sempre a mesma e o que muda é a resposta: «quantos pedidos presos na fila»,
«os últimos deploys», «o que a API de status está dizendo».

---

## 1. Decisões

| Decisão | Escolha | Por quê |
|---|---|---|
| Bancos | PostgreSQL, MySQL, MongoDB, SQLite | SQL Server ficou de fora por decisão do dono do projeto |
| Fonte HTTP | GET ou POST, JSON na resposta | Um endpoint é uma fonte de lista tão boa quanto um banco |
| Cliente MySQL | crate `mysql` (`minimal-rust` + `rustls-tls-ring`) | Os outros clientes são escritos à mão (Sherlock); o do MySQL entra como crate, sem `libz` nem OpenSSL, para o build musl continuar estático |
| Credenciais | Campos separados: máquina, porta, usuário, senha, base | Pedido explícito; a string de conexão fica para o Sherlock |
| Cache | **Só em memória** | Ao abrir pela primeira vez na sessão, executa; depois mostra o último resultado até alguém pedir `Ctrl+R`. Fechar o programa esquece |
| Túnel SSH | Opcional, nas três fontes de banco | Banco fechado atrás de uma máquina de salto é o caso comum em produção. O `ssh` daqui abre um `-L` para o banco, a consulta passa por ele, o túnel fecha. Só chave (agente, chaves padrão ou arquivo) — nunca senha, porque o túnel não pode perguntar |
| TLS | MySQL e Mongo: não / sim / sim sem validar | O Postgres negocia sozinho. «Sem validar» é para o certificado próprio e para o túnel, em que o servidor se apresenta como 127.0.0.1 |
| Só leitura | **Sempre** | Postgres: `BEGIN READ ONLY` em volta da query e sessão endurecida; MySQL: `SET SESSION TRANSACTION READ ONLY`; SQLite: arquivo aberto read-only e `PRAGMA query_only`; Mongo: só comandos de uma lista fixa de leitores, e `aggregate` sem `$out`/`$merge` |
| Teto de linhas | 1000 na tela | A query deveria ter o seu próprio `LIMIT`; a tela avisa quando cortou |
| Tempo máximo | 15 s por consulta | O mesmo teto do Sherlock, pelo mesmo motivo: uma visão não pode virar carga |

A senha fica gravada junto com a execução, como no Sherlock e no SSH: é o que permite a
visão voltar sozinha no próximo restart. A linha da lista e o título da tela **nunca**
mostram a senha — mostram `usuario@host:porta/base`.

---

## 2. O formulário

O primeiro campo é a **fonte**, e ele decide os outros:

| Fonte | Campos |
|---|---|
| PostgreSQL | máquina, porta (vazio = padrão da engine), usuário, senha, base, túnel SSH, consulta |
| MySQL | os mesmos, mais TLS |
| MongoDB | os mesmos, mais TLS; a consulta é um comando em JSON |
| SQLite | arquivo, consulta |
| HTTP JSON | URL, método (GET/POST), corpo (só no POST), autenticação (nenhuma/Bearer/Basic), credencial, caminho, colunas |

### O túnel SSH

Com «Túnel SSH: sim» aparecem a máquina de salto (`host`, `usuario@host`, ou um apelido
do `~/.ssh/config`), o usuário, a porta e a chave. A máquina e a porta do banco passam a
ser **as que a máquina de salto enxerga** — `localhost:5432` quando o banco mora nela.

Por baixo é um `ssh -N -L 127.0.0.1:<porta livre>:<banco>:<porta> <salto>`, com
`BatchMode=yes` (nunca pergunta), `ExitOnForwardFailure=yes` (falha alto) e
`StrictHostKeyChecking=accept-new`. O `ssh` só abre a porta local depois de autenticado,
então a porta aceitar conexão é a prova de que o túnel existe; se o `ssh` morrer antes,
a última linha do que ele disse vira a mensagem de erro («Permission denied
(publickey)»). O processo é amarrado ao programa pelo kernel (`PR_SET_PDEATHSIG`): um
`kill -9` no monitorzinho derruba o túnel junto.

### A consulta por fonte

* **SQL** (Postgres, MySQL, SQLite): o texto vai como está. As colunas da tabela são as
  colunas do resultado — o `AS` do SQL é a forma de nomeá-las.
* **MongoDB**: um documento de comando em JSON, o mesmo que se passaria a `db.runCommand`:

  ```json
  {"find": "pedidos", "filter": {"status": "preso"}, "sort": {"criado": -1}, "limit": 50}
  {"aggregate": "pedidos", "pipeline": [{"$group": {"_id": "$status", "n": {"$sum": 1}}}]}
  {"serverStatus": 1}
  ```

  `{"$oid": "…"}` vira ObjectId e `{"$date": ms}` vira data, para dar para filtrar por
  eles. Comandos que devolvem cursor (`find`, `aggregate`, `listCollections`,
  `listIndexes`) viram uma tabela com uma coluna por chave; os outros viram uma tabela
  campo/valor da resposta.

### HTTP JSON: o caminho e as colunas

Não há uma DSL pronta que valha a pena embutir, e um `jq` inteiro seria dez vezes o
tamanho da ferramenta. O que existe aqui é **o pedaço do `jq` que chega a um array e
escolhe campos de cada item**, escrito à mão:

```
caminho  := "." | ( "." chave | "[" n "]" )+
chave    := identificador | "\"qualquer coisa\""
```

* **Caminho** — onde está o array dentro da resposta. `.` (ou vazio) é a raiz;
  `.data.items`, `.results[0].rows`. Vazio numa resposta que é um objeto procura o
  primeiro campo cujo valor é um array e diz qual usou. Se o caminho chega num objeto em
  vez de num array, ele vira uma linha só.
* **Colunas** — opcional, separadas por vírgula, cada uma `caminho [as nome]`:

  ```
  .id, .name as nome, .address.city as cidade, .tags[0] as tag
  ```

  Vazio mostra todas as chaves de primeiro nível, na ordem em que aparecem. Um valor que
  é objeto ou array aparece como JSON compacto; `null` aparece vazio.

Com `Bearer`, a credencial é o token; com `Basic`, é `usuario:senha`. O POST manda o
corpo como `application/json`.

---

## 3. A tela

A consulta (e o corpo do POST) é escrita num **editor próprio**, aberto com `Ctrl+O` (ou
`F4`) no campo — `Enter` continua sendo «continuar», como nos outros campos: ocupa quase o terminal inteiro, numera as linhas, o cursor vai a qualquer lugar,
`Enter` quebra linha, `Tab` recua quatro espaços, colar mantém a formatação. `Esc` volta
ao formulário, que mostra a primeira linha e a contagem como prévia. É o
`ParamKind::Editor`, que qualquer ferramenta pode usar num campo de texto longo.

`Enter` na execução abre direto na tabela — não há grade de cartões, porque há um cartão
só. `↑`/`↓` andam pelas linhas e a linha sob o cursor aparece embaixo como campo/valor,
que é o que salva uma tabela larga demais para caber. **Digitar busca**, como no log: toda
linha fica, o trecho que casa é realçado e as setas pulam de acerto em acerto. `Ctrl+F`
alterna para o outro modo da mesma busca — só as linhas que casam — e volta. `Esc` limpa
a busca antes de sair. `Ctrl+R` executa de novo; o cabeçalho diz há quanto tempo foi a
última execução. `Tab` abre o log, onde ficam os erros e o tempo de cada execução.

A linha na lista mostra, nas duas colunas da ferramenta, quantas linhas vieram e quanto
tempo levou — ou `falhou` e o motivo.

---

## 4. Onde mora

* `src/tools/visao.rs` — a ferramenta inteira: formulário, plano, as cinco fontes, a
  DSL de caminho e os testes dela.
* `src/invest/feed.rs` — o cliente HTTP ganhou método, cabeçalhos e corpo (`pedir`); o
  `get` de sempre continua sendo o caso de uma linha.
* `src/tools/sherlock/{pg,mongo,bson}.rs` — reaproveitados como estão; só ficaram
  visíveis para o resto do crate.
* `src/tools/mod.rs` — o `Board` ganhou `direto` (abrir já no primeiro cartão) e os
  rótulos do cabeçalho, porque «investigado há 2s» é frase de Sherlock e aqui é
  «executado há 2s».

---

## 5. Teste

A DSL tem testes de unidade. As fontes de verdade têm testes marcados `#[ignore]` em
`visao.rs`:

* `VISAO_TESTE_URL=http://127.0.0.1:8765 cargo test servidor -- --ignored` roda contra
  um servidor de brincadeira em Python (fora do repositório) que serve várias anatomias
  de JSON — array na raiz, envelope, paginado, escalares, chaves heterogêneas, Bearer,
  Basic, redirecionamento e um POST que ecoa o corpo.
* `VISAO_TESTE_PARAMS='{"fonte":"PostgreSQL","host":...}' cargo test banco_de_verdade
  -- --ignored --nocapture` executa uma consulta de verdade com os nomes dos campos do
  formulário e imprime a tabela. Foi assim que as quatro fontes de banco, o túnel SSH
  (contra um `sshd` temporário com chave própria) e a recusa de escrita em cada uma
  foram conferidos.
