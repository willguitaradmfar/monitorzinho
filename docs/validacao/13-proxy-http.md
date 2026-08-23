# 13 — Modo proxy HTTP no túnel

**Entregue em:** v0.22.0 · **Tipo:** melhoria do túnel (`src/tools/tunnel.rs`)

## O que mudou

O túnel encaminhava tudo para **um** destino. Isso responde "o que este cliente
está dizendo àquele servidor". Um proxy responde uma pergunta maior — **tudo** o
que um cliente está dizendo, para todo mundo — que é o que se quer quando o
cliente é um programa que você não escreveu e a lista de hosts com que ele fala é
justamente o que você está procurando.

Campo novo **Modo**: `destino fixo` (como antes) ou `proxy HTTP`.

```sh
http_proxy=http://127.0.0.1:8888 https_proxy=http://127.0.0.1:8888 seu-programa
```

### Os dois caminhos

- **Requisição comum** (`GET http://host/path`): legível, então é lida. Entra no
  log inteira, passa pelas regras de rewrite, e a linha de requisição é
  reescrita da forma absoluta que um proxy recebe para a forma de origem que um
  servidor espera — a única parte que **tem** que mudar.
- **`CONNECT host:porta`**: pedido de túnel. O que passa depois é TLS que não
  temos certificado para personificar, então atravessa byte a byte e **só o
  volume é registrado**. O log diz qual host era e quanto passou.

A primeira versão registrava o conteúdo do `CONNECT` também — e o log virava
milhares de linhas de ciphertext, enterrando o que tinha sentido. Agora os
contadores sobem e o log fica quieto.

## Como testar

### 1. Os dois caminhos

Crie: **Modo** = `proxy HTTP`, **Ouvir em** = `127.0.0.1:8888`.

```sh
http_proxy=http://127.0.0.1:8888  curl -s -o /dev/null -w '%{http_code}\n' http://example.com/
https_proxy=http://127.0.0.1:8888 curl -s -o /dev/null -w '%{http_code}\n' https://example.com/
```
**Esperado:** `200` nos dois.

No log:
- conexão #1: `GET http://example.com/ HTTP/1.1  →  example.com:80`, seguido da
  requisição **reescrita** (`GET / HTTP/1.1` com `Host: example.com`) e da
  resposta inteira, legível
- conexão #2: `CONNECT example.com:443 — daqui em diante é TLS de ponta a ponta,
  só o volume é visível`, e **nenhum byte de payload**

A coluna Resultado tem que contar as duas conexões, e os bytes do `CONNECT`
entram no total (`→901.0 B ←6.1 KB` no teste desta entrega).

### 2. Nada de ciphertext no log

Depois de várias páginas HTTPS pelo proxy, o log **não pode** ter blocos de bytes
ilegíveis. Se tiver, a contagem sem registro regrediu.

### 3. Regras de rewrite valem no caminho legível

Ponha uma regra `User-Agent:.*` → `User-Agent: monitorzinho` e faça uma requisição
HTTP simples. **Esperado:** o log mostra antes/depois com a linha alterada
marcada, e o servidor recebe a versão reescrita.

Num `CONNECT` as regras não se aplicam — não há texto onde aplicar.

### 4. Erros

- destino que não resolve → `502 Bad Gateway` para o cliente e a razão no log
- requisição que não é de proxy (um `GET /` direto, sem URL absoluta) →
  `400 Bad Request` e a linha registrada

Teste o segundo com `curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8888/`
(sem `http_proxy`): esperado `400`.

### 5. Combinações recusadas na hora

- **Modo** `proxy HTTP` + **Protocolo** `UDP` → erro no formulário
- **Modo** `proxy HTTP` + **TLS no destino** ≠ `não` → erro explicando que cada
  destino tem o seu TLS e que o `CONNECT` passa cifrado de ponta a ponta

Nenhuma das duas pode criar uma execução que depois falha calada.

### 6. Destino fixo continua igual

Uma execução em `destino fixo` tem que se comportar exatamente como antes,
inclusive com TLS no destino e regras. O relay de dois sentidos passou a ser
compartilhado pelos dois modos — se o túnel comum regrediu, foi aí.

## Como saber que falhou

- `CONNECT` despejando payload no log
- Requisição encaminhada com a URL absoluta na linha de requisição (servidor
  responde 400)
- Volume do `CONNECT` não entrando nos contadores da linha
- Proxy aceitando `UDP` ou TLS no destino e falhando depois
