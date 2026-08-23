# 10 — Modo vigia no inspetor de certificado

**Entregue em:** v0.19.0 · **Tipo:** melhoria (`src/tools/cert.rs`, `tools/mod.rs`, `app.rs`)

## O que mudou

O inspetor lia o certificado quando você abria a execução, e só. Mas um
certificado **vence numa data**, não quando alguém resolve olhar — e a leitura
que interessa é a que acontece sem ninguém pedir.

Dois campos novos:

| Campo | O que faz |
| --- | --- |
| **Vigiar** | `não` (leitura sob demanda, como antes), `a cada 1h`, `a cada 6h`, `a cada 24h` |
| **Alertar abaixo de (dias)** | limite abaixo do qual toda checagem grita; padrão 30 |

Com um intervalo escolhido, a execução deixa de ser sob demanda: ela roda sozinha
desde que é criada e volta rodando quando o app reabre.

### O que ela diz, e quando

- **primeira checagem**: o relatório completo, igual ao de sempre
- **checagens seguintes**: *uma linha* — sujeito, emissor, dias restantes. Uma
  vigia que reimprime quarenta linhas por hora é uma vigia que ninguém lê
- **abaixo do limite**: a linha vira alerta vermelho
- **certificado trocado**: comparado por impressão SHA-256; quando muda, avisa e
  **relê tudo**, porque uma renovação é exatamente quando o relatório inteiro
  volta a importar

### Mudança estrutural que veio junto

`Tool::on_demand` passou a receber os parâmetros. Não era possível antes ter uma
ferramenta que é sob demanda com uma configuração e contínua com outra — e é
exatamente o que "ler um certificado" e "vigiar um certificado" são: a mesma
ferramenta com intervalo diferente.

## Como testar

### 1. Vigia simples

Crie: alvo `example.com`, **Vigiar** = `a cada 1h`, **Alertar abaixo de** = `90`.

**Esperado imediatamente**, sem abrir a execução:
- estado `rodando` (não `pronta`)
- coluna Resultado com `vence em N dias`
- ao abrir o log: o relatório completo, e em **Avaliação** a linha
  `⚠ vence em 65 dias — renove antes que vire incidente`, porque 65 < 90

Baixe o limite para `10` e recrie: a avaliação volta a `nada a apontar` — é o
mesmo certificado, o que mudou foi o que você considera perto.

### 2. O limite vale nos dois modos

Este foi um defeito real durante o desenvolvimento: o limite configurado só valia
nas checagens compactas, e o relatório completo continuava usando 30 fixo. Se
`Alertar abaixo de 90` não gerar alerta num certificado com 65 dias, é essa
regressão.

### 3. Checagem compacta

Espere o intervalo (ou use `a cada 1h` e edite para um alvo que você controla).
**Esperado:** uma linha só por checagem, não o relatório inteiro.

### 4. Certificado trocado

Com um servidor local:
```sh
openssl req -x509 -newkey rsa:2048 -keyout k.pem -out c.pem -days 30 -nodes -subj "/CN=teste.local"
openssl s_server -cert c.pem -key k.pem -accept 8443 -quiet
```
Vigie `127.0.0.1:8443`. Depois gere **outro** certificado e reinicie o `s_server`.

**Esperado:** na checagem seguinte, `o certificado mudou desde a última checagem —
releitura completa`, seguido do relatório inteiro do novo.

### 5. Volta rodando

Feche o app e abra: a vigia tem que voltar `rodando` e checar sozinha. Uma
execução com **Vigiar** = `não` volta `pronta`, sem fazer nada — é a diferença
que o `on_demand` por parâmetro implementa.

### 6. As outras ferramentas não mudaram

Scanner de portas, DNS, scanner de rede e rota continuam sob demanda: criados,
ficam `pronta` e não fazem nada até você abrir. Se alguma passar a rodar sozinha,
a mudança do `on_demand` quebrou algo.

## Como saber que falhou

- Vigia criada e ficando `pronta`
- Relatório completo a cada checagem
- Limite de dias ignorado no relatório completo
- Troca de certificado passando despercebida
- Ferramenta sob demanda passando a rodar sozinha
