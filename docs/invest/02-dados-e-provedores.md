# 02 — Dados e provedores

**Grupo:** infraestrutura · **Estado:** planejado · **Fase:** 1 (o cliente) e 2 (os provedores)
**Depende de:** [00 — Arquitetura](00-arquitetura.md)
**Arquivos novos:** `src/invest/feed.rs`, `src/invest/provider.rs`, `src/invest/quote.rs`

De onde vêm os números que não são do usuário, quanto custa buscá-los, e o que a tela diz
quando a fonte não responde.

---

## 1. A decisão, e a tensão dentro dela

Foi decidido cobrir **quatro mercados** — ações e FIIs da B3, ações US, cripto e renda
fixa — usando **só fontes sem chave**, deixando o caminho aberto para provedores
profissionais depois.

Essas duas coisas não fecham inteiramente, e é melhor dizer isso do que descobrir na
metade da construção:

> **Cripto e renda fixa brasileira têm fonte pública, gratuita, sem chave e estável.
> Ação da B3 e ação americana não têm.** O dado de bolsa é vendido; o que existe de graça
> ou é atrasado e pede cadastro, ou é raspado de página e quebra sem aviso.

A saída não é escolher entre as duas respostas. É a mesma que já foi tomada para o preço
médio: **onde não há fonte confiável, o número é informado.**

| Mercado | Provedor v1 | O que se obtém |
| --- | --- | --- |
| Cripto | Binance (público) | preço ao vivo, histórico, volume — completo |
| Câmbio | AwesomeAPI / BCB PTAX | USD/BRL, EUR/BRL ao vivo — completo |
| Renda fixa BR | BCB SGS | Selic, CDI, IPCA — completo |
| **Ações e FIIs B3** | **Manual** | preço informado, com origem e idade na tela |
| **Ações US** | **Manual** | idem |

Com isso a v1 funciona inteira e honestamente: a carteira mostra P&L de tudo, a parte que
é ao vivo diz que é ao vivo, e a parte que é informada diz há quanto tempo foi informada.
Quando entrar um provedor com chave, **nenhum módulo muda** — troca-se o provedor por
trás do `trait Provider`, e as colunas de origem e idade passam a dizer outra coisa.

É por isso que o `trait Provider` existe já na v1, com um provedor que não faz rede
nenhuma. Ele não é abstração especulativa: é o que permite que a promessa acima seja
verdade.

## 2. O `trait Provider`

```rust
/// Uma fonte de preço. Existe para que trocar de fonte não signifique reescrever módulo.
pub trait Provider: Send + Sync {
    /// Chave estável, gravada na configuração — nunca muda depois de publicada.
    fn id(&self) -> &'static str;
    fn name(&self) -> &'static str;

    /// Que ativos esta fonte sabe responder. É como o despacho escolhe quem pergunta.
    fn covers(&self, asset: &AssetId) -> bool;

    /// A qualidade do que ela devolve, mostrada na tela ao lado do preço. Não é
    /// enfeite: é a diferença entre confiar num P&L e conferi-lo.
    fn grade(&self) -> Grade;

    /// Se o mercado deste ativo está aberto agora. Decide a cadência — ver §6.
    fn is_open(&self, asset: &AssetId) -> bool;

    /// Cotação corrente de vários ativos de uma vez. Em lote porque quase toda API cobra
    /// por requisição e não por símbolo: pedir trinta preços em trinta chamadas é
    /// trinta vezes a cota por exatamente o mesmo dado.
    fn quotes(&self, assets: &[AssetId]) -> Result<Vec<Quote>, FeedError>;

    /// Série histórica de fechamentos. `None` numa fonte que não tem histórico —
    /// o módulo de gráfico simplesmente não oferece aquele ativo.
    fn history(&self, asset: &AssetId, span: Span) -> Option<Result<Vec<Candle>, FeedError>>;

    /// Teto de requisições que esta fonte tolera, para o limitador — ver §5.
    fn budget(&self) -> Budget;
}

/// O quanto se pode confiar num preço, e o que a tela mostra ao lado dele.
pub enum Grade {
    /// Bolsa/exchange respondendo agora. Nenhuma marca na tela.
    AoVivo,
    /// Fonte oficial, com atraso conhecido. Mostra «15 min».
    Atrasado(Duration),
    /// Fechamento do dia anterior. Mostra «fech. 05/09».
    Fechamento,
    /// Informado pelo usuário. Mostra «manual · há 3 d».
    Manual,
    /// Fonte não-oficial, sem contrato nenhum. Mostra «não-oficial».
    NaoOficial,
}
```

