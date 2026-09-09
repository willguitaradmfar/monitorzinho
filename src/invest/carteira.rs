//! A agregação da carteira: o que vale, quanto custou, e o que ficou de fora.
//!
//! Vive fora dos módulos porque quase todos precisam dela — Posições, Alocação,
//! Patrimônio, Risco, Corretoras, Rebalanceamento — e duas cópias da mesma soma seriam
//! dois lugares para discordarem sobre quanto alguém tem.
//!
//! A regra que atravessa tudo aqui: **nunca inventar**. Uma posição sem preço, ou sem
//! câmbio para chegar a BRL, **fica de fora do total, e o total diz que ficou**. Um total
//! que silenciosamente exclui três ativos é ruim; um que silenciosamente os inclui
//! convertidos por um câmbio de três dias atrás é pior, porque parece exato.

use crate::invest::model::{Portfolio, Position};
use crate::invest::provider::{Grade, MarketSnapshot, Quote};

/// Uma posição com tudo que se sabe calcular sobre ela agora.
#[derive(Clone, Debug)]
pub struct Linha {
    pub posicao: Position,
    /// O preço corrente na moeda da posição. `None` quando ninguém sabe dizer.
    pub preco: Option<f64>,
    pub grade: Option<Grade>,
    /// Há quanto tempo esse preço foi obtido, em segundos.
    pub idade: u64,
    /// Qual provedor deu o número — o que responde «a quem reclamar».
    pub fonte: &'static str,
    /// Valor de mercado em BRL. `None` quando falta preço **ou** falta câmbio.
    pub mercado_brl: Option<f64>,
    /// Custo em BRL. `None` quando falta câmbio — o custo em si sempre se sabe.
    pub custo_brl: Option<f64>,
    pub pnl_brl: Option<f64>,
    pub pnl_pct: Option<f64>,
    /// Variação do dia em BRL, quando a fonte deu o fechamento anterior.
    pub dia_brl: Option<f64>,
    pub variacao_pct: Option<f64>,
}

impl Linha {
    /// Se esta linha entra nos totais. Uma que não entra não é escondida — é mostrada com
    /// `—` na coluna e contada na nota do rodapé.
    pub fn conta(&self) -> bool {
        self.mercado_brl.is_some()
    }

    /// A origem do preço, para a coluna: `brapi`, `manual · há 3 d`, `yahoo não-oficial`.
    ///
    /// Sempre diz **de onde veio**, mesmo quando o dado é ao vivo. Numa aba em que um
    /// preço pode vir de uma exchange, de um agregador de terceiros, de uma fonte sem
    /// contrato ou da própria mão de quem digitou, omitir a fonte é omitir a diferença
    /// que mais importa entre eles.
    pub fn origem(&self) -> String {
        let Some(grade) = self.grade else {
            return String::new();
        };
        match grade {
            // Informado: a fonte é óbvia, e o que falta saber é de quando.
            Grade::Manual => format!("manual·{}", crate::invest::tempo::idade(self.idade)),
            // Sem contrato: a marca vem antes do nome, porque é o aviso.
            Grade::NaoOficial => format!("{} não-oficial", self.fonte),
            Grade::AoVivo => self.fonte.to_string(),
            _ => format!("{} {}", self.fonte, grade.marca()),
        }
    }
}

/// O resultado da soma, e o que ela não conseguiu somar.
#[derive(Clone, Debug, Default)]
pub struct Totais {
    pub mercado: f64,
    pub custo: f64,
    /// O valor de mercado **só das linhas que informam custo**. É contra ele que o P&L é
    /// medido — comparar o mercado inteiro com um custo parcial dá um lucro inventado.
    pub mercado_com_custo: f64,
    pub pnl: f64,
    pub dia: f64,
    /// O valor de mercado **só das linhas que sabem a variação do dia**. É contra ele que
    /// a porcentagem do dia é medida: uma fonte que não deu o fechamento anterior não sabe
    /// quanto aquela posição andou, e pôr o valor dela no denominador diluiria a variação
    /// das que sabem.
    pub mercado_com_dia: f64,
    /// Quantas linhas ficaram de fora, e por quê. É o que impede um total incompleto de
    /// parecer completo.
    pub fora_sem_preco: usize,
    pub fora_sem_cambio: usize,
    /// Quantas entraram no valor de mercado mas **não** no custo, por não terem preço
    /// médio informado. O P&L delas não existe, e o total diz isso.
    pub sem_custo: usize,
    pub linhas_contadas: usize,
}

