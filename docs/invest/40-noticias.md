# 40 — Notícias

**Grupo:** Informação · **Estado:** planejado · **Fase:** 4
**Depende de:** [02 — Provedores](02-dados-e-provedores.md)
**Precisa de:** nada
**Custo:** uma requisição por feed, a cada poucos minutos.

---

## 1. O que responde

**O que está sendo dito sobre o que eu tenho.**

## 2. RSS, e por que ele é a escolha certa aqui

RSS é XML sobre HTTP. Não tem chave, não tem cota, não tem termo de uso hostil, e quase
todo veículo financeiro publica um. É a única fonte de informação desta aba que tem as
mesmas garantias do BCB — pública, gratuita, estável — e por isso é a única que se pode
construir com a decisão de [02 §1](02-dados-e-provedores.md#1-a-decisão-e-a-tensão-dentro-dela)
intacta.

O custo: é preciso um leitor de XML, e não há um no projeto. Mas um leitor de RSS não é um
leitor de XML geral — os campos são meia dúzia (`title`, `link`, `pubDate`, `description`)
e a estrutura é rasa. Um analisador restrito a isso, com teto de tamanho e sem entidades
externas, é pequeno e auditável. Um analisador de XML genérico não seria, e não é o que se
vai escrever.

## 3. A tela é um log, e o programa já tem essa tela

Uma lista cronológica que cresce, com busca digitada direto, rolagem, e `End` para voltar
à ponta — é exatamente o `ToolMonitorFocus` da aba Ferramentas. Reaproveitar aquele
desenho, em vez de inventar outro, é o que mantém a promessa de não sair do padrão.

```
┌ Notícias ────────────────── 4 feeds · 128 itens · há 2 min ─┐
│ 14:22  infomoney   Petrobras aprova dividendos de R$ ...    │
│ 13:58  valor       Copom sinaliza manutenção da Selic ...   │
│ 13:40  reuters     Fed officials signal caution on ...      │
│ 12:15  infomoney   HGLG11 anuncia rendimento de R$ 1,10     │
├──────────────────────────────────────────────────────────────┤
│ ● marcadas: PETR4 HGLG11    ·   busca: petro                │
└─ ↑/↓ · Enter abrir · Ctrl+F só carteira · Ctrl+A feed · Esc ┘
```

## 4. Marcar o que é meu

Um item que menciona um ativo da carteira ou da watchlist é **marcado**. A busca é por
símbolo e por nome, com um mínimo de cuidado para não marcar tudo — `VALE3` é seguro,
`VALE` casaria com meia notícia do país.

`Ctrl+F` filtra para só os marcados. É o modo em que este módulo é realmente útil: quatro
feeds despejam centenas de itens por dia, e o que interessa são os que tocam a carteira.

## 5. Abrir uma notícia

`Enter` **não abre navegador**. O monitorzinho é uma interface de terminal e abrir um
processo gráfico a partir dela é sair da frente sem ter sido pedido.

O que ele faz: mostra o resumo do item (`description` do feed) numa tela de texto, com a
URL visível para copiar. É o mesmo `TextView` que a aba Containers já usa para mostrar o
que a engine responde.

## 6. Cadência

Cinco minutos por feed, com o cabeçalho `If-Modified-Since` para que uma volta sem novidade
custe uma resposta 304 e nenhum corpo. Notícia não é preço; buscar de dois em dois segundos
seria gastar banda alheia para receber a mesma lista.

Nada roda com o módulo fechado, como todo o resto da aba.

## 7. Teclas

| Tecla | O que faz |
| --- | --- |
| ↑ / ↓ / `PgUp` / `PgDn` | rola |
| letras | busca |
| `End` | volta à ponta |
| `Enter` | abre o resumo |
| `Ctrl+F` | só os marcados |
| `Ctrl+A` | adiciona um feed |
| `Esc` | limpa busca; depois sai |

## 8. Erros e degradação

* Feed fora do ar: os itens antigos ficam, e o rodapé nomeia o feed que caiu.
* XML malformado: o feed é marcado como quebrado e os outros seguem. Um feed ruim não pode
  derrubar o módulo.
* Feed enorme: teto de itens por feed e de tamanho de resposta, como toda leitura da aba.

## 9. Fica de fora na v1

* Análise de sentimento. É adivinhação com aparência de número.
* Notícia como gatilho de alerta. Fica em [42](42-alertas.md), se fizer sentido lá.
* Corpo completo do artigo. O feed dá o resumo; buscar a página é raspagem.

## 10. Como validar

* Um feed conhecido: os itens aparecem, na ordem certa, com a data certa.
* Um ativo da carteira citado: o item aparece marcado.
* Um feed apontando para um servidor que responde lixo: marcado como quebrado, os outros
  seguem.
* Segunda volta sem novidade: 304, e nada é rebaixado na tela.
