//! Os tipos que todo o resto da aba Invest fala.
//!
//! Uma regra atravessa este arquivo inteiro e está documentada em `docs/invest/03`: o
//! **preço médio é informado**, nunca derivado de um histórico de operações. A carteira
//! vem de várias origens — a corretora, uma planilha, outro sistema — e cada uma já traz
//! o preço médio dela, quase sempre sem o histórico que o produziu. Recalcular a partir
//! de lançamentos parciais daria um número errado e sobrescreveria o certo.
//!
//! Por isso `Position::preco_medio` é escrito pela importação e pela edição, e por mais
//! nada. Não existe função neste módulo que o calcule.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Em que mercado um ativo vive. É metade da identidade dele: `PETR4` sozinho não diz a
/// quem perguntar o preço, e um dia o mesmo símbolo existe em dois lugares.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub enum Market {
    #[serde(rename = "B3")]
    B3,
    #[serde(rename = "US")]
    Us,
    #[serde(rename = "BINANCE")]
    Binance,
    /// Séries do Banco Central — CDI, Selic, IPCA. Não é um ativo que se compra; é um
    /// número que se acompanha, e cabe aqui porque o caminho até ele é o mesmo.
    #[serde(rename = "BCB")]
    Bcb,
    /// Par de moedas.
    #[serde(rename = "FX")]
    Fx,
    /// Qualquer coisa que só existe porque alguém digitou: um título privado, um fundo
    /// fechado, um imóvel.
    #[serde(rename = "OUTRO")]
    Outro,
}

impl Market {
    pub const ALL: [Market; 6] = [
        Market::B3,
        Market::Us,
        Market::Binance,
        Market::Bcb,
        Market::Fx,
        Market::Outro,
    ];

    pub fn code(&self) -> &'static str {
        match self {
            Market::B3 => "B3",
            Market::Us => "US",
            Market::Binance => "BINANCE",
            Market::Bcb => "BCB",
            Market::Fx => "FX",
            Market::Outro => "OUTRO",
        }
    }

    /// A moeda em que este mercado cota, quando ela é sempre a mesma. `None` onde
    /// depende do símbolo — um par da Binance pode ser em BRL, em USDT ou em outra coisa.
    pub fn moeda(&self) -> Option<Moeda> {
        match self {
            Market::B3 => Some(Moeda::Brl),
            Market::Us => Some(Moeda::Usd),
            Market::Bcb => Some(Moeda::Brl),
            Market::Binance | Market::Fx | Market::Outro => None,
        }
    }

    fn parse(text: &str) -> Option<Market> {
        Market::ALL
            .into_iter()
            .find(|m| m.code().eq_ignore_ascii_case(text))
    }
}

/// Mercado e símbolo, que é o que identifica um ativo sem ambiguidade.
///
/// Escrito e lido como `MERCADO/SIMBOLO` (`B3/PETR4`), porque é assim que ele aparece no
/// arquivo do usuário e é assim que ele é digitado.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct AssetId {
    pub market: Market,
    pub symbol: String,
}

impl AssetId {
    pub fn new(market: Market, symbol: impl Into<String>) -> Self {
        Self {
            market,
            symbol: symbol.into().trim().to_uppercase(),
        }
    }

    /// Lê `B3/PETR4`. Um texto sem barra **não** vira um palpite de mercado: recusar aqui
    /// é a única forma de o usuário descobrir que a metade que falta importa.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        let Some((market, symbol)) = text.split_once('/') else {
            return Err(format!(
                "«{text}» não diz o mercado. Escreva assim: B3/PETR4, US/AAPL, BINANCE/BTCBRL"
            ));
        };
        let Some(market) = Market::parse(market) else {
            let known: Vec<&str> = Market::ALL.iter().map(|m| m.code()).collect();
            return Err(format!(
                "mercado «{market}» não existe. Os que existem: {}",
                known.join(", ")
            ));
        };
        if symbol.trim().is_empty() {
            return Err("falta o símbolo depois da barra".to_string());
        }
        Ok(AssetId::new(market, symbol))
    }

    /// O nome curto, para uma coluna estreita. É o símbolo sem o mercado — que é como a
    /// pessoa chama a coisa quando fala dela.
    pub fn short(&self) -> &str {
        &self.symbol
    }
}

impl fmt::Display for AssetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.market.code(), self.symbol)
    }
}

impl Serialize for AssetId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for AssetId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        AssetId::parse(&text).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub enum Moeda {
    #[serde(rename = "BRL")]
    Brl,
    #[serde(rename = "USD")]
    Usd,
    #[serde(rename = "EUR")]
    Eur,
    /// Dólar de exchange de cripto. Vale por dólar para conversão, e é uma moeda
    /// diferente porque quem tem USDT sabe que tem USDT.
    #[serde(rename = "USDT")]
    Usdt,
}

