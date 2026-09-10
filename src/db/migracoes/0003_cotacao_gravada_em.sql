-- Quando cada cotação em cache foi **gravada**, que é outra coisa que `cotacao.em`.
--
-- `em` é a hora do dado: o último negócio, ou o dia de referência de uma série do Banco
-- Central, que pode ser do mês passado. Isto é a hora em que a leitura entrou — e é o
-- que responde «este cache é de quando?» na primeira tela, antes de a busca dar a
-- primeira volta. Sem ele, um IPCA de julho lido ontem e um lido agora ficam iguais.
--
-- Nulo no que já estava gravado: não dá para inventar a hora de uma leitura que
-- aconteceu antes de alguém a anotar, e a tela diz «do cache» sem idade nesse caso.
ALTER TABLE cotacao ADD COLUMN gravado_em INTEGER;
