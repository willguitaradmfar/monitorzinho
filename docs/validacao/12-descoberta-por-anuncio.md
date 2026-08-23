# 12 — Descoberta por anúncio (mDNS e SSDP)

**Entregue em:** v0.21.0 · **Tipo:** melhoria do scanner de rede
(`src/tools/mdns.rs` novo, `src/tools/net.rs`, `dns/wire.rs` abriu o parser)

## O que mudou

A varredura provava que um host existe — respondeu, recusou, ou está na tabela de
vizinhos. O que ela não sabia dizer é que a coisa muda no `.112` é **uma
impressora**, e que o `.113` é **uma TV box**. Essa informação está no ar o dia
todo: mDNS e SSDP são como impressoras, TVs, caixas de som, celulares e NAS se
anunciam, e todos respondem a uma pergunta que qualquer um pode fazer.

Antes da sondagem, o scanner agora pergunta uma vez e escuta 4 segundos. O
resultado entra em duas colunas novas: **o nome que o dispositivo se dá** e **o
que ele diz que é**.

```
192.168.68.112   193.3 ms  TCP aberto   38:ca:84:e1:ef:88  HP Inc.    HP Smart Tank 580-59…  ·  80/http, 631/ipp  ·  http (mDNS)
192.168.68.113   155.7 ms  TCP recusado 3c:bd:3e:c5:89:fd  Xiaomi     MIBOX3-efad785458…     ·  googlecast (mDNS)
192.168.68.110             tabela viz.  24:ce:33:0f:1d:82  Amazon     SpotifyConnect #2      ·  spotify-connect (mDNS)
```

### Detalhes que importam

- **Duas rodadas de DNS-SD, não uma.** A pergunta `_services._dns-sd._udp.local`
  responde com *tipos de serviço*, não com dispositivos. Tratar a resposta como
  nome é como seis máquinas diferentes acabam chamadas `_services._dns-sd._udp` —
  foi o primeiro resultado desta implementação. A segunda rodada pergunta quem
  oferece cada tipo, e **aí** vem o rótulo que o dono digitou.
- **Não disputa a porta 5353.** As perguntas pedem resposta unicast, então as
  respostas voltam para uma porta efêmera e o `avahi-daemon` continua dono da
  5353 em paz.
- **Um dispositivo que se anunciou está vivo**, mesmo que ignore ping e recuse
  toda porta — entra na lista com `anúncio` na coluna "como".
- **O nome anunciado ganha do DNS reverso.** "Impressora do escritório" diz mais
  que `192-168-0-47.lan`.
- Reaproveita o parser DNS que já existia: mDNS **é** DNS, e escrever um segundo
  parser quase igual seria pior.

## Como testar

### 1. Rede com dispositivos de verdade

Varra a sua LAN com **Ouvir anúncios** = `sim`.

**Esperado:** a linha `ouvindo anúncios de mDNS e SSDP por 4s…`, depois
`N dispositivo(s) se anunciaram`, e no relatório nomes reais de coisas suas.

**Confira contra o sistema:**
```sh
avahi-browse -a -t -r      # mDNS
# ou
gssdp-discover -t ssdp:all -n 5   # SSDP
```
Os nomes têm que ser os mesmos. Na validação desta entrega apareceram a impressora
(`HP Smart Tank 580-59…`, via `_http._tcp`), duas caixas Amazon
(`SpotifyConnect`, `SpotifyConnect #2`) e uma TV box Xiaomi
(`MIBOX3-…`, via `_googlecast._tcp`).

### 2. O nome não pode ser um tipo de serviço

**Esperado:** nenhuma linha com nome `_services._dns-sd._udp`, `_http._tcp` ou
parecido. Se aparecer, a segunda rodada do DNS-SD quebrou.

### 3. Sem anúncios

**Ouvir anúncios** = `não`: a varredura fica igual à de antes, sem os 4 segundos e
sem as colunas novas. Numa rede sem nada que se anuncie, o resultado é o mesmo com
a opção ligada — só 4 segundos mais lento.

### 4. Convivência com o avahi

```sh
systemctl status avahi-daemon    # tem que continuar rodando e sadio
ss -lunp | grep 5353             # o avahi continua dono da 5353
```
**Esperado:** nenhum conflito, nenhum erro de bind. A varredura usa porta efêmera.

### 5. Dispositivo que só se anuncia

Um dispositivo que ignora ping e recusa todas as portas mas fala mDNS tem que
aparecer com `anúncio` na coluna "como".

### 6. Colunas

**Esperado:** colunas alinhadas mesmo com fabricante de nome comprido — cada campo
é cortado com `…` na largura dele. Se um valor invadir o vizinho (`tabela de
vizinhos24:ce:…`), é regressão.

## Como saber que falhou

- `0 dispositivo(s) se anunciaram` numa rede com impressora, TV ou Chromecast
- Nome de dispositivo que é um tipo de serviço
- Erro ao abrir socket, ou o avahi caindo
- Colunas coladas
- A varredura demorando muito mais que 4s a mais que antes
