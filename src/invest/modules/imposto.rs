//! Imposto de renda — **estimativa**, e isso está no título de propósito.
//!
//! Apuração de imposto precisa de custo por operação. O preço médio é informado e não
//! derivado de operações, então o que este módulo calcula é
//! `venda − (preço médio informado × quantidade)`. Isso é o número certo **se** o PM
//! informado for o preço médio real de todo o lote vendido — quase sempre verdade, e é
//! como a maioria das corretoras calcula. Mas não é apuração completa:
//!
//! * não há histórico de lotes, então não há FIFO de verdade;
//! * não dá para separar day trade automaticamente (as alíquotas diferem: 20% × 15%);
//! * evento corporativo que altera custo só entra se for informado.
//!
//! Um módulo de imposto que parece exato e não é produz um DARF errado, e isso tem
//! consequência fora do programa. Por isso a ressalva é permanente.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::invest::calc;
use crate::invest::model::{AssetId, Classe, Portfolio, TipoLancamento};
use crate::invest::module::{
    Ctx, Edit, Escape, Field, Group, InvestModule, Layout, ModuleView, Outcome, Pane, Row, Tone,
};
use crate::invest::modules::comum::{Formulario, Lista, hint};
use crate::invest::tempo::{self, Data};

pub struct Imposto;

/// As regras que **estão fixas em lei** — ao contrário de quase tudo nesta aba, estas
/// podem ser escritas com confiança. Cada uma é uma constante nomeada com a regra ao
/// lado, e cada uma tem teste: um número mágico no meio do código é o que faz uma mudança
/// de lei virar um bug silencioso.
pub mod regras {
    /// Ganho em ação, operação normal.
    pub const ALIQUOTA_ACAO: f64 = 15.0;
    /// Day trade, apurado à parte.
    pub const ALIQUOTA_DAY_TRADE: f64 = 20.0;
    /// FII: 20%, e **sem** a isenção dos R$ 20 mil.
    pub const ALIQUOTA_FII: f64 = 20.0;
    pub const ALIQUOTA_CRIPTO: f64 = 15.0;
    /// Isenção mensal de vendas de ação em operação normal.
    pub const ISENCAO_ACAO_MENSAL: f64 = 20_000.0;
    /// Isenção mensal de alienação de cripto.
    pub const ISENCAO_CRIPTO_MENSAL: f64 = 35_000.0;
    /// Abaixo disto não se recolhe: acumula para o mês seguinte.
    pub const DARF_MINIMO: f64 = 10.0;
}

/// A categoria fiscal de um ativo. Não é a classe da carteira: ETF e ação seguem a mesma
/// regra, e FII segue outra.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Categoria {
    Acao,
    Fii,
    Cripto,
    Outro,
}

impl Categoria {
    pub fn de(classe: Classe) -> Categoria {
        match classe {
            Classe::Acao | Classe::Etf => Categoria::Acao,
            Classe::Fii => Categoria::Fii,
            Classe::Cripto => Categoria::Cripto,
            _ => Categoria::Outro,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Categoria::Acao => "ações",
            Categoria::Fii => "FIIs",
            Categoria::Cripto => "cripto",
            Categoria::Outro => "outros",
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Categoria::Acao => "acao",
            Categoria::Fii => "fii",
            Categoria::Cripto => "cripto",
            Categoria::Outro => "outro",
        }
    }

    pub fn aliquota(&self) -> f64 {
        match self {
            Categoria::Acao => regras::ALIQUOTA_ACAO,
            Categoria::Fii => regras::ALIQUOTA_FII,
            Categoria::Cripto => regras::ALIQUOTA_CRIPTO,
            Categoria::Outro => regras::ALIQUOTA_ACAO,
        }
    }

    /// O teto de vendas isentas no mês, quando existe.
    pub fn isencao(&self) -> Option<f64> {
        match self {
            Categoria::Acao => Some(regras::ISENCAO_ACAO_MENSAL),
            Categoria::Cripto => Some(regras::ISENCAO_CRIPTO_MENSAL),
            // FII não tem isenção. É o erro mais comum de quem apura sozinho.
            Categoria::Fii | Categoria::Outro => None,
        }
    }
}

