//! O que os módulos de mercado repetem: uma tabela de cotações.
//!
//! Cotações, Cripto, Câmbio e Índices são a mesma tela com conjuntos diferentes de
//! ativos e colunas ligeiramente diferentes. Um lugar só evita quatro variações
//! ligeiramente diferentes da mesma coisa — que é exatamente o que o `Pane` existe para
//! impedir, um nível acima.

use crate::invest::calc;
use crate::invest::model::AssetId;
use crate::invest::module::{Ctx, Row, Tone};
use crate::invest::provider::Quote;
use crate::invest::tempo;

/// As colunas de uma tabela de cotação.
///
/// «Lido» é a última, e responde outra coisa que a idade colada no preço. Aquela é a do
/// **dado** — a série da B3 aqui vem de cinco em cinco minutos, então o preço tem os
/// minutos que tiver. Esta é a da **leitura**: se ela diz «há 2 s» e o preço diz «há 16
/// min», o mercado é que está devagar; se ela também envelhece, quem parou foi a busca.
pub const CABECALHO: [&str; 7] = [
    "Ativo", "Último", "Var%", "Máx 24h", "Mín 24h", "Volume", "Lido",
];

/// Os índices das colunas, por nome.
///
/// Um número solto quebra calado quando uma coluna nasce no meio — e uma nasceu: a seta
/// de momento entrou entre o ativo e o preço, e quatro testes passaram a conferir a
/// célula errada sem que nada no compilador reclamasse.
#[cfg(test)]
pub const COL_ATIVO: usize = 0;
#[cfg(test)]
pub const COL_PRECO: usize = 1;
#[cfg(test)]
pub const COL_VAR: usize = 2;
#[cfg(test)]
pub const COL_VOLUME: usize = 5;
#[cfg(test)]
pub const COL_LIDO: usize = 6;

/// A seta do último movimento **deste número**, colada na frente dele.
///
/// Colada, e não numa coluna própria: uma coluna separa a seta do valor que ela
/// qualifica, e o olho tem que ir e voltar. Ela **é** parte do número.
///
/// Um atalho para `momento::momento().seta_de`, com a chave montada aqui para todos os
/// módulos a montarem igual. A chave é por **grandeza**, e não por papel: a máxima do dia
/// pode subir numa volta em que o preço caiu, e uma seta por papel diria o contrário em
/// uma das duas colunas.
pub fn seta_de(ativo: &AssetId, campo: &str, valor: Option<f64>) -> &'static str {
    crate::invest::momento::momento().seta_de(&format!("{ativo}:{campo}"), valor)
}