impl Moeda {
    pub const ALL: [Moeda; 4] = [Moeda::Brl, Moeda::Usd, Moeda::Eur, Moeda::Usdt];

    pub fn code(&self) -> &'static str {
        match self {
            Moeda::Brl => "BRL",
            Moeda::Usd => "USD",
            Moeda::Eur => "EUR",
            Moeda::Usdt => "USDT",
        }
    }

    pub fn parse(text: &str) -> Option<Moeda> {
        Moeda::ALL
            .into_iter()
            .find(|m| m.code().eq_ignore_ascii_case(text.trim()))
    }
}

/// A que família um ativo pertence. Informada, e não deduzida do símbolo: `HGLG11` é FII
/// e `BBAS3` é ação por convenção de sufixo, mas a convenção tem exceção, e um palpete
/// errado aqui erra a alocação inteira.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub enum Classe {
    #[serde(rename = "acao")]
    Acao,
    #[serde(rename = "fii")]
    Fii,
    #[serde(rename = "etf")]
    Etf,
    #[serde(rename = "cripto")]
    Cripto,
    #[serde(rename = "renda_fixa")]
    RendaFixa,
    #[serde(rename = "caixa")]
    Caixa,
    #[serde(rename = "outro")]
    Outro,
}

impl Classe {
    pub const ALL: [Classe; 7] = [
        Classe::Acao,
        Classe::Fii,
        Classe::Etf,
        Classe::Cripto,
        Classe::RendaFixa,
        Classe::Caixa,
        Classe::Outro,
    ];

    pub fn code(&self) -> &'static str {
        match self {
            Classe::Acao => "acao",
            Classe::Fii => "fii",
            Classe::Etf => "etf",
            Classe::Cripto => "cripto",
            Classe::RendaFixa => "renda_fixa",
            Classe::Caixa => "caixa",
            Classe::Outro => "outro",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Classe::Acao => "ação",
            Classe::Fii => "FII",
            Classe::Etf => "ETF",
            Classe::Cripto => "cripto",
            Classe::RendaFixa => "renda fixa",
            Classe::Caixa => "caixa",
            Classe::Outro => "outro",
        }
    }

    pub fn parse(text: &str) -> Option<Classe> {
        let text = text.trim().to_lowercase();
        Classe::ALL
            .into_iter()
            .find(|c| c.code() == text || c.label() == text || c.label().replace(' ', "_") == text)
    }

    /// O palpite para uma classe que a importação não trouxe. É palpite e é dito como
    /// tal na tela de conferência — nunca gravado calado por cima de algo informado.
    pub fn guess(asset: &AssetId) -> Classe {
        match asset.market {
            Market::Binance => Classe::Cripto,
            Market::Bcb => Classe::RendaFixa,
            Market::Fx => Classe::Caixa,
            Market::Us => Classe::Acao,
            Market::Outro => Classe::Outro,
            // Sufixo 11 na B3 é FII na esmagadora maioria dos casos, e também é unit e
            // alguns ETFs. Por isso é palpite, e por isso a tela mostra que foi palpite.
            Market::B3 => match asset.symbol.ends_with("11") {
                true => Classe::Fii,
                false => Classe::Acao,
            },
        }
    }
}

/// A chave de uma posição, e a razão de ela ter três partes.
///
/// O mesmo ativo em duas corretoras são **duas linhas**, com preços médios diferentes, e
/// as duas ficam visíveis: consolidar apagaria de onde veio o número. É também o que faz
/// a reimportação ser idempotente — reimportar a corretora X substitui as linhas da
/// corretora X e não toca em mais nada.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct PosKey {
    pub fonte: String,
    pub conta: String,
    pub ativo: AssetId,
}