/// A apuração de uma categoria num mês.
#[derive(Clone, Debug, Default)]
pub struct Apuracao {
    /// Quantas vendas entraram sem preço médio informado — o ganho delas não foi
    /// estimado, e a tela precisa dizer isso antes que alguém recolha por este número.
    pub sem_custo: usize,
    pub vendas: f64,
    pub ganho: f64,
    pub isento: bool,
    pub base: f64,
    pub imposto: f64,
    pub prejuizo_usado: f64,
    pub prejuizo_restante: f64,
}

/// Apura um mês, por categoria, a partir dos lançamentos e do preço médio informado.
///
/// `prejuizo_anterior` é o acumulado que entra — compensável dentro da **mesma
/// categoria**, sem prazo.
pub fn apurar(
    portfolio: &Portfolio,
    mes: &str,
    prejuizo_anterior: &std::collections::BTreeMap<String, f64>,
) -> Vec<(Categoria, Apuracao)> {
    let mut por_categoria: std::collections::BTreeMap<Categoria, Apuracao> = Default::default();

    for l in portfolio
        .lancamentos
        .iter()
        .filter(|l| l.tipo == TipoLancamento::Venda)
        .filter(|l| Data::de_epoch(l.em, tempo::BRT_OFFSET).chave_mes() == mes)
    {
        let Some(ativo) = &l.ativo else { continue };
        // A classe e o preço médio vêm da posição. Sem posição, não há custo informado —
        // e chutar custo zero transformaria a venda inteira em ganho.
        let Some(posicao) = portfolio.posicoes.iter().find(|p| p.ativo == *ativo) else {
            continue;
        };
        // Sem preço médio informado não há custo, e sem custo não há ganho a estimar.
        // A venda entra no total (é ela que decide a isenção dos R$ 20 mil) e o ganho
        // fica de fora — chutar custo zero transformaria a venda inteira em lucro.
        let categoria = Categoria::de(posicao.classe);
        let entrada = por_categoria.entry(categoria).or_default();
        let bruto = l.quantidade * l.preco - l.taxas;
        entrada.vendas += bruto;
        match posicao.preco_medio {
            Some(pm) => entrada.ganho += bruto - pm * l.quantidade,
            None => entrada.sem_custo += 1,
        }
    }

    for (categoria, a) in por_categoria.iter_mut() {
        a.isento = categoria
            .isencao()
            .is_some_and(|teto| a.vendas <= teto && a.ganho > 0.0);
        let disponivel = prejuizo_anterior
            .get(categoria.code())
            .copied()
            .unwrap_or(0.0)
            .max(0.0);
        if a.isento || a.ganho <= 0.0 {
            a.base = 0.0;
            a.imposto = 0.0;
            // Prejuízo do mês soma ao acumulado; ganho isento **não** consome prejuízo.
            a.prejuizo_restante = disponivel + (-a.ganho).max(0.0);
        } else {
            a.prejuizo_usado = disponivel.min(a.ganho);
            a.base = a.ganho - a.prejuizo_usado;
            a.imposto = a.base * categoria.aliquota() / 100.0;
            a.prejuizo_restante = disponivel - a.prejuizo_usado;
        }
    }

    let mut v: Vec<(Categoria, Apuracao)> = por_categoria.into_iter().collect();
    v.sort_by_key(|(c, _)| *c);
    v
}

/// O DARF do mês: a soma dos impostos, com o mínimo legal aplicado. Abaixo de R$ 10 não
/// se recolhe — acumula para o mês seguinte.
pub fn darf(apuracoes: &[(Categoria, Apuracao)]) -> (f64, bool) {
    let total: f64 = apuracoes.iter().map(|(_, a)| a.imposto).sum();
    (total, total >= regras::DARF_MINIMO)
}

