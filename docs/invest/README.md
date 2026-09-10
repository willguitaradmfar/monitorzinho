# Invest — planejamento

A aba **Invest** é o primeiro pedaço do monitorzinho que não fala sobre a máquina em
que ele roda. É um terminal de mercado: a carteira, as cotações, o risco e as contas
que se faz em cima disso.

Estes documentos são **planejamento**, não registro de entrega. Nada aqui está
construído. Quando um módulo for entregue, ele ganha um documento em
`docs/validacao/`, como todo o resto — este continua sendo o desenho.

## As decisões que já estão tomadas

Elas restringem tudo o que vem depois, então estão aqui em cima e não enterradas:

| | |
| --- | --- |
| **Mercados** | ações e FIIs da B3, ações US, cripto, renda fixa e Tesouro |
| **Provedores** | na v1, **só fontes sem chave** — com a arquitetura pronta para APIs profissionais depois |
| **Moeda base** | tudo consolidado em **BRL** |
| **Tempo real** | polling REST de alguns segundos; WebSocket é fase própria |
| **Preço médio** | **informado**, nunca calculado a partir de operações |

A segunda e a primeira não fecham inteiramente: cripto e renda fixa brasileira têm fonte
pública e gratuita; ação da B3 e ação americana não têm. A saída está em
[02 §1](02-dados-e-provedores.md#1-a-decisão-e-a-tensão-dentro-dela) — onde não há fonte
confiável, o preço é informado, e a tela diz a origem e a idade dele.

## Leia primeiro

| | |
| --- | --- |
| [00 — Arquitetura](00-arquitetura.md) | A aba, o carregamento por demanda, o registro de módulos, a navegação e o contrato de não-impacto. **Nenhum módulo faz sentido sem isto.** |
| [01 — Lista de módulos](01-lista-de-modulos.md) | A primeira widget da aba: o catálogo, ordenado pelo último acessado. É por onde se entra em tudo. |
| [02 — Dados e provedores](02-dados-e-provedores.md) | De onde vem cotação, câmbio e juro; cache, limite de chamadas e o que a tela diz quando a fonte cai. |
| [03 — Armazenamento](03-armazenamento.md) | O que fica em disco, em que formato, e por que o preço médio nunca é recalculado. |

## Os módulos

### Carteira — o que é meu

| | | |
| --- | --- | --- |
| [10](10-posicoes.md) | **Posições** | Ativo, quantidade, preço médio informado, preço atual, P&L. A fonte da verdade. |
| [17](17-carteiras.md) | **Carteiras recomendadas** | O alvo de cada carteira, e o que aportar para chegar nele. |
| [11](11-alocacao.md) | **Alocação** | Por classe, setor, moeda, país. Alvo × real e o desvio. |
| [12](12-rebalanceamento.md) | **Rebalanceamento** | Quanto comprar e vender para voltar ao alvo — e como distribuir o aporte. |
| [13](13-patrimonio.md) | **Patrimônio** | A curva do total ao longo do tempo, e de onde veio cada movimento dela. |
| [14](14-proventos.md) | **Proventos** | Dividendos e JCP recebidos, agendados, yield on cost. |
| [15](15-lancamentos.md) | **Lançamentos** | Histórico de operações. Opcional, e **nunca** escreve no preço médio. |
| [16](16-corretoras.md) | **Corretoras** | O mesmo patrimônio visto por onde ele está custodiado. |

### Mercado — o que está acontecendo

| | | |
| --- | --- | --- |
| [20](20-cotacoes.md) | **Cotações** | A watchlist: último, variação, volume, mini-gráfico. |
| [22](22-grafico.md) | **Gráfico** | Preço no tempo, com timeframes e cursor. |
| [23](23-heatmap.md) | **Heatmap** | A grade colorida por variação — o mercado inteiro num olhar. |
| [24](24-indices-e-macro.md) | **Índices e macro** | IBOV, S&P, DXY, VIX, DI, treasury. |
| [25](25-cambio.md) | **Câmbio** | USD/BRL, EUR/BRL, e a conversão que todo o resto usa. |
| [27](27-cripto.md) | **Cripto** | Preço, funding, dominância. A única fonte com tempo real de graça. |
| [28](28-book.md) | **Book e negócios** | Profundidade e times & trades, onde a fonte permitir. |

### Análise — o que isso quer dizer

| | | |
| --- | --- | --- |
| [30](30-indicadores.md) | **Indicadores** | Médias, RSI, MACD, bandas — sobre a série do gráfico. |
| [31](31-risco.md) | **Risco** | Volatilidade, drawdown, Sharpe, beta, VaR. |
| [34](34-fundamentos.md) | **Fundamentos** | P/L, P/VP, DY, ROE, dívida. |

### Informação — o que eu preciso saber

| | | |
| --- | --- | --- |
| [40](40-noticias.md) | **Notícias** | RSS por ativo, no formato de log que a aba Ferramentas já usa. |
| [41](41-agenda.md) | **Agenda** | Resultados, COPOM, payroll, data-ex. |
| [42](42-alertas.md) | **Alertas** | Regras que rodam em segundo plano e acendem cor. |

### Operação — o que eu tenho que fazer

| | | |
| --- | --- | --- |
| [51](51-importacao.md) | **Importação** | CSV, OFX, extrato da B3. Como a carteira entra sem digitação. |

## O que a construção descobriu

O plano estava certo no essencial e errado em cinco pontos concretos. Ficam aqui porque
quem for mexer vai reencontrá-los.

| O que o plano dizia | O que se descobriu construindo |
| --- | --- |
| «`ultimos/N` do SGS traz N pontos» | **O SGS recusa N acima de 20**, com HTTP 400. Acima disso é preciso o endpoint por intervalo de datas — `src/invest/providers/bcb.rs`. |
| «o `Pane::Chart` desenha a série» | Com mais pontos que colunas, a `Sparkline` desenha os primeiros e **descarta o resto calado** — num gráfico de seis meses isso sumia com as duas últimas semanas. Reamostrar é obrigatório. |
| «a busca da tabela acha o que se digita» | Ela não achava «camb» em «Câmbio». A busca em tela cheia passou a dobrar acento — em todo o programa, não só aqui. |
| «o cursor da lista indexa os dados» | **Não indexa quando há busca**: o cursor anda pela lista filtrada. Sete módulos apagavam ou abriam a linha errada. A regra está em `Lista::atual`, com teste. |
| «três cenários bastam para o Sharpe» | Uma série de retornos constantes tem desvio de ~1e-19 e não zero, o que produzia um Sharpe da ordem de 10¹⁶. Há um piso de volatilidade em `calc::sharpe`. |
| «índice de bolsa é dado vendido» | **O Ibovespa tem fonte livre**: a API aberta da Kinvo o serve, e em lote. O módulo de índices deixou de decidir por mercado e passou a perguntar `ProviderSet::quem_busca` — assim uma fonte nova move um indicador da tabela dos informados para a dos ao vivo sem que ninguém edite a lista. |
| «o cliente HTTP não precisa de gzip» | Servidores comprimem **mesmo recebendo `Accept-Encoding: identity`** — o G1 é um. O feed vinha binário, virava «zero itens legíveis» e sumia da tela. Há um descompressor DEFLATE em `invest/gzip.rs`, testado contra o `gzip` do sistema. |
| «a série da API vem em ordem» | A da Kinvo não vem: o último elemento repete um instante anterior. Ler o último dava um preço de cinco minutos atrás apresentado como o de agora. O preço é o do **maior carimbo**. |
| «o P&L é mercado menos custo» | **Não quando parte das linhas não informa custo.** Com o extrato da B3 importado, dezenove de vinte e uma posições não trazem preço médio — e `mercado - custo` tratava o custo delas como zero, produzindo um «P&L» de R$ 1,08 milhão que não era lucro nenhum. Agora os dois lados falam do mesmo conjunto (`Totais::mercado_com_custo`) e a tela diz sobre quantas posições. |
| «uma origem pior nunca toma o lugar de uma melhor por 30 min» | O prazo media só o relógio, e **fechamento de pregão é velho por definição**. Todo fim de tarde o preço informado ganhava do preço de mercado: a cotação aparecia com variação e virava «manual». A validade agora depende de o mercado estar andando — `Provider::is_open`. |
| «o extrato traz o preço atual, então grave-o» | Ele traz o fechamento de dias atrás. Gravado como preço informado, ele carrega o carimbo da **importação** e não o da data de referência — um preço de 04/09 se apresentava como deste minuto. O importador só grava preço para o que **não tem fonte de cotação**: CDB e Tesouro. |
| «ordenar a tela cheia como o cartão» | O cartão reordena a cada cotação; a tela cheia **não pode** — `Del` apaga o ativo sob o cursor, e uma lista que anda debaixo de quem lê apaga o errado. A ordem de Cotações é a mesma nas duas, e a tela cheia a **congela ao abrir**, como `TableFocus` sempre fez. |
| «cada módulo desenha a sua tela» | A Agenda desenhava duas: a tela cheia com o calendário do IBGE e o cartão da home com uma lista feita à mão, sem ele. Duas montagens são duas chances de só uma ser corrigida — agora é uma função só, e o calendário mora no estado da aba, gravado em disco. |
| «marcar uma linha é dizer em que coluna está o assunto» | Nas tabelas do sistema a coluna é um **número**, e funciona porque cada tabela tem uma forma só. Na Invest a mesma lista aparece em três formas: «Ativo» é a coluna 1 na tela de Proventos, a 0 na aba «por ativo» do mesmo módulo, e a 1 outra vez no cartão da home. O número acertaria uma das três. A coluna passou a ser dita pelo **cabeçalho** e resolvida na hora de desenhar — `invest::marcas::tipos`. |
| «cada lista tem as suas marcas» | Verdade em toda aba menos nesta. Um papel aparece em catorze telas da Invest, e seguir PETR4 em Posições sem seguir PETR4 em Cotações não é seguir coisa nenhuma. As catorze dividem **um** id de marca; Agenda, Notícias, Corretoras e Índices, que têm assunto próprio, mantêm o seu. |
| «marca é coisa de tabela» | A maioria dos cartões da home não é tabela: é painel de fatos, de barras e uma grade. Marcar só as tabelas acendia dois cartões dos catorze. A marca pinta fato, barra e a **moldura** da célula do heatmap — e é testada contra todos os pedaços da linha, porque no cartão de Notícias o rótulo é «há 6 h» e a manchete é o valor. |
| «quantas colunas a grade tem é uma constante» | A Visão Geral desenhava **sempre** três: três painéis de vinte e seis caracteres num terminal de oitenta, e três de setenta num de duzentos e dez. A home da Invest dividia pela largura **mínima** de um cartão, o que dava quatro cartões espremidos onde cabiam três folgados. As duas passaram a dividir pela largura **boa** (`ui::colunas_para`), com teto de quatro — é o `repeat(auto-fit, minmax(40ch, 1fr))` do CSS, e vira em 80, 120 e 160 colunas. |
| «a home é uma lista de módulos» | Uma lista obriga a entrar em cada módulo para saber se ele tem algo a dizer. Cada módulo virou um cartão que já mostra, com a tecla entrando direto. |
| «ordene os cartões pelo que eles têm a dizer» | Medir o conteúdo faz a grade se rearranjar sozinha: o cartão de Cotações cresce quando o preço chega e pula para cima, levando os outros junto. A ordem **e a altura** saem de `InvestModule::destaque`, um número declarado que não muda em execução. |

Duas coisas que o plano previu e que se confirmaram medindo: a aba não faz **nenhuma**
requisição enquanto nenhum módulo está aberto (medido com `strace -e trace=connect`), e
nenhum arquivo da carteira é aberto antes de alguém entrar na aba.

## Ordem de construção

As fases estão justificadas em [00 — Arquitetura](00-arquitetura.md#12-fases). Em resumo:

1. **Fundação** — 00, 01, 02, 03, 10 (Posições), 51 (Importação).
   Entrega uma carteira que existe, é importável e mostra P&L. Sem chave de API.
2. **Mercado grátis** — 20, 25, 26, 27, 21.
   Cotação de cripto e câmbio em tempo real, juro brasileiro. Ainda sem chave.
3. **Leitura** — 22, 11, 13, 24, 31.
   O gráfico, a alocação, a curva de patrimônio e o risco.
4. **O resto**, na ordem em que doer.

O corte entre a fase 2 e a 3 não é arbitrário: é onde acaba o que se consegue de graça
e sem depender de fonte que quebra. Ver [02 §1](02-dados-e-provedores.md#1-a-decisão-e-a-tensão-dentro-dela).