`AssetId` carrega mercado e símbolo (`B3/PETR4`, `NASDAQ/AAPL`, `BINANCE/BTCBRL`,
`BCB/CDI`), porque `PETR4` e `AAPL` sozinhos não dizem a quem perguntar, e porque um dia
o mesmo símbolo vai existir em dois lugares.

### O despacho

`ProviderSet` guarda os provedores em ordem de preferência e, para cada ativo, pergunta ao
primeiro que o cobre. A ordem é: provedores configurados pelo usuário, depois os públicos,
depois o `Manual` — que cobre tudo, sempre, e por isso é o último e nunca deixa um ativo
sem resposta.

## 3. Os provedores da v1

### 3.1 Binance — cripto

Público, sem chave, sem cadastro. `grade()` = `AoVivo`.

* Cotação em lote: `GET https://api.binance.com/api/v3/ticker/24hr` — devolve último,
  variação de 24 h, volume. Sem parâmetro traz o mercado inteiro (resposta grande); com
  `symbols=[...]` traz só o que se pediu, que é o que se quer.
* Histórico: `GET /api/v3/klines?symbol=…&interval=…&limit=…`.
* O limite é por **peso**, não por contagem, e a própria resposta diz quanto já se gastou
  no cabeçalho `X-MBX-USED-WEIGHT-1m`. O limitador (§5) lê esse cabeçalho em vez de
  adivinhar — é a única fonte da v1 que se auto-reporta, e a mais fácil de respeitar.
* Pares em BRL existem direto (`BTCBRL`), o que evita a dupla conversão. Onde não
  existir, converte-se via USDT e o câmbio do §3.2.

### 3.2 Câmbio — AwesomeAPI e BCB

Câmbio é **dependência crítica**: com o patrimônio consolidado em BRL, todo ativo que não
é brasileiro passa por aqui, e um câmbio errado erra o patrimônio inteiro.

* `GET https://economia.awesomeapi.com.br/json/last/USD-BRL,EUR-BRL` — público, sem chave,
  responde rápido. Fonte primária.
* PTAX do Banco Central como segunda fonte e como referência oficial do dia. A PTAX é o
  número que vale para imposto ([50](50-imposto-de-renda.md)), então ela é buscada mesmo
  quando a primária está respondendo.

Duas cotações diferentes para a mesma moeda não é problema, é informação: a tela mostra a
de mercado, e o módulo de imposto usa a PTAX. Quem confundir as duas erra o DARF.

### 3.3 BCB / SGS — renda fixa

`GET https://api.bcb.gov.br/dados/serie/bcdata.sgs.{codigo}/dados/ultimos/{n}?formato=json`

Público, sem chave, estável, e a fonte oficial do número. `grade()` = `Fechamento` — são
séries diárias publicadas com defasagem, nunca "ao vivo", e apresentá-las como ao vivo
seria mentira.

**Os códigos de série têm que ser conferidos no catálogo do SGS na implementação**, um por
um, e fixados em constantes com o nome ao lado. Escrever de memória um código de série
econômica é o tipo de erro que não aparece — o número chega, é plausível, e é de outra
coisa. Interessam: Selic (diária e a meta anualizada), CDI, IPCA e IGP-M.

### 3.4 Manual — B3 e US na v1

Não faz rede. Devolve o preço que está na coluna `posicao.preco_manual`, com o carimbo de quando foi
escrito. `grade()` = `Manual`.

Não é um provedor de mentira nem um espaço reservado: é o provedor **correto** para uma
fonte que não existe. A alternativa — deixar a coluna vazia, ou mostrar o preço médio no
lugar do preço atual — seria pior das duas maneiras possíveis: ou esconde a posição, ou
mostra um P&L de zero que parece um fato.

O preço manual entra por três caminhos, todos de [51 — Importação](51-importacao.md):
digitado na tela de Posições, vindo do arquivo importado da corretora (que traz o preço do
dia da exportação), ou colado em lote.

### 3.5 Kinvo — B3 em lote, e o Ibovespa

`GET https://open-api.kinvo.com.br/intra-day-series?tickers=A,B,C&interval=5m&range=1d`

É a **primeira** fonte da B3, à frente da brapi, e a razão é medida: ela responde em lote.
Vinte tickers numa chamada, 1,2 s, sem chave — contra um pedido por ativo da brapi sem
token, que esbarra no limite anônimo antes de terminar uma carteira média.

Medido em 08/09/2026: ações, FIIs, ETFs e **IBOV** respondem; papel americano não; doze
pedidos seguidos vieram todos com 200 e nenhum cabeçalho anuncia cota. É justamente por
ela não dizer o limite que o intervalo é de 30 s por escolha nossa.

