//! Agenda: o que vem por aí, e em que dia.
//!
//! A tabela de feriados não é enfeite deste módulo: ela é dependência de
//! `Provider::is_open`, que decide a cadência de toda a aba. Sem ela, o programa fica
//! pedindo preço o dia inteiro num dia em que nada muda.
//!
//! COPOM e IPCA são calendários **anuais publicados uma vez** — não são API, são tabela.
//! Isso é aceitável e está escrito no código: a tabela tem ano de validade, e quando ele
//! passa a tela avisa em vez de ficar em silêncio.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::invest::calc;
use crate::invest::model::AssetId;
use crate::invest::model::TipoProvento;
use crate::invest::module::{
    Ctx, Escape, Group, InvestModule, Layout, Marcavel, ModuleView, Outcome, Pane, Row,
    TipoDeMarca, Tone,
};
use crate::invest::modules::comum::{Lista, hint};
use crate::invest::tempo::{self, Data};

pub struct Agenda;

/// Até que ano as tabelas fixas abaixo valem. Passado ele, a tela avisa.
pub const VALIDADE: i32 = 2026;

/// Feriados de mercado da B3. Fixos por ano, e é o que `Provider::is_open` precisa saber
/// além de fim de semana e hora do dia.
pub const FERIADOS_B3: &[(u32, u32, &str)] = &[
    (1, 1, "Confraternização"),
    (4, 21, "Tiradentes"),
    (5, 1, "Dia do Trabalho"),
    (9, 7, "Independência"),
    (10, 12, "Nossa Senhora Aparecida"),
    (11, 2, "Finados"),
    (11, 15, "Proclamação da República"),
    (11, 20, "Consciência Negra"),
    (12, 25, "Natal"),
];

/// As reuniões do COPOM, publicadas uma vez por ano pelo Banco Central. Segundo dia de
/// cada reunião, que é quando a decisão sai.
pub const COPOM: &[(u32, u32)] = &[
    (1, 28),
    (3, 18),
    (5, 6),
    (6, 17),
    (8, 5),
    (9, 16),
    (11, 4),
    (12, 9),
];

/// Quando sai o próximo número de um indicador.
///
/// Só o COPOM tem data exata aqui, porque é a única que o Banco Central publica como
/// calendário fechado — a tabela acima. Para IPCA e IGP-M o que existe é a janela em que
/// o instituto costuma divulgar, e ela é devolvida **marcada como aproximada**: uma data
/// inventada com cara de exata é pior que uma janela honesta.
pub fn proximo_anuncio(simbolo: &str, hoje: &Data) -> Option<(String, bool)> {
    match simbolo {
        "SELIC-META" | "SELIC" | "SELIC-DIA" | "CDI" => {
            let proxima = COPOM
                .iter()
                .find(|(m, d)| (*m, *d) >= (hoje.mes, hoje.dia))
                .map(|(m, d)| Data {
                    ano: hoje.ano,
                    mes: *m,
                    dia: *d,
                })
                .or_else(|| {
                    // Passou a última do ano: a próxima é a primeira do ano que vem.
                    COPOM.first().map(|(m, d)| Data {
                        ano: hoje.ano + 1,
                        mes: *m,
                        dia: *d,
                    })
                })?;
            Some((format!("COPOM {}", proxima.longa()), true))
        }
        // O IBGE divulga o IPCA na segunda semana do mês seguinte ao de referência; a FGV
        // fecha o IGP-M no fim do mês. São regras de costume, não calendários publicados.
        "IPCA" => Some(("IBGE, por volta do dia 10".to_string(), false)),
        "IGPM" => Some(("FGV, fim do mês".to_string(), false)),
        _ => None,
    }
}

/// Se a B3 abre neste dia. Usada por `provider::b3_aberta` para além do fim de semana.
pub fn feriado_b3(data: &Data) -> Option<&'static str> {
    FERIADOS_B3
        .iter()
        .find(|(m, d, _)| *m == data.mes && *d == data.dia)
        .map(|(_, _, nome)| *nome)
}

