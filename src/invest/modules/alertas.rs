//! Alertas: me avise quando acontecer o que eu estou esperando.
//!
//! Este módulo se parece com uma ferramenta porque **é** uma: coisas que o usuário cria,
//! que rodam em segundo plano, que sobrevivem à troca de aba, que voltam vivas depois de
//! reiniciar, e que se ligam e desligam com espaço — a mesma forma da aba Ferramentas.
//!
//! É também a única exceção à regra de custo zero fora de foco, e as condições dela estão
//! em `docs/invest/42 §4`: cada alerta é ligado explicitamente, a tela diz o custo, a
//! cadência de fundo é mais lenta, os ativos vão num lote só, e **nada de alerta sobre
//! preço informado** — um preço que só muda quando alguém o digita não tem o que disparar.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::invest::calc;
use crate::invest::model::{Alerta, AssetId, RegraAlerta};
use crate::invest::module::{
    Ctx, Edit, Escape, Field, Group, InvestModule, Layout, Marcavel, ModuleView, Need, Outcome,
    Pane, Row, Tone,
};
use crate::invest::modules::comum::{Formulario, Lista, hint};
use crate::invest::provider::MarketSnapshot;
use crate::invest::tempo::{self, Data};

pub struct Alertas;

impl InvestModule for Alertas {
    fn id(&self) -> &'static str {
        "alertas"
    }
    fn name(&self) -> &'static str {
        "Alertas"
    }
    fn description(&self) -> &'static str {
        "Regras de preço e variação que rodam mesmo com a aba fechada"
    }
    fn destaque(&self) -> u8 {
        3
    }
    fn group(&self) -> Group {
        Group::Informacao
    }

    fn marcavel(&self) -> Option<Marcavel> {
        Some(crate::invest::marcas::ATIVOS)
    }
    fn needs(&self) -> &'static [Need] {
        &[Need::Cotacao]
    }
    fn keywords(&self) -> &'static str {
        "avisar aviso gatilho regra preço variação disparo"
    }

    fn summary(&self, ctx: &Ctx) -> String {
        let ligados = ctx.portfolio.alertas.iter().filter(|a| a.ligado).count();
        match ctx.portfolio.alertas.len() {
            0 => "nenhuma regra".into(),
            n => format!("{n} regra(s) · {ligados} ligada(s)"),
        }
    }

    fn widget(&self, ctx: &Ctx) -> Option<Pane> {
        if ctx.portfolio.alertas.is_empty() {
            return None;
        }
        Some(Pane::Facts {
            title: String::new(),
            rows: ctx
                .portfolio
                .alertas
                .iter()
                .take(8)
                .map(|a| {
                    // A regra e se ela está valendo **agora**. Um alerta que já bateu e
                    // um que está longe de bater não podem parecer a mesma coisa numa
                    // tela que se olha de passagem.
                    let bateu = condicao(a, ctx.market);
                    (
                        format!("{} {}", a.ativo.short(), valor_da_regra(a)),
                        match (a.ligado, bateu) {
                            (false, _) => "desligado".into(),
                            (true, Some(true)) => "bateu".into(),
                            (true, Some(false)) => "vigiando".into(),
                            (true, None) => "sem preço".into(),
                        },
                        match (a.ligado, bateu) {
                            (false, _) => Tone::Dim,
                            (true, Some(true)) => Tone::Ruim,
                            (true, Some(false)) => Tone::Bom,
                            (true, None) => Tone::Dim,
                        },
                    )
                })
                .collect(),
        })
    }

    fn open(&self, _ctx: &Ctx, _alvo: Option<&AssetId>) -> Box<dyn ModuleView> {
        Box::new(Vista {
            lista: Lista::default(),
            form: None,
        })
    }
}

/// Se a condição de um alerta vale agora.
pub fn condicao(alerta: &Alerta, market: &MarketSnapshot) -> Option<bool> {
    let q = market.quote(&alerta.ativo)?;
    // Preço informado não dispara nada — e o formulário já recusa criar um alerta sobre
    // ele, com a caixa aberta.
    if !q.grade.ao_vivo() {
        return None;
    }
    Some(match alerta.regra {
        RegraAlerta::Acima => q.preco >= alerta.valor,
        RegraAlerta::Abaixo => q.preco <= alerta.valor,
        RegraAlerta::Varia => q.variacao().is_some_and(|v| v.abs() >= alerta.valor),
    })
}