impl Totais {
    /// O P&L em porcentagem, sobre as linhas que informam custo.
    ///
    /// `None` quando nenhuma informa. Quando só algumas informam, a porcentagem é
    /// verdadeira **sobre elas** — porque `pnl` e `custo` falam do mesmo conjunto — e quem
    /// mostra tem que dizer isso ao lado: é para isso que `sem_custo` e `ressalva` existem.
    pub fn pnl_pct(&self) -> Option<f64> {
        (self.custo > 0.0).then(|| self.pnl / self.custo * 100.0)
    }

    /// A variação do dia em porcentagem, sobre as linhas que a conhecem.
    ///
    /// A base é o fechamento de ontem daquelas linhas — `mercado_com_dia - dia` —, e não
    /// o patrimônio inteiro: dividir a variação de metade da carteira pelo total daria
    /// sempre um número menor que o real, e ele pareceria um fato.
    pub fn dia_pct(&self) -> Option<f64> {
        let ontem = self.mercado_com_dia - self.dia;
        (ontem > 0.0).then(|| self.dia / ontem * 100.0)
    }

    pub fn completo(&self) -> bool {
        self.fora_sem_preco == 0 && self.fora_sem_cambio == 0 && self.sem_custo == 0
    }

    /// A frase do rodapé quando o total não é o total. Vazia quando está tudo somado.
    pub fn ressalva(&self) -> String {
        let mut partes = Vec::new();
        if self.fora_sem_preco > 0 {
            partes.push(format!("{} sem preço", self.fora_sem_preco));
        }
        if self.fora_sem_cambio > 0 {
            partes.push(format!("{} sem câmbio", self.fora_sem_cambio));
        }
        let mut frase = match partes.is_empty() {
            true => String::new(),
            false => format!("fora do total: {}", partes.join(" · ")),
        };
        // Separado do resto: estas linhas **contam** no valor de mercado, só não no custo.
        // Dizê-las «fora do total» seria mentira.
        if self.sem_custo > 0 {
            if !frase.is_empty() {
                frase.push_str(" · ");
            }
            frase.push_str(&format!(
                "{} sem preço médio (P&L não calculado)",
                self.sem_custo
            ));
        }
        frase
    }
}

/// O preço corrente de uma posição: o do provedor, ou o informado que está na própria
/// linha. O provedor manual já devolve o informado, então a segunda metade só existe
/// para o instante entre editar e a thread republicar.
fn preco_de(
    posicao: &Position,
    quote: Option<&Quote>,
) -> (Option<f64>, Option<Grade>, &'static str) {
    if let Some(q) = quote {
        return (Some(q.preco), Some(q.grade), q.fonte);
    }
    match posicao.preco_manual {
        Some(p) => (Some(p), Some(Grade::Manual), "manual"),
        None => (None, None, ""),
    }
}

/// Monta as linhas da carteira. Uma passagem, sem I/O.
pub fn linhas(portfolio: &Portfolio, market: &MarketSnapshot, agora: u64) -> Vec<Linha> {
    portfolio
        .posicoes
        .iter()
        .map(|posicao| {
            let quote = market.quote(&posicao.ativo);
            let (preco, grade, fonte) = preco_de(posicao, quote);
            let idade = quote.map(|q| q.idade(agora)).unwrap_or_else(|| {
                posicao
                    .preco_manual_em
                    .map(|em| agora.saturating_sub(em))
                    .unwrap_or(0)
            });

            let mercado_moeda = preco.map(|p| p * posicao.quantidade);
            let mercado_brl = mercado_moeda.and_then(|v| market.para_brl(v, posicao.moeda));
            // Sem preço médio não há custo, e sem custo não há P&L. A ausência se propaga
            // em vez de virar zero — um P&L sobre custo zero seria um lucro inventado.
            let custo_brl = posicao
                .custo()
                .and_then(|c| market.para_brl(c, posicao.moeda));
            let pnl_brl = match (mercado_brl, custo_brl) {
                (Some(m), Some(c)) => Some(m - c),
                _ => None,
            };
            let pnl_pct = match (pnl_brl, custo_brl) {
                (Some(p), Some(c)) if c > 0.0 => Some(p / c * 100.0),
                _ => None,
            };

            // A variação do dia só existe onde a fonte deu o fechamento anterior — e
            // nunca para preço informado, que não tem «dia».
            let (dia_brl, variacao_pct) = match quote.filter(|q| q.grade.ao_vivo()) {
                Some(q) => {
                    let dia = q.anterior.map(|a| (q.preco - a) * posicao.quantidade);
                    (
                        dia.and_then(|d| market.para_brl(d, posicao.moeda)),
                        q.variacao(),
                    )
                }
                None => (None, None),
            };

            Linha {
                posicao: posicao.clone(),
                preco,
                grade,
                idade,
                fonte,
                mercado_brl,
                custo_brl,
                pnl_brl,
                pnl_pct,
                dia_brl,
                variacao_pct,
            }
        })
        .collect()
}