/// O que se pode seguir nesta lista. O id da tabela nunca muda depois de publicado — é
/// com ele que as marcas já gravadas se reconhecem.
static MARCAS: Marcavel = Marcavel {
    tabela: "invest-agenda",
    nome: "Agenda",
    tipos: &[TipoDeMarca {
        nome: "evento",
        coluna: "O quê",
        numerico: false,
        ajuda: "o assunto do evento",
    }],
};

impl InvestModule for Agenda {
    fn id(&self) -> &'static str {
        "agenda"
    }
    fn name(&self) -> &'static str {
        "Agenda"
    }
    fn description(&self) -> &'static str {
        "COPOM, IPCA, feriados de mercado e os seus proventos agendados"
    }
    fn destaque(&self) -> u8 {
        5
    }
    fn group(&self) -> Group {
        Group::Informacao
    }

    fn fonte_externa(&self) -> Option<&'static str> {
        Some(crate::invest::store::fonte::AGENDA)
    }

    fn marcavel(&self) -> Option<Marcavel> {
        Some(MARCAS)
    }
    fn keywords(&self) -> &'static str {
        "calendário copom feriado data-ex pagamento vencimento evento"
    }

    fn summary(&self, ctx: &Ctx) -> String {
        let hoje = Data::de_epoch(ctx.agora, tempo::BRT_OFFSET);
        match COPOM.iter().find(|(m, d)| (*m, *d) >= (hoje.mes, hoje.dia)) {
            Some((m, d)) => format!("próximo COPOM em {d:02}/{m:02}"),
            None => "próximo COPOM no ano que vem".into(),
        }
    }

    fn widget(&self, ctx: &Ctx) -> Option<Pane> {
        // **A mesma função da tela cheia.** Era uma lista montada à mão aqui, que sabia de
        // COPOM e feriado e não sabia do calendário do IBGE — a maior parte dos eventos.
        // Abrir o módulo mostrava uma agenda; a home mostrava outra.
        //
        // `get` dispara a busca em segundo plano e devolve o que já há — o cartão não
        // espera por ela. É o que enche o painel sem obrigar a entrar no módulo, e custa
        // uma chamada só: o calendário do IBGE é pedido uma vez por sessão.
        let hoje_ = Data::de_epoch(ctx.agora, tempo::BRT_OFFSET);
        let (de, ate) = crate::invest::calendario::janela(&hoje_, 90);
        let publicados = ctx.calendario.get(&de, &ate);
        let eventos = eventos(ctx, &publicados, PESO_MINIMO, 90);
        if eventos.is_empty() {
            return None;
        }
        Some(Pane::Facts {
            title: String::new(),
            rows: eventos
                .into_iter()
                .take(10)
                .map(|e| {
                    (
                        format!("{:02}/{:02}", e.data.dia, e.data.mes),
                        e.texto,
                        match e.meu {
                            true => Tone::Destaque,
                            false => Tone::Normal,
                        },
                    )
                })
                .collect(),
        })
    }

    fn open(&self, _ctx: &Ctx, _alvo: Option<&AssetId>) -> Box<dyn ModuleView> {
        Box::new(Vista {
            lista: Lista::default(),
            so_meus: false,
            peso_minimo: PESO_MINIMO,
        })
    }
}

/// O piso de relevância. «2 estrelinhas» é o corte usual de um calendário econômico:
/// abaixo dele está o ruído que faz o que importa sumir. Fica aqui, e não só dentro da
/// vista, porque a home usa o mesmo corte — duas telas com cortes diferentes mostrariam
/// agendas diferentes, que é o que este módulo acabou de deixar de fazer.
const PESO_MINIMO: u8 = 2;