/// Uma linha da carteira.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Position {
    /// De onde esta linha veio: o nome da corretora, ou `manual`.
    pub fonte: String,
    /// Qual conta dentro da fonte. Vazio quando não há mais de uma.
    #[serde(default)]
    pub conta: String,
    pub ativo: AssetId,
    pub classe: Classe,
    pub quantidade: f64,
    /// **Informado, e opcional.** Nada neste programa o calcula — ver o cabeçalho do
    /// módulo.
    ///
    /// `None` é uma posição legítima: quem acompanha um ativo sem lembrar quanto pagou,
    /// ou quem importou de uma fonte que não traz o custo, tem uma quantidade de verdade
    /// e nenhum preço médio. Obrigá-lo forçaria a inventar um número — e um custo
    /// inventado contamina P&L, alocação, yield on cost e imposto sem deixar rastro.
    ///
    /// O que depende dele simplesmente **não é calculado**: a coluna mostra `—`, e o
    /// total diz quantas linhas ficaram de fora da conta de custo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preco_medio: Option<f64>,
    pub moeda: Moeda,
    /// A última cotação conhecida, quando não há provedor ao vivo para este ativo.
    ///
    /// É um número de natureza diferente do preço médio: aquele é custo e é imutável;
    /// este é valor de mercado que por acaso foi informado à mão. Quando um provedor
    /// passar a cobrir o ativo, este campo é ignorado e nada mais precisa mudar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preco_manual: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preco_manual_em: Option<u64>,
    /// Quando a linha inteira foi importada ou editada.
    #[serde(default)]
    pub atualizado_em: u64,
    /// A carteira recomendada de que este ativo participa, pelo nome.
    ///
    /// **Uma só por ativo**, e a regra é mantida na aplicação da edição: marcar um papel
    /// como de uma carteira marca todas as linhas dele, em qualquer corretora. Um ativo
    /// que estivesse em duas carteiras seria contado duas vezes no que aportar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carteira: Option<String>,
}

impl Position {
    pub fn key(&self) -> PosKey {
        PosKey {
            fonte: self.fonte.clone(),
            conta: self.conta.clone(),
            ativo: self.ativo.clone(),
        }
    }

    /// O custo desta posição, na moeda dela. `None` sem preço médio informado — e é a
    /// ausência que se propaga: sem custo não há P&L, e um P&L sobre custo zero seria um
    /// lucro de cem por cento inventado.
    pub fn custo(&self) -> Option<f64> {
        self.preco_medio.map(|pm| self.quantidade * pm)
    }

    /// Confere o que tem que ser verdade antes de gravar, com o formulário ainda aberto —
    /// mesma regra de `Tool::start`: um formulário que aceita e falha depois é um
    /// formulário que perde o que foi digitado.
    pub fn validate(&self) -> Result<(), String> {
        if self.fonte.trim().is_empty() {
            return Err("a fonte não pode ficar vazia — é metade da identidade da linha".into());
        }
        if !self.quantidade.is_finite() || self.quantidade <= 0.0 {
            return Err("quantidade tem que ser um número maior que zero".into());
        }
        // O preço médio é opcional. Quando informado, tem que ser um número; quando não,
        // o que depende dele deixa de ser calculado — e nada é inventado no lugar.
        if let Some(pm) = self.preco_medio
            && (!pm.is_finite() || pm < 0.0)
        {
            return Err("preço médio tem que ser um número não negativo".into());
        }
        if let Some(p) = self.preco_manual
            && (!p.is_finite() || p < 0.0)
        {
            return Err("preço manual tem que ser um número não negativo".into());
        }
        Ok(())
    }
}

/// Um provento recebido ou agendado.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Provento {
    pub ativo: AssetId,
    pub tipo: TipoProvento,
    /// Epoch em segundos do pagamento.
    pub pago_em: u64,
    /// Valor bruto total recebido, na moeda do ativo.
    pub bruto: f64,
    #[serde(default)]
    pub retido: f64,
    pub moeda: Moeda,
    /// Quantidade que se tinha na data. `None` quando não se sabe — e aí o yield on cost
    /// daquele ativo fica marcado como incompleto, nunca estimado.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantidade: Option<f64>,
}

impl Provento {
    pub fn liquido(&self) -> f64 {
        self.bruto - self.retido
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum TipoProvento {
    #[serde(rename = "dividendo")]
    Dividendo,
    #[serde(rename = "jcp")]
    Jcp,
    #[serde(rename = "rendimento")]
    Rendimento,
    /// Devolução de capital. É a única coisa no programa inteiro que teria motivo para
    /// mexer no preço médio — e mesmo assim não mexe: ela **avisa** que o PM deveria ser
    /// reduzido e por quanto, e quem edita é o usuário.
    #[serde(rename = "amortizacao")]
    Amortizacao,
}

impl TipoProvento {
    pub const ALL: [TipoProvento; 4] = [
        TipoProvento::Dividendo,
        TipoProvento::Jcp,
        TipoProvento::Rendimento,
        TipoProvento::Amortizacao,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            TipoProvento::Dividendo => "dividendo",
            TipoProvento::Jcp => "JCP",
            TipoProvento::Rendimento => "rendimento",
            TipoProvento::Amortizacao => "amortização",
        }
    }
}

/// Uma carteira recomendada: **o alvo, não a posição**.
///
/// Ela descreve para onde a carteira deve ir — que ativos, em que proporção, e até que
/// preço vale a pena comprar. O que se tem de fato continua sendo as `Position`; a
/// diferença entre as duas é o que este módulo calcula.
///
/// Os percentuais **não** são normalizados nem obrigados a somar cem. Somar noventa é uma
/// carteira com dez por cento em caixa, e somar cento e dez é um erro de quem digitou —
/// os dois são informação, e corrigir em silêncio esconderia o segundo.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Carteira {
    pub nome: String,
    #[serde(default)]
    pub alvos: Vec<AlvoCarteira>,
}

