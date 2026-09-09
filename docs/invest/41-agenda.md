# 41 — Agenda

**Grupo:** Informação · **Estado:** planejado · **Fase:** 4
**Depende de:** [14 — Proventos](14-proventos.md)
**Precisa de:** nada
**Custo:** nenhum I/O na v1 — os eventos são informados ou vêm dos proventos.

---

## 1. O que responde

**O que vem por aí, e em que dia.**

## 2. O que entra

| Evento | Origem na v1 |
| --- | --- |
| Data-com e data-ex | [14 — Proventos](14-proventos.md), quando informados |
| Pagamento de provento | idem |
| Reunião do COPOM | calendário publicado pelo BCB — datas fixas do ano |
| Divulgação do IPCA | calendário do IBGE — datas fixas |
| Resultados trimestrais | **informado** — não há fonte sem chave |
| Vencimento de renda fixa | do que estiver em [Posições](10-posicoes.md) |
| Feriado de mercado | tabela própria, por praça |

O COPOM e o IPCA são os únicos automatizáveis sem chave, e ainda assim porque são
calendários anuais publicados uma vez — não são uma API, são uma tabela que se atualiza
uma vez por ano. Isso é aceitável e tem que estar escrito no código: uma tabela com ano de
validade, e um aviso quando o ano acabar em vez de silêncio.

## 3. Feriado importa mais do que parece

`Provider::is_open()` decide a cadência de toda a aba
([02 §6](02-dados-e-provedores.md#6-cadência)), e ele precisa saber que a B3 não abre na
sexta-feira santa e que a NYSE não abre no Dia de Ação de Graças. Sem isso, a aba fica
pedindo preço o dia inteiro num dia em que nada muda.

Então a tabela de feriados não é enfeite deste módulo: ela é uma dependência do §6 de
[02](02-dados-e-provedores.md), e este é o lugar onde ela mora e onde se vê o que ela tem.

## 4. A tela

```
┌ Agenda ───────────────────────────────── próximos 30 dias ─┐
│ Setembro                                                    │
│  ●  10 qua   COPOM · decisão de juros                      │
│  ●  12 sex   HGLG11 · pagamento R$ 132,00                  │
│     15 seg   BBAS3 · data-ex                                │
│     19 sex   B3 fechada · feriado                          │
│ Outubro                                                     │
│  ●  02 qui   PETR4 · resultado 3T (informado)              │
│     10 sex   IPCA de setembro                              │
├─────────────────────────────────────────────────────────────┤
│ ● toca a sua carteira                                       │
└─ ↑/↓ · ←/→ mês · Ctrl+A evento · Enter detalhe · Esc ──────┘
```

`●` marca o que toca a carteira. Numa agenda com feriados e indicadores de dois países,
essa marca é o que separa o que é seu do que é pano de fundo.

## 5. Teclas

| Tecla | O que faz |
| --- | --- |
| ↑ / ↓ | anda |
| ← / → | mês |
| `Ctrl+A` | adiciona um evento |
| `Enter` | detalhe |
| `Ctrl+F` | só o que toca a carteira |
| `Esc` | sai |

## 6. Fica de fora na v1

* Calendário de resultados automático. Sem fonte.
* Notificação fora do programa. Alerta é [42](42-alertas.md), e mesmo lá é dentro da tela.
* Sincronização com calendário externo.

## 7. Como validar

* Um provento com data de pagamento futura em [14](14-proventos.md): aparece aqui, marcado.
* Um feriado da B3: aparece, e naquele dia a cadência de cotação cai para o modo de mercado
  fechado.
* Virada de ano com a tabela de COPOM desatualizada: avisa, não fica em silêncio.
