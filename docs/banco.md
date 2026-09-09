# O banco

Tudo que o monitorzinho guarda — o histórico dos gráficos, as ferramentas que estavam
rodando, as marcas das tabelas, as regras de reescrita, o endereço da engine de
containers e a aba Invest inteira — fica num **arquivo SQLite por perfil**.

Antes eram doze arquivos JSON lado a lado, cada um com a sua própria noção de como ler,
como gravar e o que fazer quando o conteúdo não presta. Isso deu três problemas de uma
vez: fazer backup era saber quais dos doze importam, levar para outra máquina era copiar
os certos, e perguntar qualquer coisa que atravessasse dois deles não era possível sem
escrever um programa.

---

## 1. Onde

```
~/.local/share/monitorzinho/
├── db/
│   ├── padrao.db          ← um perfil
│   └── casa.db            ← outro
├── brapi.token            ← credencial, deliberadamente fora do banco
└── legado/                ← os JSON de antes, guardados na migração
```

Cada `.db` é **um arquivo só**. O modo de journal é o padrão do SQLite (`DELETE`) e não o
`WAL`, de propósito: o WAL é mais rápido e deixa dois arquivos irmãos ao lado, e um backup
que copia só o `.db` de um banco em WAL copia um banco sem as últimas transações. Aqui o
processo é um só e a escrita é minúscula — a velocidade do WAL não compra nada, e o
arquivo solto compra exatamente o que se queria.

### Backup e restauração

Copiar o arquivo é o backup. Pôr o arquivo na pasta `db/` de outra máquina é a
restauração — o monitorzinho de lá o encontra na próxima abertura e o oferece na lista de
perfis. Não há formato de exportação, e é essa a intenção: um formato de exportação é uma
segunda verdade sobre os mesmos dados, e um dia ela discorda da primeira.

Copiar com o programa aberto funciona (o SQLite mantém o arquivo consistente entre
transações), mas o certo é copiar com ele fechado — aí não há sequer a janela de uma
gravação em curso.

### O que **não** fica no banco

O token da brapi. O arquivo do perfil é o que se copia para outra máquina e o que vai num
backup; uma credencial dentro dele sai de casa sem ninguém perceber. O token continua
sendo um arquivo solto (`brapi.token`), lido e nunca escrito pelo programa — quem o
escreve escolhe as permissões dele.

---

## 2. Perfis

Um perfil é um `.db` na pasta. Ter mais de um é ter mais de uma vida separada no mesmo
programa: a carteira de casa e a da empresa, a máquina de trabalho e o servidor que se
acompanha, um banco de verdade e um de brincadeira para mexer sem medo. Eles não
conversam — cada um tem o seu histórico, as suas ferramentas, as suas marcas e a sua
carteira.

A regra de quando perguntar é a que faz o caso comum não custar nada:

| Quantos perfis | O que acontece |
| --- | --- |
| nenhum | cria `padrao` e entra. Quem nunca ouviu falar disto nunca vê a tela |
| um | entra nele direto. Ter um só é não ter escolha a fazer |
| mais de um | **pergunta, obrigatoriamente** |

Adivinhar com mais de um é abrir a carteira errada, e «o último que você usou» é
exatamente o palpite que erra no dia em que a pessoa criou o segundo perfil justamente
para não misturar as coisas.

```sh
monitorzinho                 # a regra acima
monitorzinho --perfis        # lista o que existe, sem abrir nada
monitorzinho --perfil        # abre a tela de escolha — é onde se cria um perfil novo
monitorzinho --perfil casa   # abre aquele direto, criando se não existir
```

`--perfil` é a única configuração que existe na linha de comando, e tem que ser: qual
banco abrir é decidido **antes** de haver um programa para decidir de dentro dele.

Havendo mais de um perfil, o nome do que está aberto aparece à esquerda das abas. Com um
só, não aparece — o nome dele não informaria nada.

---

## 3. Migrations

A estrutura do banco anda para a frente por migrations versionadas, em
`src/db/migracoes.rs` e `src/db/migracoes/*.sql`. A regra é a de qualquer sistema que
versiona esquema, e ela vale aqui pela mesma razão: **o banco não é do programa, é de quem
usa o programa**. Ele já existe, já tem a carteira de alguém dentro, e a versão nova
precisa alcançá-lo onde ele está.