pub fn totais(linhas: &[Linha]) -> Totais {
    let mut t = Totais::default();
    for l in linhas {
        match (l.preco, l.mercado_brl) {
            (None, _) => t.fora_sem_preco += 1,
            // Tem preço mas não virou BRL: o que faltou foi o câmbio.
            (Some(_), None) => t.fora_sem_cambio += 1,
            (Some(_), Some(m)) => {
                t.mercado += m;
                match l.custo_brl {
                    Some(c) => {
                        t.custo += c;
                        t.mercado_com_custo += m;
                    }
                    // Entra no mercado e fica fora do custo: o valor dela é conhecido, o
                    // que ela custou não é.
                    None => t.sem_custo += 1,
                }
                if let Some(d) = l.dia_brl {
                    t.dia += d;
                    t.mercado_com_dia += m;
                }
                t.linhas_contadas += 1;
            }
        }
    }
    // **Sobre o mesmo conjunto dos dois lados.** Era `mercado - custo`, e com uma carteira
    // em que só duas linhas informam preço médio isso tratava o custo das outras como
    // zero: R$ 1,9 milhão de mercado contra R$ 839 mil de custo virava «P&L de R$ 1,08
    // milhão», que não é lucro nenhum. Agora o P&L fala só das linhas que têm as duas
    // pontas, e `sem_custo` diz quantas ficaram de fora dele.
    t.pnl = t.mercado_com_custo - t.custo;
    t
}

/// O peso de cada linha no total, em porcentagem. Só entre as que contam — o peso de uma
/// linha que não entrou no total não significa nada.
pub fn peso(linha: &Linha, totais: &Totais) -> Option<f64> {
    let m = linha.mercado_brl?;
    (totais.mercado > 0.0).then(|| m / totais.mercado * 100.0)
}

/// Agrega por uma dimensão qualquer, devolvendo `(rótulo, valor em BRL)` ordenado do
/// maior para o menor.
pub fn agrupar<F: Fn(&Linha) -> String>(linhas: &[Linha], chave: F) -> Vec<(String, f64)> {
    let mut mapa: std::collections::BTreeMap<String, f64> = std::collections::BTreeMap::new();
    for l in linhas.iter().filter(|l| l.conta()) {
        *mapa.entry(chave(l)).or_default() += l.mercado_brl.unwrap_or(0.0);
    }
    let mut v: Vec<(String, f64)> = mapa.into_iter().collect();
    v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    v
}

