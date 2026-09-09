# 26 — Um banco só, e mais de um perfil

**Entregue em:** v0.38.0 · **Tipo:** infraestrutura, atravessa o programa inteiro
(`src/db/`, `src/perfil.rs`, `src/legado.rs`, e o `load`/`save` de todo módulo que
guardava alguma coisa)

## O problema

O monitorzinho gravava doze arquivos JSON lado a lado em
`~/.local/share/monitorzinho/`: `history.json`, `tools.json`, `marks.json`,
`rewrites.json`, `engine.json`, e mais sete da aba Invest. Cada um com a sua
própria noção de como ler, como gravar e o que fazer quando o conteúdo não presta.

Isso deu três problemas de uma vez:

* **Backup era saber quais dos doze importam.** E a resposta não era óbvia: um
  deles é a carteira, três são cache descartável, e o resto está no meio.
* **Levar para outra máquina era copiar os certos** — e o `.bak` junto, e não o
  `brapi.token`, que é credencial.
* **Perguntar qualquer coisa que atravessasse dois deles não era possível** sem
  escrever um programa. "Quanto recebi de provento por ativo neste ano" é uma
  pergunta que os dados respondiam e o formato não.

E um quarto, que só apareceu depois: não havia como ter **duas vidas separadas** no
mesmo programa. A carteira de casa e a da empresa, a máquina de trabalho e o
servidor que se acompanha, um banco de verdade e um de brincadeira.

## O que foi feito

Um **arquivo SQLite por perfil**, em `~/.local/share/monitorzinho/db/`. Copiar o
arquivo é o backup; pôr o arquivo na pasta de outra máquina é a restauração. Mais de
um arquivo lá é mais de um perfil, e o programa pergunta qual abrir.

A estrutura anda para a frente por **migrations versionadas**, embutidas no binário
com `include_str!` e aplicadas na abertura a partir da tabela `migracao` — o registro
do que já rodou naquele arquivo. O desenho inteiro está em
[docs/banco.md](../banco.md).

Não é um JSON numa coluna: uma tabela por coisa, com as colunas que ela tem. Era a
única forma de a mudança comprar alguma coisa além de "menos arquivos".

## Como validar

### 1. A migração dos arquivos antigos

Com o diretório de dados cheio de JSON de uma versão anterior, abra o programa.

**Esperado:** uma tela que espera uma tecla, listando o que veio e dizendo para onde
os originais foram. Depois dela, tudo no lugar: os gráficos com o histórico, as
ferramentas de volta rodando, as marcas nas tabelas, a aba Invest com a carteira.

```sh
ls ~/.local/share/monitorzinho/          # db/  legado/  (e brapi.token, se houver)
ls ~/.local/share/monitorzinho/legado/   # os doze, com o nome que tinham
```

Nada foi apagado. Confira linha por linha:

```sh
python3 - <<'EOF'
import json, sqlite3, os
d = os.path.expanduser('~/.local/share/monitorzinho')
c = sqlite3.connect(f"file:{d}/db/padrao.db?mode=ro", uri=True)
j = json.load(open(f'{d}/legado/invest.json'))
n = lambda t: c.execute(f'select count(*) from {t}').fetchone()[0]
print('posicoes ', len(j['posicoes']), n('posicao'))
print('carteiras', len(j['carteiras']), n('carteira'),
      sum(len(x['alvos']) for x in j['carteiras']), n('carteira_alvo'))
for f, t in [('marks.json','marca'), ('tools.json','execucao'),
             ('rewrites.json','regra'), ('invest-anunciados.json','anunciado'),
             ('invest-cache.json','cotacao'), ('invest-mru.json','modulo_mru')]:
    print(f, len(json.load(open(f'{d}/legado/{f}'))), n(t))
EOF
```

Os pares têm que bater. **Um deles bateu errado na primeira vez e achou um bug**: a
tabela `anunciado` tinha chave `(ativo, em)`, e uma carteira de verdade tinha 445
anúncios com 399 pares distintos — um papel anuncia dividendo **e** JCP com a mesma
data-com, com valores por cota diferentes. A chave composta comia 46 deles calada.
Hoje é um `id`, e há teste travando isso.

### 2. Abrir de novo não reimporta

Feche e abra. **Esperado:** nenhuma tela de migração, e nenhuma tabela dobrando de
tamanho. Rode a conferência acima: os mesmos números.

### 3. Um arquivo só

Depois de um dia de uso:

```sh
ls ~/.local/share/monitorzinho/db/
```

**Esperado:** só o `.db`. Nenhum `-wal`, nenhum `-shm` — é por isso que o journal é
o `DELETE` e não o `WAL`: um backup que copia só o `.db` de um banco em WAL copia um
banco sem as últimas transações.

