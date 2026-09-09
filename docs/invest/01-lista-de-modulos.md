# 01 — Lista de módulos

**Grupo:** infraestrutura · **Estado:** planejado · **Fase:** 1
**Depende de:** [00 — Arquitetura](00-arquitetura.md)
**Custo fora de foco:** zero · **Custo por amostragem:** uma ordenação de dezenas de itens

A primeira widget da aba, e a porta de entrada de tudo. É a única tela da aba Invest que
sempre existe, mesmo numa instalação em que nada foi configurado.

## 1. O que ela responde

Três perguntas, nessa ordem de importância:

1. **O que eu estava fazendo?** — o módulo do topo é o último que foi aberto.
2. **O que existe aqui?** — o catálogo inteiro, sem precisar procurar em documentação.
3. **Isto está pronto para usar?** — a coluna de estado diz o que falta, quando falta.

## 2. A tabela

`TableMonitor` comum, com `tab()` devolvendo `Tab::Invest`. `id()` é `"invest-modulos"`
e nunca muda.

### Colunas

| Coluna | Conteúdo | Largura |
| --- | --- | --- |
| `Módulo` | nome do módulo | fixa, alinhada à esquerda |
| `Grupo` | `carteira`, `mercado`, `análise`, `informação`, `operação` | fixa, minúscula, cor por grupo |
| `Estado` | vazio quando pronto; o que falta quando não | fixa |
| `Resumo` | `InvestModule::summary()` — o que o módulo diz de si fechado | o resto |

O painel compacto (`compact_headers`) mostra só `Módulo` e `Resumo`: na metade de cima da
aba não cabem quatro colunas com folga, e grupo e estado são o que menos se lê de relance.

### A coluna de estado

Vem de `InvestModule::needs()` conferido contra o `InvestCtx`. Nunca é uma cor sozinha —
é uma frase curta que diz o que fazer:

| Estado | Quando | O que a linha mostra |
| --- | --- | --- |
| pronto | tudo que o módulo precisa existe | *(vazio — o silêncio é o bom estado)* |
| `sem posições` | `Need::Posicoes` e a carteira está vazia | amarelo |
| `sem cotação` | `Need::Cotacao` e nenhum provedor cobre os ativos da carteira | amarelo |
| `preço manual` | há cotação, mas informada e não ao vivo | cinza — é um aviso, não um problema |
| `precisa de X` | `Need::Provedor("…")` que este build não tem | cinza |

Um módulo em qualquer desses estados **continua abrindo**. Ele abre e explica na própria
tela o que falta e como resolver, com um `Pane::Empty`. Uma lista que recusa a entrada é
uma lista que não ensina nada — foi assim que se decidiu que a aba aparece sempre (§2 da
arquitetura), e é a mesma decisão aqui, um nível abaixo.

## 3. A ordem

```
1. módulos já abertos alguma vez, do mais recente para o mais antigo
2. módulos nunca abertos, na ordem de `all_modules()`
```

O bloco 2 é agrupado por `Group`, na ordem `Carteira · Mercado · Análise · Informação ·
Operação` — que é a ordem em que alguém constrói o uso: primeiro o que é meu, depois o que
está acontecendo, depois o que isso quer dizer.

### Quando o carimbo é escrito

No **momento em que o módulo abre**, não quando fecha. Abrir é o gesto que expressa
interesse; um módulo aberto por engano e fechado em dois segundos ainda foi o último que a
pessoa quis ver, e reordenar por tempo de permanência seria adivinhar intenção.

Consequência que precisa estar escrita porque incomoda uma vez e depois faz sentido: ao
sair de um módulo, ele está no topo da lista, e não na linha de onde foi aberto. O cursor
volta para onde estava (o `parent: TableFocus` preserva a seleção por índice de linha), e
não segue o módulo até o topo. Isso é de propósito — o cursor marca *onde a pessoa estava
navegando*, não o que ela acabou de fazer.

### Empate e primeira execução

Sem nenhum carimbo, a lista é o bloco 2 inteiro, e o primeiro item é
[Posições](10-posicoes.md). É o módulo por onde se começa, e ficar no topo numa
instalação nova é a única orientação que a tela dá sem escrever texto de ajuda.

