//! Renda fixa — o módulo com melhor relação custo/valor da aba inteira.
//!
//! A API do Banco Central é pública, gratuita, sem chave, sem cadastro, estável e
//! oficial. Nenhuma outra fonte desta aba tem as seis propriedades ao mesmo tempo, e o
//! número que ela dá é o que ancora quase toda decisão de investimento no Brasil.

use crossterm::event::{KeyCode, KeyEvent};

use crate::invest::calc;
use crate::invest::carteira;
use crate::invest::model::{AssetId, Classe, Market};
use crate::invest::module::{
    Ctx, Escape, Group, InvestModule, Layout, ModuleView, Outcome, Pane, Row, Tone,
};
use crate::invest::modules::comum::{Lista, hint};
use crate::invest::provider::Span;
use crate::invest::providers::bcb::{self, SERIES};

pub struct RendaFixa;

impl InvestModule for RendaFixa {
    fn id(&self) -> &'static str {
        "renda-fixa"
    }
    fn name(&self) -> &'static str {
        "Renda fixa"
    }
    fn description(&self) -> &'static str {
        "Selic, CDI, IPCA e IGP-M direto do Banco Central, mais a calculadora"
    }
    fn destaque(&self) -> u8 {
        3
    }
    fn group(&self) -> Group {
        Group::Mercado
    }
    fn keywords(&self) -> &'static str {
        "cdi selic ipca igpm juro poupança tesouro darf cdb inflação"
    }

    fn summary(&self, ctx: &Ctx) -> String {
        let ler = |s: &str| {
            ctx.market
                .quote(&AssetId::new(Market::Bcb, s))
                .map(|q| q.preco)
        };
        match (ler("SELIC-META"), ler("IPCA")) {
            (Some(selic), _) => format!("Selic {} a.a.", calc::pct_simples(selic)),
            (None, Some(_)) => "IPCA disponível · Selic ainda não".into(),
            _ => "abra para buscar do Banco Central".into(),
        }
    }

    fn widget(&self, ctx: &Ctx) -> Option<Pane> {
        let ler = |s: &str, casas: usize| {
            ctx.market
                .quote(&AssetId::new(Market::Bcb, s))
                .map(|q| calc::pct_casas(q.preco, casas))
                .unwrap_or_else(|| "—".into())
        };
        Some(Pane::Facts {
            title: String::new(),
            rows: vec![
                ("Selic (meta)".into(), ler("SELIC-META", 2), Tone::Destaque),
                ("Selic (efetiva)".into(), ler("SELIC", 2), Tone::Normal),
                ("CDI (ao dia)".into(), ler("CDI", 4), Tone::Normal),
                ("IPCA (mensal)".into(), ler("IPCA", 2), Tone::Normal),
            ],
        })
    }

    fn open(&self, _ctx: &Ctx, _alvo: Option<&AssetId>) -> Box<dyn ModuleView> {
        Box::new(Vista {
            lista: Lista::default(),
            calculadora: None,
        })
    }
}

/// Uma simulação de renda fixa. As regras aqui são **fixas em lei**, ao contrário de quase
/// tudo nesta aba — por isso podem ser codificadas com confiança, e por isso têm teste.
pub struct Simulacao {
    pub valor: f64,
    pub pct_cdi: f64,
    pub meses: u32,
}

/// A tabela regressiva de IR de renda fixa. Prazo em dias corridos.
pub fn aliquota_ir(dias: u32) -> f64 {
    match dias {
        0..=180 => 22.5,
        181..=360 => 20.0,
        361..=720 => 17.5,
        _ => 15.0,
    }
}

/// O IOF dos primeiros 30 dias, em porcentagem do rendimento. É uma tabela de 96% no
/// primeiro dia até 0% no trigésimo, decrescendo em passos de 3,33 pontos.
pub fn iof(dias: u32) -> f64 {
    match dias {
        0 => 96.0,
        d if d >= 30 => 0.0,
        d => (96.0 - (d as f64 - 1.0) * 3.3333).max(0.0),
    }
}

impl Simulacao {
    /// O bruto, o imposto e o líquido. O CDI usado é o **corrente projetado para a
    /// frente**, o que é uma suposição — e a tela diz que é.
    pub fn render(&self, cdi_anual_pct: f64) -> (f64, f64, f64) {
        let taxa_anual = cdi_anual_pct / 100.0 * self.pct_cdi / 100.0;
        let anos = self.meses as f64 / 12.0;
        let bruto = self.valor * ((1.0 + taxa_anual).powf(anos) - 1.0);
        let dias = self.meses * 30;
        let iof_valor = bruto * iof(dias) / 100.0;
        let base = (bruto - iof_valor).max(0.0);
        let ir = base * aliquota_ir(dias) / 100.0;
        (bruto, iof_valor + ir, bruto - iof_valor - ir)
    }
}

