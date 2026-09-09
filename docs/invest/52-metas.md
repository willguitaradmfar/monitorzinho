# 52 — Metas

**Grupo:** Operação · **Estado:** planejado · **Fase:** 4
**Depende de:** [13 — Patrimônio](13-patrimonio.md)
**Precisa de:** `Need::Posicoes`
**Custo:** aritmética. Sem I/O.

---

## 1. O que responde

**Estou fazendo o que combinei comigo mesmo?**

## 2. Duas metas, e a diferença entre elas

| Meta | Exemplo | Depende de |
| --- | --- | --- |
| **De aporte** | R$ 2.000 por mês | só de mim |
| **De patrimônio** | R$ 500 mil até 2030 | de mim e do mercado |

A distinção não é cosmética. A meta de aporte é inteiramente controlável e o progresso nela
é um fato sobre disciplina. A meta de patrimônio depende de retorno, que não se controla —
e apresentar as duas do mesmo jeito ensina a pessoa a se sentir mal por um mercado ruim ou
bem por um mercado bom.

Então a tela separa as duas, e a de patrimônio sempre mostra a decomposição de
[13](13-patrimonio.md): quanto do progresso foi aporte e quanto foi mercado.

## 3. A tela

```
┌ Metas ──────────────────────────────────────── setembro/2026 ─┐
│ Aporte mensal · R$ 2.000,00                                    │
│ set  ████████████████░░░░  R$ 1.600 / 2.000    80%            │
│ ago  ████████████████████  R$ 2.000 / 2.000   100%            │
│ jul  ████████████████████  R$ 2.400 / 2.000   120%            │
│ 12 meses: R$ 21.400 de R$ 24.000  ·  89%                      │
├────────────────────────────────────────────────────────────────┤
│ Patrimônio · R$ 500.000 até dez/2030                           │
│ ██████░░░░░░░░░░░░░░  R$ 148.302 / 500.000    29,7%           │
│ no ritmo atual: dez/2031  ·  14 meses além da meta            │
│ desse progresso: R$ 126.000 aportado · R$ 22.302 de mercado   │
└─ ↑/↓ · Ctrl+A meta · Enter editar · Esc sair ─────────────────┘
```

A linha **"no ritmo atual"** é a única projeção do módulo, e ela usa o ritmo **observado**
— aporte médio e retorno médio realizados —, não um retorno suposto. Isso a diferencia de
[35 — Simulador](35-simulador.md), que é explicitamente uma suposição do usuário.

## 4. O que o módulo não faz

* **Não julga.** Nada de "você está atrasado", nada de vermelho por não bater a meta. Mostra
  o número e a distância. O tom de um programa que cobra é o tom que faz a pessoa parar de
  abrir o programa.
* **Não sugere aporte maior.** Isso é [12 — Rebalanceamento](12-rebalanceamento.md), e lá é
  uma conta pedida, não um conselho oferecido.

## 5. De onde vem o aporte realizado

De [15 — Lançamentos](15-lancamentos.md), tipo `aporte`. Sem lançamentos, a meta de aporte
não tem como ser medida — e a tela diz isso e manda para lá, em vez de mostrar zero.

Mostrar zero seria a mesma falha que [13 §6](13-patrimonio.md#6-erros-e-degradação) evita:
ausência de dado apresentada como dado.

## 6. Teclas

| Tecla | O que faz |
| --- | --- |
| ↑ / ↓ | anda |
| `Ctrl+A` | nova meta |
| `Enter` | edita |
| `Del` | remove |
| `Esc` | sai |

## 7. Como validar

* Meta de aporte sem lançamentos: diz que falta registrar aportes, não mostra 0%.
* Aporte de R$ 2.400 numa meta de R$ 2.000: 120%, e a barra não passa da largura.
* "No ritmo atual" com retorno zero: a data bate com `(meta − atual) / aporte mensal`.
