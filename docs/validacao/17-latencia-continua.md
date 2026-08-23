# 17 — Latência contínua: uma medição que vira gráfico

**Entregue em:** v0.25.0 · **Tipo:** ferramenta nova + mudança estrutural
(`src/tools/ping.rs`, `src/monitor/live.rs`, `ChartPanel` em `app.rs`)

## O problema

`ping` no terminal responde "está de pé agora". A pergunta que aparece de verdade
é outra: **o que aconteceu com esse link enquanto eu olhava para outra coisa?** A
perda que importa costuma durar quarenta segundos, duas vezes por hora — ninguém
está com o terminal aberto na hora.

Agora a medição fica rodando como qualquer outra execução, e a última leitura de
cada tick vira uma linha na **Visão geral**, ao lado de CPU, memória e rede.

## A parte estrutural

Os painéis de gráfico eram um vetor fixo montado no boot. Passaram a ser
`ChartPanel` — monitor, histórico, valor absoluto e capacidade **na mesma
estrutura** (eram quatro vetores paralelos indexados em sincronia) — e a lista se
reconcilia a cada tick com as execuções vivas: nasce uma execução que mede, nasce
o painel; ela sai, o painel sai junto.

O histórico é guardado **pelo alvo** (`ping:1.1.1.1`), não pelo número da
execução — número de execução é sorteado a cada início, "a latência até 1.1.1.1"
é a mesma linha de ontem.

## Como testar

### 1. Criar a medição

Aba **Ferramentas** → `a` → **Latência contínua** → Enter.

No campo **Alvo**, `◂ ▸` caminha pelas sugestões que a máquina já conhece:

| Sugestão | De onde vem |
| --- | --- |
| gateway padrão | `/proc/net/route`, rota `0.0.0.0` |
| servidor DNS em uso | `nameserver` do `/etc/resolv.conf` |
| 1.1.1.1 / 8.8.8.8 | referência pública, para separar "meu link" de "a internet" |

Deixe `1.1.1.1` e confirme. A linha aparece na lista **rodando**, com o resultado
à direita.

### 2. Conferir contra o `ping` do sistema

```
ping -c 4 -q 1.1.1.1
```

Os números têm que bater — mesma ordem de grandeza, mesmo mínimo. Se o
monitorzinho estiver medindo por TCP (veja abaixo), compare com o tempo de abrir
uma conexão, que é a mesma volta na rede:

```
python3 -c "import socket,time; t=time.perf_counter(); socket.create_connection(('1.1.1.1',443),2); print((time.perf_counter()-t)*1000)"
```

### 3. Ver por onde ele está medindo

`Enter` na execução abre o log. A primeira linha diz exatamente o que foi feito:

```
medindo 1.1.1.1 (1.1.1.1) por TCP porta 443 — ICMP indisponível neste sistema, a cada 1000 ms
```

São três formas, e o **automático** escolhe:

| Modo | O que manda | Quando serve |
| --- | --- | --- |
| **ICMP** | echo request de verdade | quando o sistema deixa — veja abaixo |
| **TCP** | abre conexão numa porta; **recusada conta igual** (o RST veio do host) | sempre; passa onde ICMP é bloqueado |
| **UDP** | datagrama para uma porta morta, cronometrando o "porta inacessível" | quando ICMP é bloqueado mas o host responde erro |

Para saber se esta máquina dá socket ICMP a quem não é root:

```
sysctl net.ipv4.ping_group_range      # "1 0" = faixa vazia, ninguém pode
id -g                                 # seu grupo precisa estar na faixa
```

Com a faixa vazia o automático cai para TCP **e diz isso no log** — não fica
fingindo que mediu ICMP.

> No modo UDP, lembre que o Linux limita as respostas de erro
> (`net.ipv4.icmp_ratelimit`, 1/s por padrão): intervalo abaixo de 1000 ms lê como
> perda que não existe. O campo de intervalo avisa.

### 4. O gráfico

Aba **Visão geral**. O painel novo se chama **Latência 1.1.1.1** e tem atalho
próprio (`[9]`, `[a]`... conforme a ordem) para abrir em tela cheia.

* **Valor atual e `máx`** no título, como todo painel.
* **Um ponto por tick da interface**, não por pacote — o log tem todos os pacotes,
  o gráfico tem a forma.

### 5. Ele continua desenhando fora da aba

Vá para **Ferramentas** (ou Processos), espere uns 15 segundos, volte para a
Visão geral: **a linha andou**. Os painéis da máquina só rodam na aba deles
(ler `/proc` custa caro); um painel alimentado por ferramenta custa uma leitura
atômica e roda em qualquer aba — que é o motivo de deixar a medição ligada.

### 6. Perda

Crie uma segunda medição com alvo `192.0.2.1` (TEST-NET-1, não roteia) e tempo
limite `800`:

* a lista mostra **`perdido ×N`** — a sequência de perdas seguidas, não só a
  porcentagem, porque um buraco de seis pacotes e seis perdas espalhadas são
  problemas diferentes;
* o resumo mostra `N enviados, nenhum respondido`;
* o painel mostra **`perdido`**. No gráfico a perda é um **zero** — não o tempo
  limite: cronometrar o silêncio seria inventar um número e ainda puxar a média
  para cima.

Com perda intermitente, o resumo ganha `pior sequência N perdidos`.

### 7. Sai a execução, sai o painel

`Del` na execução. A confirmação agora diz **o que é verdade para aquela linha**:

* `Está rodando agora: para na hora.`
* `N conexão(ões) aberta(s) através dela caem junto.` — **só quando há** (um túnel
  com gente conectada é uma perda diferente de uma sonda entre medições);
* `O gráfico dele sai da Visão geral...`

Confirme e volte para a Visão geral: **o painel sumiu**. Crie a mesma medição de
novo na mesma sessão — a linha **continua de onde parou**, porque o histórico é
do alvo.

### 8. Sobrevive ao fechamento

Com a medição rodando, espere o `máx` subir, saia com `Ctrl+C` duas vezes e abra
de novo. A execução volta (como toda execução salva) e o painel volta **com o
histórico**:

```
python3 -c "
import json;d=json.load(open('$HOME/.local/share/monitorzinho/history.json'))
print('ping:1.1.1.1' in d, len(d.get('ping:1.1.1.1',[])))"
```

### 9. Ela conversa com o resto

`Ctrl+P` na execução oferece o endereço medido para as outras ferramentas
(varrer portas, ler o certificado, traçar a rota). E no caminho contrário:
qualquer achado de **IP** ou **domínio** — de um scanner de rede, de uma
investigação DNS, de um certificado — passa a oferecer
**"medir a latência até X continuamente"**.

## O que ficou de fora, e por quê

* **IPv6** — tudo que é feito com socket cru aqui é IPv4; um alvo que só tem AAAA
  é recusado na hora de criar, com a razão escrita.
* **Um ponto por pacote no gráfico** — o gráfico é amostrado pelo tick da
  interface. Quem quer pacote a pacote tem o log, que guarda cada um com número
  de sequência.