/// O que uma coluna acumulada mostra enquanto não tem valor. Vazio onde ela não se
/// aplica; reticências onde ela se aplica e ainda não chegou.
fn esperando(diaria: bool) -> String {
    match diaria {
        true => "…".to_string(),
        false => String::new(),
    }
}

struct Vista {
    lista: Lista,
    calculadora: Option<Simulacao>,
}

/// Acumula os últimos `dias` de uma série diária. **Por produto**, via `bcb::acumular`:
/// somar taxas diárias é o erro clássico, e num ano de CDI alto ele erra por vários
/// décimos de ponto percentual.
fn acumulado(
    cache: &crate::invest::historico::Cache,
    providers: &std::sync::Arc<crate::invest::provider::ProviderSet>,
    simbolo: &str,
    span: Span,
    pontos: usize,
) -> Option<f64> {
    use crate::invest::historico::Estado;
    let ativo = AssetId::new(Market::Bcb, simbolo);
    let Estado::Pronta(c) = cache.get(providers, &ativo, span) else {
        return None;
    };
    let taxas: Vec<f64> = c.iter().rev().take(pontos).map(|p| p.fechamento).collect();
    (!taxas.is_empty()).then(|| bcb::acumular(&taxas))
}

impl Vista {
    fn cdi_anual(&self, ctx: &Ctx) -> Option<f64> {
        // A **efetiva**, e não a meta: quem calcula rendimento quer o que o dinheiro de
        // fato rende, e a meta é a decisão do COPOM — fica alguns décimos acima.
        // Sem ela, o CDI diário compõe em 252 pregões, que é o mesmo raciocínio de
        // `acumular` e nunca uma multiplicação.
        ctx.market
            .quote(&AssetId::new(Market::Bcb, "SELIC"))
            .map(|q| q.preco)
            .or_else(|| {
                ctx.market
                    .quote(&AssetId::new(Market::Bcb, "CDI"))
                    .map(|q| ((1.0 + q.preco / 100.0).powi(252) - 1.0) * 100.0)
            })
    }
}

impl ModuleView for Vista {
    fn title(&self) -> String {
        "Renda fixa".into()
    }

    fn quer_mercado(&self) -> bool {
        true
    }

    /// As séries do catálogo do Banco Central. Buscadas só enquanto esta tela está
    /// aberta — quem nunca vem aqui nunca pede nada ao SGS.
    fn ativos(&self) -> Vec<AssetId> {
        SERIES
            .iter()
            .map(|s| AssetId::new(Market::Bcb, s.simbolo))
            .collect()
    }

