//! A série diária do patrimônio — a única coisa da aba que só cresce.
//!
//! Um ponto **por dia**, e não por tick: um patrimônio amostrado a cada dois segundos são
//! trezentos pontos de ruído intradiário para uma grandeza que se lê em meses.
//!
//! **A série é histórica e não se recalcula.** O patrimônio de 12 de março foi o que foi,
//! com os preços daquele dia; recalcular com o preço de hoje daria outro número e apagaria
//! o registro. É a mesma família de decisão do preço médio informado.

use serde::{Deserialize, Serialize};

use crate::db;
use crate::invest::store;
use crate::invest::tempo::{self, Data};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Ponto {
    /// `2026-09-08`, para ordenar alfabeticamente na ordem certa e ser legível no arquivo.
    pub dia: String,
    pub total_brl: f64,
    #[serde(default)]
    pub aporte: f64,
    #[serde(default)]
    pub retirada: f64,
    #[serde(default)]
    pub proventos: f64,
}

impl Ponto {
    pub fn data(&self) -> Option<Data> {
        let mut p = self.dia.split('-');
        Some(Data {
            ano: p.next()?.parse().ok()?,
            mes: p.next()?.parse().ok()?,
            dia: p.next()?.parse().ok()?,
        })
    }
}

pub fn chave_dia(agora: u64) -> String {
    let d = Data::de_epoch(agora, tempo::BRT_OFFSET);
    format!("{}-{:02}-{:02}", d.ano, d.mes, d.dia)
}

/// A série inteira, do dia mais antigo ao mais novo. `ORDER BY dia` porque a chave é
/// `2026-09-08` — que ordena alfabeticamente na ordem certa, e é metade da razão de o dia
/// ser guardado assim e não como um epoch.
pub fn load() -> Vec<Ponto> {
    db::ler(Vec::new(), |conn| {
        let mut stmt = conn.prepare(
            "SELECT dia, total_brl, aporte, retirada, proventos FROM patrimonio ORDER BY dia",
        )?;
        let linhas = stmt.query_map([], |row| {
            Ok(Ponto {
                dia: row.get(0)?,
                total_brl: row.get(1)?,
                aporte: row.get(2)?,
                retirada: row.get(3)?,
                proventos: row.get(4)?,
            })
        })?;
        linhas.collect()
    })
}

/// Grava a série. Um `UPSERT` por dia e nenhum `DELETE`: o dia de hoje é reescrito
/// enquanto ele é hoje, e um dia que não está na lista da memória — porque este binário
/// leu a série antes de alguma coisa acrescentar a ele — continua no banco em vez de
/// sumir. **O passado não se reescreve, e aqui ele também não se apaga.**
pub fn save(pontos: &[Ponto]) {
    db::escrever(|conn| {
        let mut stmt = conn.prepare(
            "INSERT INTO patrimonio (dia, total_brl, aporte, retirada, proventos)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(dia) DO UPDATE SET
                 total_brl = excluded.total_brl,
                 aporte    = excluded.aporte,
                 retirada  = excluded.retirada,
                 proventos = excluded.proventos",
        )?;
        for p in pontos {
            stmt.execute((&p.dia, p.total_brl, p.aporte, p.retirada, p.proventos))?;
        }
        Ok(())
    });
}

/// Grava o ponto de hoje, se ainda não houver um.
///
/// **Não sobrescreve** um dia já registrado com um valor mais novo do mesmo dia? Sobrescreve
/// sim, e de propósito: dentro do mesmo dia o número mais recente é o melhor retrato dele.
/// O que nunca acontece é um dia **passado** ser reescrito.
pub fn registrar(
    pontos: &mut Vec<Ponto>,
    agora: u64,
    total: f64,
    aporte: f64,
    retirada: f64,
    proventos: f64,
) -> bool {
    let dia = chave_dia(agora);
    let novo = Ponto {
        dia: dia.clone(),
        total_brl: total,
        aporte,
        retirada,
        proventos,
    };
    match pontos.iter_mut().find(|p| p.dia == dia) {
        Some(slot) => {
            let mudou = (slot.total_brl - total).abs() > 0.005;
            *slot = novo;
            mudou
        }
        None => {
            pontos.push(novo);
            pontos.sort_by(|a, b| a.dia.cmp(&b.dia));
            true
        }
    }
}

