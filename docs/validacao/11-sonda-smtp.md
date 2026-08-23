# 11 — Sonda SMTP

**Entregue em:** v0.20.0 · **Tipo:** ferramenta nova (`src/tools/smtp.rs`)

## O que é

O que um servidor de e-mail diz sobre si mesmo antes de alguém tentar mandar
qualquer coisa por ele. O inspetor de certificado já fala STARTTLS, então "o
certificado está bom" se responde ao lado. Sobra tudo o mais que um servidor
anuncia e ninguém confere: quais extensões oferece, se aceita senha em conexão
limpa, que tamanho de mensagem aceita, se o nome com que se apresenta é o dele —
e **se ele repassa e-mail de estranho**, que é a pergunta cuja resposta errada
termina numa blocklist.

**Nada é enviado.** O teste de relay para no `RCPT TO` e faz `RSET`: nesse ponto o
servidor já decidiu, e ir adiante significaria mandar e-mail para alguém de verdade.

## Como testar

### 1. Submissão com STARTTLS (porta 587)

Servidor `smtp.gmail.com`, porta `587`, TLS `automático`.

**Esperado, nesta ordem:**
- `Saudação  220 smtp.gmail.com ESMTP …`
- extensões **antes** do TLS: `SIZE`, `8BITMIME`, `STARTTLS`, `PIPELINING`… e
  **sem `AUTH`**
- `TLS  por STARTTLS — TLSv1_3 · TLS13_AES_256_GCM_SHA384`
- **segundo EHLO**, agora com `AUTH LOGIN PLAIN XOAUTH2 …`
- `Maior mensagem 34.2 MB` (o `SIZE 35882577` traduzido)
- Avaliação: `nada a apontar`

O segundo EHLO é o ponto: **a lista de extensões muda depois do TLS**, e é onde o
`AUTH` aparece. Se a sonda mostrar só uma lista, ou se o log terminar em
`Connection reset by peer` depois do STARTTLS, é a regressão que este item
consertou — falar texto puro com um servidor que está esperando handshake.

### 2. MX de verdade (porta 25) e o teste de relay

Servidor `gmail-smtp-in.l.google.com`, porta `25`, **Testar relay** = `sim`.

> A porta 25 de saída costuma ser **bloqueada em conexão residencial**. Aqui deu
> `não conectou … connection timed out` e o teste real foi feito de um servidor.

**Esperado:**
- `Nome anunciado  mx.google.com` (diferente do host consultado — informação, não erro)
- `AUTH  não oferecido nem com TLS — este não é um servidor de submissão`
- Relay: `MAIL FROM 250 — aceito`, `RCPT TO 550 — recusado, que é o esperado: …`
- Avaliação: `nada a apontar`

### 3. Relay aberto (o achado que importa)

Contra um servidor mal configurado, o esperado é:
```
RCPT TO   250 — ACEITO
⚠ RELAY ABERTO: aceitou destinatário externo vindo de domínio inexistente —
  este servidor será usado para spam e acaba em blocklist
```
Para reproduzir em laboratório, suba um Postfix com `mynetworks` aberto **numa
rede isolada**. Não faça isso em máquina exposta.

### 4. Sem TLS

Servidor que não oferece STARTTLS (ou TLS = `sem TLS`).
**Esperado:** `STARTTLS  NÃO oferecido — esta conexão só existe em texto puro`, e
o alerta correspondente. Se além disso ele anunciar `AUTH`, sai o segundo alerta:
`aceita AUTH antes de qualquer TLS — uma senha enviada aqui viaja legível`.

### 5. TLS direto (porta 465)

Porta `465`, TLS `automático`.
**Esperado:** `TLS  direto na conexão — …` antes de qualquer diálogo, sem STARTTLS.

### 6. Encadeamento

`Ctrl+P` oferece investigar o DNS do servidor, ler o certificado dele e varrer as
portas — o host e o IP entram como achados.

### 7. Respostas multilinha

Toda resposta SMTP pode vir em várias linhas (`250-` continua, `250 ` termina). O
`SIZE`, o `AUTH` e o motivo de uma recusa vêm daí. Se alguma linha de extensão
sumir, o leitor de resposta quebrou.

## Como saber que falhou

- Só uma lista de extensões numa conexão que fez STARTTLS
- `Connection reset by peer` logo após o STARTTLS
- Relay: `MAIL FROM`/`RCPT TO` sem o `RSET` depois (verificar que nada é enviado)
- Um servidor de submissão sem `AUTH` no segundo EHLO
- Alerta de relay aberto num servidor que recusou o `RCPT TO`
