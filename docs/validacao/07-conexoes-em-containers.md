# 07 — Conexões dentro de containers

**Entregue em:** v0.16.0 · **Tipo:** melhoria estrutural
(`src/monitor/netns.rs` novo, `src/monitor/connections.rs`, `ports.rs`, `mod.rs`, `ui.rs`)

## O problema, medido

Numa VPS com cinco containers conversando entre si o dia inteiro, o painel
**Connections** mostrava **três conexões — as três de SSH**. Não estava lento nem
vazio: estava confiantemente errado, porque o dump netlink `SOCK_DIAG` responde
**pelo namespace de onde é perguntado**, e ele era perguntado do host.

## O que mudou

Agora as conexões de todos os namespaces de rede da máquina entram no painel,
marcadas com o container a que pertencem:

```
TCP [docker-watchtower-1] /watchtower   172.18.0.12:58014 → 54.84.142.142:443   115d1h   -   -
```

Sem `setns` e sem exigir root para o caso comum: `/proc/<pid>/net/tcp` **é** a
tabela de sockets do namespace daquele processo. Ler o processo é ler o namespace.

O que se perde em relação ao netlink são os contadores por socket (`tcp_info` não
existe em `/proc`), então tráfego e taxa aparecem como `-` — **um traço diz "não
medido aqui"; um zero diria "nada passou"**, e a diferença importa.

### Nomes

O nome do container vem do próprio arquivo de estado do runtime
(`config.v2.json`, no diretório do Docker — `/var/lib/docker/...` no comum,
`~/.local/share/docker/...` no rootless). Sem socket do daemon, sem API para
acompanhar. Sem o arquivo, cai para o id curto de 12 caracteres — que é o que o
`docker ps` mostra na outra coluna.

### O que não dá para ver, é dito

Rodando como usuário comum numa máquina com Docker convencional, os namespaces
dos containers pertencem ao root e não abrem. O rodapé do painel passa a dizer:

```
5 containers não legíveis — rode como root para incluí-los
```

Isso veio de uma capacidade nova das tabelas (`TableMonitor::note`): uma tabela
que só enxerga parte do que descreve tem que dizer isso. Um painel incompleto que
não avisa é lido como o quadro inteiro.

## Como testar

### 1. Docker rootless (usuário comum)

```sh
docker ps --format '{{.Names}}'
# escolha um container com conexões e conte a verdade:
pid=$(docker inspect -f '{{.State.Pid}}' <container>)
awk 'NR>1 && $4=="01"' /proc/$pid/net/tcp | wc -l
```
Abra a aba 2, painel **Connections** (tecla `2`).

**Esperado:** o mesmo número de linhas `[<container>]`, com os mesmos endereços.
Tráfego e taxa em `-`.

### 2. Docker convencional, como root

```sh
sudo monitorzinho
```
**Esperado:** conexões dos containers **com o processo atribuído** (`/watchtower`,
`postgres`, …) — como root os descritores do container são legíveis. Confira a
contagem por container do mesmo jeito do item 1.

Na VPS de teste isto levou o painel de "3 conexões SSH" para as 7 do watchtower
mais as do host, com nome amigável em todas.

### 3. Como usuário comum numa máquina com Docker de root

**Esperado:** o rodapé do painel diz quantos containers ficaram de fora, e o
número tem que bater com `docker ps -q | wc -l`. Contar caminhos de cgroup em vez
de identidades de container inflava esse número (dava 7 para 5) — se voltar a
divergir, é regressão.

### 4. Detalhe de uma conexão de container

`Enter` numa linha `[nome]`.

**Esperado:**
- `Container  <nome> — namespace de rede próprio, lido por /proc`
- Em Tráfego: `Contadores indisponíveis para conexão de container — /proc não traz
  tcp_info, só as filas abaixo`, e as filas de recepção/envio preenchidas
- **Sem** a seção "Caminho" (RTT, cwnd) — ela vem do `tcp_info`, que não existe aqui
- **Sem** os gráficos de taxa no topo — não há o que traçar

### 5. Dois containers da mesma imagem

Suba dois containers iguais que falem com o mesmo destino.
**Esperado:** duas linhas distintas, uma por container. A identidade de uma linha
inclui o namespace justamente por isso; sem isso as duas viravam uma linha que
pisca entre as duas conexões.

### 6. Máquina sem container nenhum

**Esperado:** nada muda, nenhum rodapé extra, mesmo custo de antes.

## Como saber que falhou

- Painel mostrando só conexões do host numa máquina com containers ativos
- Contadores de tráfego (em vez de `-`) numa linha de container: seriam inventados
- Número de "não legíveis" diferente de `docker ps -q | wc -l`
- Duas linhas idênticas para containers diferentes, ou uma linha piscando entre eles
- Seção "Caminho" numa conexão de container (viria toda zerada)