/// Os pontos dos últimos `dias`.
pub fn janela(pontos: &[Ponto], agora: u64, dias: u32) -> Vec<&Ponto> {
    let corte = tempo::Data::de_epoch(agora.saturating_sub(dias as u64 * 86400), tempo::BRT_OFFSET);
    let corte = format!("{}-{:02}-{:02}", corte.ano, corte.mes, corte.dia);
    pontos.iter().filter(|p| p.dia >= corte).collect()
}

/// A decomposição de um período: quanto foi aporte, quanto foi mercado, quanto foi
/// provento.
///
/// **A variação de mercado é o resto, de propósito.** É o único termo que não se informa,
/// e fechá-lo por diferença garante que a decomposição some exatamente ao que aconteceu.
pub struct Decomposicao {
    pub aportes: f64,
    pub retiradas: f64,
    pub proventos: f64,
    pub mercado: f64,
    pub total: f64,
}

pub fn decompor(janela: &[&Ponto]) -> Option<Decomposicao> {
    let (primeiro, ultimo) = (janela.first()?, janela.last()?);
    if janela.len() < 2 {
        return None;
    }
    let aportes: f64 = janela.iter().skip(1).map(|p| p.aporte).sum();
    let retiradas: f64 = janela.iter().skip(1).map(|p| p.retirada).sum();
    let proventos: f64 = janela.iter().skip(1).map(|p| p.proventos).sum();
    let total = ultimo.total_brl - primeiro.total_brl;
    Some(Decomposicao {
        aportes,
        retiradas,
        proventos,
        mercado: total - aportes + retiradas - proventos,
        total,
    })
}

/// O rumo de um período: quanto o patrimônio andou, e quanto disso foi mercado.
///
/// Os dois números são diferentes e os dois importam. «O patrimônio subiu R$ 40 mil» é
/// verdade e pode ser só um aporte; «o mercado deu R$ 3 mil» é o que responde se as
/// escolhas estão indo bem. Separar os dois é o motivo de a série guardar os fluxos.
///
/// `None` quando a janela não tem dois pontos — e aí a tela diz que ainda não tem o que
/// dizer, em vez de mostrar zero. Zero é uma afirmação, e ela seria falsa.
pub struct Rumo {
    /// O patrimônio no começo da janela, base da porcentagem.
    pub de: f64,
    /// Quanto o patrimônio andou no total, aportes incluídos.
    pub total: f64,
    /// A parte que foi mercado — o resto, depois de tirar aporte, retirada e provento.
    pub mercado: f64,
    /// Quantos dias do período a série de fato cobre.
    pub cobre: u32,
    /// Quantos dias o período tem. Pedir trinta e um e ter cinco é informação, não
    /// detalhe: uma performance «do mês passado» sobre cinco dias não é a do mês.
    pub pedido: u32,
}

impl Rumo {
    /// Se a série cobre o período pedido de ponta a ponta.
    ///
    /// Fim de semana e feriado não têm ponto, então exigir todos os dias recusaria toda
    /// semana. O que se exige é que a série comece no primeiro dia útil do período e vá
    /// até o último — na prática, que ela não tenha um buraco maior que um fim de semana
    /// prolongado nas pontas.
    pub fn completo(&self) -> bool {
        self.cobre + 4 >= self.pedido
    }

    pub fn pct(&self) -> Option<f64> {
        (self.de > 0.0).then(|| self.total / self.de * 100.0)
    }
}

/// O rumo entre duas datas de calendário, inclusive nas duas pontas.
///
/// **Período fechado, e não janela móvel.** «A performance da semana passada» é uma coisa
/// que já aconteceu e não muda mais; uma janela de sete dias para trás dá um número
/// diferente a cada dia e nunca fecha. Ver `tempo::semana_anterior` e `tempo::mes_passado`.
pub fn rumo(pontos: &[Ponto], de: &Data, ate: &Data) -> Option<Rumo> {
    let (a, b) = (chave(de), chave(ate));
    let janela: Vec<&Ponto> = pontos.iter().filter(|p| p.dia >= a && p.dia <= b).collect();
    let d = decompor(&janela)?;
    let (pa, pb) = (janela.first()?.data()?, janela.last()?.data()?);
    Some(Rumo {
        de: janela.first()?.total_brl,
        total: d.total,
        mercado: d.mercado,
        cobre: (tempo::dias_de(pb.ano, pb.mes, pb.dia) - tempo::dias_de(pa.ano, pa.mes, pa.dia))
            .max(0) as u32
            + 1,
        pedido: (tempo::dias_de(ate.ano, ate.mes, ate.dia) - tempo::dias_de(de.ano, de.mes, de.dia))
            .max(0) as u32
            + 1,
    })
}

