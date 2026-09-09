# 03 — Armazenamento

**Grupo:** infraestrutura · **Estado:** entregue · **Fase:** 1
**Depende de:** [00 — Arquitetura](00-arquitetura.md)
**Arquivo:** `src/invest/store.rs` · **Estrutura:** [docs/banco.md](../banco.md)

O que fica gravado, em que formato, e as duas regras que não se negociam.

---

## 1. As duas regras

### 1.1 O preço médio é informado, nunca calculado

A **posição** é a fonte da verdade. Ticker, quantidade, preço médio, moeda e conta vêm
informados ou importados, e o programa nunca deriva o preço médio de um histórico de
operações.

A razão é o uso real: a carteira vem de várias origens diferentes — a carteira da
corretora, planilhas, outros sistemas — e cada uma já traz o preço médio dela, muitas
vezes sem o histórico de operações que o produziu. Recalcular a partir de lançamentos
parciais daria um número errado e sobrescreveria o certo.

Consequências, todas registradas nos módulos que sofrem com elas:

* [15 — Lançamentos](15-lancamentos.md) é histórico opcional e **não escreve no PM**.
* Editar quantidade e PM à mão é fluxo normal, não escotilha de emergência.

Na estrutura, isso é a coluna `posicao.preco_medio`: escrita pela importação e pela
edição, e por mais nada. Não existe função em `store.rs` que a calcule.

### 1.2 A carteira é gravada atomicamente

Um histórico de gráfico truncado por queda de energia é um gráfico feio. Um registro do
patrimônio de alguém truncado é outra coisa.

Isto já foi «arquivo temporário ao lado, `fsync`, `rename` por cima», feito à mão. Agora é
uma **transação**: ou as onze tabelas da carteira mudaram juntas, ou nenhuma mudou. A
garantia passou a valer para cada uma delas, e não só para o arquivo todo de uma vez —
com `synchronous = FULL`, porque o que se ganharia afrouxando isso é tempo que ninguém
sente, e o que se perde numa queda de energia é a última edição.

---

## 2. Onde

`~/.local/share/monitorzinho/db/<perfil>.db`, junto de tudo o que o programa guarda: o
histórico dos gráficos, as ferramentas, as marcas. Um arquivo por perfil, e nada de
segunda noção de «onde a gente guarda coisa».

A estrutura completa, a engine de migrations, os perfis e o backup estão em
[docs/banco.md](../banco.md). Aqui ficam só as tabelas desta aba.

| Tabela | Conteúdo | Gravada |
| --- | --- | --- |
| `posicao` | a carteira, uma linha por `(fonte, conta, ativo)` | ao editar, e a cada `SAVE_EVERY_N_TICKS` |
| `watchlist`, `alvo`, `setor`, `feed`, `mapeamento` | o resto do que é do usuário | idem |
| `provento`, `lancamento`, `alerta` | histórico e regras | idem |
| `carteira`, `carteira_alvo` | as carteiras recomendadas | idem |
| `patrimonio` | a série diária, um ponto por dia | ao sair da aba, e ao fechar |
| `cotacao` | último retrato bom de cada série de mercado | ao sair da aba, e a cada 60 s |
| `agenda`, `noticia`, `anunciado` | o que a home mostra e a home não busca | quando a busca volta |
| `modulo_mru` | id → epoch da última abertura | ao abrir um módulo |

A separação entre elas continua tendo o mesmo sentido que os três arquivos JSON tinham:
umas são do usuário e são preciosas, uma é preferência descartável, e as últimas são dado
que se pode buscar de novo. Apagar `cotacao` não custa nada; apagar `posicao` custa a
carteira.

---

## 3. A carteira, coluna a coluna