/// Uma linha de cotação, com a regra de cor que vale em toda a aba.
///
/// **Preço informado ou não-oficial nunca é colorido**, mesmo tendo variação calculável
/// entre dois valores digitados: colorir lhe daria a mesma autoridade visual de um preço
/// ao vivo. E a origem aparece na própria coluna do preço, não escondida.
pub fn linha(
    ativo: &AssetId,
    quote: Option<&Quote>,
    na_carteira: bool,
    agora: u64,
    ctx: &Ctx,
) -> Row {
    let marca = match na_carteira {
        true => "●",
        false => " ",
    };
    let nome = format!("{marca}{}", ativo.short());

    let Some(q) = quote else {
        // «Sem provedor» é uma afirmação, e ela era falsa na primeira volta: a fonte
        // existe e ainda não respondeu. O usuário via «sem provedor» piscar e virar
        // preço — uma linha que se contradiz em dois segundos.
        let ainda_nao = ctx.providers.quem_cobre(ativo).is_some();
        return Row::tinted(
            vec![
                nome,
                "—".into(),
                String::new(),
                String::new(),
                String::new(),
                match ainda_nao {
                    true => "buscando…".into(),
                    false => "nenhuma fonte cobre este ativo".to_string(),
                },
                String::new(),
            ],
            Tone::Dim,
        );
    };

    let ao_vivo = q.grade.ao_vivo();
    // A fonte sempre aparece, e a marca de qualidade junto quando ela existe. «48,08» não
    // diz nada; «48,08 brapi» diz a quem reclamar, e «48,08 yahoo não-oficial» diz que
    // não há contrato nenhum atrás daquele número.
    // **Sem a palavra «atrasado».** Ela ocupava espaço em toda linha para dizer o que a
    // cor diz de relance — e agora diz: o rabicho da célula (a fonte e a idade) sai em
    // amarelo quando o preço não é ao vivo. Quem é ao vivo continua sem aviso nenhum,
    // que é o bom estado.
    use crate::invest::provider::Grade;
    let origem = match q.grade {
        Grade::Manual => "manual".to_string(),
        Grade::NaoOficial => format!("{} não-oficial", q.fonte),
        _ => q.fonte.to_string(),
    };
    // A idade vai junto do preço, sempre — é a única coluna em que ela é sobre **este**
    // número. Antes ela só aparecia na coluna de volume, e só nos ativos sem volume: os
    // que tinham volume não diziam de quando era o preço.
    let rabicho = format!("{origem}·{}", tempo::idade(q.idade(agora)));
    let preco = format!(
        "{}{} {rabicho}",
        seta_de(ativo, "preco", Some(q.preco)),
        calc::preco(q.preco)
    );
    // Amarelo é «este número não é de agora». Um preço ao vivo não ganha cor nenhuma.
    let tom_rabicho = match q.grade {
        Grade::AoVivo => Tone::Dim,
        _ => Tone::Aviso,
    };
    // Cada número tem a **própria** seta: a máxima do dia pode subir numa volta em que o
    // preço caiu, e repetir a seta do preço nas outras colunas diria o contrário numa
    // delas.
    let extremo = |campo: &str, v: Option<f64>| {
        v.map(|v| format!("{}{}", seta_de(ativo, campo, Some(v)), calc::preco(v)))
            .unwrap_or_default()
    };
    // A variação só é mostrada onde ela significa alguma coisa.
    let (variacao, tom_var) = match q.variacao().filter(|_| ao_vivo) {
        Some(v) => (
            format!("{}{}", seta_de(ativo, "var", Some(v)), calc::pct(v)),
            match v >= 0.0 {
                true => Tone::Bom,
                false => Tone::Ruim,
            },
        ),
        None => (String::new(), Tone::Dim),
    };

    let volume = match q.volume {
        Some(v) if v > 0.0 => calc::moeda(v),
        _ => String::new(),
    };

    Row::new(vec![
        nome,
        preco,
        variacao,
        extremo("max", q.max24),
        extremo("min", q.min24),
        volume,
        // Do cache **com a idade**: o valor veio do disco e ainda não foi rebuscado
        // nesta execução, e o que se quer saber é se aquele disco é de uma hora ou de
        // uma semana.
        match (ctx.market.idade_da_leitura(ativo, agora), q.fonte, q.grade) {
            // Informado nunca foi lido de lugar nenhum, e a coluna fica calada em vez de
            // inventar uma leitura. A idade do número está colada no preço.
            (_, _, Grade::Manual) => String::new(),
            (Some(s), "cache", _) => format!("do cache · {}", tempo::idade(s)),
            (Some(s), _, _) => tempo::idade(s),
            (None, _, _) => "do cache".into(),
        },
    ])
    .with_cell_tones(vec![
        match na_carteira {
            true => Tone::Normal,
            false => Tone::Dim,
        },
        match ao_vivo {
            true => Tone::Normal,
            false => Tone::Dim,
        },
        tom_var,
        Tone::Dim,
        Tone::Dim,
        Tone::Dim,
        Tone::Dim,
    ])
    .com_rabicho(1, rabicho, tom_rabicho)
}

