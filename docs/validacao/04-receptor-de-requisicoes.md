# 04 — Receptor de requisições

**Entregue em:** v0.14.0 · **Tipo:** ferramenta nova (`src/tools/listen.rs`)

## O que é

Uma porta que **recebe e anota, e não encaminha nada**. O túnel precisa de um
destino; esta é para quando não existe destino — um webhook que você pediu ao
provedor para chamar, um redirect de OAuth, um dispositivo que faz POST a cada
minuto, um script que alguém jura que está mandando a coisa certa.

`nc -l` também aceita a conexão, e aí os bytes rolam pela tela e acabou. O que
esta acrescenta é o mesmo log das outras execuções — com busca, hex, rolagem, e
ainda lá uma hora depois — mais uma resposta que serve para alguma coisa: um
status e um corpo, para o chamador ver um 200 e parar de tentar de novo, ou ver o
500 que você pediu e mostrar o que ele faz com erro.

| Campo | Opções |
| --- | --- |
| Protocolo | TCP (aceita conexões e responde) · UDP (recebe datagramas) |
| Ouvir em | `0.0.0.0:8080` aceita da rede — é o que um webhook de fora precisa |
| Responder com | `HTTP 200`, `HTTP 204`, `HTTP 400`, `HTTP 500`, `eco`, `nada` |
| Corpo da resposta | vai no corpo do HTTP; JSON é detectado e ganha `application/json` |

Em UDP só `eco` e `nada` significam algo — não há requisição HTTP para responder —
e a linha da execução **diz isso**, em vez de prometer um status que não vai
mandar.

## Como testar

### 1. Webhook com corpo JSON

Crie: TCP, `127.0.0.1:8099`, responder `HTTP 200`, corpo `{"ok":true}`.

```sh
curl -s -i -X POST http://127.0.0.1:8099/webhook/pagamento \
  -H 'X-Assinatura: abc123' -H 'Content-Type: application/json' \
  -d '{"evento":"pagamento.aprovado","valor":4250}'
```

**Esperado no terminal:**
```
HTTP/1.1 200 OK
Server: monitorzinho
Content-Type: application/json     <- detectado pelo corpo começar com {
Content-Length: 11
Connection: close

{"ok":true}
```

**Esperado no log da execução:** a requisição inteira — linha de método, todos os
cabeçalhos (inclusive o `X-Assinatura`) e o corpo — e depois a resposta que saiu.
A coluna Resultado vira `1 requisição`.

### 2. A resposta tem que ser imediata

```sh
curl -s -o /dev/null -w '%{http_code} em %{time_total}s\n' http://127.0.0.1:8099/ping
```
**Esperado:** algo como `200 em 0.05s`. Se demorar ~30s, o fim da requisição não
está sendo detectado e ela só termina pelo tempo de ociosidade — é bug.

Uma requisição HTTP termina na linha em branco, a menos que traga corpo; com
`Content-Length`, termina quando o corpo chegou inteiro. Teste os dois: o GET
acima (sem corpo) e o POST do item 1 (com corpo).

### 3. Payload que não é HTTP

```sh
printf 'linha solta sem http\n' | nc 127.0.0.1 8099
```
**Esperado:** responde assim que o remetente se cala (regra do silêncio), e o log
mostra a linha. Nada que não seja HTTP pode ficar preso esperando cabeçalho.

### 4. Eco

Crie outra: TCP, `127.0.0.1:8098`, responder `eco`.
```sh
printf 'PING alguma coisa\n' | nc 127.0.0.1 8098
```
**Esperado:** volta exatamente `PING alguma coisa`.

### 5. UDP

Crie: UDP, `127.0.0.1:8097`, responder `eco`.
```sh
printf 'ping udp' | nc -u -w2 127.0.0.1 8097
```
**Esperado:** o eco volta, e o log mostra `→ 8.0 B` e `← 8.0 B` no mesmo fluxo.
Cada remetente diferente vira um fluxo numerado próprio.

Troque a resposta para `HTTP 200` com `e` (editar): a linha da execução tem que
passar a dizer **`não responde (UDP)`** — e realmente não responder.

### 6. Erros de propósito

Responder `HTTP 500` com corpo `{"erro":"proposital"}` e apontar seu serviço para
ele: serve para ver o que o seu código faz quando o webhook falha — que é a
metade que ninguém testa.

### 7. Persistência e limpeza

Feche o app e abra de novo: as execuções voltam ouvindo (elas são persistidas
como qualquer outra). `Del` remove, com confirmação, e a porta é liberada —
confira com `ss -ltn | grep 809`.

## Como saber que falhou

- Resposta demorando ~30s (fim de requisição não detectado)
- `Content-Type` `text/plain` num corpo que começa com `{`
- Uma execução UDP dizendo "responde HTTP 200" na lista
- Porta continuar em `LISTEN` depois de remover a execução
- Log sem o corpo da requisição quando ela tem `Content-Length`