```sql
CREATE TABLE posicao (
    id              INTEGER PRIMARY KEY,  -- guarda a ordem da lista
    fonte           TEXT NOT NULL,        -- de onde esta linha veio
    conta           TEXT NOT NULL,        -- qual conta, dentro da fonte
    ativo           TEXT NOT NULL,        -- 'B3/PETR4' — ver AssetId em 02
    classe          TEXT NOT NULL,        -- 'acao', 'fii', 'renda_fixa'…
    quantidade      REAL NOT NULL,
    preco_medio     REAL,                 -- INFORMADO. Nunca escrito pelo programa.
    moeda           TEXT NOT NULL,
    preco_manual    REAL,                 -- usado quando não há provedor ao vivo
    preco_manual_em INTEGER,              -- quando esse preço foi informado
    atualizado_em   INTEGER NOT NULL,     -- quando a linha inteira foi importada/editada
    carteira        TEXT                  -- a carteira recomendada de que participa
);
CREATE INDEX posicao_chave ON posicao(fonte, conta, ativo);
```

### `fonte` e `conta` são parte da identidade

A chave de uma posição é `(fonte, conta, ativo)`, não `ativo`. Isso é o que faz a
reimportação ser idempotente: reimportar a corretora X substitui as linhas da corretora X
e não toca em mais nada.

O mesmo ativo em duas corretoras dá **duas linhas**, com preços médios diferentes, e as
duas ficam visíveis. Consolidar apaga de onde veio o número, que é informação real.

Ela é um **índice e não uma restrição**, de propósito. Recusar uma linha na hora de gravar
perderia a linha; o lugar de reclamar de uma duplicata é a tela que a criou.

### O total consolidado, quando o mesmo ativo aparece duas vezes

Média ponderada pela quantidade dos preços médios **informados**. Isso continua sendo
"informado": é agregação de valores informados, não derivação a partir de operações. A
distinção importa e está aqui para quem for implementar não confundir as duas coisas.

### `preco_manual` é separado de `preco_medio`

São dois números com duas naturezas. O preço médio é o custo — do usuário, imutável pelo
programa. O preço manual é a última cotação conhecida — um valor de mercado que por acaso
foi informado à mão porque não há provedor. Quando um provedor ao vivo passar a cobrir o
ativo, `preco_manual` é ignorado e nada mais precisa mudar.

### `preco_medio` é `NULL` de verdade

Uma posição sem preço médio é legítima: quem acompanha um ativo sem lembrar quanto pagou.
A coluna é anulável e o `None` sobrevive à ida e volta — um zero no lugar dele seria um
custo inventado, e um custo inventado contamina P&L, alocação, yield on cost e imposto sem
deixar rastro. O que depende dele simplesmente **não é calculado**.

E dá para perguntar quais são:

```sql
SELECT fonte, conta, ativo, quantidade FROM posicao WHERE preco_medio IS NULL;
```

---

## 4. O cache de cotações

```sql
CREATE TABLE cotacao (
    ativo TEXT PRIMARY KEY, preco REAL NOT NULL, anterior REAL,
    em INTEGER NOT NULL, grade TEXT NOT NULL, moeda TEXT NOT NULL
) WITHOUT ROWID;
```

**Sempre desenhado com a idade ao lado**, nunca apresentado como atual. É a mesma regra que
o retrato de tamanhos dos containers já segue com `measured_at`: um número de quatro
minutos atrás é útil; um número de quatro minutos atrás apresentado como agora não é.

O descarte por idade acontece no `WHERE` da leitura, e não numa passada depois — uma linha
vencida não chega a virar objeto:

```sql
WHERE ?agora - em <= CASE grade WHEN 'fechamento' THEN ?serie ELSE ?preco END
```

**Dois tetos, e não um.** A regra ser única era um bug: uma série do Banco Central não
envelhece como um preço. O IPCA de julho é o IPCA corrente até o de agosto sair, e ele
nasce com semanas de idade porque o carimbo é o **primeiro dia do mês de referência**, não
o da busca. Com o teto de 24 h as quatro linhas do BCB eram descartadas em toda abertura e
voltavam a «buscando…» por dezenas de segundos. Um preço vale 24 h; uma série publicada,
120 dias — ver `CACHE_MAX_IDADE_SERIE`, onde o número está justificado com a medição.