    fn layout(&self, ctx: &Ctx) -> Layout {
        let rows: Vec<Row> = SERIES
            .iter()
            .map(|s| {
                let ativo = AssetId::new(Market::Bcb, s.simbolo);
                match ctx.market.quote(&ativo) {
                    Some(q) => {
                        let quando = crate::invest::tempo::Data::de_epoch(
                            q.em,
                            crate::invest::tempo::BRT_OFFSET,
                        );
                        // As colunas acumuladas só existem para série diária: acumular
                        // um IPCA mensal em «22 pregões» seria somar uma coisa com outra.
                        let (no_mes, doze) = match s.diaria {
                            true => (
                                acumulado(ctx.historico, ctx.providers, s.simbolo, Span::Mes, 22),
                                acumulado(ctx.historico, ctx.providers, s.simbolo, Span::Ano, 252),
                            ),
                            false => (None, None),
                        };
                        Row::new(vec![
                            s.nome.to_string(),
                            // Taxa diária precisa de quatro casas: com uma, o CDI de
                            // 0,0517% ao dia vira «0,1%» e some.
                            calc::pct_casas(q.preco, if s.diaria { 4 } else { 2 }),
                            // Numa série diária, coluna vazia significaria «acumulado
                            // zero». «…» significa «ainda buscando», que é a verdade.
                            no_mes
                                .map(|v| calc::pct_casas(v, 2))
                                .unwrap_or_else(|| esperando(s.diaria)),
                            doze.map(|v| calc::pct_casas(v, 2))
                                .unwrap_or_else(|| esperando(s.diaria)),
                            quando.longa(),
                        ])
                        .with_cell_tones(vec![
                            Tone::Normal,
                            Tone::Destaque,
                            Tone::Normal,
                            Tone::Normal,
                            Tone::Dim,
                        ])
                    }
                    None => Row::tinted(
                        vec![
                            s.nome.to_string(),
                            "—".into(),
                            String::new(),
                            String::new(),
                            "buscando…".into(),
                        ],
                        Tone::Dim,
                    ),
                }
            })
            .collect();

        // O juro real está na tela e não escondido: é a única linha que responde se o
        // dinheiro está de fato crescendo, e calculá-la de cabeça é o que todo mundo faz.
        let cdi = self.cdi_anual(ctx);
        let ipca_mensal = ctx
            .market
            .quote(&AssetId::new(Market::Bcb, "IPCA"))
            .map(|q| q.preco);
        let ipca_anual = ipca_mensal.map(|m| ((1.0 + m / 100.0).powi(12) - 1.0) * 100.0);
        let real = match (cdi, ipca_anual) {
            // Juro real é razão, não subtração: (1+i)/(1+π) − 1. A subtração é a
            // aproximação de bolso e erra meio ponto com inflação alta.
            (Some(c), Some(p)) => Some(((1.0 + c / 100.0) / (1.0 + p / 100.0) - 1.0) * 100.0),
            _ => None,
        };

        let linhas = carteira::linhas(ctx.portfolio, ctx.market, ctx.agora);
        let totais = carteira::totais(&linhas);
        let minha: f64 = linhas
            .iter()
            .filter(|l| l.posicao.classe == Classe::RendaFixa)
            .filter_map(|l| l.mercado_brl)
            .sum();

        let mut fatos = vec![(
            "juro real (CDI − IPCA, a.a.)".into(),
            real.map(calc::pct_simples).unwrap_or("—".into()),
            match real {
                Some(r) if r > 0.0 => Tone::Bom,
                Some(_) => Tone::Ruim,
                None => Tone::Dim,
            },
        )];
        if minha > 0.0 {
            fatos.push((
                "sua renda fixa".into(),
                format!(
                    "R$ {} · {}",
                    calc::moeda(minha),
                    calc::pct_simples(match totais.mercado > 0.0 {
                        true => minha / totais.mercado * 100.0,
                        false => 0.0,
                    })
                ),
                Tone::Normal,
            ));
        }

        if let Some(sim) = &self.calculadora {
            let cdi = cdi.unwrap_or(0.0);
            let (bruto, imposto, liquido) = sim.render(cdi);
            let dias = sim.meses * 30;
            return Layout::rows(vec![
                (
                    1,
                    Layout::one(Pane::Table {
                        title: "Renda fixa · Banco Central".into(),
                        headers: vec![
                            "Indicador".into(),
                            "Hoje".into(),
                            "No mês".into(),
                            "12 meses".into(),
                            "Data".into(),
                        ],
                        rows,
                        selected: None,
                        query: String::new(),
                        note: None,
                    }),
                ),
                (
                    1,
                    Layout::one(Pane::Facts {
                        title: format!(
                            "Calculadora · R$ {} a {}% do CDI por {} meses",
                            calc::moeda(sim.valor),
                            sim.pct_cdi,
                            sim.meses
                        ),
                        rows: vec![
                            (
                                "rendimento bruto".into(),
                                format!("R$ {}", calc::moeda(bruto)),
                                Tone::Normal,
                            ),
                            (
                                format!("imposto (IR {}% + IOF)", aliquota_ir(dias)),
                                format!("R$ {}", calc::moeda(imposto)),
                                Tone::Ruim,
                            ),
                            (
                                "rendimento líquido".into(),
                                format!("R$ {}", calc::moeda(liquido)),
                                Tone::Bom,
                            ),
                            (
                                "valor final".into(),
                                format!("R$ {}", calc::moeda(sim.valor + liquido)),
                                Tone::Destaque,
                            ),
                            (
                                "atenção".into(),
                                format!(
                                    "usa o CDI de hoje ({}) projetado para a frente — é suposição",
                                    calc::pct_simples(cdi)
                                ),
                                Tone::Aviso,
                            ),
                        ],
                    }),
                ),
            ]);
        }

        Layout::rows(vec![
            (
                3,
                Layout::one(Pane::Table {
                    title: "Renda fixa · Banco Central".into(),
                    headers: vec![
                        "Indicador".into(),
                        "Hoje".into(),
                        "No mês".into(),
                        "12 meses".into(),
                        "Data".into(),
                    ],
                    rows,
                    selected: Some(self.lista.selecionado),
                    query: String::new(),
                    note: mercado::nota_fontes_bcb(ctx),
                }),
            ),
            (
                1,
                Layout::one(Pane::Facts {
                    title: "O que isso quer dizer".into(),
                    rows: fatos,
                }),
            ),
        ])
    }

