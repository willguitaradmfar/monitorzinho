# 11 — Alocação

**Grupo:** Carteira · **Estado:** planejado · **Fase:** 3
**Depende de:** [10 — Posições](10-posicoes.md)
**Precisa de:** `Need::Posicoes`, `Need::Cotacao`, `Need::Cambio`
**Custo:** agregação sobre dezenas de linhas por tick. Sem I/O.

---

## 1. O que responde

**Como o dinheiro está distribuído, e o quanto isso difere do que eu queria.**

## 2. Por que existe

O peso por ativo já está em [Posições](10-posicoes.md). O que não está é o peso por
**dimensão** — e é aí que mora a surpresa: uma carteira com quinze ações e três FIIs pode
ser 70% de um setor só, e nenhuma linha de posição mostra isso.

## 3. As dimensões

| Dimensão | De onde vem |
| --- | --- |
| Classe | campo `classe` da posição — informado |
| Moeda | campo `moeda` da posição |
| Corretora | campo `conta`/`fonte` |
| Setor | **informado**, com um padrão por classe — nenhuma fonte sem chave dá setor confiável |
| País | derivado do mercado do `AssetId` (`B3` → BR, `NASDAQ` → US, `BINANCE` → global) |

Setor é informado pelo mesmo raciocínio do preço médio: onde não há fonte confiável, o
usuário informa e a tela diz que foi informado. Um setor errado vindo de raspagem é pior
que um setor em branco, porque leva a uma conclusão sobre concentração.

## 4. Alvo × real

Os alvos ficam em `invest.json` (`"alvos"`), por classe na v1. Uma dimensão sem alvo
mostra só a distribuição real, sem coluna de desvio.

```
┌ Alocação · por classe ───────────────────────────────────────────────┐
│ Classe        Real      Alvo    Desvio                               │
│ ação         44,2%     40,0%    +4,2 p.p.  ████████████████████░░░   │
│ FII          18,1%     20,0%    −1,9 p.p.  ████████░░                │
│ cripto       14,2%     10,0%    +4,2 p.p.  ██████░░                  │
│ renda fixa   23,5%     30,0%    −6,5 p.p.  ██████████░░░░            │
├──────────────────────────────────────────────────────────────────────┤
│ desvio total 8,3 p.p. · maior desvio: renda fixa                     │
└─ ←/→ dimensão · e editar alvos · r rebalancear · Esc sair ───────────┘
```

`Pane::Bars` com valor e alvo por linha — a barra desenha o real, e a marca desenha o alvo.

O desvio é sempre em **pontos percentuais**, nunca em porcentagem: "cripto subiu 4,2%" e
"cripto está 4,2 p.p. acima do alvo" são frases diferentes, e confundi-las é como se toma
a decisão errada de aporte.

## 5. Teclas

| Tecla | O que faz |
| --- | --- |
| ← / → | troca a dimensão (classe, moeda, corretora, setor, país) |
| ↑ / ↓ | anda pelas linhas |
| `Enter` | abre as posições daquela fatia |
| `Ctrl+E` | edita os alvos |
| `r` | leva para [Rebalanceamento](12-rebalanceamento.md) com esta dimensão |
| `Esc` | sai |

## 6. Erros e degradação

* Sem alvo definido: mostra só o real, e o rodapé convida a definir.
* Alvos que não somam 100%: **não é erro**, e não é normalizado calado. A tela mostra a
  soma e diz que ela não fecha. Normalizar por conta própria muda os números que a pessoa
  escreveu, sem avisar.
* Ativo sem preço: fica de fora, e o rodapé diz quantos ficaram — mesma regra de
  [Posições](10-posicoes.md#8-erros-e-degradação).

## 7. Fica de fora na v1

* Look-through de fundo (abrir um FII ou ETF nos ativos que ele carrega): exige composição
  de carteira de terceiros, que nenhuma fonte sem chave dá.
* Alvo por dimensão que não seja classe. Depois, e sem mudar o formato do arquivo — o
  campo já é um mapa.

## 8. Como validar

* Somar os pesos: dá 100,0% quando todos têm preço; dá menos, e diz por quê, quando não.
* Trocar a dimensão com ←/→ e voltar: os mesmos números.
* Alvos somando 90%: a tela diz isso e não conserta sozinha.
