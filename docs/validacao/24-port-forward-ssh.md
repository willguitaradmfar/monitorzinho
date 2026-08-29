# 24 — Port forward SSH: o túnel que fica de pé

**Entregue em:** v0.34.0 · **Tipo:** ferramenta nova
(`src/tools/sshfwd.rs`, `Tool::off_note` em `src/tools/mod.rs`, `app.rs`)

## O problema

O túnel de porta por SSH é o `ssh -N -L` que todo mundo sabe de cor — e mora num
terminal esquecido atrás de outras dez janelas. Quando a conexão cai (o notebook
dormiu, o link piscou, o servidor reiniciou), **nada avisa**: a porta some, o
cliente do outro lado começa a dar erro, e a descoberta é sempre pelo caminho
mais longo. Reconectar é ir achar aquele terminal e digitar de novo.

Agora o túnel é uma execução como qualquer outra: guardada, vigiada, ligável e
desligável com espaço — e que **volta sozinha**.

Nos dois sentidos, que é a única coisa em que as duas configurações diferem:

| Sentido | A porta abre | Quem conecta sai por | Serve para |
| --- | --- | --- | --- |
| `local (-L)` | **aqui** | pelo servidor | alcançar o banco que só escuta no loopback de lá |
| `remoto (-R)` | **no servidor** | por esta máquina | mostrar para alguém o que está rodando aqui |

A coluna **Detalhe** diz isso com todas as letras: `-L 127.0.0.1:8080 aqui →
127.0.0.1:5432 lá`.

## A decisão que precisa ser dita

Esta é a **única** ferramenta que roda outro programa. Todo o resto aqui fala o
protocolo na mão, até o formato de fita do DNS — e SSH é exatamente onde isso
deixa de valer a pena. Não pelo protocolo: pelo que existe em volta dele. O `ssh`
lê o `~/.ssh/config`, fala com o agente, confere o `known_hosts`, pula por um
`ProxyJump` e honra os dez anos de opções que uma configuração que funciona
acumulou. Um cliente escrito aqui seria um segundo `ssh`, pior, ignorando tudo
isso — e a máquina em que você precisa de um túnel nunca é a simples.

Duas consequências de ser um processo filho, ambas resolvidas e não torcidas:

- **Ele nunca pergunta nada.** `BatchMode=yes` e stdin em `/dev/null`, porque um
  `ssh` pedindo senha atrás da tela cheia é um travamento sem causa visível.
  Chave com senha, então, só pelo agente.
- **Ele nunca sobrevive ao app.** `PR_SET_PDEATHSIG` antes do `exec`: fechar o
  monitorzinho — ou `kill -9` nele — leva os túneis junto, e as portas voltam.

## Como testar sem mexer em nada

Dá para validar tudo com um `sshd` próprio, na sua conta, numa porta alta, sem
root e sem tocar no `/etc` nem no seu `~/.ssh/authorized_keys`:

```bash
D=/tmp/sshfwd-teste && mkdir -p $D && cd $D
ssh-keygen -q -t ed25519 -f $D/host_key -N ''
ssh-keygen -q -t ed25519 -f $D/client_key -N ''
cp $D/client_key.pub $D/authorized_keys && chmod 600 $D/authorized_keys
cat > $D/sshd_config <<EOF
Port 2222
ListenAddress 127.0.0.1
HostKey $D/host_key
AuthorizedKeysFile $D/authorized_keys
StrictModes no
UsePAM no
PidFile $D/pid
PasswordAuthentication no
EOF
/usr/sbin/sshd -f $D/sshd_config -E $D/sshd.log

# o serviço que o túnel vai alcançar
echo 'oi do alvo' > $D/index.html && (cd $D && python3 -m http.server 19000 --bind 127.0.0.1 &)
```

No fim: `kill $(cat $D/pid)`, o `python3`, e `ssh-keygen -R '[127.0.0.1]:2222'`
se você tiver deixado o campo em «aceitar host novo».