Apagável a qualquer momento sem consequência. A única perda é a aba abrir com traços na
primeira vez.

---

## 5. Carregar e falhar

| O que acontece | O que o programa faz |
| --- | --- |
| o perfil é novo | carteira vazia, lista de módulos completa, tudo abre |
| uma linha que este binário não sabe ler | **pulada**, e as outras continuam vindo |
| coluna nova, ausente no banco antigo | uma migration a acrescenta com o padrão |
| coluna que não existe mais | fica lá até uma migration a tirar; a leitura a ignora |
| o arquivo do perfil não aceita escrita | a tela diz na hora, e a edição não finge que gravou |
| o banco é de uma versão futura | **não abre**, e a tela diz até onde ele foi |

A regra do segundo caso é a que `tools::persist::restore_params` já seguia: uma
atualização do programa nunca rejeita o que veio antes por inteiro. Um mercado que só
existe numa versão mais nova faz aquela linha ser pulada, e não a carteira inteira ser
perdida — o que vale para um downgrade tanto quanto para um upgrade.

Os dois últimos casos são o contrário do resto do programa, que trata dado ruim como
«comece do zero». Aqui, começar do zero é apagar a carteira, e isso não pode ser o
comportamento de recuperação.

---

## 6. Quando grava

* **Ao editar** — uma posição alterada é gravada na hora. Uma carteira que perde a última
  edição por um `kill` é uma carteira em que não se confia.
* **A cada `SAVE_EVERY_N_TICKS`** — o mesmo ritmo com que o app já salva histórico e
  ferramentas, para o que muda sozinho.
* **Ao sair da aba** — o cache e a série do patrimônio, para a próxima abertura ser rápida.
* **Ao fechar o programa** — em `App::persist_invest()`, junto do que já é salvo lá.

As coleções são **reescritas do zero** em vez de comparadas linha a linha. São dezenas ou
centenas de linhas dentro de uma transação, o que custa microssegundos, e o que se ganha é
que não existe caminho pelo qual o banco fique diferente do que está na memória.

A exceção é `patrimonio`: ali é `UPSERT` por dia e **nenhum `DELETE`**. O dia de hoje é
reescrito enquanto ele é hoje; um dia passado nunca é reescrito, e um dia que não está na
lista da memória continua no banco em vez de sumir.

---

## 7. O que isto deliberadamente não guarda

* **Credencial de provedor.** O arquivo do perfil é o que se copia para outra máquina e o
  que vai num backup; uma credencial dentro dele sai de casa sem ninguém perceber. O token
  da brapi é um arquivo solto ao lado, lido e nunca escrito pelo programa.
* **Histórico de preço.** Isso é cache, é grande, e se busca de novo. `cotacao` guarda o
  último ponto, não a série.
* **Nada derivado.** P&L, alocação, risco e correlação são calculados de novo a cada
  abertura. Guardar derivado é criar duas verdades sobre a mesma coisa, e um dia elas
  discordam.

---

## 8. Como validar

* Matar o programa com `kill -9` durante a gravação, em laço, algumas centenas de vezes: a
  carteira está sempre inteira, ou é a anterior. Nunca meio.
* Reimportar a mesma corretora duas vezes: o número de linhas não muda.
* Importar duas corretoras com o mesmo ativo: duas linhas, ambas visíveis, e o consolidado
  com a média ponderada.
* Apagar as linhas de `cotacao`: nada quebra, a aba só abre com traços na primeira volta.
* Editar `preco_medio` à mão com o `sqlite3` e reabrir: o número está lá, intacto. Nada o
  recalculou.
* `chmod 444` no `.db`: a aba diz que está somente para leitura, e a edição não finge.
* Uma linha com um mercado inventado (`INSERT INTO posicao ... 'MARTE/XPTO3' ...`): ela é
  pulada, e as outras continuam aparecendo.
