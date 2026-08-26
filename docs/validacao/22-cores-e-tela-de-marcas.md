# 22 — Uma cor por marca, e a tela que lista todas

**Entregue em:** v0.30.0 · **Tipo:** recurso novo sobre as marcações
(`MarkColor`, `MarksScreen`, `src/monitor/mark.rs`, `app.rs`, `ui.rs`)

## O problema

As marcações resolveram seguir **uma** linha numa lista que se reordena
([16 — Marcações](16-marcacoes.md)). O que elas não resolviam era seguir
**várias**: com uma estrela amarela em tudo, uma tabela com quatro coisas marcadas
diz "quatro destas importam" e mais nada. Qual é o build, qual é o banco, qual é
aquilo que você marcou ontem e não lembra mais por quê — tudo lê igual.

E marca é barata de fazer: nasce de uma linha, no meio de outra coisa, e
**sobrevive ao app**. Depois de uma semana ninguém sabe o que está seguindo, e a
única forma de desmarcar era achar a linha original de novo — o que é exatamente o
problema que a marca existia para resolver.

Duas coisas, então: **cor por marca**, e uma **tela que lista todas**.

## Cor

Sete cores, as mesmas da paleta do resto do app: `amarelo`, `verde`, `ciano`,
`azul`, `roxo`, `laranja`, `vermelho`. A cor vale para a estrela **e** para a
linha inteira, no painel compacto e em tela cheia, e os descendentes de uma marca
com árvore herdam a cor do pai — um build e tudo que ele lançou lêem como uma
coisa só.

A caixa do `Ctrl+E` ganhou o campo *Cor* e, com ele, um cursor de campo:

```
 ▶ Seguir por        ◂ comando ▸
   Valor             /sbin/init splash
   Cor                 ● amarelo
   Incluir a árvore    sim
```

`↑/↓` movem o cursor, `←/→` respondem o campo em que ele está, e **digitar vai
sempre para o Valor** de qualquer campo — é o único campo em que há o que digitar,
e essa caixa se abre com pressa: uma letra que se perdesse porque o cursor estava
uma linha acima seria uma letra perdida.

**A cor inicial não é sempre a mesma.** A caixa abre na primeira cor que aquela
tabela ainda não usa — marcar três coisas seguidas dá três cores diferentes sem
ninguém escolher nada. Esgotadas as sete, recomeça.

## A tela de marcas — `Ctrl+G`

```
┌ Marcas ─────────────────────────────────────────────────────────┐
│                                                                 │
│ ▶ ★ Top CPU  comando  «cargo»  + árvore                         │
│   ★ Ports    porta    «5432»                                    │
│   ★ Top CPU  comando  «claude»  + árvore                        │
│                                                                 │
│   Salvas por máquina, e valem entre execuções do app.           │
└──── ↑/↓ navegar · ←/→ cor · Enter/e editar · Del remover · Esc ─┘
```

Abre **por cima** do que estiver na tela, da tabela ou do painel: marca se faz a
partir das tabelas, e voltar para a que se estava lendo é o motivo de fechar.

| Tecla | O quê |
| --- | --- |
| `↑/↓`, `PgUp/PgDn` | andar na lista |
| `←/→` | trocar a cor da marca sob o cursor, ali mesmo |
| `Enter` ou `e` | reabrir a caixa do `Ctrl+E` sobre ela, já preenchida |
| `Del` | remover — sem confirmação |
| `Esc` | voltar |

**Por que `←/→` recolorem na lista.** Distinguir duas marcas é coisa que se faz
*olhando para a lista*; uma ida e volta por um formulário para mudar um campo
seria toda ida e volta.

**Por que `Del` não pergunta.** Marca é destaque, e refazê-la são as mesmas duas
teclas que a fizeram. A caixa vermelha de confirmação é para o que não se desfaz.

**Nome da tabela, não o id.** A lista mostra `Top CPU`, não `top-cpu`. O id é o
que o arquivo guarda para que renomear um painel não órfãe nada — e é exatamente o
que ninguém quer ler aqui. Uma marca de uma tabela que esta versão não tem mais
aparece com o id cru: não dá para editar (não há tipos contra os quais editar),
mas o `Del` alcança.

## Como testar

### 1. Três marcas, três cores, sem escolher

1. Aba Processos → `3` (Top CPU)
2. `Ctrl+E` → `Enter` numa linha
3. `Ctrl+E` → `Enter` em outra
4. `Ctrl+E` → `Enter` numa terceira

**Esperado:** amarelo, verde e ciano — nessa ordem, sem ninguém ter tocado no
campo *Cor*. As três linhas coloridas de forma diferente, cada estrela na cor da
sua linha.

### 2. Escolher a cor na hora

`Ctrl+E` → `↓` `↓` até *Cor* → `←/→`. O `● nome` muda **de cor junto com o nome**,
e a borda da caixa acompanha. `Enter` e a linha sai naquela cor.

### 3. A árvore herda a cor

Marque um processo com filhos em roxo, *Incluir a árvore* em `sim`, `→` para
expandir.

**Esperado:** o pai e **cada descendente** em roxo. Se os filhos saírem amarelos,
a herança está pegando a cor errada.

### 4. Compatibilidade com o que já estava salvo

```sh
cat ~/.local/share/monitorzinho/marks.json
```

Um `marks.json` escrito por uma versão anterior **não tem** o campo `color`.
Abra o app: as marcas antigas têm que voltar, todas amarelas — que é exatamente a
cor que elas já tinham. Nenhuma marca pode sumir.

### 5. A lista

`Ctrl+G` de dentro de uma tabela em tela cheia **e** do painel compacto: abre nos
dois. Confira que a lista mostra **toda** marca da máquina, de todas as tabelas —
não só as da tabela de onde foi aberta.

`Esc` fecha e devolve **exatamente** o que estava embaixo: mesma tabela, mesma
seleção, mesma busca.

### 6. Recolorir pela lista

`←/→` sobre uma marca. **Esperado:** a estrela na lista muda de cor na hora, e a
linha na tabela por baixo também — sem esperar o próximo tick.

### 7. Editar pela lista

`Enter` sobre uma marca → a caixa abre com título **`Editar marca em …`** e todos
os campos preenchidos. Mude o valor e `Enter`.

**Esperado:** a marca continua **na mesma posição da lista** — editar não é
apagar e criar. Se ela pular para o fim, o `replace` virou `add`.

### 8. Remover pela lista

`Del` na última marca da lista. **Esperado:** ela some, o cursor fica numa linha
que existe (não fora do fim), e o `marks.json` perde a entrada. Apague todas: a
tela mostra "Nenhuma marca ainda" e o rodapé passa a oferecer só `Esc`.

### 9. `Ctrl+G` não vira `g`

Na aba Visão Geral, letras soltas ampliam painéis; na Ferramentas, `a`, `e` e `r`
agem sobre a execução selecionada. Em nenhuma das duas `Ctrl+G` pode fazer o que a
letra sozinha faria: ele abre a lista de marcas, e só.

## Como saber que falhou

- Marca antiga sumindo depois de atualizar (o `color` virou campo obrigatório)
- Duas marcas seguidas saindo da mesma cor
- Filhos de uma marca com árvore numa cor diferente da do pai
- Editar pela lista mandando a marca para o fim, ou duplicando-a
- `Del` na última linha deixando o cursor apontando para o vazio
- `Esc` na lista fechando a tabela em vez de só a lista
- Digitar na caixa com o cursor em *Cor* não indo para o *Valor*
