# 02 — Inspetor de certificado TLS/SSL

**Entregue em:** v0.12.0 · **Tipo:** ferramenta nova
(`src/tools/cert.rs`, ampliação de `src/tools/x509.rs`)

## O que é

Uma ferramenta na aba **Ferramentas** que lê o certificado TLS de um host ou IP
por inteiro. Roda sob demanda (nada acontece até você abrir a execução), como o
scanner de portas e a investigação DNS.

O `openssl s_client` mostra os mesmos bytes e deixa a leitura por sua conta. Esta
faz a leitura: a cadeia inteira que o servidor mandou, cada certificado com nomes,
datas, chave, usos e endereços de revogação, as duas perguntas que um navegador
faz (**o nome confere?** e **a cadeia fecha numa raiz confiável?**) respondidas
separadamente, e no fim uma lista do que está errado.

### Por que conecta duas vezes

O primeiro handshake **aceita qualquer certificado** — um certificado que falha
na verificação é justamente o que interessa ler, e recusá-lo responderia a
pergunta com silêncio. O segundo verifica de verdade, e a mensagem de erro dele é
o veredito da linha "Cadeia confiável".

### Parâmetros

| Campo | Para quê |
| --- | --- |
| Alvo | host ou IP, sem protocolo |
| Porta | 443 web, 993 IMAP, 465 SMTP sobre TLS, 5432 Postgres com TLS |
| Nome no handshake (SNI) | vazio usa o alvo; preencha quando o alvo for um IP |
| STARTTLS | `não`, `smtp`, `imap`, `pop3` — para portas que sobem para TLS a pedido |
| Tempo limite | conexão e handshake |

## Como testar

### 1. Alvo público saudável — e conferência contra o openssl

1. `a` → **Inspetor de certificado** → Alvo `example.com`, porta `443`
2. `Enter` até criar, `Enter` para rodar

**Esperado:** seções `Conexão`, `Certificado do servidor`, `Cadeia enviada pelo
servidor`, `Verificação` e `Avaliação`. Em `Avaliação`, `nada a apontar`.

**Confira número por número:**
```sh
openssl s_client -connect example.com:443 -servername example.com </dev/null 2>/dev/null \
  | openssl x509 -noout -serial -fingerprint -sha256 -subject -issuer -dates -ext subjectAltName,keyUsage,extendedKeyUsage,basicConstraints,authorityInfoAccess
```
Têm que bater exatamente: **número de série**, **impressão SHA-256** (byte a
byte), sujeito, emissor, as duas datas, os SAN, os usos e as URLs de OCSP/CRL.

A cadeia mostrada tem que ter a mesma quantidade de certificados que:
```sh
openssl s_client -connect example.com:443 -servername example.com </dev/null 2>/dev/null | grep -c "^ [0-9] s:"
```

### 2. Autoassinado, curto, com nome que não bate — o caso dos alertas

```sh
cd /tmp
openssl req -x509 -newkey rsa:2048 -keyout k.pem -out c.pem -days 5 -nodes \
  -subj "/CN=teste.local/O=Laboratorio" -addext "subjectAltName=DNS:teste.local"
openssl s_server -cert c.pem -key k.pem -accept 8443 -quiet
```
Inspecione `127.0.0.1` porta `8443`.

**Esperado — exatamente estes quatro alertas:**
```
⚠ vence em 4 dias — renove antes que vire incidente
⚠ o nome pedido não está entre os nomes do certificado
⚠ a cadeia não fecha num certificado raiz confiado por esta máquina
⚠ autoassinado — só é confiável para quem já o conhece
```
E o resumo da linha na lista: `4 alerta(s) · CN=teste.local, O=Laboratorio`.

Note que **não** aparece "o servidor mandou só o certificado de ponta": num
autoassinado não há intermediário para faltar, e o alerta sabe disso.

### 3. Certificado vencido

```sh
faketime '2020-01-01' openssl req -x509 -newkey rsa:2048 -keyout kv.pem -out cv.pem \
  -days 1 -nodes -subj "/CN=vencido.local"
openssl s_server -cert cv.pem -key kv.pem -accept 8444 -quiet
```
(sem `faketime`: use `-not_after 20200101000000Z`)

**Esperado:** `VENCIDO há N dias` na coluna Resultado da linha, em maiúsculas, e o
alerta correspondente dizendo que todo cliente que verifica recusa a conexão.

### 4. Chave fraca e assinatura SHA-1

```sh
openssl req -x509 -newkey rsa:1024 -sha1 -keyout kf.pem -out cf.pem -days 30 -nodes -subj "/CN=fraco.local"
openssl s_server -cert cf.pem -key kf.pem -accept 8445 -quiet -cipher 'DEFAULT@SECLEVEL=0'
```
**Esperado:** `chave RSA de 1024 bits — fraca demais para hoje` e `assinado com
SHA-1 — nenhum cliente atual aceita`. (O `SECLEVEL=0` é necessário porque o
próprio OpenSSL se recusa a *servir* uma chave dessas com a configuração padrão.)

### 5. IP com SNI

Inspecione um IP direto (ex.: `104.20.23.154`, porta 443) **sem** SNI:
espera-se `o nome pedido não está entre os nomes do certificado`. Repita
preenchendo **SNI** com `example.com`: o alerta some e "Nome confere" vira sim.
É a prova de que o SNI é enviado e usado na conferência.

### 6. STARTTLS

Contra um servidor SMTP de submissão (porta `587`, STARTTLS `smtp`) ou IMAP
(`143`, `imap`). **Esperado:** a leitura acontece igual. Se o servidor recusar o
STARTTLS, a execução termina com `STARTTLS recusado pelo servidor: <resposta>` —
a resposta dele, não uma mensagem genérica.

### 7. Curinga

Um certificado com `*.exemplo.com` cobre `api.exemplo.com` e **não** cobre
`a.b.exemplo.com` nem `exemplo.com` — é a regra dos navegadores, e há teste
automatizado dela:
```sh
cargo test x509
```
Sete testes: certificado real conferido campo a campo, curingas, calendário
(inclusive 29 de fevereiro), datas UTC e recusa de certificado truncado.

### 8. Encadeamento

Com a execução aberta, `Ctrl+P` oferece **varrer as portas do endereço
inspecionado** — o inspetor publica o IP que resolveu como achado.

## Como saber que falhou

- Qualquer divergência de série, impressão SHA-256 ou datas contra o `openssl`
- "Cadeia confiável: sim" para um autoassinado, ou "não" para um site público
  normal (ex.: google.com)
- Um certificado vencido sem o alerta de vencimento
- Seção `Avaliação` vazia num certificado que você sabe estar quebrado
- Pânico ao ler um certificado exótico: o parser trabalha sobre bytes da rede e
  precisa degradar para "campo ausente", nunca abortar
