# 09 — Sonda HTTP

**Entregue em:** v0.18.0 · **Tipo:** ferramenta nova (`src/tools/http.rs`)

## O que é

Chama uma URL de tempos em tempos e diz **onde o tempo foi**. "Está no ar" é a
metade fácil; a metade que decide o que fazer em seguida é *qual parte* demorou —
um nome que levou 900 ms para resolver, um handshake de dois segundos, ou um
servidor que aceitou a conexão na hora e depois ficou sentado em cima da
requisição. São três problemas diferentes, e o total esconde os três.

As quatro fases medidas são as mesmas que o `curl -w` imprime: **DNS**, **conexão**,
**TLS** e **primeiro byte**, mais o total.

Fica rodando (não é sob demanda): um endpoint que respondeu uma vez não é um
endpoint no ar, e a forma útil disto é uma coluna de respostas ao longo do tempo
com as falhas se destacando nela.

## Como testar

### 1. Contra o `curl`, no mesmo alvo

Crie a sonda para `https://example.com/`, intervalo 5s. Rode e compare:
```sh
curl -s -o /dev/null -w 'ttfb-líquido %{time_starttransfer} - %{time_appconnect} | total %{time_total}\n' https://example.com/
```
Lembre que os tempos do `curl` são **cumulativos**: o primeiro byte líquido é
`time_starttransfer − time_appconnect`.

**Esperado:** valores na mesma ordem. Na validação desta entrega, a terceira
requisição deu `dns 2 ms · conexão 11 ms · tls 19 ms · primeiro byte 17 ms · total
50 ms` contra `ttfb 18 ms · total 63 ms` do `curl`.

**Dois erros que já foram cometidos aqui e têm que continuar corrigidos:**

- **Primeiro byte inflado em ~40 ms**: falta de `TCP_NODELAY`. A requisição é uma
  escrita pequena; sem isso ela espera o ACK atrasado do outro lado, e a sonda
  reporta 40 ms de fabricação própria como tempo do servidor.
- **TLS inflado (86 ms contra 20 ms) e total 2,5× maior**: o cliente TLS sendo
  construído a cada requisição, o que reparseia o trust store inteiro do sistema.
  Ele é criado uma vez por host e guardado. Se a primeira requisição for bem mais
  cara que as seguintes **em TLS**, é isso voltando.

### 2. Status esperado

Ponha `Status esperado` = `204` num alvo que devolve 200.
**Esperado:** a linha vira erro, com `← esperado 204` no fim, e a coluna Resumo
mostra a taxa de acerto caindo.

`2xx`, `3xx` etc. casam a classe inteira; `200`, `204` casam exatamente.

### 3. Redirecionamento

Alvo `http://example.com/` (sem TLS) com **Seguir redirecionamento** = `sim`.
**Esperado:** uma linha por salto (`301 Moved Permanently → https://...`) e o
status final sendo o do destino.

Com `não`: o 301 é o resultado, e se `Status esperado` for `2xx` conta como falha.

Um laço de redirecionamento termina em `mais de 5 redirecionamentos — parece laço`.

### 4. HEAD

Método `HEAD`: mesma medição, resposta sem corpo — o tamanho cai para o dos
cabeçalhos. É o que um health check costuma usar.

### 5. Falhas

- porta fechada → `não conectou em <ip>:<porta>: Connection refused`
- host que não resolve → `não resolveu <host>`
- certificado inválido → `handshake TLS falhou: …` (a sonda **verifica** o
  certificado, ao contrário do scanner de portas, que só sonda)
- servidor que aceita e não responde → o tempo limite da fase, com a falha contada

Cada falha é uma linha em vermelho e mexe na taxa da coluna Resumo.

### 6. Encadeamento

`Ctrl+P` oferece, a partir dos achados: investigar o DNS do host, ler o
certificado, varrer as portas — e o mesmo para o IP resolvido.

### 7. Remoção imediata

Com intervalo de 1 hora, `Del` tem que remover a execução **na hora**, não ao fim
do intervalo — a espera é feita em fatias justamente por isso.

## Como saber que falhou

- Primeiro byte sistematicamente ~40 ms acima do `curl` (Nagle de volta)
- Primeira requisição muito mais cara em TLS que as seguintes (trust store por requisição)
- Soma das fases maior que o total
- Redirecionamento seguido sem aparecer no log
- `Del` demorando o intervalo inteiro para fazer efeito
