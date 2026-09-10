-- Quando cada fonte externa foi consultada pela última vez, com sucesso.
--
-- É uma tabela e não uma coluna em cada uma das outras porque a pergunta não é sobre um
-- dado, é sobre uma **consulta**: «há quanto tempo o programa não fala com o feed» tem
-- resposta mesmo quando a busca voltou sem nada novo, e é justamente aí que ela importa.
-- Uma coluna por linha de notícia responderia «quando esta manchete chegou», que é outra
-- coisa e que a coluna `noticia.em` já responde.
--
-- A chave é o nome da fonte — `cotacao`, `noticia`, `agenda`, `anunciado`, `historico`,
-- `fundamento` —, e não o do ativo: a granularidade por ativo já existe no carimbo de
-- cada cotação, e o que faltava era o carimbo do **último contato**, que é por fonte.
CREATE TABLE busca (
    chave TEXT PRIMARY KEY,
    em    INTEGER NOT NULL
) WITHOUT ROWID;