## 4. Buscar

Nada a implementar. Uma tabela em tela cheia já trata toda letra como busca
(`App::search_push`), já pula para o primeiro acerto ao digitar, e já anda entre acertos
com ↑/↓.

Duas coisas a garantir na construção das linhas:

* **A busca casa com qualquer célula**, então digitar `carteira` acha o grupo inteiro e
  digitar `dividendo` acha [Proventos](14-proventos.md) pelo resumo. É o comportamento
  padrão de `TableFocus::match_indices` — basta que as células tenham as palavras.
* **A descrição do módulo entra na busca sem ocupar coluna.** Uma célula não pode ser
  invisível, mas o resumo pode carregar as palavras que importam. Onde não couber, a
  alternativa é `Pane`/`TableRow::key` guardar um texto de busca — decidir na
  implementação, com a tabela na frente.

## 5. Entrar

`Enter` na linha selecionada. O caminho:

```rust
// em App::open_row(), um terceiro ramo ao lado dos dois que já existem ali
if let Focus::Table(tf) = &self.focus
    && self.table_monitors[tf.table_index].id() == "invest-modulos"
{
    self.open_invest_module();
    return;
}
```

`open_invest_module` lê o `InvestModule::id` de `TableRow::key`, marca o carimbo do MRU,
chama `InvestModule::open(ctx)` — que é onde o custo nasce —, toma a `TableFocus` de
dentro do foco e monta o `Focus::Module`.

O `open()` pode demorar. Um módulo que precisa de série histórica de 90 dias começa a
pedir isso agora. A regra: **`open()` retorna imediatamente**, deixando o carregamento
para a thread e a tela mostrando o que já tem — mesma decisão que `switch_tab` toma ao não
amostrar antes do primeiro desenho, e pelo mesmo motivo: uma tecla que responde na hora e
números que chegam um instante depois.

## 6. Teclas

| Tecla | O que faz |
| --- | --- |
| `1` | abre a lista em tela cheia (é a widget número 1 da aba) |
| letras | busca, digitada direto |
| ↑ / ↓ | anda pela lista, ou entre acertos quando há busca |
| `PgUp` / `PgDn` | dez linhas, sem dar a volta |
| `Enter` | entra no módulo |
| `Esc` | limpa a busca; com a busca vazia, sai da tela cheia |
| `Backspace` | apaga uma letra da busca |

Nenhuma delas é código novo. Todas vêm da tela cheia de tabela que já existe.

`Delete` **não faz nada** aqui. Módulo não se apaga: o catálogo é o que o programa sabe
fazer, não uma lista do usuário.

## 7. O painel compacto

Na metade de cima da aba, mostrando as primeiras `compact_rows()` linhas — as mais
recentes, que é justamente a informação útil num painel curto. `compact_rows()` devolve
o mesmo `OVERVIEW_TABLE_ROWS` (10) das outras tabelas, salvo se a medida com a grade real
na frente disser outra coisa.

## 8. Custo

`sample()` faz: ler os carimbos já em memória, chamar `summary()` de cada módulo, ordenar.
`summary()` é obrigado a não fazer I/O — ele roda para **todos** os módulos, inclusive os
fechados, a cada tick da aba. Um `summary()` que abrisse socket transformaria a lista no
lugar mais caro do programa.

Onde um resumo precisa de um número que só a rede sabe, ele lê o cache
(a tabela `cotacao`, carregada em memória) e mostra a idade junto. É a mesma regra do
`measured_at` dos tamanhos de container.

## 9. Como validar

* Abrir o monitorzinho e não entrar na aba: `strace`/`lsof` não devem mostrar leitura de
  o banco. Nada da Invest existe até a aba ser visitada.
* Entrar na aba: a lista aparece preenchida no **primeiro** desenho, não no segundo.
* `1`, digitar `posi`, `Enter`: entra em Posições.
* `Esc`, `Enter`: volta com `posi` ainda na caixa de busca e o cursor na mesma linha.
* Voltar depois de abrir três módulos diferentes: os três estão no topo, na ordem inversa
  de abertura.
* Esvaziar `modulo_mru` com o programa aberto: nada quebra; a ordem volta a ser a de
  registro na próxima leitura.