/// Avalia todos os alertas e devolve os que **acabaram de** disparar.
///
/// Uma regra dispara uma vez e fica armada de novo só depois de a condição deixar de
/// valer. Um alerta de «BTC acima de 500 mil» que dispara a cada volta enquanto o preço
/// fica acima é um alerta que se aprende a ignorar em dez minutos.
pub fn avaliar(alertas: &mut [Alerta], market: &MarketSnapshot, agora: u64) -> Vec<usize> {
    let mut disparos = Vec::new();
    for (i, a) in alertas.iter_mut().enumerate() {
        if !a.ligado {
            continue;
        }
        let Some(vale) = condicao(a, market) else {
            continue;
        };
        if vale && !a.armado {
            a.armado = true;
            a.ultimo_disparo = Some(agora);
            disparos.push(i);
        } else if !vale {
            a.armado = false;
        }
    }
    disparos
}

struct Vista {
    lista: Lista,
    form: Option<Formulario>,
}

/// O valor de uma regra, na unidade dela.
fn valor_da_regra(a: &Alerta) -> String {
    match a.regra.e_percentual() {
        true => calc::pct_casas(a.valor, 2),
        false => calc::preco(a.valor),
    }
}

/// As linhas da tabela, na mesma ordem de `portfolio.alertas`.
fn linhas(ctx: &Ctx) -> Vec<Row> {
    ctx.portfolio
        .alertas
        .iter()
        .map(|a| {
            let estado = match a.ligado {
                true => "●",
                false => "○",
            };
            let atual = ctx
                .market
                .quote(&a.ativo)
                .map(|q| calc::preco(q.preco))
                .unwrap_or("—".into());
            Row::new(vec![
                estado.to_string(),
                a.ativo.short().to_string(),
                format!("{} {}", a.regra.label(), valor_da_regra(a)),
                atual,
                a.ultimo_disparo
                    .map(|d| Data::de_epoch(d, tempo::BRT_OFFSET).longa())
                    .unwrap_or("—".into()),
            ])
            .with_cell_tones(vec![
                match a.ligado {
                    true => Tone::Bom,
                    false => Tone::Dim,
                },
                Tone::Normal,
                Tone::Normal,
                Tone::Dim,
                match a.armado {
                    true => Tone::Aviso,
                    false => Tone::Dim,
                },
            ])
        })
        .collect()
}

impl ModuleView for Vista {
    fn title(&self) -> String {
        "Alertas".into()
    }

    fn quer_mercado(&self) -> bool {
        true
    }

    fn ativos(&self) -> Vec<AssetId> {
        Vec::new()
    }