/// A média ponderada de preços médios **informados**, para quando o mesmo ativo aparece
/// em mais de uma fonte.
///
/// Isto continua sendo «informado»: é agregação de valores informados, não derivação a
/// partir de operações. A distinção importa e está aqui para quem for mexer não confundir.
pub fn preco_medio_consolidado(posicoes: &[&Position]) -> Option<f64> {
    // Só as que têm custo informado entram. Misturar uma sem preço médio como se
    // custasse zero puxaria a média para baixo e inventaria um custo que ninguém informou.
    let com_custo: Vec<&&Position> = posicoes.iter().filter(|p| p.custo().is_some()).collect();
    if com_custo.len() != posicoes.len() || com_custo.is_empty() {
        return None;
    }
    let quantidade: f64 = com_custo.iter().map(|p| p.quantidade).sum();
    if quantidade <= 0.0 {
        return None;
    }
    let custo: f64 = com_custo.iter().filter_map(|p| p.custo()).sum();
    Some(custo / quantidade)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invest::model::{AssetId, Classe, Market, Moeda};
    use crate::invest::provider::Quote;

    fn pos(simbolo: &str, qtd: f64, pm: f64, moeda: Moeda) -> Position {
        Position {
            fonte: "t".into(),
            conta: String::new(),
            ativo: AssetId::new(
                if moeda == Moeda::Usd {
                    Market::Us
                } else {
                    Market::B3
                },
                simbolo,
            ),
            classe: Classe::Acao,
            quantidade: qtd,
            preco_medio: Some(pm),
            moeda,
            preco_manual: None,
            preco_manual_em: None,
            atualizado_em: 0,
        }
    }

    fn market_com(precos: &[(&Position, f64)]) -> MarketSnapshot {
        let mut m = MarketSnapshot::default();
        for (p, preco) in precos {
            m.quotes.insert(
                p.ativo.clone(),
                Quote {
                    fonte: "teste",
                    ativo: p.ativo.clone(),
                    preco: *preco,
                    anterior: None,
                    moeda: p.moeda,
                    grade: Grade::AoVivo,
                    em: 0,
                    volume: None,
                    max24: None,
                    min24: None,
                },
            );
        }
        m
    }

    #[test]
    fn pnl_e_mercado_menos_custo() {
        let mut p = Portfolio::default();
        let petr = pos("PETR4", 300.0, 32.10, Moeda::Brl);
        p.posicoes.push(petr.clone());
        let market = market_com(&[(&petr, 38.42)]);
        let l = linhas(&p, &market, 0);
        let t = totais(&l);
        assert!((t.mercado - 11526.0).abs() < 0.01);
        assert!((t.custo - 9630.0).abs() < 0.01);
        assert!((t.pnl - 1896.0).abs() < 0.01);
        assert!(t.completo());
    }

    #[test]
    fn sem_preco_a_linha_fica_de_fora_e_o_total_diz() {
        let mut p = Portfolio::default();
        let petr = pos("PETR4", 300.0, 32.10, Moeda::Brl);
        let vale = pos("VALE3", 100.0, 60.0, Moeda::Brl);
        p.posicoes.push(petr.clone());
        p.posicoes.push(vale);
        // Só a PETR4 tem preço.
        let market = market_com(&[(&petr, 38.42)]);
        let l = linhas(&p, &market, 0);
        let t = totais(&l);
        assert_eq!(t.fora_sem_preco, 1);
        assert_eq!(t.linhas_contadas, 1);
        assert!(!t.completo());
        assert!(t.ressalva().contains("1 sem preço"));
        // E o total é só o que dava para somar — nada inventado.
        assert!((t.mercado - 11526.0).abs() < 0.01);
    }

    #[test]
    fn sem_cambio_ativo_estrangeiro_fica_de_fora_e_e_dito() {
        let mut p = Portfolio::default();
        let aapl = pos("AAPL", 10.0, 180.0, Moeda::Usd);
        p.posicoes.push(aapl.clone());
        // Preço existe, câmbio não.
        let market = market_com(&[(&aapl, 220.0)]);
        let l = linhas(&p, &market, 0);
        let t = totais(&l);
        assert_eq!(t.fora_sem_cambio, 1, "faltou câmbio, não preço");
        assert_eq!(t.mercado, 0.0, "converter sem câmbio seria inventar");
        assert!(t.ressalva().contains("sem câmbio"));
    }

    #[test]
    fn com_cambio_o_estrangeiro_entra_convertido() {
        let mut p = Portfolio::default();
        let aapl = pos("AAPL", 10.0, 180.0, Moeda::Usd);
        p.posicoes.push(aapl.clone());
        let mut market = market_com(&[(&aapl, 220.0)]);
        let par = AssetId::new(Market::Fx, "USDBRL");
        market.quotes.insert(
            par.clone(),
            Quote {
                fonte: "teste",
                ativo: par,
                preco: 5.0,
                anterior: None,
                moeda: Moeda::Brl,
                grade: Grade::AoVivo,
                em: 0,
                volume: None,
                max24: None,
                min24: None,
            },
        );
        let l = linhas(&p, &market, 0);
        let t = totais(&l);
        assert!((t.mercado - 11000.0).abs() < 0.01, "10 × 220 × 5");
        assert!((t.custo - 9000.0).abs() < 0.01);
        assert!(t.completo());
    }

    #[test]
    fn preco_informado_nao_gera_variacao_do_dia() {
        let mut p = Portfolio::default();
        let mut petr = pos("PETR4", 100.0, 30.0, Moeda::Brl);
        petr.preco_manual = Some(38.0);
        p.posicoes.push(petr);
        let market = MarketSnapshot::default();
        let l = linhas(&p, &market, 0);
        assert_eq!(l[0].preco, Some(38.0));
        assert_eq!(l[0].grade, Some(Grade::Manual));
        assert_eq!(l[0].variacao_pct, None, "informado não tem «dia»");
        assert_eq!(l[0].dia_brl, None);
    }

    #[test]
    fn media_ponderada_de_pms_informados() {
        // Mesma ação em duas corretoras: 100 a 30 e 300 a 40 dão PM consolidado de 37,50.
        let a = pos("PETR4", 100.0, 30.0, Moeda::Brl);
        let b = pos("PETR4", 300.0, 40.0, Moeda::Brl);
        let pm = preco_medio_consolidado(&[&a, &b]).unwrap();
        assert!((pm - 37.5).abs() < 1e-9, "deu {pm}");
        assert_eq!(preco_medio_consolidado(&[]), None);
    }

    #[test]
    fn pesos_somam_cem_quando_tudo_conta() {
        let mut p = Portfolio::default();
        let a = pos("PETR4", 100.0, 30.0, Moeda::Brl);
        let b = pos("VALE3", 100.0, 60.0, Moeda::Brl);
        p.posicoes.push(a.clone());
        p.posicoes.push(b.clone());
        let market = market_com(&[(&a, 40.0), (&b, 60.0)]);
        let l = linhas(&p, &market, 0);
        let t = totais(&l);
        let soma: f64 = l.iter().filter_map(|x| peso(x, &t)).sum();
        assert!((soma - 100.0).abs() < 1e-6, "deu {soma}");
    }
}