### 4. Backup e restauração

```sh
cp ~/.local/share/monitorzinho/db/padrao.db /tmp/backup.db
# mexa na carteira, crie uma marca, ligue uma ferramenta, feche
cp /tmp/backup.db ~/.local/share/monitorzinho/db/padrao.db
```

**Esperado:** abre exatamente como estava no momento da cópia. Numa outra máquina,
o mesmo — pôr o arquivo em `db/` e abrir.

### 5. Perfis

```sh
monitorzinho --perfis                 # lista, sem abrir nada
monitorzinho --perfil casa            # cria e abre
monitorzinho                          # agora **pergunta**
```

**Esperado, na ordem:**

* Com **um** perfil, uma execução sem flag entra direto — sem tela.
* Com **dois**, ela para e pergunta, obrigatoriamente. Adivinhar aqui é abrir a
  carteira errada.
* `n` na tela abre a caixa de nome; o perfil novo nasce **vazio**, não é cópia.
* `Esc` sai sem abrir nada.
* Havendo mais de um, o nome do aberto aparece à esquerda das abas. Com um só, não
  aparece — não informaria nada.

Um nome com barra não anda pela árvore: `--perfil ../../etc/passwd` vira
`etc-passwd.db` dentro de `db/`, e há teste travando a propriedade.

### 6. As perguntas que o formato antigo não respondia

```sh
sqlite3 ~/.local/share/monitorzinho/db/padrao.db \
  "SELECT ativo, sum(bruto - retido) FROM provento
   WHERE pago_em > strftime('%s','2026-01-01') GROUP BY ativo ORDER BY 2 DESC"

sqlite3 ~/.local/share/monitorzinho/db/padrao.db \
  "SELECT fonte, conta, ativo, quantidade FROM posicao WHERE preco_medio IS NULL"

sqlite3 ~/.local/share/monitorzinho/db/padrao.db \
  "SELECT serie, count(*) FROM historico GROUP BY serie"
```

### 7. Migrations

```sh
sqlite3 ~/.local/share/monitorzinho/db/padrao.db \
  "SELECT versao, nome, datetime(aplicada_em,'unixepoch') FROM migracao"
```

**Esperado:** uma linha por migration aplicada, com nome e carimbo. Abrir de novo
não acrescenta nada — o caso normal é uma consulta e nenhuma escrita.

Um perfil do futuro não é aberto:

```sh
sqlite3 /tmp/futuro.db "CREATE TABLE migracao (versao INTEGER PRIMARY KEY,
  nome TEXT NOT NULL, aplicada_em INTEGER NOT NULL);
  INSERT INTO migracao VALUES (9999, 'de amanhã', 0)"
cp /tmp/futuro.db ~/.local/share/monitorzinho/db/futuro.db
monitorzinho --perfil futuro
```

**Esperado:** a tela diz até onde o arquivo foi e até onde este binário vai, e
**não toca no arquivo**. Confira com o `mtime`.

### 8. Somente leitura

```sh
chmod 444 ~/.local/share/monitorzinho/db/padrao.db
```

**Esperado:** a aba Invest diz, no rodapé, que o perfil está somente para leitura e
que nada será gravado. A alternativa — deixar alguém editar por meia hora e só
depois descobrir — é pior que não abrir.

### 9. A carteira inteira, ida e volta

O teste `invest::store::tests::a_carteira_inteira_sobrevive_a_ida_e_volta` cobre o
que uma inspeção visual não cobre: que `preco_medio: None` continua `None` e não
virou zero, que um alerta desligado volta desligado, que `teto` e `ordem` de uma
carteira recomendada sobrevivem, que a conta dentro da fonte não se perde.

E `uma_linha_ilegivel_nao_derruba_a_leitura_das_outras`: uma posição com um mercado
que este binário não conhece é **pulada**, e as outras continuam vindo. É a regra que
o JSON já seguia com um campo desconhecido — vale para um downgrade tanto quanto para
um upgrade, e um downgrade não pode custar a carteira.

## O que ficou de fora, de propósito

* **O `brapi.token` não entrou no banco.** O arquivo do perfil é o que se copia para
  outra máquina e o que vai num backup; uma credencial dentro dele sai de casa sem
  ninguém perceber.
* **Não há formato de exportação.** Um formato de exportação é uma segunda verdade
  sobre os mesmos dados, e um dia ela discorda da primeira. O arquivo *é* o formato.
* **Não há `down` nas migrations.** Um `down` que ninguém roda é código morto, e um
  que alguém roda apaga a coluna com o dado dentro. Para voltar de versão existe a
  cópia do arquivo — que é um arquivo só, e é metade do motivo de tudo isto estar em
  SQLite.