/// Um ativo dentro de uma carteira recomendada.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AlvoCarteira {
    /// A posição do ativo na recomendação — o `1`, `2`, `3` da lista publicada.
    ///
    /// **É a ordem do analista, e a tela a respeita.** Ordenar pelo que falta comprar era
    /// impor um critério nosso sobre uma lista que já vem priorizada, e a prioridade é
    /// parte da recomendação: o primeiro da lista é o primeiro por uma razão que este
    /// programa não conhece.
    #[serde(default)]
    pub ordem: u32,
    pub ativo: AssetId,
    /// Quanto por cento da carteira ele deve ocupar.
    pub percentual: f64,
    /// O preço máximo que se aceita pagar por ele.
    ///
    /// `None` é «sem teto», e é diferente de zero: zero significaria «nunca comprar». Um
    /// ativo acima do teto **não** some da conta — ele aparece com o aporte suspenso e a
    /// razão ao lado, porque o alvo continua sendo o alvo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub teto: Option<f64>,
}

impl Carteira {
    /// Quanto os alvos somam. Cem é o esperado, e o que difere disso a tela mostra.
    pub fn soma(&self) -> f64 {
        self.alvos.iter().map(|a| a.percentual).sum()
    }
}

/// Tudo que é do usuário. O que fica em `invest.json`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Portfolio {
    /// Presente desde a primeira gravação: um arquivo de carteira vai sobreviver a
    /// mudanças de formato, e migrar sem saber de onde se está migrando é adivinhação.
    #[serde(default = "versao_atual")]
    pub versao: u32,
    #[serde(default = "moeda_base_padrao")]
    pub moeda_base: Moeda,
    #[serde(default)]
    pub posicoes: Vec<Position>,
    #[serde(default)]
    pub watchlist: Vec<AssetId>,
    /// Alvo de alocação por classe, em porcentagem. Não é normalizado calado quando não
    /// soma 100 — ver `docs/invest/11`.
    #[serde(default)]
    pub alvos: BTreeMap<String, f64>,
    #[serde(default)]
    pub proventos: Vec<Provento>,
    /// Setor informado por ativo. Informado, e não raspado: um setor errado é pior que um
    /// setor em branco, porque leva a uma conclusão sobre concentração.
    #[serde(default)]
    pub setores: BTreeMap<String, String>,
    /// Mapeamento de colunas de CSV já aprendido, por fonte. É o que transforma a
    /// importação de uma tarefa chata numa que se usa todo mês.
    #[serde(default)]
    pub mapeamentos: BTreeMap<String, BTreeMap<String, String>>,
    /// As carteiras recomendadas — o alvo para onde a carteira de verdade caminha.
    #[serde(default)]
    pub carteiras: Vec<Carteira>,
    #[serde(default)]
    pub feeds: Vec<String>,
}

pub const VERSAO_ATUAL: u32 = 1;

fn versao_atual() -> u32 {
    VERSAO_ATUAL
}

fn moeda_base_padrao() -> Moeda {
    Moeda::Brl
}

impl Default for Portfolio {
    fn default() -> Self {
        Self {
            versao: VERSAO_ATUAL,
            moeda_base: Moeda::Brl,
            posicoes: Vec::new(),
            watchlist: Vec::new(),
            alvos: BTreeMap::new(),
            proventos: Vec::new(),
            setores: BTreeMap::new(),
            mapeamentos: BTreeMap::new(),
            carteiras: Vec::new(),
            feeds: Vec::new(),
        }
    }
}

impl Portfolio {
    /// Todo ativo que interessa a alguém: o que se tem e o que se acompanha. É o que a
    /// thread de provedores vai buscar.
    pub fn ativos_de_interesse(&self) -> Vec<AssetId> {
        let mut out: Vec<AssetId> = Vec::new();
        for p in &self.posicoes {
            if !out.contains(&p.ativo) {
                out.push(p.ativo.clone());
            }
        }
        for a in self.watchlist.iter() {
            if !out.contains(a) {
                out.push(a.clone());
            }
        }
        out
    }