#[cfg(test)]
mod sem_pm_tests {
    use super::*;
    use crate::invest::model::{AssetId, Classe, Market, Moeda, Portfolio, Position};
    use crate::invest::provider::{Grade, Quote};

    fn pos(simbolo: &str, qtd: f64, pm: Option<f64>) -> Position {
        Position {
            fonte: "t".into(),
            conta: String::new(),
            ativo: AssetId::new(Market::B3, simbolo),
            classe: Classe::Acao,
            quantidade: qtd,
            preco_medio: pm,
            moeda: Moeda::Brl,
            preco_manual: None,
            preco_manual_em: None,
            atualizado_em: 0,
        }
    }

    fn com_preco(p: &Portfolio, precos: &[(&str, f64)]) -> MarketSnapshot {
        let mut m = MarketSnapshot::default();
        for (s, preco) in precos {
            let ativo = AssetId::new(Market::B3, *s);
            m.quotes.insert(
                ativo.clone(),
                Quote {
                    fonte: "teste",
                    ativo,
                    preco: *preco,
                    anterior: None,
                    moeda: Moeda::Brl,
                    grade: Grade::AoVivo,
                    em: 0,
                    volume: None,
                    max24: None,
                    min24: None,
                },
            );
        }
        let _ = p;
        m
    }

    #[test]
    fn sem_preco_medio_a_posicao_vale_mas_nao_tem_pnl() {
        // A regra que o preço médio opcional cria: a linha **conta** no valor de mercado
        // (o quanto ela vale é sabido) e **não** tem P&L (o quanto custou, não).
        let mut p = Portfolio::default();
        p.posicoes.push(pos("PETR4", 300.0, None));
        let market = com_preco(&p, &[("PETR4", 48.0)]);
        let l = linhas(&p, &market, 0);
        assert_eq!(l[0].mercado_brl, Some(14_400.0));
        assert_eq!(l[0].custo_brl, None, "sem PM não há custo");
        assert_eq!(l[0].pnl_brl, None, "e sem custo não há P&L");
        assert_eq!(l[0].pnl_pct, None);

        let t = totais(&l);
        assert_eq!(
            t.mercado, 14_400.0,
            "o valor de mercado é conhecido e entra"
        );
        assert_eq!(t.custo, 0.0);
        assert_eq!(t.sem_custo, 1);
        assert!(!t.completo());
        assert!(t.ressalva().contains("sem preço médio"));
    }