/// Monta a agenda: calendário publicado, COPOM, feriado da B3 e os proventos da carteira.
///
/// **Uma função só, usada pela tela cheia e pelo cartão da home.** Eram duas, e elas
/// divergiram: o cartão sabia de COPOM e feriado, e não sabia do calendário do IBGE — que
/// é a maior parte da lista. Duas montagens são duas chances de só uma ser corrigida.
///
/// `publicados` é o que já foi buscado. Quem chama decide se dispara a busca: a tela cheia
/// dispara, a home nunca.
pub fn eventos(
    ctx: &Ctx,
    publicados: &[crate::invest::calendario::Evento],
    peso_minimo: u8,
    dias: i64,
) -> Vec<Evento> {
    let hoje = Data::de_epoch(ctx.agora, tempo::BRT_OFFSET);
    let hoje_dias = tempo::dias_de(hoje.ano, hoje.mes, hoje.dia);
    let mut eventos: Vec<Evento> = Vec::new();

    let mut empurrar = |data: Data, texto: String, meu: bool| {
        let dia = tempo::dias_de(data.ano, data.mes, data.dia);
        // Do hoje para a frente, e nada do passado — uma agenda que mostra o que já passou
        // é uma lista, não uma agenda.
        if dia >= hoje_dias && dia - hoje_dias <= dias {
            eventos.push(Evento {
                dia,
                data,
                texto,
                meu,
            });
        }
    };

    for (mes, dia) in COPOM {
        empurrar(
            Data {
                ano: hoje.ano,
                mes: *mes,
                dia: *dia,
            },
            "COPOM · decisão de juros".into(),
            false,
        );
    }
    // O calendário econômico de verdade: IBGE para o Brasil, e as regras que valem sem
    // consultar ninguém para o exterior.
    for e in publicados
        .iter()
        .cloned()
        .chain(crate::invest::calendario::por_regra(&hoje, 4))
        .filter(|e| e.peso >= peso_minimo)
    {
        let estrelas = "★".repeat(e.peso as usize);
        let marca = match e.origem.marca() {
            "" => String::new(),
            m => format!(" ({m})"),
        };
        empurrar(
            e.data,
            format!("{estrelas} {} · {}{marca}", e.pais, e.titulo),
            false,
        );
    }
    for (mes, dia, nome) in FERIADOS_B3 {
        empurrar(
            Data {
                ano: hoje.ano,
                mes: *mes,
                dia: *dia,
            },
            format!("B3 fechada · {nome}"),
            false,
        );
    }
    // Os proventos agendados — o que de fato toca a carteira.
    for p in ctx
        .portfolio
        .proventos
        .iter()
        .filter(|p| p.pago_em > ctx.agora)
    {
        empurrar(
            Data::de_epoch(p.pago_em, tempo::BRT_OFFSET),
            format!(
                "{} · {} de R$ {}",
                p.ativo.short(),
                match p.tipo {
                    TipoProvento::Jcp => "JCP",
                    TipoProvento::Rendimento => "rendimento",
                    TipoProvento::Amortizacao => "amortização",
                    TipoProvento::Dividendo => "dividendo",
                },
                crate::invest::calc::moeda(p.liquido())
            ),
            true,
        );
    }
    eventos.sort_by_key(|e| e.dia);
    eventos
}

struct Vista {
    lista: Lista,
    so_meus: bool,
    /// O piso de relevância. «2 estrelinhas» é o corte usual de um calendário econômico:
    /// abaixo dele está o ruído que faz o que importa sumir.
    peso_minimo: u8,
}

/// Um evento na agenda: quando, o que é, e se toca a carteira.
pub struct Evento {
    pub dia: i64,
    pub data: Data,
    pub texto: String,
    /// Se toca a carteira — um provento de um ativo que se tem. É o que ganha a marca na
    /// tela, e o que o filtro «só os meus» usa.
    pub meu: bool,
}