### 1. Um `-L` de ponta a ponta

`a` → **Port forward SSH**. Servidor `127.0.0.1`, porta do SSH `2222`, porta que
abre `127.0.0.1:18095`, conectar em `127.0.0.1:19000`, chave
`/tmp/sshfwd-teste/client_key`. Enter, Enter.

```
curl -s http://127.0.0.1:18095/      # oi do alvo
```

A linha diz `no ar` e, depois do `curl`, `1 conexão` — o número vem do próprio
`ssh`, que a `-v` anuncia cada conexão que atravessa.

### 2. Um `-R`, que é o contrário

Mesma coisa com **sentido** em `remoto (-R)` e outra porta. `curl` nela responde
igual — mas quem abriu a porta foi o servidor, e quem conectou no alvo foi esta
máquina. A coluna Detalhe troca de `aqui →  lá` para `lá → aqui`.

### 3. Volta sozinho

Com o túnel no ar, mate **o `ssh`**, não o app:

```bash
kill -9 $(pgrep -f '18095:127.0.0.1:19000')
```

A linha fica vermelha e diz `caiu · volta em 2s` — ou `5s`, se a conexão que
morreu tinha menos de trinta segundos de vida; o log mostra o código 255 e a
mesma espera. Aguarde: `túnel no ar` aparece de novo e o `curl` volta
a funcionar. O passo cresce (2, 5, 15, 30, 60 s) enquanto as tentativas
fracassam, e **zera** quando uma conexão dura mais de trinta segundos — a espera
existe para o servidor que está desligado, não para o link que pisca uma vez por
dia.

### 4. Desligar solta a porta — e diz onde ela estava

Espaço na linha. `curl` na porta passa a dar recusa (`rc=7`), e a última coisa
no log é a frase desta ferramenta:

```
desligada — a conexão SSH caiu e a porta 127.0.0.1:18095 nesta máquina foi liberada
```

Num `-R` a mesma frase diz **no servidor**, que é a metade que ninguém
adivinharia — e é por isso que existe agora um `Tool::off_note`: as três frases
que o app sabia deduzir sozinho não cobriam esta.

### 5. Nada sobrevive ao app

Com os dois túneis no ar, `kill -9` no **monitorzinho**. Nenhum `ssh` fica para
trás e as duas portas voltam a ser livres:

```bash
pgrep -af 19000 ; curl -s -m 2 http://127.0.0.1:18095/   # nada, e rc=7
```

### 6. O log é o que responde quando não sobe

Aponte para uma chave que o servidor não conhece. A linha fica vermelha, e o log
termina com `Permission denied (publickey)` precedido de todas as chaves que
foram oferecidas. Uma porta remota ocupada termina em `remote port forwarding
failed for listen port ...` — o `ExitOnForwardFailure=yes` existe para isso: sem
ele a conexão fica de pé encaminhando nada, o que daqui é idêntico a um túnel
que funciona.

Toda tentativa começa registrando a linha de comando inteira. Copiar e colar num
terminal é o caminho mais curto para o resto da resposta.

### 7. O que o formulário já sabe

O campo **Servidor SSH** oferece os `Host` do seu `~/.ssh/config`, com o
`HostName` de cada um ao lado (`~/.ssh/config · 10.0.0.5`) — `Host *` fica de
fora, que não é servidor nenhum. O campo **Chave privada** oferece as chaves de
`~/.ssh`, achadas pelo `.pub` ao lado. **Usuário** e **Porta do SSH** vazios
deixam o `ssh` responder pelo `~/.ssh/config`: um apelido que já diz `Port 2222`
continuaria valendo, e um `-p 22` posto por um formulário que ninguém preencheu
o atropelaria calado.

### 8. Encadeia

`Ctrl+P` numa execução `-L` no ar oferece **túnel gravando o tráfego de
127.0.0.1:18095**, além do endereço do servidor. É assim que se lê o conteúdo do
que passa por dentro de um túnel SSH: o túnel gravador na frente, o SSH atrás.
