# 18 — Repetir requisição: mandar de novo o que passou

**Entregue em:** v0.26.0 · **Tipo:** ferramenta nova + captura no túnel e no receptor
(`src/tools/replay.rs`, `src/tools/payload.rs`, `tunnel.rs`, `listen.rs`)

## O problema

O túnel mostra a requisição passando. A pergunta seguinte é sempre a mesma: **e
se rodar de novo?** Refazer aquilo no `curl` é copiar cabeçalho por cabeçalho — e
o cabeçalho que causa o problema costuma ser justo o que ninguém copiou.

Agora a requisição viaja inteira: os bytes que o túnel viu já vêm preenchidos, e
mandar de novo é uma tecla.

* **Repetir igual** responde "foi coisa minha ou deles?"
* **Editar antes de repetir** responde "qual parte?"

## Como a requisição cabe num campo de texto

Requisição é bytes: CRLF, às vezes corpo que nem texto é. O assistente edita
linhas — e isso vale a pena manter, porque *poder mudar* o caminho ou um
cabeçalho é metade do motivo de repetir. Então os bytes vão escapados como numa
string de C: `\r`, `\n`, `\t`, `\\` e `\xNN` para o resto.

A ida e volta é exata, inclusive para corpo binário. Texto UTF-8 mantém os
acentos em vez de virar uma parede de `\xc3\xa1` — o campo é para ser lido.
(`cargo test payload` cobre os três casos.)

## Como testar

### 1. Um serviço para receber

```
python3 - <<'PY' &
from http.server import BaseHTTPRequestHandler, HTTPServer
import json, itertools
c = itertools.count(1)
class H(BaseHTTPRequestHandler):
    def do_POST(self):
        body = self.rfile.read(int(self.headers.get('content-length', 0)))
        out = json.dumps({"pedido": next(c), "recebi": body.decode()}).encode()
        self.send_response(201); self.send_header('Content-Length', str(len(out)))
        self.end_headers(); self.wfile.write(out)
    def log_message(self, *a): pass
HTTPServer(('127.0.0.1', 8900), H).serve_forever()
PY
```

O contador `pedido` é o ponto: **ele prova que a requisição rodou de novo de
verdade**, e não que a tela repetiu uma resposta guardada.

### 2. Um túnel na frente

Ferramentas → `a` → **Túnel TCP/UDP** → ouvir em `127.0.0.1:8899`, destino
`127.0.0.1:8900`. Depois:

```
curl -s -X POST -H 'X-Teste: abc' -d 'primeiro' http://127.0.0.1:8899/api/pedidos
curl -s -X POST -H 'X-Teste: xyz' -d 'segundo'  http://127.0.0.1:8899/api/outros
```

### 3. O menu de achados

`Enter` no túnel para abrir o log, depois **`Ctrl+P`**. As duas requisições estão
lá, cada uma pela sua linha de requisição:

```
repetir POST /api/pedidos HTTP/1.1
repetir POST /api/outros HTTP/1.1
```

Escolha a primeira. A execução nasce **já com o destino preenchido**
(`127.0.0.1:8900`) — o destino não está na requisição, está na configuração do
túnel, e é o próprio túnel que preenche.

### 4. Mandar

`Enter` abre e manda. O log mostra os bytes que saíram e a resposta inteira, do
mesmo jeito que o túnel mostra tráfego (com `Tab` para hex e busca por texto).
Confira o `pedido` na resposta: **é o próximo número**.

`r` manda de novo. A linha mostra o status e onde o tempo foi:

```
HTTP/1.0 201 Created   conectou em 0.3 ms · primeiro byte 1.4 ms · 206 bytes · tentativa 2
```

### 5. Editar antes de repetir

`e` na execução abre o formulário com a requisição inteira no campo
**Requisição**. Mude `/api/pedidos` para `/api/outros`, ou tire o `X-Teste:`, e
confirme. Repetiu diferente — que é como se descobre qual parte importa.

> Mexeu no corpo? Ajuste o `Content-Length` também. A ferramenta manda **o que
> você escreveu**, byte a byte, sem corrigir nada por conta própria — que é
> exatamente o que se quer de uma ferramenta de repetir requisição.

### 6. Com TLS

Crie uma na mão: destino `example.com:443`, TLS `sim`, requisição
`GET / HTTP/1.1\r\nHost: example.com\r\nConnection: close\r\n\r\n`. O resumo
separa o handshake do resto:

```
conectou em 33.4 ms · TLS 30.1 ms · primeiro byte 47.2 ms
```

`sim, sem validar certificado` serve para certificado interno ou autoassinado —
está escrito por extenso justamente para não passar despercebido.

### 7. Requisição partida em dois pacotes

Requisição não é pacote. Este teste manda o cabeçalho, espera, e só então o
corpo:

```
python3 - <<'PY'
import socket, time
s = socket.create_connection(("127.0.0.1", 8899), 3)
s.sendall(b"POST /lento HTTP/1.1\r\nHost: 127.0.0.1:8899\r\nContent-Length: 21\r\n\r\n")
time.sleep(0.6)
s.sendall(b'corpo em outro pacote')
print(s.recv(400).decode().splitlines()[0]); s.close()
PY
```

No `Ctrl+P` aparece `repetir POST /lento HTTP/1.1`, e ao repetir vão **113 bytes**
— cabeçalho **mais** corpo. É o `Content-Length` que diz onde a requisição acaba;
metade de uma requisição repetida não seria a requisição.

### 8. Do receptor, para outro lugar

O **Receptor de requisições** também captura — e aí o destino é a única coisa que
ninguém sabe. Ouça em `127.0.0.1:8901`, mande um webhook para lá, e peça
`Ctrl+P` → `repetir POST /hook HTTP/1.1`.

Como falta o destino, **o formulário abre sozinho**, com tudo preenchido e o
cursor no campo que falta. Preencha `127.0.0.1:8900` e confirme: o webhook que
chegou ali foi entregue no serviço de verdade.

## Limites, ditos de propósito

| Situação | O que acontece |
| --- | --- |
| `Transfer-Encoding: chunked` | não é oferecida — sem `Content-Length` não dá para saber onde acaba, e um cabeçalho sem corpo deixaria o servidor esperando |
| requisição acima de 64 KB | não é oferecida (um upload não é coisa para caber num campo de texto) |
| mais de 20 requisições distintas | as próximas seguem sendo relaiadas e registradas, e o log **diz** que parou de guardá-las — menu com mil linhas não é menu |
| túnel em modo proxy HTTP | oferece a requisição, mas **sem** destino: ali cada requisição vai para um lugar diferente, então preencher seria chutar |
| túnel TLS opaco (CONNECT) | nada é capturado — é tráfego cifrado de outra pessoa |

## O que não se perde de vista

Nada sai daqui sozinho: é **sob demanda**, como o scanner de portas. Uma
requisição que move dinheiro se repete quando alguém quer, não porque o app
abriu — nem quando a execução volta no próximo início.