Três armadilhas que a implementação carrega escritas:

* **A série vem fora de ordem.** O último elemento do vetor repete um instante anterior.
  Ler o último dava 148,20 no HGLG11 quando o carimbo mais novo dizia 147,75 — quarenta e
  cinco centavos de preço velho apresentados como o de agora. O preço é o do **maior
  carimbo**, sempre.
* **Ticker inexistente responde 200** com as listas vazias, e viraria uma cotação de zero.
  É descartado, e aí a cadeia de reserva pergunta a quem sabe.
* **O IBOV não tem série histórica própria** — `sector-historic-quotation/IBOV` responde
  200 com a lista vazia. Ele viaja como carona na resposta de outro papel, e `PETR4` é o
  portador.

`grade()` = `Atrasado(n)`, com o `n` **medido** pelo carimbo e não fixo. Nunca `AoVivo`:
uma série de cinco em cinco minutos não é ao vivo.

É uma fonte **sem contrato**, como o Yahoo — é a API que alimenta o site da Kinvo, não um
produto publicado com compromisso de estabilidade. Por isso a brapi fica logo atrás na
cadeia: se este endereço mudar de forma amanhã, a aba troca de fonte sozinha.

#### O que a Kinvo tem e ainda não está ligado

Dois endereços medidos, funcionando, e **deliberadamente não ligados**:

* `monthly-dividends-heatmap?tickers=X` — proventos por mês e ano, com `eventDate`,
  `payment` por cota e `dividendYield`. Não vira `Provento` automaticamente: um provento é
  **o que a pessoa recebeu**, e isso depende da quantidade que ela tinha na data. Escrever
  na carteira um valor calculado a partir de um pagamento por cota seria inventar
  histórico — o mesmo erro que a regra do preço médio informado existe para evitar
  ([10](10-posicoes.md)). Entraria como dado de referência ao lado, nunca como registro.
* `market-agenda/documents/{TICKER}` — fatos relevantes e comunicados da CVM, com data e
  link. Cabe em [41 — Agenda](41-agenda.md) ou em [40 — Notícias](40-noticias.md), e é
  decisão de onde, não de se dá.

### 3.6 Reservados, desligados, e a decisão fica para depois

Ficam desenhados atrás do trait, sem serem construídos:

| Provedor | O que traria | Por que não agora |
| --- | --- | --- |
| **Stooq** | fechamento diário em CSV | só fechamento; útil para histórico, não para preço |
| **profissional** | tudo, ao vivo, com book | pago |

## 4. O cliente: `invest/feed.rs`

Não existe na base um "pegue este JSON". `tools/http.rs` mede as quatro fases de uma
requisição e **descarta o corpo** — é uma sonda de latência. `container/http.rs` fala com
o daemon por socket Unix. Então é código novo, e é pequeno de propósito:

```rust
/// GET com TLS que devolve o corpo. Nada de cliente genérico: só o que estes
/// provedores usam, para que a superfície seja pequena o bastante para se confiar nela.
pub fn get_json(url: &Url, headers: &[(&str, &str)], budget: &Budget) -> Result<Vec<u8>, FeedError>;
```

O que ele faz, e nada além:

* TLS reaproveitando a montagem de configuração de `tools/tls.rs`, com as raízes do
  sistema (`rustls-native-certs`) e o `webpki-roots` de reserva — como já é hoje.
* Redirecionamento, com o mesmo teto de 5 de `tools/http.rs`. Um sexto é laço.
* `Content-Length` e `Transfer-Encoding: chunked`. As duas, porque as APIs usam as duas.
* Teto de corpo. Uma resposta que passa dele é erro, não uma leitura pela metade.
* Timeout separado para conectar e para ler. Um provedor que aceita a conexão e some é o
  caso comum, e um timeout único trata isso como se fosse DNS lento.
* `Connection: keep-alive` reaproveitado por provedor: o handshake TLS custa mais que a
  requisição, e refazê-lo a cada 2 s é gastar mais em cerimônia do que em dado.

O que ele **não** faz na v1, e por que isso é sustentável:

* **`gzip`.** Várias APIs comprimem por padrão *quando se pede*. Não pedindo
  `Accept-Encoding: gzip`, elas mandam texto puro. Custa banda; não custa dependência.
  Quando o volume justificar, entra um decodificador — e aí é decisão própria, com o
  número de bytes na mão.
* **WebSocket.** Fase própria. Ver §7.
* **POST, autenticação, assinatura de requisição.** Nenhum provedor da v1 precisa.

### Erros

