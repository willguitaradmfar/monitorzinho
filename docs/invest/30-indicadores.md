# 30 — Indicadores

**Grupo:** Análise · **Estado:** planejado · **Fase:** 4
**Depende de:** [22 — Gráfico](22-grafico.md)
**Precisa de:** `Need::Historico`
**Custo:** aritmética sobre a série já carregada. Sem I/O.

---

## 1. O que responde

**O que a série de preço está dizendo, além do preço.**

## 2. Não é um módulo com tela própria — é uma camada sobre o gráfico

Um indicador sem o preço ao lado não significa nada. Então este módulo abre **sobre**
[22 — Gráfico](22-grafico.md), acrescentando linhas na área do preço e painéis embaixo.

Ele aparece na lista de módulos mesmo assim, porque é onde se configura quais indicadores
existem e com que parâmetros — e porque quem procura "RSI" na busca da lista tem que achar
alguma coisa.

## 3. Os indicadores da v1

Cinco, escolhidos por serem os que quase todo mundo usa e por caberem na tela:

| Indicador | Onde desenha | Parâmetros |
| --- | --- | --- |
| Média móvel simples | sobre o preço | período (padrão 20, 50, 200) |
| Média móvel exponencial | sobre o preço | período (padrão 9, 21) |
| Bandas de Bollinger | sobre o preço | período 20, 2 desvios |
| RSI | painel próprio, embaixo | período 14 |
| MACD | painel próprio, embaixo | 12, 26, 9 |

Cada um pode ser ligado, desligado, e ter o período trocado. A configuração é por ativo e
fica no banco: quem olha um gráfico diário e um de cinco anos não quer a mesma
média.

## 4. As contas, e onde elas erram

Três armadilhas, todas com teste:

* **A EMA precisa de um valor inicial.** O padrão da indústria é semear com a SMA do
  primeiro período, e não com o primeiro preço. Semear errado desloca a curva inteira nos
  primeiros períodos.
* **O RSI usa média suavizada de Wilder**, não média simples de ganhos e perdas. As duas
  dão números diferentes, e a de Wilder é a que todo mundo mostra.
* **Período maior que a série** não é erro: é um indicador que ainda não tem valor. Ele
  não desenha nada e a legenda diz "faltam N períodos", em vez de desenhar uma linha
  errada nos primeiros pontos.

## 5. A tela

```
┌ PETR4 · 6 meses · MM20 MM50 RSI ─────────────────── 38,42 ─┐
│                                     ╭──── MM20             │
│                      ▁▃▄▅▆▇█▇▅▄▃▄▅▆▇                       │
│  ▃▄▅▄▃▂▁▂▃▄▅▆ ╭─ MM50                                      │
├─────────────────────────────────────────────────────────────┤
│ RSI 14        68,2                              ─── 70 ─── │
│         ▃▄▅▆▇█▇▆▅▆▇█                            ─── 30 ─── │
└─ i ligar/desligar · ←/→ período · Esc voltar ao gráfico ───┘
```

As linhas de referência do RSI (30 e 70) são desenhadas. Um RSI sem elas é um número entre
0 e 100 sem escala de leitura.

## 6. Teclas

| Tecla | O que faz |
| --- | --- |
| `i` | abre a lista de indicadores, para ligar e desligar |
| `p` | edita o período do indicador selecionado |
| ← / → | período do gráfico |
| `Esc` | fecha a lista, se aberta; depois volta ao gráfico |

## 7. Fica de fora na v1

* Indicador definido pelo usuário. Precisaria de uma linguagem de expressão, o que é um
  projeto próprio.
* Sinais e cruzamentos automáticos ("MM20 cruzou MM50"). Isso é [42 — Alertas](42-alertas.md),
  e é lá que deve morar.
* Desenho à mão (linhas de tendência, suportes). Um terminal não tem ponteiro.

## 8. Como validar

* Comparar MM20, RSI e MACD com qualquer plataforma conhecida, na mesma série: batem até a
  segunda casa.
* Série com 10 pontos e MM20 ligada: nada desenhado, e a legenda diz quantos faltam.
* Ligar cinco indicadores: a tela continua legível ou avisa que não cabe.