    /// Insere ou substitui pela chave `(fonte, conta, ativo)`. É o que faz a reimportação
    /// ser idempotente.
    pub fn upsert(&mut self, position: Position) {
        let key = position.key();
        match self.posicoes.iter_mut().find(|p| p.key() == key) {
            Some(slot) => *slot = position,
            None => self.posicoes.push(position),
        }
    }

    pub fn remove(&mut self, key: &PosKey) {
        self.posicoes.retain(|p| p.key() != *key);
    }

    /// As fontes que existem na carteira, na ordem em que aparecem.
    pub fn fontes(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for p in &self.posicoes {
            if !out.contains(&p.fonte) {
                out.push(p.fonte.clone());
            }
        }
        out
    }

    pub fn setor(&self, asset: &AssetId) -> Option<&str> {
        self.setores.get(&asset.to_string()).map(|s| s.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_id_exige_o_mercado() {
        // Sem barra não há palpite: a metade que falta é justamente a que decide a quem
        // perguntar o preço.
        assert!(AssetId::parse("PETR4").is_err());
        assert!(AssetId::parse("XPTO/PETR4").is_err());
        assert!(AssetId::parse("B3/").is_err());

        let asset = AssetId::parse(" b3/petr4 ").unwrap();
        assert_eq!(asset.market, Market::B3);
        assert_eq!(asset.symbol, "PETR4");
        assert_eq!(asset.to_string(), "B3/PETR4");
    }

    #[test]
    fn upsert_e_idempotente() {
        let mut p = Portfolio::default();
        let base = Position {
            fonte: "corretora-x".into(),
            conta: "1".into(),
            ativo: AssetId::parse("B3/PETR4").unwrap(),
            classe: Classe::Acao,
            quantidade: 100.0,
            preco_medio: Some(31.4),
            moeda: Moeda::Brl,
            preco_manual: None,
            preco_manual_em: None,
            atualizado_em: 0,
            carteira: None,
        };
        p.upsert(base.clone());
        p.upsert(base.clone());
        assert_eq!(p.posicoes.len(), 1, "reimportar não pode duplicar");

        // Mesma ação, outra corretora: duas linhas, e as duas ficam.
        let mut outra = base.clone();
        outra.fonte = "corretora-y".into();
        outra.preco_medio = Some(29.8);
        p.upsert(outra);
        assert_eq!(p.posicoes.len(), 2);
    }

    #[test]
    fn preco_medio_nao_e_tocado_por_upsert_de_outra_fonte() {
        let mut p = Portfolio::default();
        let a = Position {
            fonte: "a".into(),
            conta: String::new(),
            ativo: AssetId::parse("B3/PETR4").unwrap(),
            classe: Classe::Acao,
            quantidade: 100.0,
            preco_medio: Some(31.4),
            moeda: Moeda::Brl,
            preco_manual: None,
            preco_manual_em: None,
            atualizado_em: 0,
            carteira: None,
        };
        let mut b = a.clone();
        b.fonte = "b".into();
        b.preco_medio = Some(40.0);
        p.upsert(a);
        p.upsert(b);
        let pm_a = p
            .posicoes
            .iter()
            .find(|x| x.fonte == "a")
            .unwrap()
            .preco_medio;
        assert_eq!(
            pm_a,
            Some(31.4),
            "o PM de uma fonte não pode ser tocado por outra"
        );
    }

    #[test]
    fn validacao_recusa_o_que_nao_pode_ser_gravado() {
        let mut p = Position {
            fonte: "a".into(),
            conta: String::new(),
            ativo: AssetId::parse("B3/PETR4").unwrap(),
            classe: Classe::Acao,
            quantidade: 0.0,
            preco_medio: Some(31.4),
            moeda: Moeda::Brl,
            preco_manual: None,
            preco_manual_em: None,
            atualizado_em: 0,
            carteira: None,
        };
        assert!(p.validate().is_err(), "quantidade zero não é posição");
        p.quantidade = 100.0;
        assert!(p.validate().is_ok());
        p.preco_medio = Some(f64::NAN);
        assert!(p.validate().is_err());
    }

    #[test]
    fn classe_e_palpite_so_onde_nao_foi_informada() {
        assert_eq!(
            Classe::guess(&AssetId::parse("B3/HGLG11").unwrap()),
            Classe::Fii
        );
        assert_eq!(
            Classe::guess(&AssetId::parse("B3/PETR4").unwrap()),
            Classe::Acao
        );
        assert_eq!(
            Classe::guess(&AssetId::parse("BINANCE/BTCBRL").unwrap()),
            Classe::Cripto
        );
    }
}
