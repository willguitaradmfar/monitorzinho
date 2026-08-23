# 08 — Rota até o host (traceroute)

**Entregue em:** v0.17.0 · **Tipo:** ferramenta nova
(`src/tools/rota.rs`, `src/tools/icmp.rs` novo, `net.rs` deixou de duplicar ICMP)

## O que é

Cada roteador entre esta máquina e um host, com a latência de cada salto. Preenche
o vão entre "o host existe" (scanner de rede) e "o host não responde": um host
mudo pode estar desligado, filtrado na própria porta, ou atrás de um link que
morre três saltos antes — e são três tardes diferentes.

## Como funciona, e por que não precisa de privilégio

O caminho é descoberto pelos **erros ICMP** que um pacote provoca ao ficar sem
saltos. Esses erros nunca chegam na fila normal do socket: o kernel põe na **fila
de erro**, lida com `recvmsg(MSG_ERRQUEUE)`, e ali vem tanto o erro quanto o
endereço do roteador que respondeu.

E é por isso que a sonda é **UDP, não ICMP**. Um socket ICMP não privilegiado
depende de `net.ipv4.ping_group_range`, que estava **vazio nas duas máquinas** onde
isto foi desenvolvido — inclusive para o root. Um socket UDP com `IP_RECVERR` não
depende de ninguém e colhe as mesmas respostas: o roteador responde "tempo
excedido" ao datagrama exatamente como responderia a um eco, e o destino responde
"porta inalcançável", que é como se sabe que chegou. É o que o `tracepath` faz.

A porta usada é a faixa reservada a partir de 33434, pelo mesmo motivo de sempre:
nada escuta lá, então a recusa do destino é confiável.

## Como testar

### 1. Contra o `mtr` ou o `traceroute` do sistema

Crie a execução com alvo `1.1.1.1` (ou um host seu) e rode.
```sh
mtr -n -c 2 -r 1.1.1.1
# ou
traceroute -n 1.1.1.1
```
**Esperado:** mesma quantidade de saltos e os mesmos roteadores, na mesma ordem.

Diferenças **aceitáveis**: um salto ocasional com endereço vizinho (`172.68.16.107`
contra `172.68.16.129`) — é balanceamento de carga, e o próprio `mtr` varia entre
execuções. Diferença **inaceitável**: quantidade de saltos diferente, ou um salto
que aparece numa ferramenta e não na outra de forma consistente.

Na validação desta entrega: 11 saltos nos dois, com os mesmos endereços em 1, 2,
5, 8, 9 e o destino.

### 2. Três sondas por salto

**Esperado:** três tempos por linha. Quando uma se perde, a linha mostra
`(1 de 3 sem resposta)` — perda parcial num salto é informação, não ruído.

### 3. Roteador calado

**Esperado:** `  6   * * *` e a rota **continua**. Roteador configurado para não
responder é comum e não significa que o caminho acabou.

### 4. Caminho que morre

Trace um endereço não roteável (`10.255.255.1`) ou um host atrás de um firewall
que descarta tudo.

**Esperado:** estrelas até o limite, e a mensagem `cinco saltos seguidos sem
resposta — o caminho para aqui`, em vez de encher trinta linhas de estrelas.
A execução termina com `não chegou`.

### 5. Destino que recusa

Trace um host que responda "administrativamente proibido".
**Esperado:** a linha do salto seguida de `<ip> respondeu «proibido
administrativamente» — o caminho termina aqui`, com a razão traduzida do código
ICMP (RFC 792).

### 6. Nomes

Com **Resolver nomes** em `sim`, cada salto ganha o reverso (`_gateway`,
`201-1-226-35.dsl.telesp.net.br`). Em `não`, só os endereços — e a varredura fica
visivelmente mais rápida.

### 7. Encadeamento

Cada salto é publicado como achado do tipo `ip`. Com o log aberto, `Ctrl+P`
oferece **varrer portas** e **ler o certificado** de qualquer roteador do caminho —
sem nenhuma linha de código de oferta escrita nesta ferramenta.

### 8. IPv6

Alvo só-IPv6 → erro no formulário: `… não tem endereço IPv4 — só IPv4 por
enquanto`. É uma limitação declarada, não um silêncio.

## Como saber que falhou

- Coluna inteira de estrelas onde o `mtr` mostra saltos: a fila de erro não está
  sendo lida (`IP_RECVERR` ou o parsing do `cmsghdr`)
- Traceroute que termina no primeiro salto: o `IP_TTL` não está sendo aplicado
- Chegar ao destino e não perceber (segue até 30): a recusa de porta do destino
  não está sendo reconhecida como chegada
- Latências absurdas (ordens de grandeza acima do `mtr`)
