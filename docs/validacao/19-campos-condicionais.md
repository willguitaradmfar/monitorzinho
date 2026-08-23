# 19 — Campos que só aparecem quando fazem sentido

**Entregue em:** v0.27.0 · **Tipo:** melhoria transversal do assistente
(`ParamSpec::only_when`, `ToolWizard` em `app.rs`, `ui.rs`, cinco ferramentas)

## O problema

O formulário de nova execução mostrava **todos** os campos da ferramenta, sempre.
O caso que denunciou: um túnel em **modo proxy HTTP não tem destino** — ele tira
o destino de cada requisição — e o formulário continuava pedindo "Encaminhar
para".

Um campo que não faz nada ali ensina errado. Quem preenche acredita que mudou
para onde o tráfego vai; quem deixa em branco fica se perguntando o que faltou.

## Como funciona

Um `ParamSpec` pode declarar do que depende:

```rust
ParamSpec::text("target", "Encaminhar para", ...).only_when("modo", FIXED)
```

Encadeando, todas as condições precisam valer ao mesmo tempo — o TLS do túnel só
existe em **destino fixo** *e* **TCP**:

```rust
.only_when("modo", FIXED).only_when("proto", TCP)
```

O assistente pula os campos inativos ao desenhar, ao navegar com `↑/↓` e na tela
de confirmação, e reposiciona o foco se a escolha que você acabou de mudar
escondeu o campo em que ele estava.

## Como testar

### 1. Túnel: destino fixo × proxy HTTP

`a` → **Túnel TCP/UDP** → Enter. Em **destino fixo** o formulário tem:

```
Modo · Protocolo · Ouvir em · Encaminhar para · TLS no destino · Regex/replace
```

`→` no campo **Modo** para `proxy HTTP`: **"Encaminhar para" e "TLS no destino"
somem**. Faz sentido: cada requisição diz para onde vai, e o `CONNECT` do HTTPS
já é cifrado de ponta a ponta — daí o proxy recusar TLS por configuração.

### 2. Túnel: TCP × UDP, e o SNI

Volte para destino fixo, vá ao **Protocolo** e escolha `UDP`: **"TLS no destino"
some** — TLS ali é conversa de TCP.

Volte para `TCP`, vá até **TLS no destino** e ligue (`sim`): aparece **"Nome no
certificado"**. Desligue: some de novo. Um SNI sem handshake não vai a lugar
nenhum.

### 3. Latência contínua: a porta

`a` → **Latência contínua**. Em `automático` e em `TCP` existe **"Porta (modo
TCP)"**; em `ICMP` e `UDP` ela **some** — esses dois não têm porta. (O
`automático` mantém porque é justamente ele que cai para TCP quando o sistema não
dá socket ICMP.)

### 4. Receptor: corpo da resposta

`a` → **Receptor de requisições**. Com `HTTP 200`, `HTTP 400` ou `HTTP 500` existe
**"Corpo da resposta"**. Com **`HTTP 204`** ela some — "no content" é o único
status que promete não ter corpo. Com `eco` e `nada` também some, e com protocolo
`UDP` idem: não há resposta HTTP a compor.

### 5. DNS: servidores da propagação

`a` → **Investigação DNS**. Com **"Checar propagação"** em `não`, a lista
**"Servidores da propagação"** some — ninguém vai ser consultado.

### 6. Navegação não para no vazio

Com o túnel em modo proxy, desça com `↑/↓` de cima até embaixo: o cursor **nunca
para** num campo escondido. E mude o modo para proxy **estando com o cursor no
Modo**: o foco continua onde estava, mesmo com dois campos saindo da lista abaixo
dele.

### 7. A confirmação diz o que Enter faz

Na última tela, o rodapé mudou de uma frase única para a verdade de cada
ferramenta:

| Ferramenta | Rodapé |
| --- | --- |
| Túnel / Receptor (escutam) | `Enter inicia agora — a porta passa a ser ouvida imediatamente.` |
| Latência contínua (roda direto) | `Enter inicia agora — começa a trabalhar imediatamente.` |
| Scanner, DNS, certificado… (sob demanda) | `Enter cria a execução — nada roda até você abri-la ou apertar r.` |
| Editando uma existente | `Enter aplica agora — a execução atual para e recomeça…` (ou, sob demanda, `volta a esperar`) |

"A porta passa a ser ouvida" sobre uma medição de latência era a mesma classe de
mentirinha que os campos: pequena, repetida toda vez, e desnecessária.

## O que não mudou

O valor de um campo escondido **continua guardado** e segue sendo salvo — voltar
o modo para trás traz o que você tinha digitado. As validações também ficam:
`start` continua recusando um proxy com TLS, por exemplo, porque um `tools.json`
editado à mão não passa por formulário nenhum.
