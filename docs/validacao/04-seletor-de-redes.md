# 04 — Seletor de redes no scanner de rede

**Entregue em:** v0.14.0 · **Tipo:** melhoria de usabilidade
(`src/tools/mod.rs`, `src/tools/net.rs`, `src/app.rs`, `src/ui.rs`)

## O que mudou

O campo **Rede** do Scanner de rede era uma caixa de texto vazia. Vazia ela
significa "varrer todas as redes locais detectadas nas rotas" — mas isso só
estava escrito na ajuda, e quem quisesse varrer *uma* rede tinha que ir ler o
CIDR num `ip addr` e digitar de volta.

Agora o campo oferece as redes da própria máquina, percorridas com **←/→**:

```
 ▶ Rede                  ◂ ▏ ▸   todas as redes locais detectadas
 ▶ Rede                  ◂ 10.10.0.0/24▏ ▸   wg0
 ▶ Rede                  ◂ 192.168.68.0/24▏ ▸   wlp3s0
```

A primeira opção é a vazia, e ela passou a se anunciar em vez de ser um campo em
branco que o usuário adivinha. Ao lado de cada rede vai o nome da interface que a
alcança — é a resposta para "qual dessas é o meu wifi", que um CIDR sozinho não
dá.

A lista sai do mesmo `local_networks()` que a varredura vazia já usava (as rotas
do kernel, em `/proc/net/route`), então o seletor e o comportamento do campo
vazio nunca discordam. Redes largas demais ficam de fora pelo mesmo motivo de
sempre: o `docker0` e as bridges são `/16`, e oferecer 65 mil endereços a uma
tecla de distância é oferecer uma varredura que o próprio scanner recusaria
depois.

**O campo continua sendo de texto.** Sugestão aqui é atalho, não conjunto fechado
de respostas: qualquer CIDR pode ser digitado por cima, como antes.

### O mecanismo, para as próximas ferramentas

`ParamSpec` ganhou `suggestions: Vec<Suggestion>`, preenchido pelo builder
`.suggesting(...)`. Cada `Suggestion` tem `value` — o que vai para o parâmetro,
e portanto tem que ser válido para quem o lê — e `note`, que só é exibida. Vale
para qualquer campo de texto de qualquer ferramenta; um campo sem sugestões
continua se comportando exatamente como antes, inclusive ignorando ←/→.

## Como testar

### 1. As redes desta máquina aparecem

1. Aba **Ferramentas** → `a` → **Scanner de rede**
2. Com o foco no campo **Rede**, aperte `→` algumas vezes

**Esperado:** a primeira posição mostra `◂ ▏ ▸   todas as redes locais
detectadas`; cada `→` traz uma rede com o nome da interface ao lado. O rodapé do
formulário mostra `←/→ sugestões · digite para editar`.

**Confira contra o sistema:** `ip -4 route | grep -v default` — cada rota com
máscara `/22` ou mais estreita, fora a `lo`, tem que estar na lista, com a mesma
interface. As `/16` do Docker **não** entram.

### 2. Dá a volta nos dois sentidos

Continue apertando `→` além da última rede: volta para a opção vazia. `←` na
opção vazia vai direto para a última rede.

### 3. Digitar continua funcionando

Com uma rede selecionada, digite `9` — vira texto comum, e a nota da interface
some (o valor deixou de ser um dos oferecidos). Apague e digite
`10.0.0.0/30` inteiro: a execução tem que começar normalmente nesse CIDR.

**Esperado depois de digitar:** `←` leva para a última sugestão, `→` para a
primeira. Nada digitado é substituído sem que a tecla seja apertada.

### 4. A tela de confirmação diz o que o vazio significa

Deixe o campo na opção vazia e siga até **confirmar**.

**Esperado:** a linha lê `Rede   (todas as redes locais detectadas)`, não
`(vazio)`. Um campo opcional de outra ferramenta, deixado em branco, continua
lendo `(vazio)`.

### 5. A varredura respeita o que foi escolhido

Escolha uma rede, confirme e abra a execução com `Enter`.

**Esperado:** o log abre em `pronto para varrer <CIDR> — N endereço(s)`, com o
CIDR escolhido e só ele. Com a opção vazia, aparecem todas as redes, cada uma
seguida da sua interface entre parênteses — que é o formato que já existia.

## Como saber que falhou

- O campo mostra `◂ ▸` mas `←/→` não mudam nada — a lista veio vazia; a máquina
  não tem rota nenhuma com prefixo `/22`+ ou o `/proc/net/route` não foi lido
- Uma rede oferecida que a varredura depois recusa por tamanho
- A nota da interface aparecendo colada num valor digitado à mão
- `←/→` mexendo em campo de texto de outra ferramenta, que não tem sugestão
  nenhuma
- Uma sugestão escolhida que o `parse_cidr` rejeita — `value` tem que ser sempre
  o CIDR puro, o nome da interface é só exibição