impl InvestModule for Imposto {
    fn id(&self) -> &'static str {
        "imposto"
    }
    fn name(&self) -> &'static str {
        "Imposto de renda"
    }
    fn description(&self) -> &'static str {
        "Estimativa mensal, DARF e prejuízo acumulado — a partir do preço médio informado"
    }
    fn destaque(&self) -> u8 {
        3
    }
    fn group(&self) -> Group {
        Group::Operacao
    }
    fn keywords(&self) -> &'static str {
        "darf ir tributação isenção 20 mil prejuízo day trade leão receita"
    }

    fn summary(&self, ctx: &Ctx) -> String {
        if ctx.portfolio.lancamentos.is_empty() {
            return "sem vendas registradas — nada a apurar".into();
        }
        let mes = Data::de_epoch(ctx.agora, tempo::BRT_OFFSET).chave_mes();
        let (total, recolhe) = darf(&apurar(
            ctx.portfolio,
            &mes,
            &ctx.portfolio.prejuizo_abertura,
        ));
        match recolhe {
            true => format!("estimativa do mês: R$ {}", calc::moeda(total)),
            false => "nada a recolher neste mês (estimativa)".into(),
        }
    }

    fn widget(&self, ctx: &Ctx) -> Option<Pane> {
        if ctx.portfolio.lancamentos.is_empty() {
            return None;
        }
        let mes = Data::de_epoch(ctx.agora, tempo::BRT_OFFSET).chave_mes();
        let apuracoes = apurar(ctx.portfolio, &mes, &ctx.portfolio.prejuizo_abertura);
        let (total, recolhe) = darf(&apuracoes);
        let mut rows: Vec<(String, String, Tone)> = apuracoes
            .iter()
            .filter(|(_, a)| a.vendas > 0.0)
            .map(|(categoria, a)| {
                (
                    categoria.label().to_string(),
                    format!(
                        "vendas R$ {} · ganho R$ {}",
                        calc::moeda(a.vendas),
                        calc::moeda(a.ganho)
                    ),
                    match a.isento {
                        true => Tone::Dim,
                        false => Tone::Normal,
                    },
                )
            })
            .collect();
        if rows.is_empty() {
            return None;
        }
        rows.push((
            format!("DARF de {mes}"),
            match recolhe {
                true => format!("R$ {}", calc::moeda(total)),
                false => "abaixo do mínimo".into(),
            },
            match recolhe {
                true => Tone::Destaque,
                false => Tone::Dim,
            },
        ));
        Some(Pane::Facts {
            title: String::new(),
            rows,
        })
    }

    fn open(&self, ctx: &Ctx, _alvo: Option<&AssetId>) -> Box<dyn ModuleView> {
        Box::new(Vista {
            mes: Data::de_epoch(ctx.agora, tempo::BRT_OFFSET),
            lista: Lista::default(),
            form: None,
        })
    }
}

struct Vista {
    mes: Data,
    lista: Lista,
    form: Option<Formulario>,
}

impl ModuleView for Vista {
    fn title(&self) -> String {
        format!(
            "Imposto · estimativa · {}/{}",
            tempo::nome_mes(self.mes.mes),
            self.mes.ano
        )
    }

