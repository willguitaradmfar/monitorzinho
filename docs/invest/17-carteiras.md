# 17 — Carteiras recomendadas

**Grupo:** Carteira · **Estado:** construído
**Depende de:** [10 — Posições](10-posicoes.md), [20 — Cotações](20-cotacoes.md)

---

## 1. O que responde

**Para onde esta carteira deve ir, e o que fazer no próximo aporte para chegar lá.**

Uma carteira recomendada é uma lista de ativos com o peso que cada um deve ter e o preço
máximo que se aceita pagar por ele. Ela é o **alvo**; as `Position` são o que se tem. A
tela mostra a distância entre os dois.

## 2. O vínculo mora na posição, e é um só

Cada ativo diz de qual carteira recomendada ele é — e **de uma só**. O campo fica no
formulário de [Posições](10-posicoes.md), como escolha entre as carteiras cadastradas, e
`Edit::SetCarteiraDoAtivo` o aplica a **todas as linhas daquele papel**, em qualquer
corretora: o vínculo é do ativo, não da linha. Duas linhas discordando fariam o mesmo
dinheiro contar em duas carteiras.

É esse vínculo que faz o total de cada carteira ser **o dela**. Uma carteira recomendada
de ações não pode ser diluída pelo Tesouro que está fora dela, e usar o patrimônio inteiro
como denominador daria alvos que nunca se alcançam.

## 3. A conta

Em `invest/carteiras.rs`, fora da tela e com teste. Para cada ativo do alvo:

```
alvo_valor = total_da_carteira × percentual / 100
delta      = alvo_valor − quanto_ele_vale_hoje
quantidade = delta / preço_de_agora
```

Reais, pontos percentuais e quantidade são **a mesma diferença dita de três jeitos**, e
cada um responde a uma pergunta: quanto dinheiro separar, quão longe do alvo, e quantas
ações mandar comprar. A tecla `u` alterna a unidade das barras.

`Enter` edita o alvo e o teto da linha sob o cursor — a mesma tela que `Ctrl+T` usa para
adicionar, porque gravar **substitui** o alvo daquele ativo em vez de somar outro:
adicionar e editar são a mesma operação, e uma tela só é uma tela a menos para divergir.

Três decisões que a conta carrega:

* **O teto governa a compra, não a venda.** Vender um papel que passou do preço máximo
  não é problema — é o que se costuma querer. Quem desenha a linha olha o sinal do delta
  junto com o teto; a primeira versão escrevia «acima do teto» no lugar de «vender» e
  escondia a ordem que importava.
* **O teto suspende a compra, não o alvo.** Um ativo acima do preço máximo continua com o
  alvo dele; o que muda é que a linha diz para não comprar agora. Tirá-lo da conta
  redistribuiria o dinheiro dele entre os outros — decisão de quem investe, não de uma
  fórmula.
* **Alvo zero é alvo.** Um papel que está na carteira e saiu da recomendação não some da
  tela: ele é justamente o que precisa ser desfeito. Mas uma carteira **sem alvo nenhum**
  é outra coisa: pela regra, todo ativo dela pediria venda, o que numa carteira
  recém-criada vira «venda tudo». Ela não está dizendo isso — está dizendo que ainda não
  foi configurada, e é isso que a tela mostra.
* **Os percentuais não são normalizados.** Somar noventa é uma carteira com dez por cento
  em caixa; somar cento e dez é erro de digitação. Os dois são informação, e corrigir em
  silêncio esconderia o segundo — a tela diz a soma quando ela não é cem.

**A ordem é a da recomendação.** Cada alvo tem uma posição numérica — o `1`, `2`, `3` da
lista publicada —, e a tela a respeita. Ordenar pelo que falta comprar impunha um critério
nosso sobre uma lista que já vem priorizada, e a prioridade é parte da recomendação: o
primeiro é o primeiro por uma razão que este programa não conhece. Quem saiu do alvo vai
para o fim: ele não tem posição na lista porque não está mais nela.

O desvio é a soma das distâncias **dividida por dois**: cada real fora do lugar falta num
ativo e sobra noutro, e contá-lo duas vezes mostraria o dobro do desalinhamento real.

## 4. Alvo sem posição é recomendação de compra

Uma carteira pode ter ativos que ainda não se tem: eles entram com valor zero e a linha
diz quanto comprar. É o caso normal de uma recomendação recém-assinada — a carteira
descreve para onde ir, e o caminho começa comprando o que falta.

## 5. Uma carteira, um cartão

Cada carteira recomendada vira **um cartão próprio** na home, com o que fazer em cada
papel e o desvio no rodapé. É o que `InvestModule::cartazes` existe para permitir: quase
todo módulo põe um cartão, este põe um por carteira. A tecla do cartão abre o módulo já
naquela carteira — `InvestModule::open_em`.

Uma linha «3 carteiras» num painel não responde nada sobre nenhuma delas.

O rodapé do cartão — **na moldura**, como o total em Posições e a variação do dia em
Cotações — diz que fatia do patrimônio aquela carteira é, em porcento e em reais. Como
linha de dentro ele disputava espaço com os ajustes e era o primeiro a ser cortado. De dentro da carteira essa pergunta não tem resposta — lá o total dela é sempre cem
por cento —, e é a primeira coisa que se quer saber num painel com várias.