    fn layout(&self, ctx: &Ctx) -> Layout {
        // As linhas na mesma ordem dos alertas — é o que permite resolver o cursor
        // filtrado para a regra certa. Ver `Lista::atual`.
        if let Some(form) = &self.form {
            return Layout::one(Pane::Form {
                title: form.titulo.clone(),
                fields: form.campos.clone(),
                selected: form.selecionado,
                error: form.erro.clone(),
                hint: hint(&["↑/↓ campo", "←/→ regra", "Enter criar", "Esc cancelar"]),
            });
        }
        if ctx.portfolio.alertas.is_empty() {
            return Layout::one(Pane::Empty {
                title: "Alertas".into(),
                note: "Nenhuma regra ainda.\n\n\
                       Ctrl+A cria uma. Um alerta ligado é a única coisa desta aba que \n\
                       roda com a aba fechada — por isso ele é ligado por você, um a um, \n\
                       e a tela mostra quantas requisições por minuto o conjunto gera.\n\n\
                       Ele avisa dentro do programa: um contador ao lado de «Invest» na \n\
                       barra, e o histórico aqui. Não interrompe a tela, não abre caixa \n\
                       por cima do que você está fazendo, e não emite som."
                    .into(),
            });
        }

        let rows: Vec<Row> = linhas(ctx);

        let ligados = ctx.portfolio.alertas.iter().filter(|a| a.ligado).count();
        // Um lote por volta, e a volta de fundo é de 60 s — então o custo é uma
        // requisição por minuto por provedor, não uma por alerta.
        let provedores: std::collections::BTreeSet<&str> = ctx
            .portfolio
            .alertas
            .iter()
            .filter(|a| a.ligado)
            .filter_map(|a| ctx.providers.quem_cobre(&a.ativo).map(|p| p.id()))
            .collect();

        Layout::rows(vec![
            (
                3,
                Layout::one(Pane::Table {
                    title: format!(
                        "Alertas · {} · {ligados} ligada(s)",
                        calc::plural(rows.len(), "regra", "regras")
                    ),
                    headers: vec![
                        String::new(),
                        "Ativo".into(),
                        "Regra".into(),
                        "Agora".into(),
                        "Último disparo".into(),
                    ],
                    rows,
                    selected: Some(self.lista.selecionado),
                    query: self.lista.busca.clone(),
                    note: None,
                }),
            ),
            (
                2,
                Layout::one(Pane::Text {
                    title: format!(
                        "Histórico · {}",
                        calc::plural(ctx.disparos.len(), "disparo", "disparos")
                    ),
                    lines: match ctx.disparos.is_empty() {
                        true => vec![("  nada disparou desde que a aba abriu".into(), Tone::Dim)],
                        false => ctx
                            .disparos
                            .iter()
                            .map(|(em, texto)| {
                                (
                                    format!(
                                        "  {}  {texto}",
                                        Data::de_epoch(*em, tempo::BRT_OFFSET).longa()
                                    ),
                                    Tone::Aviso,
                                )
                            })
                            .collect(),
                    },
                    scroll: 0,
                }),
            ),
            (
                1,
                Layout::one(Pane::Facts {
                    title: "O que isto custa".into(),
                    rows: vec![
                        (
                            "com a aba fechada".into(),
                            match ligados {
                                0 => "nada — nenhuma regra ligada".to_string(),
                                _ => format!(
                                    "{} requisição(ões) por minuto, para {}",
                                    provedores.len().max(1),
                                    provedores
                                        .iter()
                                        .copied()
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                ),
                            },
                            match ligados {
                                0 => Tone::Dim,
                                _ => Tone::Aviso,
                            },
                        ),
                        (
                            "cadência".into(),
                            "60 s em segundo plano — um alerta não precisa da resolução de uma tela"
                                .into(),
                            Tone::Dim,
                        ),
                        (
                            "dispara uma vez".into(),
                            "e rearma só quando a condição deixa de valer".into(),
                            Tone::Dim,
                        ),
                    ],
                }),
            ),
        ])
    }

