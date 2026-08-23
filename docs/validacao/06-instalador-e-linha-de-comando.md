# 06 — Instalador confiável e `--version`

**Entregue em:** v0.15.1 · **Tipo:** melhoria (`install.sh`, `src/main.rs`)

## O que mudou

Três defeitos do instalador e duas asperezas do binário.

### 1. Instalado como root, ficava fora do PATH

O instalador sempre usava `~/.local/bin`. Como root, isso é `/root/.local/bin`,
que **não está no PATH do root** na maioria das distribuições — foi exatamente o
que aconteceu na VPS: o binário estava lá, e `monitorzinho` respondia
"command not found". Agora, como root, instala em `/usr/local/bin`; como usuário
comum, segue em `~/.local/bin`. `DEST_DIR=/onde/quiser` sobrepõe os dois.

### 2. Download interrompido destruía a instalação boa

`curl -o "$dest"` **trunca o arquivo de destino antes** de saber se o download vai
dar certo. Uma queda de rede no meio trocava um programa que funcionava por um
arquivo pela metade. Agora baixa para um temporário ao lado, e só então um `mv`
— um rename, atômico: ou é o binário velho, ou é o novo, nunca metade de um.

### 3. Não verificava nada

A release publica um `.sha256` ao lado do binário e ninguém olhava. Agora
confere, e **recusa a instalação** se não bater, dizendo os dois hashes.

### 4. `--version` e `--help`

O binário não sabia dizer a própria versão a não ser abrindo a interface inteira.
Agora responde `monitorzinho 0.15.1`, e o instalador imprime isso no fim, como
confirmação do que acabou de instalar.

### 5. Sem terminal, erro em português

Rodar com a saída redirecionada dava `Error: Os { code: 6, kind: Uncategorized }`.
Agora diz o que é e o que fazer.

## Como testar

### Instalação como usuário comum
```sh
DEST_DIR=/tmp/mz-teste sh install.sh
```
**Esperado:** `Checksum verified.`, `Installed to /tmp/mz-teste/monitorzinho`, a
linha de versão, e o aviso de PATH (porque `/tmp/mz-teste` não está nele).

### Instalação como root
```sh
sudo sh install.sh
command -v monitorzinho    # /usr/local/bin/monitorzinho
monitorzinho --version
```
**Esperado:** vai para `/usr/local/bin` e responde pelo nome, sem mexer no PATH.

### Checksum errado é recusado
```sh
# aponte o script para um asset e um .sha256 que não combinam, ou corrompa o temporário
```
**Esperado:** `Error: checksum mismatch — refusing to install.` com os dois
hashes, e **o binário antigo intacto** — confira com `monitorzinho --version`.

### Download interrompido não destrói o que existe
```sh
monitorzinho --version          # anote
# derrube a rede no meio de um install (ou aponte a URL para algo inexistente)
sh install.sh ; monitorzinho --version   # tem que responder a mesma versão de antes
```
Confira também que não sobrou lixo: `ls -a /usr/local/bin | grep monitorzinho` só
pode mostrar o binário, nenhum `.monitorzinho.XXXXXX`.

### Linha de comando
```sh
monitorzinho --version     # monitorzinho X.Y.Z
monitorzinho -V            # idem
monitorzinho --help        # descrição e as teclas principais
monitorzinho --bobagem     # erro em stderr, sai com código 2
monitorzinho > /dev/null   # "precisa de um terminal", sai com código 1
echo $?
```

## Como saber que falhou

- Instalar como root e `command -v monitorzinho` não achar nada
- Instalação sobrevivente a um checksum errado
- Um download que falha deixando o destino truncado ou um `.monitorzinho.*` para trás
- `--version` abrindo a interface em vez de imprimir uma linha
