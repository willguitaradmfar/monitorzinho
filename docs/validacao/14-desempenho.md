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

## Segunda rodada: o lag do Tab (v0.22.2)

O consumo em regime já estava resolvido, mas **trocar de aba continuava travando**
na máquina com k3s. A causa é outra: `switch_tab` amostrava **antes** de desenhar,
e essa amostragem custava, medida com o `--bench` novo:

```
refresh do /proc (sysinfo)     121.6 ms
Ports                          110.9 ms
Connections                    457.8 ms   ← netlink 11 ms · namespaces 440 ms
Interfaces / System Info        27.4 ms
TOTAL                          718.1 ms
```

Quatro mudanças, e o mesmo comando agora dá **291 ms** — e nenhum deles é sentido,
porque a tela é desenhada primeiro:

1. **Ler os 44 namespaces em paralelo** (440 → 111 ms). Cada `/proc/<pid>/net/tcp`
   faz o kernel percorrer e formatar a tabela inteira; o trabalho é dele, não
   nosso, então espalhar por núcleos vira tempo de parede que ninguém espera.
2. **Varrer `/proc/*/fd` em paralelo** (111 → 43 ms) — dezenas de milhares de
   `readlink` numa máquina cheia.
3. **`cwd` lido uma vez por processo, não a cada tick.** Era um `readlink` por
   processo, por tick, e só serve para o detalhe — que agora recarrega o **seu**
   processo por inteiro (`refresh_one`).
4. **O `Tab` desenha antes de amostrar.** A aba aparece na hora com o que já tinha,
   e os números entram logo atrás.

### `--bench`

```sh
monitorzinho --bench
```
Mede uma amostragem da aba Processos — exatamente o que uma troca de aba paga — e
imprime onde o tempo foi, painel por painel. Duas passagens: a segunda é a que
conta.

## Terceira rodada: pagar pelo que a máquina é (v0.23.1)

Duas mudanças que atacam justamente a máquina cara, sem tirar nada da barata.

### Tick proporcional ao custo

Amostrar custa o que a máquina **tem**. Numa que responde em 50 ms, dois segundos
está ótimo; numa que leva 400 ms, gastar um sexto de cada segundo nisso é um
monitor competindo com o que ele monitora.

O intervalo agora é função do custo: até 100 ms, dois segundos como sempre; acima
disso, esticado na mesma proporção, até o teto de 8 s. A conta é uma função pura
(`interval_for`) e o `--bench` imprime o intervalo que ela escolheria:

```
TOTAL                          198.1 ms
TICK                             4.0 s    (intervalo escolhido para este custo)
```

### Conexões de container relidas conforme o que custam

Ler os 44 namespaces custa ~300 ms naquela máquina e ~2 ms numa comum. A leitura
agora vale por **vinte vezes o que custou** (mínimo: o tick; teto: 10 s), então a
máquina barata relê a cada tick e a cara a cada ~6 s — e o painel **diz isso** no
rodapé, em vez de mostrar algo mais velho do que parece:

```
conexões de container relidas a cada 6s (custam 299 ms)
```

### Resultado acumulado no nó com k3s

| | v0.22.0 | v0.22.2 | v0.23.1 |
| --- | --- | --- | --- |
| Uma amostragem | 718 ms | 418 ms | **198 ms** |
| Intervalo | 2 s | 2 s | **4 s** |
| CPU em regime | ~95% de um núcleo | ~20% | **12%** |

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
- `--bench` acima de ~350 ms num nó de porte grande (o valor de referência aqui é
  291 ms com 787 processos e 44 namespaces)
- A aba não aparecer imediatamente ao teclar Tab, mesmo que os números demorem
- `--bench` mostrando `TICK 2.0 s` para um custo acima de 100 ms (adaptação não entrou)
- Rodapé do painel de conexões calado numa máquina onde a releitura está espaçada