    fn key(&mut self, key: KeyEvent, ctx: &Ctx) -> Outcome {
        if let Some(form) = &mut self.form {
            if form.tecla(key) {
                let ativo = match AssetId::parse(&form.valor("ativo")) {
                    Ok(a) => a,
                    Err(e) => {
                        form.erro = Some(e);
                        return Outcome::Ok;
                    }
                };
                // Recusado com a caixa aberta: um preço que só muda quando você o digita
                // não tem o que disparar.
                if ctx.market.quote(&ativo).is_some_and(|q| !q.grade.ao_vivo()) {
                    form.erro = Some(
                        "esse ativo usa preço informado — ele não muda sozinho, então não há o que disparar"
                            .into(),
                    );
                    return Outcome::Ok;
                }
                let Some(valor) = calc::ler_numero(&form.valor("valor")) else {
                    form.erro = Some("valor não é um número".into());
                    return Outcome::Ok;
                };
                let regra = RegraAlerta::ALL
                    .iter()
                    .find(|r| r.label() == form.valor("regra"))
                    .copied()
                    .unwrap_or(RegraAlerta::Acima);
                let mut alertas = ctx.portfolio.alertas.clone();
                alertas.push(Alerta {
                    ativo,
                    regra,
                    valor,
                    ligado: true,
                    armado: false,
                    ultimo_disparo: None,
                });
                self.form = None;
                return Outcome::Editar(vec![Edit::SetAlertas(alertas)]);
            }
            return Outcome::Ok;
        }

        // Pelo índice **visível**: com busca ativa, `selecionado` aponta para a lista
        // filtrada, e usá-lo aqui ligaria ou apagaria outra regra.
        let rows = linhas(ctx);
        let atual = self.lista.atual(&rows);
        match key.code {
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.form = Some(Formulario::novo(
                    "Nova regra",
                    vec![
                        Field::text("ativo", "BINANCE/", "Só ativos com preço ao vivo"),
                        Field::choice(
                            "regra",
                            RegraAlerta::Acima.label(),
                            RegraAlerta::ALL
                                .iter()
                                .map(|r| r.label().to_string())
                                .collect(),
                            "←/→ escolhe",
                        ),
                        Field::text("valor", "", "O preço, ou a variação percentual do dia"),
                    ],
                ));
                Outcome::Ok
            }
            // Espaço liga e desliga, mesmo gesto da aba Ferramentas.
            KeyCode::Char(' ') if atual.is_some() => {
                let i = atual.unwrap_or(0);
                let mut alertas = ctx.portfolio.alertas.clone();
                alertas[i].ligado = !alertas[i].ligado;
                alertas[i].armado = false;
                Outcome::Editar(vec![Edit::SetAlertas(alertas)])
            }
            KeyCode::Delete if atual.is_some() => {
                let mut alertas = ctx.portfolio.alertas.clone();
                alertas.remove(atual.unwrap_or(0));
                Outcome::Editar(vec![Edit::SetAlertas(alertas)])
            }
            _ => match self.lista.tecla(key, rows.len()) {
                true => Outcome::Ok,
                false => Outcome::Ignorada,
            },
        }
    }

    fn escape(&mut self) -> Escape {
        match self.form.take().is_some() {
            true => Escape::Consumido,
            false => self.lista.escape(),
        }
    }

    fn hint(&self) -> String {
        hint(&[
            "espaço ligar/desligar",
            "Ctrl+A nova regra",
            "Del apagar",
            "Esc sair",
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invest::model::Market;
    use crate::invest::model::Moeda;
    use crate::invest::provider::{Grade, Quote};

    fn market(preco: f64, grade: Grade, anterior: Option<f64>) -> MarketSnapshot {
        let ativo = AssetId::new(Market::Binance, "BTCBRL");
        let mut m = MarketSnapshot::default();
        m.quotes.insert(
            ativo.clone(),
            Quote {
                fonte: "teste",
                ativo,
                preco,
                anterior,
                moeda: Moeda::Brl,
                grade,
                em: 0,
                volume: None,
                max24: None,
                min24: None,
            },
        );
        m
    }

    fn alerta(regra: RegraAlerta, valor: f64) -> Alerta {
        Alerta {
            ativo: AssetId::new(Market::Binance, "BTCBRL"),
            regra,
            valor,
            ligado: true,
            armado: false,
            ultimo_disparo: None,
        }
    }

    #[test]
    fn dispara_uma_vez_e_nao_a_cada_volta() {
        let mut alertas = vec![alerta(RegraAlerta::Acima, 500_000.0)];
        let m = market(510_000.0, Grade::AoVivo, None);
        assert_eq!(
            avaliar(&mut alertas, &m, 100).len(),
            1,
            "o primeiro dispara"
        );
        assert!(
            avaliar(&mut alertas, &m, 200).is_empty(),
            "enquanto a condição vale, não dispara de novo"
        );
        // Só depois de a condição deixar de valer é que ele rearma.
        let baixo = market(490_000.0, Grade::AoVivo, None);
        assert!(avaliar(&mut alertas, &baixo, 300).is_empty());
        assert_eq!(
            avaliar(&mut alertas, &m, 400).len(),
            1,
            "rearmado, dispara de novo"
        );
    }

    #[test]
    fn preco_informado_nunca_dispara() {
        let mut alertas = vec![alerta(RegraAlerta::Acima, 100.0)];
        let m = market(510_000.0, Grade::Manual, None);
        assert!(avaliar(&mut alertas, &m, 100).is_empty());
        assert_eq!(condicao(&alertas[0], &m), None);
    }

    #[test]
    fn alerta_desligado_nao_e_avaliado() {
        let mut alertas = vec![alerta(RegraAlerta::Acima, 100.0)];
        alertas[0].ligado = false;
        let m = market(510_000.0, Grade::AoVivo, None);
        assert!(avaliar(&mut alertas, &m, 100).is_empty());
    }

    #[test]
    fn variacao_dispara_nos_dois_sentidos() {
        let mut alertas = vec![alerta(RegraAlerta::Varia, 3.0)];
        // −4% em módulo passa dos 3%.
        let m = market(96.0, Grade::AoVivo, Some(100.0));
        assert_eq!(avaliar(&mut alertas, &m, 100).len(), 1);
    }
}
