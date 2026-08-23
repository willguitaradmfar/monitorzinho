# 15 — STUN, Wake-on-LAN e portas de saída

**Entregue em:** v0.23.0 · **Tipo:** três ferramentas novas
(`src/tools/stun.rs`, `wol.rs`, `egress.rs` — o código entrou junto com a v0.22.1,
este release é o que as valida e documenta)

Três perguntas pequenas que não tinham onde ser feitas, cada uma respondida por
uma tela própria.

---

## Endereço público (STUN)

O endereço com que esta máquina aparece na internet, a porta que o NAT deu a ela,
e **o que o NAT faz** com as duas.

`curl ifconfig.me` dá o endereço e mais nada — e dá pedindo a um servidor web que
conte, o que funciona até o problema ser justamente UDP não voltar. STUN responde
no nível onde o NAT vive (RFC 5389: cabeçalho de 20 bytes, um cookie mágico, e o
endereço na resposta ofuscado por XOR contra esse mesmo cookie — de propósito,
para que um NAT que reescreve endereços dentro de pacotes não "conserte" a
resposta no caminho).

### Como testar

Rode com os dois servidores padrão e compare:
```sh
curl -s https://ifconfig.me; echo
```
**Esperado:** o mesmo endereço. Na validação: `187.116.88.134` nos dois.

E mais duas coisas que o `curl` não dá:
```
Comportamento do NAT  mesma porta para destinos diferentes — cone, atravessável (P2P funciona)
Porta                 40418 local → 40418 pública (preservada)
```
Dois servidores de operadores diferentes existem por isso: **a comparação é o
teste**. Mesma porta para destinos diferentes = NAT cone, dá para furar; porta
diferente por destino = NAT simétrico, e P2P direto não vai funcionar por mais
que se insista.

Sem resposta de nenhum servidor → `UDP de saída pode estar bloqueado`, que é a
conclusão certa e não um erro genérico.

---

## Acordar máquina (Wake-on-LAN)

Uma máquina desligada com WoL ativo mantém a placa de rede escutando **um único
padrão**: seis bytes `FF` seguidos do MAC dela repetido dezesseis vezes, em
qualquer lugar de qualquer pacote. É o protocolo inteiro.

### Como testar — conferindo o pacote byte a byte

```sh
python3 -c "
import socket
s=socket.socket(socket.AF_INET, socket.SOCK_DGRAM); s.bind(('127.0.0.1',9999))
d,_=s.recvfrom(2048); open('/tmp/wol.bin','wb').write(d)" &
```
Crie a execução com MAC `aa:bb:cc:dd:ee:ff` e **Enviar para** `127.0.0.1:9999`.

```sh
xxd /tmp/wol.bin | head -2
```
**Esperado, exatamente:**
```
00000000: ffff ffff ffff aabb ccdd eeff aabb ccdd
00000010: eeff aabb ccdd eeff aabb ccdd eeff aabb
```
102 bytes (6 + 16×6), e **três envios** — nada responde a um pacote mágico, então
mandar uma vez só é apostar que nenhum se perdeu.

O log diz isso em voz alta: `nada responde a um pacote mágico: espere uns segundos
e procure a máquina com o scanner de rede`. A ferramenta relata **o que fez**, não
o que aconteceu, porque o protocolo não permite saber.

### Encadeamento

O scanner de rede publica o MAC de tudo que vê, então `Ctrl+P` sobre uma varredura
oferece **acordar** qualquer máquina que ele já tenha visto — que é como se
descobre o MAC de uma máquina que agora está desligada.

### Erros

MAC inválido (`aa:bb`, letras fora do hex) → erro no formulário, com o motivo.
Sem permissão de broadcast → erro explícito, não um envio silencioso que não sai.

---

## Portas de saída

A pergunta ao contrário: não o que o destino aceita, mas **o que daqui consegue
partir**. É a resposta para toda uma categoria de tarde perdida — o serviço está
no ar, o endereço certo, o firewall do outro lado aberto, e a conexão nunca chega,
porque a rede *desta* máquina só deixa sair 80 e 443.

Funciona conectando a um host que atende em **qualquer** porta (`portquiz.net`
existe para isso; um servidor seu com um listener genérico serve igual).

### Como testar

Portas `22,25,80,443,587,3306,5432,8080` contra `portquiz.net`.

**Esperado numa conexão residencial brasileira típica:**
```
   22  sai   (202 ms)
⚠  25  BLOQUEADA
   80  sai   (208 ms)
  443  sai   (201 ms)
 5432  sai   (recusada no destino — a saída funciona)
7 de 8 porta(s) saem; 1 bloqueada(s)
```

Dois detalhes que importam:

- **Recusa é sucesso.** Uma conexão recusada é uma *resposta*, e a resposta teve
  que sair desta rede — a porta de saída funciona, mesmo que nada atenda do outro
  lado. Confundir as duas coisas inverteria o resultado.
- **Bloqueio é silêncio.** Uma porta filtrada normalmente não recusa: ela cala, e
  só o tempo limite revela. Por isso o padrão é 3 s e não 300 ms.

**Confirmação cruzada:** o bloqueio da 25 aqui é o mesmo que fez a sonda SMTP
falhar com `connection timed out` contra um MX real (doc `11`) — duas ferramentas
independentes chegando à mesma conclusão sobre a rede.

## Como saber que falhou

- STUN dando endereço diferente do `ifconfig.me`
- STUN dizendo "NAT simétrico" numa rede onde a porta é claramente preservada
- Pacote mágico com tamanho diferente de 102 bytes ou com o MAC errado
- Egress marcando como bloqueada uma porta que o `nc -zv portquiz.net <porta>` abre
- Egress marcando como "sai" uma porta que só deu timeout