**Todo início do monitorzinho passa pela engine.** Abrir um perfil lê a tabela `migracao`
— o registro do que já rodou naquele arquivo —, compara com a lista compilada, e aplica o
que faltar, na ordem. Num banco em dia isso é uma consulta e nenhuma escrita.

**A tabela é a fonte da verdade, e não um contador.** O que decide se a migration 7 roda é
ela não estar registrada naquele banco, e não «o banco está na versão 6». A diferença
aparece quando duas migrations nascem em paralelo e a de número menor é publicada depois:
com um contador ela seria pulada para sempre no banco de quem já passou da maior; pelo
registro, ela roda na próxima abertura.

`PRAGMA user_version` continua sendo escrito, espelhando a maior versão aplicada. Ele não
decide nada — serve para `sqlite3 arquivo.db "PRAGMA user_version"` responder de fora sem
precisar saber o nome da tabela.

### Escrever uma

Um `.sql` novo em `src/db/migracoes/`, e uma linha em `TODAS`:

```rust
pub const TODAS: &[Migracao] = &[
    Migracao { versao: 1, nome: "0001_estrutura_inicial", sql: include_str!("migracoes/0001_estrutura_inicial.sql") },
    Migracao { versao: 2, nome: "0002_coluna_corretora",  sql: include_str!("migracoes/0002_coluna_corretora.sql") },
];
```

O `include_str!` lê o arquivo **em tempo de compilação**: o SQL fica dentro do binário como
qualquer outra constante, e o monitorzinho continua sendo um binário único que não procura
nada no disco para subir. O arquivo separado é só para o SQL ser lido como SQL — com
destaque de sintaxe, e com um `diff` que mostra a estrutura mudando em vez de uma string
mudando.

As regras:

* **Uma migration publicada nunca é editada.** O banco de alguém já a registrou como
  aplicada, e mudar o SQL só mudaria o que acontece na máquina de quem ainda não a rodou —
  criando duas estruturas diferentes com o mesmo número. Corrigir a de ontem é escrever a
  de hoje: `ALTER TABLE ... ADD COLUMN`, uma tabela nova, um índice novo.
* **Cada uma roda na própria transação.** Uma que falha no meio não deixa metade da
  estrutura aplicada nem se registra: o programa recusa abrir aquele perfil e diz qual
  falhou, em vez de seguir com um banco pela metade.
* **Não há *down*.** Um `down` que ninguém roda é código morto, e um que alguém roda apaga
  a coluna com o dado dentro. Para voltar de versão existe a cópia do arquivo — que é um
  arquivo só, e é metade do motivo de tudo isto estar em SQLite.

### Um banco do futuro

Um perfil que já rodou uma migration que esta instalação não conhece **não é aberto**. É a
mesma trava que o `invest.json` tinha: uma versão que não se entende não é sobrescrita por
esta, que não sabe o que há dentro dela. A tela diz até onde o arquivo foi e até onde este
binário vai, e sugere atualizar ou abrir outro perfil.

---

## 4. As tabelas

Uma tabela por coisa que o programa guarda, com as colunas que ela realmente tem — e não
um `json` numa coluna só, que teria sido uma tradução literal dos arquivos antigos e não
teria comprado nada. O ponto de estar em SQL é o `SELECT`.

| Tabela | O que guarda |
| --- | --- |
| `migracao` | o que já rodou neste arquivo, com nome e carimbo |
| `config` | escalares: moeda base, endereço da engine de containers |
| `historico` | uma linha por amostra de gráfico (`serie`, `pos`, `valor`) |
| `marca` | as marcas das tabelas, na ordem em que foram escritas |
| `execucao`, `execucao_param` | as ferramentas que voltam a subir no próximo início |
| `regra` | o histórico compartilhado de regras de reescrita do túnel |
| `posicao` | a carteira. `preco_medio` é **informado** — ver [invest/03](invest/03-armazenamento.md) |
| `watchlist`, `alvo`, `setor`, `feed`, `mapeamento` | o resto do que é do usuário |
| `provento`, `lancamento`, `alerta` | histórico e regras da aba Invest |
| `carteira`, `carteira_alvo` | as carteiras recomendadas: o alvo, não a posição |
| `patrimonio` | a série diária, um ponto por dia |
| `cotacao`, `agenda`, `noticia`, `anunciado` | cache — descartável a qualquer momento |
| `modulo_mru` | a ordem da lista de módulos |