    #[test]
    fn a_variacao_do_dia_e_medida_contra_quem_a_conhece() {
        // Mesma disciplina do P&L: só uma das duas posições tem fechamento anterior, e a
        // porcentagem do dia é a dela — não a variação diluída pelo patrimônio inteiro.
        // Pôr no denominador uma posição que não sabe quanto andou dá sempre um número
        // menor que o real, e ele apareceria como fato.
        let mut p = Portfolio::default();
        p.posicoes.push(pos("PETR4", 100.0, Some(30.0)));
        p.posicoes.push(pos("VALE3", 100.0, Some(70.0)));
        let mut market = com_preco(&p, &[("PETR4", 50.0), ("VALE3", 80.0)]);
        // Só a PETR4 traz o fechamento anterior.
        if let Some(q) = market.quotes.get_mut(&AssetId::parse("B3/PETR4").unwrap()) {
            q.anterior = Some(40.0);
        }
        let t = totais(&linhas(&p, &market, 0));

        assert!((t.dia - 1_000.0).abs() < 1e-6, "100 × (50 − 40)");
        assert!(
            (t.mercado_com_dia - 5_000.0).abs() < 1e-6,
            "só a PETR4 entra na base"
        );
        let pct = t.dia_pct().expect("há base");
        assert!(
            (pct - 25.0).abs() < 1e-6,
            "1000 sobre 4000, e não sobre 12000"
        );
        assert!(t.mercado > t.mercado_com_dia, "a VALE3 conta no patrimônio");
    }

    #[test]
    fn o_pnl_fala_so_das_linhas_que_tem_as_duas_pontas() {
        // O bug que isto trava: com PETR4 informando custo e VALE3 não, o P&L era
        // `mercado - custo` sobre a carteira **inteira** — o valor de mercado da VALE3
        // entrava contra custo zero e virava lucro. Numa carteira em que só duas de vinte
        // e uma linhas informam preço médio, isso produzia um «P&L» de sete dígitos.
        let mut p = Portfolio::default();
        p.posicoes.push(pos("PETR4", 300.0, Some(32.10)));
        p.posicoes.push(pos("VALE3", 100.0, None));
        let market = com_preco(&p, &[("PETR4", 48.0), ("VALE3", 79.0)]);
        let t = totais(&linhas(&p, &market, 0));

        let so_petr4 = 300.0 * (48.0 - 32.10);
        assert!(
            (t.pnl - so_petr4).abs() < 1e-6,
            "o P&L é só o da PETR4, e não {} ",
            t.pnl
        );
        assert!(t.mercado > t.mercado_com_custo, "a VALE3 conta no mercado");
        assert_eq!(t.sem_custo, 1);
        // A porcentagem existe e é verdadeira **sobre esse conjunto**. Quem a mostra tem
        // que mostrar a ressalva junto — é para isso que ela está no total.
        assert!(t.ressalva().contains("sem preço médio"));
        assert!(!t.completo());
    }

    #[test]
    fn com_todas_as_posicoes_informadas_o_pnl_volta() {
        let mut p = Portfolio::default();
        p.posicoes.push(pos("PETR4", 300.0, Some(32.10)));
        let market = com_preco(&p, &[("PETR4", 48.0)]);
        let t = totais(&linhas(&p, &market, 0));
        assert!(t.completo());
        assert!(t.pnl_pct().is_some());
    }

    #[test]
    fn o_pm_consolidado_recusa_misturar_com_e_sem_custo() {
        // Tratar a sem-PM como custo zero puxaria a média para baixo e inventaria um
        // custo que ninguém informou.
        let a = pos("PETR4", 100.0, Some(30.0));
        let b = pos("PETR4", 300.0, None);
        assert_eq!(preco_medio_consolidado(&[&a, &b]), None);
        let c = pos("PETR4", 300.0, Some(40.0));
        assert_eq!(preco_medio_consolidado(&[&a, &c]), Some(37.5));
    }
}