    fn layout(&self, ctx: &Ctx) -> Layout {
        if let Some(form) = &self.form {
            return Layout::one(Pane::Form {
                title: form.titulo.clone(),
                fields: form.campos.clone(),
                selected: form.selecionado,
                error: form.erro.clone(),
                hint: hint(&["↑/↓ campo", "Enter gravar", "Esc cancelar"]),
            });
        }

        let chave = self.mes.chave_mes();
        let apuracoes = apurar(ctx.portfolio, &chave, &ctx.portfolio.prejuizo_abertura);
        let (total, recolhe) = darf(&apuracoes);

        let rows: Vec<Row> = apuracoes
            .iter()
            .map(|(cat, a)| {
                Row::new(vec![
                    cat.label().to_string(),
                    calc::moeda(a.vendas),
                    calc::moeda(a.ganho),
                    match cat.isencao() {
                        None => "não tem".to_string(),
                        Some(_) if a.isento => "sim".to_string(),
                        Some(_) => "não".to_string(),
                    },
                    calc::moeda(a.base),
                    calc::moeda(a.imposto),
                ])
                .with_cell_tones(vec![
                    Tone::Normal,
                    Tone::Dim,
                    match a.ganho >= 0.0 {
                        true => Tone::Bom,
                        false => Tone::Ruim,
                    },
                    match a.isento {
                        true => Tone::Bom,
                        false => Tone::Dim,
                    },
                    Tone::Dim,
                    match a.imposto > 0.0 {
                        true => Tone::Ruim,
                        false => Tone::Dim,
                    },
                ])
            })
            .collect();

        let vence = self.mes.mes_seguinte();
        let mut fatos = vec![(
            "DARF do mês".into(),
            match (recolhe, total > 0.0) {
                (true, _) => format!(
                    "R$ {} · vence no último dia útil de {}",
                    calc::moeda(total),
                    tempo::nome_mes(vence.mes)
                ),
                (false, true) => format!(
                    "R$ {} — abaixo do mínimo de R$ {}, acumula para o mês seguinte",
                    calc::moeda(total),
                    calc::moeda(crate::invest::modules::imposto::regras::DARF_MINIMO)
                ),
                _ => "nada a recolher".into(),
            },
            match recolhe {
                true => Tone::Aviso,
                false => Tone::Dim,
            },
        )];
        for (cat, a) in &apuracoes {
            if a.sem_custo > 0 {
                fatos.push((
                    format!("{} · sem preço médio", cat.label()),
                    format!(
                        "{} venda(s) sem custo informado — o ganho delas NÃO entrou nesta conta",
                        a.sem_custo
                    ),
                    Tone::Aviso,
                ));
            }
            if a.prejuizo_restante > 0.0 {
                fatos.push((
                    format!("prejuízo acumulado · {}", cat.label()),
                    format!("R$ {}", calc::moeda(a.prejuizo_restante)),
                    Tone::Dim,
                ));
            }
        }

        Layout::rows(vec![
            (
                3,
                Layout::one(Pane::Table {
                    title: format!(
                        "Apuração · {}/{}",
                        tempo::nome_mes(self.mes.mes),
                        self.mes.ano
                    ),
                    headers: vec![
                        "Categoria".into(),
                        "Vendas".into(),
                        "Ganho".into(),
                        "Isento".into(),
                        "Base".into(),
                        "Imposto".into(),
                    ],
                    rows,
                    selected: Some(self.lista.selecionado),
                    query: String::new(),
                    note: None,
                }),
            ),
            (
                1,
                Layout::one(Pane::Facts {
                    title: "Resultado".into(),
                    rows: fatos,
                }),
            ),
            (
                2,
                Layout::one(Pane::Empty {
                    title: "ESTIMATIVA — leia antes de recolher".into(),
                    note: format!(
                        "Calculada sobre o preço médio informado, sem histórico de lotes.\n\n\
                         • Não há FIFO de verdade: o custo usado é o PM que você informou.\n\
                         • Day trade não é separado automaticamente — a alíquota dele é {}.\n\
                         • Evento corporativo que muda custo só entra se você registrar.\n\n\
                         Confira antes de recolher. Ctrl+E informa o prejuízo acumulado \n\
                         de antes de você começar a usar isto — sem ele, o primeiro mês \n\
                         erra.",
                        calc::pct_simples(regras::ALIQUOTA_DAY_TRADE)
                    ),
                }),
            ),
        ])
    }