impl ModuleView for Vista {
    fn layout(&self, ctx: &Ctx) -> Layout {
        let hoje = Data::de_epoch(ctx.agora, tempo::BRT_OFFSET);
        // A tela cheia **pede** o calendário, e é este `get` que dispara a busca. O cartão
        // da home lê o mesmo cache com `ja_tem`, que não dispara nada.
        let (de, ate) = crate::invest::calendario::janela(&hoje, 90);
        let publicados = ctx.calendario.get(&de, &ate);
        let hoje_dias = tempo::dias_de(hoje.ano, hoje.mes, hoje.dia);
        let eventos = eventos(ctx, &publicados, self.peso_minimo, 90);
        let rows: Vec<Row> = eventos
            .iter()
            .filter(|e| !self.so_meus || e.meu)
            .map(|e| {
                let faltam = e.dia - hoje_dias;
                Row::new(vec![
                    match e.meu {
                        true => "●".to_string(),
                        false => " ".to_string(),
                    },
                    e.data.longa(),
                    match faltam {
                        0 => "hoje".to_string(),
                        1 => "amanhã".to_string(),
                        n => format!("em {n} dias"),
                    },
                    e.texto.clone(),
                ])
                .with_cell_tones(vec![
                    Tone::Bom,
                    Tone::Dim,
                    match faltam <= 3 {
                        true => Tone::Aviso,
                        false => Tone::Dim,
                    },
                    match e.meu {
                        true => Tone::Normal,
                        false => Tone::Dim,
                    },
                ])
            })
            .collect();

        // A tabela tem ano de validade, e quando ele passa a tela **avisa** — em vez de
        // mostrar datas de um ano que não é este e ficar em silêncio.
        let nota = match hoje.ano > VALIDADE {
            true => Some(format!(
                "as datas de COPOM e feriado são de {VALIDADE} e não foram atualizadas para {} — confira antes de contar com elas",
                hoje.ano
            )),
            false => {
                let mut partes = vec!["● toca a sua carteira".to_string()];
                partes.push(format!(
                    "★ mínimo {} (Ctrl+R muda)",
                    "★".repeat(self.peso_minimo as usize)
                ));
                // O que é publicado e o que é regra fica dito: uma data calculada não pode
                // parecer uma data anunciada.
                match (ctx.calendario.buscando(), ctx.calendario.erro()) {
                    (_, Some(e)) => partes.push(e),
                    (true, _) => partes.push("buscando o calendário do IBGE…".into()),
                    _ => partes.push(
                        "Brasil pelo IBGE e pelo COPOM; exterior só o que tem regra fixa".into(),
                    ),
                }
                Some(partes.join(" · "))
            }
        };

        Layout::one(Pane::Table {
            title: format!(
                "Agenda · próximos 90 dias · {}",
                calc::plural(rows.len(), "evento", "eventos")
            ),
            headers: vec![
                String::new(),
                "Data".into(),
                "Quando".into(),
                "O quê".into(),
            ],
            rows,
            selected: Some(self.lista.selecionado),
            query: self.lista.busca.clone(),
            note: nota.map(|n| (n, Tone::Aviso)),
        })
    }

    fn key(&mut self, key: KeyEvent, _ctx: &Ctx) -> Outcome {
        match key.code {
            KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.so_meus = !self.so_meus;
                self.lista.selecionado = 0;
                Outcome::Ok
            }
            // O corte de relevância, entre 1 e 3 estrelas. `Ctrl+R` de relevância, e
            // não o `Ctrl+E` que era: esse virou a tecla de marcar, em toda tela do
            // programa.
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.peso_minimo = match self.peso_minimo {
                    3 => 1,
                    n => n + 1,
                };
                self.lista.selecionado = 0;
                Outcome::Ok
            }
            _ => match self.lista.tecla(key, 100) {
                true => Outcome::Ok,
                false => Outcome::Ignorada,
            },
        }
    }

    fn escape(&mut self) -> Escape {
        self.lista.escape()
    }

    fn hint(&self) -> String {
        hint(&[
            "↑/↓ andar",
            "Ctrl+F só a minha carteira",
            "Ctrl+R relevância",
            "Esc sair",
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feriado_da_b3_e_reconhecido() {
        assert_eq!(
            feriado_b3(&Data {
                ano: 2026,
                mes: 9,
                dia: 7
            }),
            Some("Independência")
        );
        assert_eq!(
            feriado_b3(&Data {
                ano: 2026,
                mes: 9,
                dia: 8
            }),
            None
        );
    }

    #[test]
    fn as_tabelas_nao_tem_data_repetida() {
        for (m, d, _) in FERIADOS_B3 {
            let n = FERIADOS_B3
                .iter()
                .filter(|(a, b, _)| a == m && b == d)
                .count();
            assert_eq!(n, 1, "feriado {d:02}/{m:02} repetido");
        }
    }

    #[test]
    fn as_reunioes_do_copom_estao_em_ordem() {
        let mut anterior = (0, 0);
        for (m, d) in COPOM {
            assert!((*m, *d) > anterior, "COPOM fora de ordem em {d:02}/{m:02}");
            anterior = (*m, *d);
        }
    }
}
