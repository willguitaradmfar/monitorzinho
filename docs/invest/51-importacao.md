# 51 — Importação

**Grupo:** Operação · **Estado:** planejado · **Fase:** 1
**Depende de:** [03 — Armazenamento](03-armazenamento.md)
**Precisa de:** nada
**Custo:** leitura de um arquivo local. Sem rede.

O módulo que faz a carteira existir sem digitação. Está na fase 1 junto com
[10 — Posições](10-posicoes.md) porque uma carteira que só se preenche à mão é uma carteira
que não se preenche.

---


## O extrato consolidado da B3

`invest/b3.rs` lê o JSON que a área do investidor da B3 exporta e o **achata na mesma
grade de um CSV** — daí em diante ele segue exatamente o mesmo fluxo de conferência. Um
segundo caminho de importação seria um segundo lugar para a conferência não acontecer.

Ele obriga duas coisas que um CSV de corretora não obrigava:

* **Várias corretoras num arquivo só.** Daí `Campo::Fonte`: a corretora sai de uma coluna,
  e a importação grava **uma substituição por corretora**. Sem isso, ou todas viravam uma
  fonte só — e o módulo de Corretoras deixava de responder «quanto está onde» — ou, pior,
  uma fonte do arquivo apagava as linhas de outra.
* **Ele não traz preço médio de renda variável.** Traz o valor de mercado. A coluna sai
  vazia e a posição entra sem preço médio, que é a regra 3 aplicada ao caso em que a fonte
  simplesmente não sabe.

Duas decisões que valem a pena estarem escritas:

* **Tesouro Direto entra com preço médio.** A B3 informa `valorAplicado` — o custo, dito
  pela fonte. Dividi-lo pela quantidade é a mesma informação por unidade, e não uma
  reconstrução a partir de histórico nenhum. Onde a fonte não diz o custo, não há PM.
* **Papel cotado não recebe preço informado.** O extrato traz o fechamento de dias atrás.
  Numa ação isso não acrescenta nada — a Kinvo e a brapi dão o preço de hoje — e atrapalha:
  gravado como informado, ele carrega o carimbo da *importação* e não o da data de
  referência, então um preço de 04/09 se apresentava como sendo deste minuto e ganhava do
  preço ao vivo. O sintoma era a cotação aparecer e virar «manual». Só CDB e Tesouro, que
  ninguém cota, recebem o valor do extrato.

## 1. O que responde

**Como colocar aqui dentro o que já está em outro lugar.**

## 2. O contrato de importação

Quatro regras, e as quatro vêm de [03](03-armazenamento.md):

1. **A chave é `(fonte, conta, ativo)`.** Importar a corretora X substitui as linhas da
   corretora X e não toca em mais nada.
2. **Idempotente.** Importar o mesmo arquivo duas vezes dá o mesmo resultado que uma.
3. **O preço médio vem do arquivo e é gravado como informado.** Nada é recalculado.
4. **Nada é gravado antes da conferência.** A importação mostra o que vai mudar, e só grava
   depois de confirmada.

A regra 4 é a que faz a diferença entre uma ferramenta em que se confia e uma que dá medo.

## 3. A tela de conferência

```
┌ Importar · corretora-x · posicoes-2026-09.csv ────────────┐
│ 14 linhas lidas · 12 reconhecidas · 2 com problema        │
│                                                            │
│ Ativo     Qtd            PM              O que muda       │
│ PETR4     300 (era 200)  32,10 (era 31,40)  alterada      │
│ VALE3     150            61,20              nova          │
│ BBAS3     — (era 100)    —                  removida      │
│ HGLG11    120            158,90             igual         │
│ …                                                          │
│ ⚠ linha 9: quantidade «1.200,50» — ação é inteira         │
│ ⚠ linha 12: ativo «PETRO4» não reconhecido                │
├────────────────────────────────────────────────────────────┤
│ resultado: 3 alteradas · 1 nova · 1 removida · 2 ignoradas│
└─ ↑/↓ · espaço incluir/excluir linha · Enter importar · Esc ┘
```

