# 20 — A lista mostra quem está falhando agora

**Entregue em:** v0.27.1 · **Tipo:** melhoria (`Execution::failing`, lista da aba Ferramentas)

## O problema

Quando a **Sonda HTTP** recebia um status fora do esperado, o log dela mostrava a
linha em vermelho — mas **só lá dentro**. Na lista de execuções, a linha continuava
com a mesma cara de sempre. A lista é justamente a tela onde se olha várias
execuções de relance, então ela mentia por omissão: só se descobria a falha
entrando na execução.

## O que mudou

Se **o último evento** que a execução registrou foi um erro, a linha inteira fica
vermelha na lista. Quando algo volta a dar certo, ela volta à cor normal
**sozinha**.

É lido do log, não de um sinalizador que cada ferramenta levanta — então vale
para todas do mesmo jeito: sonda HTTP com status fora do esperado, túnel que não
alcança o destino, medição perdendo pacote, o que for.

> "Está falhando agora" é diferente de "já falhou". Uma execução que teve um erro
> há uma hora e está funcionando desde então **não** fica vermelha — senão a cor
> nunca mais sairia, e uma cor que não sai é uma cor que não se lê.

## Como testar

### 1. Uma sonda apontando para o vazio

`a` → **Sonda HTTP** → URL `http://127.0.0.1:8902/`, intervalo `3` s. Nada está
ouvindo nessa porta, então a primeira tentativa já falha: a linha da sonda fica
**vermelha** na lista, com `falhou` / `0 de N ok (0%)`.

Ponha o cursor em outra linha para ver a cor — a linha selecionada é desenhada
invertida e cobre qualquer cor de fundo.

### 2. Suba o serviço

```
python3 -m http.server 8902 --bind 127.0.0.1
```

Em uma ou duas rodadas a linha **volta ao normal**, com `200 em 3 ms`.

### 3. Derrube de novo

`Ctrl+C` no servidor. Na próxima rodada a linha fica **vermelha de novo**.

### 4. Vale para as outras

Crie uma **Latência contínua** para `192.0.2.1` (não roteia): como cada perda é
registrada como erro, a linha fica vermelha enquanto estiver perdendo, e volta
assim que um pacote responder.