Perguntas que antes precisavam de um programa:

```sh
sqlite3 ~/.local/share/monitorzinho/db/padrao.db \
  "SELECT ativo, sum(bruto - retido) FROM provento
   WHERE pago_em > strftime('%s','2026-01-01') GROUP BY ativo ORDER BY 2 DESC"

sqlite3 ~/.local/share/monitorzinho/db/padrao.db \
  "SELECT fonte, conta, ativo, quantidade FROM posicao WHERE preco_medio IS NULL"
```

---

## 5. Como grava

Toda gravação é uma **transação**: ou o conjunto inteiro entrou, ou nada entrou. É o que
substitui o «temporário ao lado, `fsync`, `rename` por cima» que a carteira fazia à mão — e
substitui com vantagem, porque a garantia agora vale para as onze tabelas dela e não só
para o arquivo todo de uma vez. `synchronous = FULL`, porque o que se ganharia afrouxando
isso é tempo que ninguém sente, e o que se perde numa queda de energia é a última edição.

As coleções pequenas — posições, marcas, execuções — são **reescritas do zero** em vez de
comparadas linha a linha. São dezenas ou centenas de linhas dentro de uma transação, o que
custa microssegundos, e o que se ganha é que não existe caminho pelo qual o banco fique
diferente do que está na memória.

Nada aqui derruba o programa. Uma escrita que falha é ignorada como as gravações antigas
eram, e uma leitura que falha devolve o padrão. Um monitor que não abre porque o banco tem
um problema é pior que um monitor sem histórico. A exceção é a carteira: ali a falha vira
uma linha na tela, porque perder uma edição em silêncio é o que não pode acontecer.

Se o arquivo do perfil não aceita escrita — um pendrive montado somente para leitura, um
`chmod` de quem foi fazer backup —, a aba Invest diz isso na hora. Deixar alguém editar por
meia hora e só depois descobrir que nada foi gravado seria pior que não abrir.

---

## 6. A migração dos JSON antigos

Quem já usava o monitorzinho tem um diretório de dados cheio. No **nascimento do primeiro
perfil**, e só nele, cada arquivo é lido com o mesmo `serde` de antes e gravado pelas
mesmas funções `save` de sempre — que agora escrevem no banco. Não há um segundo
entendimento do formato antigo em lugar nenhum, e por isso não há como os dois discordarem.

Os originais **não são apagados**: vão para `legado/` ao lado, com o nome que tinham. Custa
alguns quilobytes e é a diferença entre uma migração que se pode conferir e uma que só se
pode acreditar. Um arquivo que não deu para ler fica onde está, e a tela diz qual foi.

A tela que resume isso aparece uma vez e espera uma tecla. Ela move a carteira de alguém de
lugar — não é coisa que se conte num rodapé que some no primeiro tick.

---

## 7. Como validar

* Abrir com o diretório de dados cheio de JSON: a tela lista o que veio, `legado/` tem os
  originais, e a aba Invest mostra a mesma carteira de antes.
* Abrir de novo: nada é reimportado, e nenhuma tabela dobra de tamanho.
* `cp padrao.db outra-maquina:~/.local/share/monitorzinho/db/`: o monitorzinho de lá
  oferece o perfil, e ele abre com tudo.
* Criar um segundo perfil: a próxima abertura **pergunta**, e o perfil novo nasce vazio.
* `chmod 444` no `.db`: a aba Invest diz que está somente para leitura, e não deixa a
  edição parecer que foi gravada.
* `PRAGMA user_version = 9999` num perfil: ele não abre, e a tela diz por quê — sem tocar
  no arquivo.
* Depois de um dia de uso: `ls db/` mostra **um** arquivo, sem `-wal` nem `-shm` ao lado.
