# 03 — Armazenamento

**Grupo:** infraestrutura · **Estado:** planejado · **Fase:** 1
**Depende de:** [00 — Arquitetura](00-arquitetura.md)
**Arquivo novo:** `src/invest/store.rs`

O que fica em disco, em que formato, e as duas regras que não se negociam.

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
* [50 — Imposto de renda](50-imposto-de-renda.md) só consegue **estimar**, e diz isso.
* Editar quantidade e PM à mão é fluxo normal, não escotilha de emergência.

### 1.2 A carteira é gravada atomicamente

`history.json` truncado por queda de energia é um gráfico feio. `invest.json` truncado é o
registro do patrimônio da pessoa.

Então, diferente de todo o resto do programa: arquivo temporário ao lado, `fsync`,
`rename` por cima. O `rename` no mesmo sistema de arquivos é atômico — ou o arquivo velho
está inteiro, ou o novo está inteiro, e nunca há um estado intermediário no disco.

Além disso, `invest.json.bak`: a versão anterior, guardada antes de cada gravação. Custa um
arquivo pequeno e é a diferença entre um susto e uma perda.

## 2. Onde

`~/.local/share/monitorzinho/`, via `history::data_file` — o mesmo lugar de
`history.json`, `tools.json` e `marks.json`, e a mesma função, para não haver uma segunda
noção de "onde a gente guarda coisa".

| Arquivo | Conteúdo | Gravado |
| --- | --- | --- |
| `invest.json` | posições, watchlist, alvos, configuração | ao editar, e a cada `SAVE_EVERY_N_TICKS` |
| `invest-mru.json` | id → epoch da última abertura | ao abrir um módulo |
| `invest-cache.json` | último retrato bom de cada série de mercado | ao sair da aba, e a cada 60 s |

Três arquivos e não um, porque têm vidas diferentes: um é do usuário e é precioso, um é
preferência descartável, e um é dado que se pode buscar de novo. Apagar o terceiro não
custa nada; apagar o primeiro custa a carteira. Separá-los deixa isso óbvio para quem for
mexer, e permite as regras de gravação diferentes do §1.2.

## 3. `invest.json`

```jsonc
{
  "versao": 1,
  "moeda_base": "BRL",
  "posicoes": [
    {
      "fonte": "corretora-x",           // de onde esta linha veio
      "conta": "12345-6",               // qual conta, dentro da fonte
      "ativo": "B3/PETR4",              // mercado/símbolo — ver AssetId em 02
      "classe": "acao",
      "quantidade": 300,
      "preco_medio": 32.1,              // INFORMADO. Nunca escrito pelo programa.
      "moeda": "BRL",
      "preco_manual": 38.42,            // usado quando não há provedor ao vivo
      "preco_manual_em": 1757308800,    // quando esse preço foi informado
      "atualizado_em": 1757308800       // quando a linha inteira foi importada/editada
    }
  ],
  "watchlist": ["BINANCE/BTCBRL", "B3/IBOV"],
  "alvos": { "acao": 40.0, "fii": 20.0, "cripto": 10.0, "renda_fixa": 30.0 },
  "provedores": { "ordem": ["binance", "awesomeapi", "bcb", "manual"] }
}
```

### `versao`

Presente desde a primeira gravação. Um arquivo de carteira vai sobreviver a mudanças de
formato, e migrar sem saber de onde se está migrando é adivinhação. A leitura de uma versão
desconhecida **recusa e avisa**, em vez de interpretar por conta própria — o contrário do
que `tools::persist` faz, onde ignorar um campo estranho é barato.

### `fonte` e `conta` são parte da identidade

A chave de uma posição é `(fonte, conta, ativo)`, não `ativo`. Isso é o que faz a
reimportação ser idempotente: reimportar a corretora X substitui as linhas da corretora X
e não toca em mais nada.

O mesmo ativo em duas corretoras dá **duas linhas**, com preços médios diferentes, e as
duas ficam visíveis. Consolidar apaga de onde veio o número, que é informação real.

### O total consolidado, quando o mesmo ativo aparece duas vezes

Média ponderada pela quantidade dos preços médios **informados**. Isso continua sendo
"informado": é agregação de valores informados, não derivação a partir de operações. A
distinção importa e está aqui para quem for implementar não confundir as duas coisas.

### `preco_manual` é separado de `preco_medio`

