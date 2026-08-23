# 14 — Desempenho: uma varredura por tick, não três

**Entregue em:** v0.22.1 · **Tipo:** correção de regressão + melhoria estrutural
(`src/monitor/netns.rs`, `mod.rs`, `connections.rs`, `ports.rs`, `summary.rs`)

## A regressão, medida

Num nó com **789 processos e 44 namespaces de rede** (k3s), a aba Processos
passou a consumir **95% de um núcleo**, contra 20% na v0.15.1. O Tab demorava
para responder porque trocar de aba faz uma amostragem sincronizada.

| | v0.15.1 | v0.22.0 | v0.22.1 |
| --- | --- | --- | --- |
| Aba Processos | 81–84 ticks/4s | **380** | 73–96 |
| Connections em tela cheia | 98 | 141 | 109 |

(ticks de CPU por 4 segundos; 400 = um núcleo inteiro)

## As três causas

### 1. Testar legibilidade lendo a tabela de sockets de cada processo

A enumeração de namespaces (v0.16.0) percorria os 789 processos e, para cada um
que não estava no nosso namespace, **lia `/proc/<pid>/net/tcp`** só para saber se
conseguia ler. Isso é ~700 varreduras de tabela de sockets do kernel por tick,
para descobrir 44 namespaces.

Agora agrupa por inode do namespace primeiro — um `readlink` por processo, sem
leitura nenhuma — e só então abre **um** pid por namespace.

### 2. Reenumerar namespaces duas vezes por segundo

Container nasce e morre na escala de deploy, não de tick. A lista passou a ser
reenumerada **no máximo a cada 5s**; as tabelas de sockets continuam lidas a cada
tick, porque essas mudam o tempo todo e são o assunto do painel.

### 3. Três painéis fazendo a mesma varredura no mesmo tick

`inode_to_pid()` — que lê `/proc/<pid>/fd` de **todo** processo — era chamado
pelo painel de portas, pelo de conexões e pelo detalhe de processo, cada um
pagando a conta inteira. Idem para a tabela de sockets e a lista de interfaces.

`SystemState` agora guarda essas três respostas por tick (`OnceCell`, limpo no
início de cada refresh): **uma varredura por tick, por mais painéis que peçam.**

E as leituras que não podem mudar com a máquina ligada — DMI, modelo, kernel,
virtualização — são feitas **uma vez na abertura**, não a cada tick.

### Bônus: filtrar antes de formatar

A tabela de sockets de um container é quase toda `LISTEN` e `TIME_WAIT`. O
parser formatava os dois endereços de **cada linha** para depois descartar; agora
o estado é conferido antes, e só o que interessa vira texto.

## Como testar

### 1. Medir, não achar

```sh
# numa máquina movimentada, com o monitorzinho na aba Processos:
P=$(pgrep -n monitorzinho)
A=$(awk '{print $14+$15}' /proc/$P/stat); sleep 4
B=$(awk '{print $14+$15}' /proc/$P/stat); echo "$((B-A)) ticks/4s"
```
**Esperado:** na casa de 70–100 num nó com centenas de processos. Acima de 300 é
a regressão de volta.

Compare por aba: **Visão Geral** tem que ficar perto de zero (2 ticks/4s no teste),
e **Ferramentas** sem execuções, em zero.

### 2. Onde o tempo vai

```sh
timeout 5 strace -c -f -p $(pgrep -n monitorzinho) 2>&1 | tail -15
```
**Esperado:** `readlink` na casa das centenas por 5s, não das dezenas de milhares;
poucas centenas de `openat`. Se aparecerem milhares de `read` em `/proc/*/net/tcp`,
o teste de legibilidade por processo voltou.

### 3. Nada perdido em troca

Com Docker rodando: as conexões de container continuam aparecendo com o nome
(`[rabbitmq]`), o painel Interfaces continua completo, e o System Info continua
mostrando máquina, placa e virtualização.

### 4. Container novo aparece em até 5s

Suba um container e conecte algo dele. **Esperado:** ele aparece no painel dentro
de ~5 segundos — o intervalo do cache da lista de namespaces. Se demorar mais,
o cache não está expirando; se aparecer instantaneamente, ele não está sendo usado.

## Como saber que falhou

- Aba Processos acima de ~150 ticks/4s numa máquina de porte médio
- Trocar de aba demorando visivelmente
- `strace` mostrando leitura de `/proc/*/net/tcp` por processo
- Container novo demorando muito mais que 5s para entrar na lista