/// O estado das fontes, para a linha que responde «por que este número não mexe».
pub fn nota_fontes(ctx: &Ctx) -> Option<(String, Tone)> {
    // Nenhuma fonte na lista é diferente de todas ok: significa que a busca ainda não
    // deu a primeira volta. Dizer isso evita a leitura de que não há o que buscar.
    if ctx.market.fontes.is_empty() {
        return Some(("buscando pela primeira vez…".to_string(), Tone::Aviso));
    }
    let mut partes: Vec<String> = ctx
        .market
        .fontes
        .iter()
        .map(|f| {
            match (
                f.ok.then_some(()).xor(Some(())).and(f.erro.as_ref()),
                f.espera,
            ) {
                (Some(e), Some(espera)) => format!(
                    "{} caiu: {e} — tento de novo em {}s",
                    f.nome,
                    espera.as_secs()
                ),
                (Some(e), None) => format!("{} caiu: {e}", f.nome),
                // Quando está tudo bem, a linha diz **desde quando**: uma fonte «ok» que
                // respondeu pela última vez há dez minutos não está ok, e sem a idade isso
                // ficaria invisível. E a cota gasta, na fonte que se auto-reporta.
                _ => {
                    let idade = f
                        .ultima_volta
                        .map(|em| format!(" ({})", tempo::idade(ctx.agora.saturating_sub(em))))
                        .unwrap_or_default();
                    let peso = f.peso.map(|p| format!(" cota {p}")).unwrap_or_default();
                    format!("{} ok{idade}{peso}", f.nome)
                }
            }
        })
        .collect();

    // Três contagens diferentes, porque são três situações diferentes — e chamar todas
    // de «informado» era errado: um preço do Yahoo não foi digitado por ninguém, ele
    // simplesmente vem de uma fonte sem contrato.
    use crate::invest::provider::Grade;
    let mut informados = 0;
    let mut nao_oficiais = 0;
    let mut sem_preco = 0;
    for p in &ctx.portfolio.posicoes {
        match ctx.market.quote(&p.ativo).map(|q| q.grade) {
            Some(Grade::Manual) => informados += 1,
            Some(Grade::NaoOficial) => nao_oficiais += 1,
            None if p.preco_manual.is_some() => informados += 1,
            None => sem_preco += 1,
            _ => {}
        }
    }
    if informados > 0 {
        partes.push(format!("{informados} em preço informado"));
    }
    if nao_oficiais > 0 {
        partes.push(format!("{nao_oficiais} de fonte não-oficial"));
    }
    if sem_preco > 0 {
        partes.push(format!("{sem_preco} sem preço nenhum"));
    }
    // Amarela, como sempre foi: esta nota fala do estado das fontes, e o estado das
    // fontes é ressalva, nunca resultado.
    (!partes.is_empty()).then(|| (partes.join(" · "), Tone::Aviso))
}

