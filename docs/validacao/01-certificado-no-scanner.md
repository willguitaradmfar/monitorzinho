# 01 — Certificado no scanner de portas

**Entregue em:** v0.11.2 · **Tipo:** melhoria de ferramenta existente
(`src/tools/scan.rs`, `src/tools/x509.rs`)

## O que mudou

O scanner de portas já fazia um handshake TLS completo em cada porta aberta para
descobrir se ela fala TLS — e jogava o certificado fora, guardando só a versão do
protocolo e a cifra. O certificado é o que a porta manda para provar quem é, e ele
estava sendo lido e descartado.

Agora a linha de uma porta com TLS traz também **para quem** o certificado foi
emitido, **quem** assinou, **quanto falta** para vencer e **quantos nomes** ele
cobre:

```
443/tcp aberta  ·  10.3 ms  ·  https  ·  TLS (TLSv1_3, TLS13_AES_256_GCM_SHA384)
                ·  example.com, por Cloudflare TLS Issuing ECC CA 3, vence em 65 dias, 2 nomes
```

O DER é lido por um parser X.509 mínimo escrito para isto (`src/tools/x509.rs`) —
mesma escolha do resto do projeto, que já monta e lê DNS byte a byte. Ele lê CN do
sujeito, CN (ou O) do emissor, as duas datas de validade e os `dNSName` do
subjectAltName. Nada mais: sem checar assinatura (o rustls já decidiu isso), sem
outras extensões. Campo que não der para ler vira ausência de uma linha, nunca o
fim da leitura.

## Como testar

### 1. Alvo público, certificado válido

1. Aba **Ferramentas** → `a` → **Scanner de portas**
2. Alvo `example.com`, campo **Portas** `443`, resto no padrão
3. `Enter` até criar, `Enter` na execução para rodar

**Esperado:** a linha da porta 443 traz, depois da cifra, o nome do certificado,
o emissor e "vence em N dias".

**Confira contra a verdade:**
```sh
openssl s_client -connect example.com:443 -servername example.com </dev/null 2>/dev/null \
  | openssl x509 -noout -subject -issuer -enddate -ext subjectAltName
```
O CN do `subject`, o CN do `issuer` e o `notAfter` têm que bater. A contagem de
dias é `notAfter` menos agora, em dias inteiros.

### 2. Certificado com muitos nomes

Alvo `google.com`, porta `443`. Um certificado da Google cobre dezenas de nomes.

**Esperado:** o sufixo `N nomes` com N grande. Com um nome só, esse trecho some —
não escreve "1 nomes".

### 3. Certificado autoassinado / CA interna

Sirva um localmente:
```sh
openssl req -x509 -newkey rsa:2048 -keyout /tmp/k.pem -out /tmp/c.pem -days 3 -nodes -subj "/CN=teste.local"
openssl s_server -cert /tmp/c.pem -key /tmp/k.pem -accept 8443 -quiet
```
Alvo `127.0.0.1`, porta `8443`.

**Esperado:** lê normalmente e mostra `teste.local, por teste.local, vence em 2
dias`. O scanner nunca verificou certificado (é uma sondagem, não uma conexão de
confiança), então autoassinado não é obstáculo.

### 4. Certificado vencido — o caso que importa

```sh
openssl req -x509 -newkey rsa:2048 -keyout /tmp/k2.pem -out /tmp/c2.pem -nodes \
  -subj "/CN=vencido.local" -not_after 20200101000000Z
openssl s_server -cert /tmp/c2.pem -key /tmp/k2.pem -accept 8444 -quiet
```

**Esperado:** `VENCIDO há N dias`, em maiúsculas. É o único estado que a linha
grita, porque é o único que já é um problema.

### 5. Porta que fala TLS mas não manda certificado utilizável

Qualquer serviço com TLS exótico serve. **Esperado:** a linha continua saindo com
`TLS (versão, cifra)` e simplesmente sem o trecho do certificado — a leitura do
certificado nunca pode custar a identificação da porta.

### 6. Testes automatizados

```sh
cargo test x509
```
Cinco testes, incluindo um certificado real do example.com guardado em
`src/tools/testdata/example-com.der` e conferido campo a campo, e um que exige que
um certificado truncado seja **recusado** em vez de adivinhado.

## Como saber que falhou

- Linha da porta 443 sem nenhum trecho de certificado num alvo público conhecido
- Data de vencimento diferente da que o `openssl x509 -noout -enddate` mostra
- Nome do emissor vazio quando o `openssl` mostra um
- Qualquer pânico ou travamento ao varrer uma porta TLS — o parser trabalha sobre
  bytes vindos da rede, e o teste do certificado truncado existe exatamente por isso