/// A chave de um dia, no mesmo formato de `Ponto::dia` — que ordena alfabeticamente na
/// ordem certa, e é por isso que a comparação de datas aqui é comparação de texto.
fn chave(d: &Data) -> String {
    format!("{}-{:02}-{:02}", d.ano, d.mes, d.dia)
}

/// Quantos dias da janela não têm ponto. Desenhados como lacuna, nunca como linha reta:
/// interpolar sobre uma semana sem o programa aberto inventaria uma história.
pub fn lacunas(janela: &[&Ponto]) -> u32 {
    let (Some(a), Some(b)) = (
        janela.first().and_then(|p| p.data()),
        janela.last().and_then(|p| p.data()),
    ) else {
        return 0;
    };
    let esperados = (tempo::dias_de(b.ano, b.mes, b.dia) - tempo::dias_de(a.ano, a.mes, a.dia) + 1)
        .max(0) as u32;
    esperados.saturating_sub(janela.len() as u32)
}

/// Aportes, retiradas e proventos de um dia, lidos dos lançamentos.
pub fn fluxos_do_dia(portfolio: &crate::invest::model::Portfolio, agora: u64) -> (f64, f64, f64) {
    use crate::invest::model::TipoLancamento;
    let hoje = chave_dia(agora);
    let mut fluxos = (0.0, 0.0, 0.0);
    for l in portfolio
        .lancamentos
        .iter()
        .filter(|l| chave_dia(l.em) == hoje)
    {
        match l.tipo {
            TipoLancamento::Aporte => fluxos.0 += l.valor,
            TipoLancamento::Retirada => fluxos.1 += l.valor,
            TipoLancamento::Provento => fluxos.2 += l.valor,
            _ => {}
        }
    }
    let _ = store::agora;
    fluxos
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ponto(dia: &str, total: f64, aporte: f64) -> Ponto {
        Ponto {
            dia: dia.into(),
            total_brl: total,
            aporte,
            retirada: 0.0,
            proventos: 0.0,
        }
    }

    #[test]
    fn a_decomposicao_soma_exatamente_a_diferenca_das_pontas() {
        // Um patrimônio que subiu 10 mil com 4 mil de aporte subiu 6 mil de mercado.
        let pontos = [
            ponto("2026-01-01", 100_000.0, 0.0),
            ponto("2026-02-01", 110_000.0, 4_000.0),
        ];
        let janela: Vec<&Ponto> = pontos.iter().collect();
        let d = decompor(&janela).unwrap();
        assert!((d.total - 10_000.0).abs() < 1e-9);
        assert!((d.aportes - 4_000.0).abs() < 1e-9);
        assert!((d.mercado - 6_000.0).abs() < 1e-9);
        // E a identidade fecha, que é o ponto de o mercado ser o resto.
        assert!((d.aportes - d.retiradas + d.proventos + d.mercado - d.total).abs() < 1e-9);
    }

    #[test]
    fn um_aporte_nao_vira_valorizacao() {
        // A mentira mais cara que este módulo poderia contar.
        let pontos = [
            ponto("2026-01-01", 100_000.0, 0.0),
            ponto("2026-01-02", 110_000.0, 10_000.0),
        ];
        let janela: Vec<&Ponto> = pontos.iter().collect();
        let d = decompor(&janela).unwrap();
        assert!(
            d.mercado.abs() < 1e-9,
            "mercado deveria ser zero, deu {}",
            d.mercado
        );
    }

    #[test]
    fn um_ponto_so_nao_da_decomposicao() {
        let pontos = [ponto("2026-01-01", 100.0, 0.0)];
        let janela: Vec<&Ponto> = pontos.iter().collect();
        assert!(decompor(&janela).is_none());
    }

    #[test]
    fn registrar_e_idempotente_no_mesmo_dia() {
        let mut pontos = Vec::new();
        let agora = Data {
            ano: 2026,
            mes: 9,
            dia: 8,
        }
        .epoch_inicio(tempo::BRT_OFFSET)
            + 3600;
        registrar(&mut pontos, agora, 100.0, 0.0, 0.0, 0.0);
        registrar(&mut pontos, agora, 105.0, 0.0, 0.0, 0.0);
        assert_eq!(pontos.len(), 1, "um ponto por dia");
        assert_eq!(
            pontos[0].total_brl, 105.0,
            "dentro do dia, o mais recente vale"
        );
    }

    #[test]
    fn lacunas_sao_contadas_e_nao_disfarçadas() {
        let pontos = [
            ponto("2026-09-01", 100.0, 0.0),
            ponto("2026-09-05", 100.0, 0.0),
        ];
        let janela: Vec<&Ponto> = pontos.iter().collect();
        // Cinco dias esperados, dois presentes.
        assert_eq!(lacunas(&janela), 3);
    }
}