São dois números com duas naturezas. O preço médio é o custo — do usuário, imutável pelo
programa. O preço manual é a última cotação conhecida — um valor de mercado que por acaso
foi informado à mão porque não há provedor. Quando um provedor ao vivo passar a cobrir o
ativo, `preco_manual` é ignorado e nada mais precisa mudar.

## 4. `invest-mru.json`

```json
{ "posicoes": 1757308800, "correlacao": 1757305200 }
```

Id do módulo → epoch em segundos da última abertura. Só isso.

Escrito no momento em que o módulo abre. Gravação simples, sem `fsync`: perder isto custa
a ordem da lista até o próximo uso, e nada mais. Um id desconhecido — de um módulo que
sumiu numa atualização — é ignorado na leitura, sem erro.

## 5. `invest-cache.json`

Último retrato bom por série, com o carimbo de quando foi obtido:

```jsonc
{
  "BINANCE/BTCBRL": { "preco": 512340.0, "em": 1757308791, "grade": "ao_vivo" },
  "BCB/CDI":        { "preco": 10.4,     "em": 1757222400, "grade": "fechamento" }
}
```

**Sempre desenhado com a idade ao lado**, nunca apresentado como atual. É a mesma regra que
o retrato de tamanhos dos containers já segue com `measured_at`: um número de quatro
minutos atrás é útil; um número de quatro minutos atrás apresentado como agora não é.

Uma entrada mais velha que 24 h é descartada na leitura em vez de mostrada — a partir de
certa idade o número deixa de informar e passa a enganar, e um preço de ontem numa tela de
mercado é dessa categoria.

Apagável a qualquer momento sem consequência. A única perda é a aba abrir com traços na
primeira vez.

## 6. Carregar e falhar

| O que acontece | O que o programa faz |
| --- | --- |
| arquivo não existe | carteira vazia, lista de módulos completa, tudo abre |
| JSON inválido | **não sobrescreve**. Renomeia para `.corrompido`, avisa na tela, segue com o `.bak` |
| `versao` desconhecida | recusa, avisa qual versão o arquivo tem e qual o programa entende, e **não grava por cima** |
| campo novo, ausente no arquivo antigo | vale o padrão — igual a `restore_params` faz com as ferramentas |
| campo que não existe mais | ignorado na leitura, sumindo na próxima gravação |

A regra que os dois últimos casos seguem é a de `tools::persist::restore_params`: uma
atualização do programa nunca rejeita um arquivo antigo por inteiro.

Os dois primeiros casos são o contrário do resto do programa, que trata arquivo ruim como
"comece do zero". Aqui, começar do zero é apagar a carteira, e isso não pode ser o
comportamento de recuperação.

## 7. Quando grava

* **Ao editar** — uma posição alterada é gravada na hora. Uma carteira que perde a última
  edição por um `kill` é uma carteira que não se confia.
* **A cada `SAVE_EVERY_N_TICKS`** — o mesmo ritmo com que o app já salva histórico e
  ferramentas, para o que muda sozinho.
* **Ao sair da aba** — o cache, para a próxima abertura ser rápida.
* **Ao fechar o programa** — em `App::persist()`, junto do que já é salvo lá.

## 8. O que este arquivo deliberadamente não guarda

* **Credencial de provedor.** Nenhum provedor da v1 tem chave. Quando tiver, a decisão de
  onde a chave mora é própria e não é aqui — um arquivo de configuração legível junto da
  carteira é o lugar errado.
* **Histórico de preço.** Isso é cache, é grande, e se busca de novo. O cache guarda o
  último ponto, não a série.
* **Nada derivado.** P&L, alocação, risco e correlação são calculados de novo a cada
  abertura. Guardar derivado é criar duas verdades sobre a mesma coisa, e um dia elas
  discordam.

## 9. Como validar

* Matar o programa com `kill -9` durante a gravação, em laço, algumas centenas de vezes:
  `invest.json` está sempre inteiro, ou é o anterior. Nunca meio.
* Corromper `invest.json` à mão: o programa abre, avisa, e o `.bak` salva a carteira.
* Reimportar a mesma corretora duas vezes: o número de linhas não muda.
* Importar duas corretoras com o mesmo ativo: duas linhas, ambas visíveis, e o consolidado
  com a média ponderada.
* Apagar `invest-cache.json`: nada quebra, a aba só abre com traços na primeira volta.
* Editar `preco_medio` à mão no arquivo e reabrir: o número está lá, intacto. Nada o
  recalculou.
