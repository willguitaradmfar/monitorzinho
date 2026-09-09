-- 0001 — a estrutura inicial.
--
-- Uma tabela por coisa que o programa guarda, com as colunas que ela realmente tem — e
-- não um `json` numa coluna só, que teria sido uma tradução literal dos doze arquivos
-- JSON que existiam antes e não teria comprado nada. O ponto de estar em SQL é o
-- `SELECT`: quanto se recebeu de provento por ativo no ano, que posições não têm preço
-- médio informado, quando cada alerta disparou.
--
-- Esta migration já foi publicada. Ela não se edita — o que corrige alguma coisa aqui é
-- uma migration nova, ver o cabeçalho de `src/db/migracoes.rs`.

-- Escalares e preferências, o que não merece uma tabela própria.
CREATE TABLE config (
    chave TEXT PRIMARY KEY,
    valor TEXT NOT NULL
) WITHOUT ROWID;

-- O histórico dos gráficos: uma linha por amostra, `pos` do mais antigo ao mais novo.
CREATE TABLE historico (
    serie TEXT NOT NULL,
    pos   INTEGER NOT NULL,
    valor REAL NOT NULL,
    PRIMARY KEY (serie, pos)
) WITHOUT ROWID;

-- As marcas das tabelas. `id` guarda a ordem em que foram escritas, que é a ordem em
-- que a tela de marcas as mostra.
CREATE TABLE marca (
    id        INTEGER PRIMARY KEY,
    tabela    TEXT NOT NULL,
    tipo      TEXT NOT NULL,
    valor     TEXT NOT NULL,
    subarvore INTEGER NOT NULL DEFAULT 0,
    cor       TEXT NOT NULL DEFAULT 'amarelo'
);
CREATE INDEX marca_por_tabela ON marca(tabela);

-- As execuções de ferramentas que voltam a subir no próximo início.
CREATE TABLE execucao (
    id         INTEGER PRIMARY KEY,
    ferramenta TEXT NOT NULL,
    ligada     INTEGER NOT NULL DEFAULT 1
);
CREATE TABLE execucao_param (
    execucao_id INTEGER NOT NULL REFERENCES execucao(id) ON DELETE CASCADE,
    chave       TEXT NOT NULL,
    valor       TEXT NOT NULL,
    PRIMARY KEY (execucao_id, chave)
) WITHOUT ROWID;

-- O histórico compartilhado de regras de reescrita do túnel.
CREATE TABLE regra (
    id       INTEGER PRIMARY KEY,
    padrao   TEXT NOT NULL,
    troca    TEXT NOT NULL,
    usada_em INTEGER NOT NULL
);
CREATE UNIQUE INDEX regra_unica ON regra(padrao, troca);

-- ---------------------------------------------------------------------------
-- Invest
-- ---------------------------------------------------------------------------

-- A carteira. `preco_medio` é INFORMADO: nada neste programa o calcula — ver
-- docs/invest/03. A chave de verdade é (fonte, conta, ativo), e ela é um índice e não
-- uma restrição de propósito: recusar uma linha na hora de gravar perderia a linha, e o
-- lugar de reclamar de uma duplicata é a tela que a criou.
CREATE TABLE posicao (
    id              INTEGER PRIMARY KEY,
    fonte           TEXT NOT NULL,
    conta           TEXT NOT NULL DEFAULT '',
    ativo           TEXT NOT NULL,
    classe          TEXT NOT NULL,
    quantidade      REAL NOT NULL,
    preco_medio     REAL,
    moeda           TEXT NOT NULL,
    preco_manual    REAL,
    preco_manual_em INTEGER,
    atualizado_em   INTEGER NOT NULL DEFAULT 0,
    carteira        TEXT
);
CREATE INDEX posicao_chave ON posicao(fonte, conta, ativo);
CREATE INDEX posicao_ativo ON posicao(ativo);

-- A watchlist e os feeds são conjuntos, e a chave diz isso: o mesmo ativo duas vezes na
-- lista é a mesma linha, e `ordem` só guarda em que posição ela aparece na tela.
CREATE TABLE watchlist (
    ativo TEXT PRIMARY KEY,
    ordem INTEGER NOT NULL
) WITHOUT ROWID;

CREATE TABLE alvo (
    classe     TEXT PRIMARY KEY,
    percentual REAL NOT NULL
) WITHOUT ROWID;

CREATE TABLE setor (
    ativo TEXT PRIMARY KEY,
    setor TEXT NOT NULL
) WITHOUT ROWID;

CREATE TABLE feed (
    url   TEXT PRIMARY KEY,
    ordem INTEGER NOT NULL
) WITHOUT ROWID;

-- O de-para de colunas de CSV já aprendido, por fonte.
CREATE TABLE mapeamento (
    fonte  TEXT NOT NULL,
    campo  TEXT NOT NULL,
    coluna TEXT NOT NULL,
    PRIMARY KEY (fonte, campo)
) WITHOUT ROWID;

