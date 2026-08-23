# 03 — Achados com tipo, encadeando ferramenta em ferramenta

**Entregue em:** v0.13.0 · **Tipo:** mudança de estrutura + melhorias
(`src/tools/mod.rs`, todas as ferramentas, detalhes dos painéis)

## O que mudou

Antes, o `Ctrl+P` só oferecia **varrer portas**, e só a partir de achados do tipo
IP, porque cada ferramenta escrevia à mão a sua própria lista de ofertas. Um
domínio encontrado numa investigação DNS não virava nada; um certificado lido não
virava nada.

Agora o achado **tem tipo**, e o que se pode fazer com ele é decidido pelo tipo,
num lugar só (`tools::offers_for`). Um endereço é um endereço, tenha sido
encontrado por uma varredura de rede, por um DNS ou por um certificado — e o que
um endereço vale é propriedade do endereço, não de quem o achou.

| Tipo de achado | O que é oferecido |
| --- | --- |
| `ip` | varrer portas · ler o certificado (443) |
| `dominio` | investigar o DNS · ler o certificado · varrer portas |
| `mx` | ler o certificado (SMTP 25, com STARTTLS) · investigar o DNS |
| `porta` (`host:porta`) | túnel gravando o tráfego |
| `porta-tls` | ler o certificado · túnel **decifrando** o tráfego |
| `rede` (CIDR) | varrer a rede |

O efeito para quem escreve ferramenta: `Tool::handoffs` agora tem implementação
padrão. **Basta a ferramenta registrar o que achou** com `Recorder::found` e ela
fica ligada a todas as outras — e uma ferramenta nova que consuma endereços passa
a estar disponível a partir de todas as existentes de uma vez.

### Quem passou a publicar achados

| Ferramenta | Publica |
| --- | --- |
| Scanner de rede | `ip` de cada host vivo |
| Investigação DNS | `dominio` do alvo, cada subdomínio existente e cada nome citado nos registros; `mx`; `ip` |
| Scanner de portas | `porta` ou `porta-tls` de cada porta aberta |
| Inspetor de certificado | `ip` resolvido, `dominio` do alvo e **cada nome do SAN** |
| Detalhe de conexão | o endereço remoto, pelo mesmo caminho |
| Detalhe de interface | a rede da interface |

## Como testar

### 1. DNS → tudo

1. `a` → **Investigação DNS** → `example.com` → rodar
2. Com o log aberto, `Ctrl+P`

**Esperado:** a lista traz, para o domínio, `investigar o DNS`, `ler o
certificado` e `varrer portas`; para cada endereço, `varrer portas` e `ler o
certificado ...:443`; e para cada subdomínio encontrado, as mesmas três de
domínio. Use a busca do picker (digite `www`) para achar um nome no meio.

**Confira:** todo endereço listado no log tem que ter as duas ofertas
correspondentes, e nenhum nome deve aparecer duas vezes.

### 2. Certificado → DNS (o caminho inverso)

1. `a` → **Inspetor de certificado** → `example.com` → rodar
2. `Ctrl+P`

**Esperado:** ofertas para o IP resolvido **e para cada nome do SAN** do
certificado. Um certificado é uma das melhores fontes de nomes que existe — é
esse o caminho que antes não existia.

Nomes com curinga (`*.example.com`) **não** aparecem: não nomeiam host nenhum.

### 3. Scanner de portas → certificado e túnel

1. Varra `example.com` porta `443`
2. `Ctrl+P`

**Esperado:** `ler o certificado de example.com:443` **e** `túnel decifrando o
tráfego de example.com:443` — este último já vem com TLS ligado no destino, que é
o que faz o log do túnel mostrar texto claro em vez de bytes cifrados.

Varra uma porta sem TLS (ex.: 80): a oferta vira `túnel gravando o tráfego`, sem
TLS, e não há oferta de certificado.

### 4. MX

Investigue um domínio que receba e-mail (`gmail.com`, `uol.com.br`).

**Esperado:** para cada MX, `ler o certificado de <mx> (SMTP, porta 25)` — já com
STARTTLS configurado — e `investigar o DNS de <mx>`.

**Caso de borda que já mordeu:** `example.com` publica MX nulo (`.`, RFC 7505).
Esse achado **não** pode aparecer — nem como "ler o certificado de  (SMTP...)"
com o nome vazio. Achado vazio é recusado no momento de registrar.

### 5. Painéis (aba 2)

- Detalhe de uma **conexão** com endereço remoto público → `Ctrl+P` traz relay
  para as duas pontas **mais** varrer portas e ler certificado do remoto.
  Conexão para loopback não oferece as duas últimas — não há o que varrer ali.
- Detalhe de uma **interface** com IPv4 → oferece varrer a rede dela.

### 6. Sem duplicatas

Investigue um domínio grande (`google.com`). Um mesmo endereço costuma aparecer
como A, como resposta de vários servidores e no reverso.

**Esperado:** uma linha por oferta. A deduplicação é por ferramenta + parâmetros.

## Como saber que falhou

- `Ctrl+P` num log com achados abrindo vazio, ou oferecendo só varredura de portas
- Oferta com nome ou endereço vazio no rótulo
- A mesma oferta repetida
- Uma ferramenta nova que registra `found("ip", ...)` e não recebe ofertas — isso
  significa que ela sobrescreveu `handoffs()` sem precisar