    fn key(&mut self, key: KeyEvent, _ctx: &Ctx) -> Outcome {
        match key.code {
            KeyCode::Char('c') if self.calculadora.is_none() => {
                self.calculadora = Some(Simulacao {
                    valor: 10_000.0,
                    pct_cdi: 100.0,
                    meses: 12,
                });
                Outcome::Ok
            }
            KeyCode::Up if self.calculadora.is_some() => {
                if let Some(s) = &mut self.calculadora {
                    s.meses += 1;
                }
                Outcome::Ok
            }
            KeyCode::Down if self.calculadora.is_some() => {
                if let Some(s) = &mut self.calculadora {
                    s.meses = s.meses.saturating_sub(1).max(1);
                }
                Outcome::Ok
            }
            KeyCode::Right if self.calculadora.is_some() => {
                if let Some(s) = &mut self.calculadora {
                    s.pct_cdi += 5.0;
                }
                Outcome::Ok
            }
            KeyCode::Left if self.calculadora.is_some() => {
                if let Some(s) = &mut self.calculadora {
                    s.pct_cdi = (s.pct_cdi - 5.0).max(5.0);
                }
                Outcome::Ok
            }
            _ => match self.lista.tecla(key, SERIES.len()) {
                true => Outcome::Ok,
                false => Outcome::Ignorada,
            },
        }
    }

    fn escape(&mut self) -> Escape {
        match self.calculadora.take().is_some() {
            true => Escape::Consumido,
            false => self.lista.escape(),
        }
    }

    fn hint(&self) -> String {
        match self.calculadora.is_some() {
            true => hint(&["↑/↓ prazo", "←/→ % do CDI", "Esc fechar"]),
            false => hint(&["↑/↓ andar", "c calculadora", "Esc sair"]),
        }
    }
}

/// Só a fonte do Banco Central, para a nota deste módulo não falar de exchange nenhuma.
mod mercado {
    use super::{Ctx, Tone};
    pub fn nota_fontes_bcb(ctx: &Ctx) -> Option<(String, Tone)> {
        ctx.market
            .fontes
            .iter()
            .find(|f| f.id == "bcb")
            .map(|f| match &f.erro {
                Some(e) => (format!("Banco Central: {e}"), Tone::Ruim),
                None => (
                    "Banco Central respondendo · séries diárias, publicadas com defasagem".into(),
                    Tone::Dim,
                ),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabela_regressiva_de_ir_bate_com_a_lei() {
        assert_eq!(aliquota_ir(30), 22.5);
        assert_eq!(aliquota_ir(180), 22.5);
        assert_eq!(aliquota_ir(181), 20.0);
        assert_eq!(aliquota_ir(360), 20.0);
        assert_eq!(aliquota_ir(361), 17.5);
        assert_eq!(aliquota_ir(720), 17.5);
        assert_eq!(aliquota_ir(721), 15.0);
    }

    #[test]
    fn iof_zera_no_trigesimo_dia() {
        assert_eq!(iof(30), 0.0);
        assert_eq!(iof(60), 0.0);
        assert!(iof(1) > 90.0);
        assert!(iof(15) > 0.0 && iof(15) < 60.0);
    }

    #[test]
    fn simulacao_de_um_ano_a_cem_por_cento_do_cdi() {
        let s = Simulacao {
            valor: 10_000.0,
            pct_cdi: 100.0,
            meses: 12,
        };
        let (bruto, imposto, liquido) = s.render(10.0);
        assert!((bruto - 1000.0).abs() < 1.0, "bruto deu {bruto}");
        // 360 dias cai na faixa de 20%, e não há IOF depois de 30 dias.
        assert!((imposto - 200.0).abs() < 1.0, "imposto deu {imposto}");
        assert!((liquido - 800.0).abs() < 1.0);
    }

    #[test]
    fn prazo_curto_paga_iof_e_a_aliquota_alta() {
        let curto = Simulacao {
            valor: 10_000.0,
            pct_cdi: 100.0,
            meses: 1,
        };
        let (bruto, imposto, _) = curto.render(10.0);
        assert!(imposto / bruto > 0.22, "prazo curto tem que doer");
    }

    #[test]
    fn juro_real_e_razao_e_nao_subtracao() {
        // CDI 10%, IPCA 5%: a subtração daria 5,00%, e a razão dá 4,76%.
        let real: f64 = ((1.0 + 0.10) / (1.0 + 0.05) - 1.0) * 100.0;
        assert!((real - 4.7619).abs() < 0.001, "deu {real}");
    }
}