CREATE TABLE provento (
    id         INTEGER PRIMARY KEY,
    ativo      TEXT NOT NULL,
    tipo       TEXT NOT NULL,
    pago_em    INTEGER NOT NULL,
    bruto      REAL NOT NULL,
    retido     REAL NOT NULL DEFAULT 0,
    moeda      TEXT NOT NULL,
    quantidade REAL
);
CREATE INDEX provento_ativo ON provento(ativo, pago_em);

CREATE TABLE lancamento (
    id         INTEGER PRIMARY KEY,
    em         INTEGER NOT NULL,
    tipo       TEXT NOT NULL,
    ativo      TEXT,
    quantidade REAL NOT NULL DEFAULT 0,
    preco      REAL NOT NULL DEFAULT 0,
    taxas      REAL NOT NULL DEFAULT 0,
    valor      REAL NOT NULL DEFAULT 0,
    moeda      TEXT,
    nota       TEXT NOT NULL DEFAULT ''
);
CREATE INDEX lancamento_em ON lancamento(em);

CREATE TABLE alerta (
    id             INTEGER PRIMARY KEY,
    ativo          TEXT NOT NULL,
    regra          TEXT NOT NULL,
    valor          REAL NOT NULL,
    ligado         INTEGER NOT NULL DEFAULT 1,
    armado         INTEGER NOT NULL DEFAULT 0,
    ultimo_disparo INTEGER
);

-- As carteiras recomendadas: o alvo, não a posição.
CREATE TABLE carteira (
    nome  TEXT PRIMARY KEY,
    ordem INTEGER NOT NULL
) WITHOUT ROWID;
CREATE TABLE carteira_alvo (
    id         INTEGER PRIMARY KEY,
    carteira   TEXT NOT NULL REFERENCES carteira(nome) ON DELETE CASCADE,
    ativo      TEXT NOT NULL,
    ordem      INTEGER NOT NULL DEFAULT 0,
    percentual REAL NOT NULL,
    teto       REAL
);
CREATE INDEX carteira_alvo_de ON carteira_alvo(carteira, ordem);

-- A série diária do patrimônio: um ponto por dia, e o passado não se reescreve.
CREATE TABLE patrimonio (
    dia       TEXT PRIMARY KEY,
    total_brl REAL NOT NULL,
    aporte    REAL NOT NULL DEFAULT 0,
    retirada  REAL NOT NULL DEFAULT 0,
    proventos REAL NOT NULL DEFAULT 0
) WITHOUT ROWID;

-- O último retrato bom de cada série de mercado. Descartável: apagar custa a aba abrir
-- com traços na primeira volta, e nada mais.
CREATE TABLE cotacao (
    ativo    TEXT PRIMARY KEY,
    preco    REAL NOT NULL,
    anterior REAL,
    em       INTEGER NOT NULL,
    grade    TEXT NOT NULL DEFAULT '',
    moeda    TEXT NOT NULL DEFAULT ''
) WITHOUT ROWID;

-- O calendário econômico já buscado, porque a home o mostra e a home não busca nada.
CREATE TABLE agenda (
    id     INTEGER PRIMARY KEY,
    ano    INTEGER NOT NULL,
    mes    INTEGER NOT NULL,
    dia    INTEGER NOT NULL,
    pais   TEXT NOT NULL,
    titulo TEXT NOT NULL,
    peso   INTEGER NOT NULL DEFAULT 1,
    origem TEXT NOT NULL DEFAULT 'publicado'
);
CREATE INDEX agenda_data ON agenda(ano, mes, dia);

-- As manchetes já lidas, pelo mesmo motivo do calendário.
CREATE TABLE noticia (
    id     INTEGER PRIMARY KEY,
    fonte  TEXT NOT NULL,
    titulo TEXT NOT NULL,
    link   TEXT NOT NULL,
    data   TEXT NOT NULL DEFAULT '',
    resumo TEXT NOT NULL DEFAULT '',
    em     INTEGER
);

-- Proventos anunciados, para a lista de sugestões nascer cheia.
--
-- A chave é um `id` e não `(ativo, em)`, e isso foi medido e não suposto: um mesmo papel
-- anuncia dividendo **e** JCP com a mesma data-com, e são duas linhas com dois valores
-- por cota. Numa carteira de verdade eram 445 anúncios com 399 pares distintos — uma
-- chave composta teria comido 46 deles calada.
CREATE TABLE anunciado (
    id       INTEGER PRIMARY KEY,
    ativo    TEXT NOT NULL,
    em       INTEGER NOT NULL,
    por_cota REAL NOT NULL,
    dy       REAL NOT NULL DEFAULT 0
);
CREATE INDEX anunciado_ativo ON anunciado(ativo, em);

-- Id do módulo -> epoch da última abertura, que é a ordem da lista da aba.
CREATE TABLE modulo_mru (
    id        TEXT PRIMARY KEY,
    aberto_em INTEGER NOT NULL
) WITHOUT ROWID;