#[cfg(test)]
mod rumo_tests {
    use super::{Ponto, rumo};
    use crate::invest::tempo::Data;

    fn dia(ano: i32, mes: u32, dia: u32, total: f64, aporte: f64) -> Ponto {
        Ponto {
            dia: format!("{ano}-{mes:02}-{dia:02}"),
            total_brl: total,
            aporte,
            retirada: 0.0,
            proventos: 0.0,
        }
    }

    fn d(ano: i32, mes: u32, dia: u32) -> Data {
        Data { ano, mes, dia }
    }

    /// A semana de 31/08 a 04/09 de 2026 — segunda a sexta.
    fn semana() -> (Data, Data) {
        (d(2026, 8, 31), d(2026, 9, 4))
    }

    #[test]
    fn o_aporte_nao_vira_valorizacao() {
        // O erro caro deste módulo: o patrimônio subiu R$ 10 mil e R$ 8 mil foram dinheiro
        // novo. Dizer «subiu 10 mil» é verdade e não responde nada; o que responde é o
        // mercado separado.
        let pontos = vec![
            dia(2026, 8, 31, 100_000.0, 0.0),
            dia(2026, 9, 4, 110_000.0, 8_000.0),
        ];
        let (de, ate) = semana();
        let r = rumo(&pontos, &de, &ate).expect("dois pontos bastam");
        assert_eq!(r.total, 10_000.0);
        assert_eq!(r.mercado, 2_000.0, "o aporte sai da conta do mercado");
        assert!((r.pct().unwrap() - 10.0).abs() < 1e-9);
        assert_eq!(r.pedido, 5, "segunda a sexta são cinco dias");
        assert!(r.completo(), "as duas pontas do período estão na série");
    }

    #[test]
    fn o_periodo_e_fechado_e_ignora_o_que_esta_fora_dele() {
        // O ponto de ser calendário e não janela móvel: um dia depois da sexta não entra
        // na performance da semana passada, por mais recente que ele seja.
        let pontos = vec![
            dia(2026, 8, 28, 90_000.0, 0.0),
            dia(2026, 8, 31, 100_000.0, 0.0),
            dia(2026, 9, 4, 110_000.0, 0.0),
            dia(2026, 9, 8, 200_000.0, 0.0),
        ];
        let (de, ate) = semana();
        let r = rumo(&pontos, &de, &ate).unwrap();
        assert_eq!(r.de, 100_000.0, "começa na segunda, não antes");
        assert_eq!(r.total, 10_000.0, "termina na sexta, não hoje");
    }

    #[test]
    fn com_um_ponto_so_nao_ha_rumo_e_isso_nao_e_zero() {
        // Zero é uma afirmação, e ela seria falsa: não é que não andou, é que não se sabe.
        let (de, ate) = semana();
        assert!(rumo(&[dia(2026, 9, 1, 100_000.0, 0.0)], &de, &ate).is_none());
        assert!(rumo(&[], &de, &ate).is_none());
    }

    #[test]
    fn uma_serie_que_cobre_meio_periodo_diz_que_cobre_meio() {
        // «O mês passado» sobre cinco dias não é o mês passado, e a tela precisa saber
        // disso para não rotular errado.
        let pontos: Vec<Ponto> = (1..=5)
            .map(|i| dia(2026, 8, 20 + i, 100_000.0 + i as f64 * 1_000.0, 0.0))
            .collect();
        let r = rumo(&pontos, &d(2026, 8, 1), &d(2026, 8, 31)).unwrap();
        assert_eq!(r.pedido, 31);
        assert_eq!(r.cobre, 5);
        assert!(!r.completo(), "cinco de trinta e um não é o mês");
    }

    #[test]
    fn um_fim_de_semana_sem_ponto_nao_torna_o_periodo_incompleto() {
        // Sábado e domingo não têm pregão, e feriado também não. Exigir todos os dias
        // recusaria toda semana — a folga de quatro dias existe por isso.
        let pontos = vec![
            dia(2026, 8, 31, 100_000.0, 0.0),
            dia(2026, 9, 1, 101_000.0, 0.0),
            dia(2026, 9, 4, 103_000.0, 0.0),
        ];
        let (de, ate) = semana();
        assert!(rumo(&pontos, &de, &ate).unwrap().completo());
    }
}
