# 21 — Espaço liga e desliga uma execução

**Entregue em:** v0.28.0 · **Tipo:** recurso novo na aba Ferramentas
(`State::Off`, `Execution::switch_off`, `ExecutionSpec::enabled`, `app.rs`, `ui.rs`)

## O problema

Só havia dois estados úteis: existir e rodar, ou não existir. Para parar um túnel
por uma hora era preciso **remover** — perdendo a configuração, o log e o lugar na
lista — e depois recriar tudo na mão.

Agora **espaço** desliga e liga a execução selecionada. A linha continua ali,
riscada e apagada, com tudo que ela é.

## O que "desligada" significa em cada ferramenta

O estado é um só; o que ele custa depende do que a ferramenta faz — e é isso que o
log dela diz no momento em que desliga:

| Ferramenta | Ao desligar |
| --- | --- |
| Túnel, Receptor de requisições | `a porta foi liberada e ninguém é mais atendido aqui` |
| Latência contínua, Sonda HTTP, Seguir arquivo | `parou de trabalhar` |
| Scanner, DNS, Certificado, Rota… (sob demanda) | `não roda nem quando aberta` |

**Desligada não roda de jeito nenhum.** `Enter` numa sob demanda desligada abre o
log para ler, mas **não dispara** a varredura; `r` não reinicia. Para funcionar,
liga primeiro.

## Como testar

### 1. Desligar solta a porta de verdade

Crie um **Receptor de requisições** em `127.0.0.1:8901`. Confirme que responde:

```
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8901/antes    # 200
```

Selecione a linha e aperte **espaço**. Agora:

```
curl -s -m 2 -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8901/depois   # 000 — recusada
```

Não é só cor: a execução parou e devolveu a porta.

### 2. A linha diz o que é

Com o cursor em **outra** linha (a selecionada é desenhada invertida e cobre a
cor), a linha desligada aparece **apagada e riscada de ponta a ponta**, e a coluna
de estado diz `desligada`.

O rodapé também muda com a linha sob o cursor: numa desligada ele oferece
`espaço ligar · Enter ver o log`, **sem** `r reiniciar` — que ali não faria nada.

### 3. O log continua lá

`Enter` na desligada: tudo que ela registrou antes continua, com uma última linha
explicando o que desligar significou para aquela ferramenta. Desligar não é
remover — o log costuma ser justamente o motivo de estar desligando.

### 4. Sob demanda desligada não roda

Crie um **Scanner de portas**, desligue com espaço e aperte `Enter`. O log mostra
`desligada — não roda nem quando aberta` e **nenhuma varredura acontece**. Aperte
`r`: nada. Aperte **espaço** e depois `Enter`: aí sim varre.

### 5. Sobrevive ao fechamento — sem sair executando

Com a execução desligada, feche (`Ctrl+C` duas vezes) e abra de novo. Ela volta
**desligada**, riscada, e a porta continua livre:

```
curl -s -m 2 -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8901/   # 000
python3 -c "
import json;d=json.load(open('$HOME/.local/share/monitorzinho/tools.json'))
print([(e['tool'], e.get('enabled')) for e in d])"
```

O arquivo guarda `\"enabled\": false`. Um `tools.json` escrito por uma versão
anterior não tem esse campo e é lido como ligado — que é o que toda execução dele
era.

### 6. Editar não liga sozinho

`e` numa execução desligada, mude um parâmetro e confirme: ela continua
**desligada**, com os parâmetros novos. Foi desligada de propósito, e a tecla que
reverte isso é o espaço — não uma confirmação de formulário.

### 7. Ligar recomeça

Espaço numa desligada cria a execução de novo, a partir da mesma configuração:
porta ouvida outra vez, contadores zerados, log novo. É o mesmo que o `r` faz —
uma ferramenta pega porta e threads ao iniciar, não ao levantar um sinalizador.