Cada linha diz **o que muda**, com o valor anterior ao lado. `espaço` exclui uma linha da
importação — porque um arquivo com uma linha ruim não pode obrigar a escolher entre
importar o erro e não importar nada.

Uma remoção — ativo que estava na carteira e não está no arquivo — é destacada. É a mudança
mais perigosa que uma importação faz, e a que um arquivo exportado pela metade produz.

## 4. Os formatos

| Formato | Situação | Nota |
| --- | --- | --- |
| **CSV** | v1 | o caminho principal. Ver §5 |
| **OFX** | v1 | formato de extrato bancário, bem definido, analisável |
| **Colar texto** | v1 | uma caixa onde se cola linhas soltas. Resolve o caso de quem tem os números mas não tem arquivo |
| Extrato B3 (CSV) | v1, como perfil de CSV | é CSV com colunas conhecidas |
| PDF de nota | não | extrair tabela de PDF é um projeto, e o resultado é frágil |
| XLSX | não | exportar como CSV é um clique |

## 5. CSV: mapeamento em vez de formato fixo

Não existe um CSV de corretora. Cada uma tem colunas com nomes diferentes, em ordens
diferentes, com decimal em vírgula ou ponto, com ou sem cabeçalho.

Exigir um formato fixo empurraria o trabalho de conversão para fora do programa, que é
onde ele mais dói. Então:

1. O programa lê o cabeçalho e **propõe** um mapeamento, adivinhando pelos nomes das
   colunas (`ativo`/`ticker`/`código`, `quantidade`/`qtd`, `preço médio`/`pm`/`custo`).
2. O usuário corrige o que ele errou, numa tela de colunas.
3. **O mapeamento é salvo por fonte.** Da segunda importação em diante daquela corretora,
   é só escolher o arquivo.

O passo 3 é o que transforma isto de uma ferramenta chata numa que se usa todo mês.

Detalhes que se descobre errando e por isso estão escritos: separador `,` ou `;`
(detectado pela contagem na primeira linha), decimal `,` ou `.` (detectado pelo padrão da
coluna inteira, não da primeira célula), codificação UTF-8 ou Latin-1 (detectada; um `ç`
mal lido num nome de ativo vira um ativo diferente), e aspas com separador dentro.

## 6. Escolher o arquivo

Uma tela que navega o sistema de arquivos, começando no diretório de downloads. Não há
diálogo de arquivo em terminal — e digitar o caminho inteiro à mão é como se desiste de
usar a funcionalidade.

Filtra por extensão conhecida, mostra tamanho e data, e permite digitar o caminho para
quem preferir. As setas andam, `Enter` entra na pasta ou escolhe o arquivo.

## 7. Teclas

| Tecla | O que faz |
| --- | --- |
| ↑ / ↓ | anda |
| `espaço` | inclui/exclui a linha da importação |
| `Enter` | importa o que está incluído |
| `Ctrl+M` | reabre o mapeamento de colunas |
| `Esc` | cancela — e nada foi gravado |

## 8. Erros e degradação

* Arquivo ilegível ou vazio: diz o quê, com o caminho.
* Nenhuma coluna reconhecida: abre direto o mapeamento, em vez de reclamar.
* Linha com problema: marcada, excluída por padrão, e o motivo escrito ao lado.
* Arquivo com zero linhas válidas: recusa a importar e explica. Não apaga a carteira.

O último é importante: uma importação de arquivo vazio que "sincronizasse" apagaria tudo.

## 9. Como validar

* Importar duas vezes o mesmo arquivo: o resultado é igual, e a segunda vez mostra
  "nenhuma mudança".
* CSV com decimal em vírgula e separador ponto-e-vírgula: lido certo.
* CSV em Latin-1: acentos certos.
* Arquivo com uma linha a menos: a remoção aparece destacada na conferência, e só acontece
  se for confirmada.
* `Esc` na conferência: nada foi gravado. Confere contando as linhas de `posicao` antes
  e depois.