/// Se um ativo está na carteira — o que ganha a marca `●`.
pub fn na_carteira(ctx: &Ctx, ativo: &AssetId) -> bool {
    ctx.portfolio.posicoes.iter().any(|p| p.ativo == *ativo)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invest::model::{Market, Moeda, Portfolio};
    use crate::invest::module::{ctx_de_teste, providers_de_teste};
    use crate::invest::provider::{Grade, MarketSnapshot};

    /// Um contexto de teste vivo enquanto a chamada durar.
    macro_rules! com_ctx {
        (|$ctx:ident| $corpo:expr) => {{
            let portfolio = Portfolio::default();
            let market = MarketSnapshot::default();
            let providers = providers_de_teste();
            let $ctx = &ctx_de_teste(&portfolio, &market, &providers);
            $corpo
        }};
    }

    fn quote(grade: Grade, preco: f64, anterior: Option<f64>) -> Quote {
        Quote {
            fonte: "teste",
            ativo: AssetId::new(Market::B3, "PETR4"),
            preco,
            anterior,
            moeda: Moeda::Brl,
            grade,
            em: 0,
            volume: None,
            max24: None,
            min24: None,
        }
    }

    #[test]
    fn preco_informado_nunca_ganha_cor_de_variacao() {
        let ativo = AssetId::new(Market::B3, "PETR4");
        let q = quote(Grade::Manual, 38.42, Some(38.0));
        let r = com_ctx!(|ctx| linha(&ativo, Some(&q), true, 0, ctx));
        assert!(r.cells[COL_VAR].is_empty(), "informado não mostra variação");
        assert_eq!(r.cell_tones[COL_VAR], Tone::Dim);
        assert!(
            r.cells[COL_PRECO].contains("manual"),
            "a origem fica visível na coluna"
        );
    }

    #[test]
    fn preco_ao_vivo_ganha_verde_ou_vermelho() {
        let ativo = AssetId::new(Market::B3, "PETR4");
        let sobe = com_ctx!(|ctx| linha(
            &ativo,
            Some(&quote(Grade::AoVivo, 38.42, Some(38.0))),
            true,
            0,
            ctx
        ));
        assert_eq!(sobe.cell_tones[COL_VAR], Tone::Bom);
        let cai = com_ctx!(|ctx| linha(
            &ativo,
            Some(&quote(Grade::AoVivo, 37.0, Some(38.0))),
            true,
            0,
            ctx
        ));
        assert_eq!(cai.cell_tones[COL_VAR], Tone::Ruim);
    }

    #[test]
    fn sem_provedor_a_linha_diz_isso_em_vez_de_zerar() {
        let ativo = AssetId::new(Market::B3, "PETR4");
        // Sem nenhum provedor cobrindo o ativo, a linha **afirma** isso. Quando há fonte
        // e ela ainda não respondeu, a linha diz «buscando» — as duas coisas são
        // diferentes, e trocá-las fazia a tela se contradizer em dois segundos.
        let r = com_ctx!(|ctx| linha(&ativo, None, false, 0, ctx));
        assert_eq!(r.cells[COL_PRECO], "—");
        assert!(
            r.cells[COL_VOLUME].contains("buscando"),
            "com fonte cobrindo, a linha tem que dizer que está buscando: «{}»",
            r.cells[COL_VOLUME]
        );
    }

    #[test]
    fn a_marca_separa_o_que_e_meu_do_que_eu_so_acompanho() {
        let ativo = AssetId::new(Market::B3, "PETR4");
        let minha = com_ctx!(|ctx| linha(&ativo, None, true, 0, ctx));
        let outra = com_ctx!(|ctx| linha(&ativo, None, false, 0, ctx));
        assert!(minha.cells[COL_ATIVO].starts_with('●'));
        assert!(!outra.cells[COL_ATIVO].starts_with('●'));
    }

    #[test]
    fn a_idade_do_preco_anda_junto_com_o_preco() {
        // Na coluna do preço e não na do volume: a idade é sobre **este** número, e um
        // ativo com volume não pode ficar sem ela só porque a coluna estava ocupada.
        let ativo = AssetId::new(Market::B3, "PETR4");
        let q = quote(Grade::AoVivo, 38.42, None);
        let parada = com_ctx!(|ctx| linha(&ativo, Some(&q), true, 600, ctx));
        assert!(
            parada.cells[COL_PRECO].contains("há 10 min"),
            "deu «{}»",
            parada.cells[COL_PRECO]
        );
        let fresca = com_ctx!(|ctx| linha(&ativo, Some(&q), true, 0, ctx));
        assert!(
            fresca.cells[COL_PRECO].contains("agora"),
            "deu «{}»",
            fresca.cells[COL_PRECO]
        );
    }

    /// A idade da **leitura** é outra coluna que a idade do dado, e as duas juntas
    /// separam «o mercado está devagar» de «a busca parou».
    #[test]
    fn a_coluna_lido_e_a_da_leitura_e_nao_a_do_dado() {
        let ativo = AssetId::new(Market::B3, "PETR4");
        let q = quote(Grade::AoVivo, 38.42, None);
        // Sem leitura registrada — o que veio do disco —, a coluna diz de onde veio.
        let r = com_ctx!(|ctx| linha(&ativo, Some(&q), true, 600, ctx));
        assert_eq!(r.cells[COL_LIDO], "do cache");
        // E a idade do dado continua onde estava, colada no preço.
        assert!(r.cells[COL_PRECO].contains("há 10 min"));
    }

    #[test]
    fn sem_preco_nenhum_a_linha_nao_inventa_idade() {
        let ativo = AssetId::new(Market::B3, "PETR4");
        let r = com_ctx!(|ctx| linha(&ativo, None, true, 600, ctx));
        assert!(
            !r.cells[COL_PRECO].contains("há "),
            "deu «{}»",
            r.cells[COL_PRECO]
        );
    }
}