    fn key(&mut self, key: KeyEvent, ctx: &Ctx) -> Outcome {
        if let Some(form) = &mut self.form {
            if form.tecla(key) {
                let mut edits = Vec::new();
                for cat in [Categoria::Acao, Categoria::Fii, Categoria::Cripto] {
                    let bruto = form.valor(cat.label());
                    if bruto.trim().is_empty() {
                        continue;
                    }
                    match calc::ler_numero(&bruto) {
                        Some(v) if v >= 0.0 => {
                            edits.push(Edit::SetPrejuizoAbertura(cat.code().to_string(), v))
                        }
                        _ => {
                            form.erro = Some(format!("«{bruto}» não é um número"));
                            return Outcome::Ok;
                        }
                    }
                }
                self.form = None;
                return Outcome::Editar(edits);
            }
            return Outcome::Ok;
        }

        match key.code {
            KeyCode::Left => {
                self.mes = self.mes.mes_anterior();
                Outcome::Ok
            }
            KeyCode::Right => {
                self.mes = self.mes.mes_seguinte();
                Outcome::Ok
            }
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.form = Some(Formulario::novo(
                    "Prejuízo acumulado de abertura",
                    [Categoria::Acao, Categoria::Fii, Categoria::Cripto]
                        .iter()
                        .map(|c| {
                            Field::text(
                                c.label(),
                                ctx.portfolio
                                    .prejuizo_abertura
                                    .get(c.code())
                                    .map(|v| calc::moeda(*v))
                                    .unwrap_or_default(),
                                "O que você já tinha de prejuízo antes de usar isto",
                            )
                        })
                        .collect(),
                ));
                Outcome::Ok
            }
            _ => Outcome::Ignorada,
        }
    }

    fn escape(&mut self) -> Escape {
        match self.form.take().is_some() {
            true => Escape::Consumido,
            false => Escape::Nao,
        }
    }

    fn hint(&self) -> String {
        hint(&["←/→ mês", "Ctrl+E prejuízo de abertura", "Esc sair"])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invest::model::{AssetId, Lancamento, Moeda, Position};
    use std::collections::BTreeMap;

    /// Um lançamento de venda, que é o único tipo que a apuração olha.
    fn venda(ativo: &str, qtd: f64, preco: f64, em: u64) -> Lancamento {
        Lancamento {
            em,
            tipo: TipoLancamento::Venda,
            ativo: Some(AssetId::parse(ativo).unwrap()),
            quantidade: qtd,
            preco,
            taxas: 0.0,
            valor: 0.0,
            moeda: None,
            nota: String::new(),
        }
    }

    fn carteira(simbolo: &str, classe: Classe, pm: f64) -> Position {
        Position {
            fonte: "t".into(),
            conta: String::new(),
            ativo: AssetId::parse(simbolo).unwrap(),
            classe,
            quantidade: 1000.0,
            preco_medio: Some(pm),
            moeda: Moeda::Brl,
            preco_manual: None,
            preco_manual_em: None,
            atualizado_em: 0,
        }
    }

    fn epoch(dia: u32) -> u64 {
        Data {
            ano: 2026,
            mes: 9,
            dia,
        }
        .epoch_inicio(tempo::BRT_OFFSET)
            + 3600
    }

    #[test]
    fn venda_de_acao_abaixo_de_vinte_mil_e_isenta() {
        let mut p = Portfolio::default();
        p.posicoes.push(carteira("B3/PETR4", Classe::Acao, 30.0));
        // 500 × 36,80 = 18.400, abaixo do teto.
        p.lancamentos
            .push(venda("B3/PETR4", 500.0, 36.80, epoch(10)));
        let a = apurar(&p, "2026-09", &BTreeMap::new());
        let (_, acao) = a.iter().find(|(c, _)| *c == Categoria::Acao).unwrap();
        assert!(acao.isento, "18.400 está abaixo dos 20 mil");
        assert_eq!(acao.imposto, 0.0);
        // E a venda aparece mesmo isenta: é o número que decide a isenção.
        assert!((acao.vendas - 18_400.0).abs() < 0.01);
    }

    #[test]
    fn venda_de_acao_acima_de_vinte_mil_paga_quinze_por_cento() {
        let mut p = Portfolio::default();
        p.posicoes.push(carteira("B3/PETR4", Classe::Acao, 30.0));
        // 1000 × 40 = 40.000, ganho de 10.000.
        p.lancamentos
            .push(venda("B3/PETR4", 1000.0, 40.0, epoch(10)));
        let a = apurar(&p, "2026-09", &BTreeMap::new());
        let (_, acao) = a.iter().find(|(c, _)| *c == Categoria::Acao).unwrap();
        assert!(!acao.isento);
        assert!((acao.ganho - 10_000.0).abs() < 0.01);
        assert!(
            (acao.imposto - 1_500.0).abs() < 0.01,
            "deu {}",
            acao.imposto
        );
    }

    #[test]
    fn fii_nao_tem_isencao_de_vinte_mil() {
        // O erro mais comum de quem apura sozinho.
        let mut p = Portfolio::default();
        p.posicoes.push(carteira("B3/HGLG11", Classe::Fii, 150.0));
        // 40 × 171 = 6.840, ganho de 840.
        p.lancamentos
            .push(venda("B3/HGLG11", 40.0, 171.0, epoch(12)));
        let a = apurar(&p, "2026-09", &BTreeMap::new());
        let (_, fii) = a.iter().find(|(c, _)| *c == Categoria::Fii).unwrap();
        assert!(
            !fii.isento,
            "FII não tem isenção, por menor que seja a venda"
        );
        assert!((fii.imposto - 168.0).abs() < 0.01, "deu {}", fii.imposto);
    }

    #[test]
    fn prejuizo_acumulado_reduz_a_base_antes_da_aliquota() {
        let mut p = Portfolio::default();
        p.posicoes.push(carteira("B3/PETR4", Classe::Acao, 30.0));
        p.lancamentos
            .push(venda("B3/PETR4", 1000.0, 40.0, epoch(10)));
        let mut prejuizo = BTreeMap::new();
        prejuizo.insert("acao".to_string(), 4_000.0);
        let a = apurar(&p, "2026-09", &prejuizo);
        let (_, acao) = a.iter().find(|(c, _)| *c == Categoria::Acao).unwrap();
        assert!((acao.prejuizo_usado - 4_000.0).abs() < 0.01);
        assert!((acao.base - 6_000.0).abs() < 0.01);
        assert!((acao.imposto - 900.0).abs() < 0.01);
        assert_eq!(acao.prejuizo_restante, 0.0);
    }

    #[test]
    fn prejuizo_maior_que_o_ganho_zera_a_base_e_o_resto_fica() {
        let mut p = Portfolio::default();
        p.posicoes.push(carteira("B3/PETR4", Classe::Acao, 30.0));
        p.lancamentos
            .push(venda("B3/PETR4", 1000.0, 40.0, epoch(10)));
        let mut prejuizo = BTreeMap::new();
        prejuizo.insert("acao".to_string(), 15_000.0);
        let a = apurar(&p, "2026-09", &prejuizo);
        let (_, acao) = a.iter().find(|(c, _)| *c == Categoria::Acao).unwrap();
        assert_eq!(acao.base, 0.0);
        assert_eq!(acao.imposto, 0.0);
        assert!((acao.prejuizo_restante - 5_000.0).abs() < 0.01);
    }

    #[test]
    fn imposto_abaixo_do_minimo_nao_gera_darf() {
        let mut p = Portfolio::default();
        p.posicoes.push(carteira("B3/HGLG11", Classe::Fii, 150.0));
        // Ganho de 40 reais: imposto de 8, abaixo do mínimo de 10.
        p.lancamentos
            .push(venda("B3/HGLG11", 40.0, 151.0, epoch(12)));
        let a = apurar(&p, "2026-09", &BTreeMap::new());
        let (total, recolhe) = darf(&a);
        assert!(total > 0.0 && total < regras::DARF_MINIMO);
        assert!(!recolhe, "abaixo de R$ 10 acumula, não se recolhe");
    }

    #[test]
    fn venda_de_outro_mes_nao_entra() {
        let mut p = Portfolio::default();
        p.posicoes.push(carteira("B3/PETR4", Classe::Acao, 30.0));
        p.lancamentos
            .push(venda("B3/PETR4", 1000.0, 40.0, epoch(10)));
        assert!(apurar(&p, "2026-08", &BTreeMap::new()).is_empty());
    }

    #[test]
    fn venda_sem_posicao_e_ignorada_em_vez_de_virar_ganho_total() {
        // Sem posição não há custo informado, e chutar zero transformaria a venda inteira
        // em ganho — um imposto inventado.
        let mut p = Portfolio::default();
        p.lancamentos
            .push(venda("B3/PETR4", 1000.0, 40.0, epoch(10)));
        assert!(apurar(&p, "2026-09", &BTreeMap::new()).is_empty());
    }
}