```rust
pub enum FeedError {
    Rede(String),        // DNS, conexão, TLS
    Tempo,               // estourou o timeout
    Status(u16),         // o servidor respondeu, e respondeu que não
    Cota { retry_after: Option<Duration> },  // 429, ou o cabeçalho de peso estourado
    Formato(String),     // respondeu 200 com algo que não é o que se esperava
}
```

`Cota` é separado de `Status` porque a reação é diferente: um 500 é para tentar de novo
daqui a pouco, um 429 é para **parar** e esperar o tempo que ele mandou esperar. Tratar os
dois igual é como se perde acesso a uma API pública.

`Formato` é separado porque é o erro que denuncia que a fonte mudou por baixo — e é
exatamente o que vai acontecer no dia em que um provedor não-oficial for ligado.

## 5. Cota e recuo

Um `Budget` por provedor, com um limitador de balde:

* **Piso de intervalo**: nunca mais rápido que 2 s, mesmo que a cota permita.
* **Teto por janela**: quantas requisições por minuto, do provedor.
* **Peso**, onde a API fala em peso — lido do cabeçalho da resposta, não estimado.
* **Recuo exponencial** a partir de 2 s, dobrando até 5 min, com ruído para que várias
  séries que caíram juntas não voltem juntas.
* **`Retry-After` manda.** Se o servidor disse quanto esperar, é esse o tempo.

O estado do limitador aparece na tela quando está apertando. Um preço que parou de
atualizar porque a cota estourou tem que dizer isso — senão é indistinguível de um preço
que não mudou, que é a pior confusão possível numa tela de mercado.

## 6. Cadência

Repetindo o §9 da arquitetura, porque é aqui que se implementa:

| Situação | Frequência |
| --- | --- |
| módulo aberto, mercado aberto | o que a cota permitir, piso de 2 s |
| módulo aberto, mercado fechado | 60 s |
| aba visível, nenhum módulo aberto | só o que as favoritas pedirem (v1: nada) |
| aba fora de foco | parado, salvo alertas e favoritas ligadas |

"Mercado fechado" vem de `Provider::is_open()`, e não de relógio local: a Binance nunca
fecha, a B3 fecha às 18h de Brasília, a NYSE tem feriado próprio, e nenhuma dessas três
coisas se descobre olhando a hora da máquina.

## 7. WebSocket — a fase que não é esta

Preço no tick precisa de WebSocket, e **não há biblioteca de WebSocket no projeto**. O
que existiria a fazer: o handshake de upgrade (é HTTP, e o `feed.rs` já faz HTTP), o
enquadramento do RFC 6455 (cabeçalho de tamanho variável, mascaramento do lado cliente,
fragmentação, ping/pong), e a reconexão com recuo.

É trabalho contido e conhecido, mas é um módulo de infraestrutura inteiro, e não um
detalhe do provedor da Binance. Fica para uma fase própria, atrás do mesmo
`trait Provider` — quando entrar, o provedor da Binance passa a se alimentar de stream em
vez de polling e **nenhum módulo fica sabendo**.

## 8. O retrato, e quem lê o quê

Igual ao `container::store::Store`, pelo mesmo motivo: a UI nunca espera por um socket.

* Uma thread por provedor ativo, publicando um `Snapshot` inteiro sob mutex a cada volta.
  Trocado inteiro, nunca editado no lugar.
* A UI lê com `read(|snap| …)` e no pior caso espera outra thread soltar o mutex, que é o
  tempo de trocar alguns vetores.
* Um mutex envenenado devolve o último retrato bom, como já é feito lá.
* Um contador de revisão para o laço principal saber se vale redesenhar — o mesmo
  `container_revision()` que já existe, para que um preço que mudou não repinte um gráfico
  de CPU.

## 9. O cache

a tabela `cotacao`, detalhada em [03 — Armazenamento](03-armazenamento.md). Existe para
uma coisa só: **abrir a aba mostrando números, e não traços.** Sempre desenhado com a
idade ao lado, nunca apresentado como atual.

## 10. Como validar

* Cortar a rede com um módulo aberto: os preços ficam, envelhecem visivelmente, e a tela
  diz que a fonte caiu. Nada de traço, nada de zero, nada de travar.
* Forçar 429 (apontando para um servidor local que só responde isso): o intervalo recua,
  a tela mostra que recuou, e não há tempestade de tentativas.
* Um provedor devolvendo JSON de outro formato: erro `Formato`, dito na tela, e os outros
  provedores continuam funcionando.
* Trocar um ativo de `Manual` para um provedor ao vivo: nenhum módulo muda de código, e a
  coluna de origem passa a dizer outra coisa.
* Deixar a aba e voltar: nenhuma requisição saiu enquanto ela estava fora de foco. Confere
  com `tcpdump`, não com boa vontade.
