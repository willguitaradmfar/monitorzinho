//! Heatmap: o mercado num olhar.
//!
//! Funciona bem num terminal porque um heatmap **é** uma grade de retângulos coloridos com
//! um rótulo dentro, e uma tela de terminal é exatamente isso. Não há nada a aproximar
//! aqui — diferente do candle, que é uma forma que o terminal não tem.
//!
//! A escala de cor é **fixa e escrita na legenda**, e não relativa ao dia. Escala relativa
//! é o erro comum: num dia calmo ela pinta de vermelho forte uma queda de 0,3%, e num
//! crash pinta de amarelo uma queda de 6%. A cor tem que significar a mesma coisa todo
//! dia, senão não ensina nada.
//!
//! **A cor e o rótulo são sempre a variação do dia; o que muda é o tamanho.** É a regra
//! que faz o mapa continuar legível quando se troca de modo: quem aprendeu a ler as cores
//! não reaprende nada, e a pergunta que muda é só «tamanho de quê». Ver [`Tamanho`].

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::invest::carteira::{self, Linha};
use crate::invest::model::AssetId;
use crate::invest::module::{
    Cell, Ctx, Escape, Group, InvestModule, Layout, Marcavel, ModuleView, Need, Outcome, Pane, Tone,
};
use crate::invest::modules::comum::hint;
use crate::invest::provider::Span;
use crate::invest::tempo::{self, Data};
use crate::invest::{calc, carteiras, historico};

pub struct Heatmap;

/// Os degraus da escala, em porcento. Fixos, e desenhados na legenda.
const DEGRAUS: [f64; 4] = [0.5, 1.0, 2.0, 5.0];

/// O que o **tamanho** da caixa mede. A cor e o rótulo não mudam com isto.
///
/// São quatro perguntas diferentes sobre a mesma carteira, e cada uma quer uma área
/// diferente na tela: «onde está o meu dinheiro», «de onde veio o meu lucro», «o que
/// andou nesta semana» e «o que falta comprar». Uma tela por pergunta seriam quatro
/// telas; uma tecla que troca o que a área significa é uma.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tamanho {
    /// O valor de mercado da posição, em reais. O padrão: é o mapa da carteira.
    Posicao,
    /// O lucro acumulado contra o preço médio informado.
    LucroTotal,
    /// Quanto a posição andou desde o primeiro dia deste mês.
    LucroMes,
    /// O mesmo, desde a segunda-feira desta semana.
    LucroSemana,
    /// Quanto falta comprar ou vender para chegar na carteira recomendada.
    Ajuste,
}

impl Tamanho {
    /// A ordem em que a tecla anda: primeiro onde está o dinheiro, depois o que ele
    /// rendeu — do total para o recente —, e por último o que fazer a respeito.
    const ALL: [Tamanho; 5] = [
        Tamanho::Posicao,
        Tamanho::LucroTotal,
        Tamanho::LucroMes,
        Tamanho::LucroSemana,
        Tamanho::Ajuste,
    ];

    fn proximo(self) -> Tamanho {
        let i = Self::ALL.iter().position(|t| *t == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }

    fn label(self) -> &'static str {
        match self {
            Tamanho::Posicao => "posição em R$",
            Tamanho::LucroTotal => "lucro total (contra o PM)",
            Tamanho::LucroMes => "lucro do mês",
            Tamanho::LucroSemana => "lucro da semana",
            Tamanho::Ajuste => "ajuste à carteira alvo",
        }
    }

    /// Se o modo precisa de série histórica — e portanto pode passar por «buscando».
    fn quer_serie(self) -> bool {
        matches!(self, Tamanho::LucroMes | Tamanho::LucroSemana)
    }
}

/// A cor de uma variação. Preço informado ou não-oficial fica cinza — mesma regra de
/// Cotações: colorir daria a ele a autoridade visual de um preço ao vivo.
pub fn tom(variacao: Option<f64>) -> Tone {
    match variacao {
        None => Tone::Dim,
        Some(v) if v >= DEGRAUS[0] => Tone::Bom,
        Some(v) if v <= -DEGRAUS[0] => Tone::Ruim,
        Some(_) => Tone::Normal,
    }
}

/// Uma caixa do mapa, já resolvida: quem é, como está hoje, e quanto ela mede no modo
/// que está ligado.
struct Caixa {
    ativo: AssetId,
    variacao: Option<f64>,
    /// A grandeza do modo, em BRL e **com sinal** — o sinal vai no rótulo de detalhe, e
    /// o tamanho usa o módulo dela.
    valor: f64,
}

/// O primeiro instante do mês corrente, no fuso de Brasília.
fn inicio_do_mes(agora: u64) -> u64 {
    let hoje = Data::de_epoch(agora, tempo::BRT_OFFSET);
    Data { dia: 1, ..hoje }.epoch_inicio(tempo::BRT_OFFSET)
}

/// O primeiro instante da segunda-feira desta semana, no fuso de Brasília.
///
/// Segunda e não domingo: a semana que interessa aqui é a de pregão, e ela começa quando
/// a bolsa abre.
fn inicio_da_semana(agora: u64) -> u64 {
    let hoje = Data::de_epoch(agora, tempo::BRT_OFFSET);
    // `dia_da_semana` dá 0 no domingo; a distância até a segunda anterior é (dow+6)%7.
    let recuo = (hoje.dia_da_semana() + 6) % 7;
    let dias = tempo::dias_de(hoje.ano, hoje.mes, hoje.dia) - recuo as i64;
    tempo::data_de_dias(dias).epoch_inicio(tempo::BRT_OFFSET)
}

/// Quanto a posição andou desde `desde`, em BRL.
///
/// A referência é o **último fechamento antes** do começo da janela, e não o primeiro
/// ponto dentro dela: o lucro do mês é medido contra onde o papel fechou o mês passado,
/// não contra onde ele abriu o dia 1.
fn lucro_desde(l: &Linha, ctx: &Ctx, desde: u64) -> Option<f64> {
    let preco = l.preco?;
    // Uma janela só de série mensal serve as duas: trinta dias cobrem o mês corrente e
    // sobra para a semana, e é **uma** busca por ativo em vez de duas.
    let historico::Estado::Pronta(velas) =
        ctx.historico
            .get(ctx.providers, &l.posicao.ativo, Span::Mes)
    else {
        return None;
    };
    let referencia = velas.iter().rfind(|v| v.em < desde)?.fechamento;
    ctx.market
        .para_brl((preco - referencia) * l.posicao.quantidade, l.posicao.moeda)
}

/// O que ainda falta comprar ou vender, por ativo, somando todas as carteiras
/// recomendadas. Um ativo participa de uma só — ver `Position::carteira` —, então não há
/// o que somar duas vezes.
fn ajustes(ctx: &Ctx) -> Vec<(AssetId, f64)> {
    ctx.portfolio
        .carteiras
        .iter()
        .flat_map(|c| {
            carteiras::plano(c, ctx.portfolio, ctx.market, ctx.agora)
                .ajustes
                .into_iter()
                .map(|a| (a.ativo, a.delta))
        })
        .filter(|(_, delta)| delta.abs() > 0.005)
        .collect()
}

/// As caixas do mapa no modo pedido, da maior para a menor.
///
/// Devolve também **quantas ficaram de fora por falta de número** — um ativo sem preço
/// médio não tem lucro total, um sem série não tem lucro do mês, e um fora de toda
/// carteira recomendada não tem ajuste. Eles não entram com zero: uma caixa de área zero
/// é uma caixa invisível, e o mapa tem que dizer que ela existe em vez de sumir com ela.
fn caixas(ctx: &Ctx, modo: Tamanho) -> (Vec<Caixa>, usize) {
    let (mut caixas, cabiveis): (Vec<Caixa>, usize) = match modo {
        Tamanho::Ajuste => {
            // Aqui o conjunto **não** é o das posições: um papel recomendado que ainda
            // não se tem é justamente o que tem o maior ajuste, e deixá-lo de fora
            // esconderia a maior coisa a fazer.
            let a = ajustes(ctx);
            let n = a.len();
            (
                a.into_iter()
                    .map(|(ativo, delta)| Caixa {
                        variacao: ctx.market.quote(&ativo).and_then(|q| q.variacao()),
                        ativo,
                        valor: delta,
                    })
                    .collect(),
                n,
            )
        }
        _ => {
            let linhas = carteira::linhas(ctx.portfolio, ctx.market, ctx.agora);
            let n = linhas.len();
            (
                linhas
                    .iter()
                    .filter_map(|l| {
                        let valor = match modo {
                            Tamanho::Posicao => l.mercado_brl,
                            Tamanho::LucroTotal => l.pnl_brl,
                            Tamanho::LucroMes => lucro_desde(l, ctx, inicio_do_mes(ctx.agora)),
                            Tamanho::LucroSemana => {
                                lucro_desde(l, ctx, inicio_da_semana(ctx.agora))
                            }
                            Tamanho::Ajuste => None,
                        }?;
                        Some(Caixa {
                            ativo: l.posicao.ativo.clone(),
                            variacao: l.variacao_pct,
                            valor,
                        })
                    })
                    .collect(),
                n,
            )
        }
    };
    caixas.retain(|c| c.valor.abs() > 0.005);
    caixas.sort_by(|a, b| b.valor.abs().total_cmp(&a.valor.abs()));
    let de_fora = cabiveis.saturating_sub(caixas.len());
    (caixas, de_fora)
}

/// A célula de uma caixa. **A cor e a linha do meio são sempre a variação do dia**,
/// qualquer que seja o modo; o que o modo muda é a área e a terceira linha.
fn celula(c: &Caixa, ctx: &Ctx, modo: Tamanho) -> Cell {
    Cell {
        label: c.ativo.short().to_string(),
        sub: match c.variacao {
            Some(v) => calc::pct(v),
            None => ctx
                .market
                .quote(&c.ativo)
                .map(|q| match q.grade.marca().is_empty() {
                    true => "—".to_string(),
                    false => q.grade.marca().to_string(),
                })
                .unwrap_or("—".into()),
        },
        // Curto de propósito, e nunca cortado: ver `calc::moeda_curta`. A caixa mais
        // estreita do mapa tem oito colunas, e um `R$ 30.291,00` cortado nelas viraria
        // `R$ 30.29` — outro número, não um número truncado.
        detalhe: match modo {
            Tamanho::Ajuste => match c.valor >= 0.0 {
                true => format!("+{}", calc::moeda_curta(c.valor)),
                false => format!("−{}", calc::moeda_curta(-c.valor)),
            },
            _ => format!("R$ {}", calc::moeda_curta(c.valor)),
        },
        weight: c.valor.abs(),
        tone: tom(c.variacao),
    }
}

impl InvestModule for Heatmap {
    fn id(&self) -> &'static str {
        "heatmap"
    }
    fn name(&self) -> &'static str {
        "Heatmap"
    }
    fn description(&self) -> &'static str {
        "A grade colorida por variação, com a área de cada papel pelo que se escolher medir"
    }
    fn destaque(&self) -> u8 {
        4
    }
    fn group(&self) -> Group {
        Group::Mercado
    }

    fn fonte_externa(&self) -> Option<&'static str> {
        Some(crate::invest::store::fonte::COTACAO)
    }

    /// Divide a lista de marcas dos ativos: cada célula é um papel — a marca acende a moldura dela.
    fn marcavel(&self) -> Option<Marcavel> {
        Some(crate::invest::marcas::ATIVOS)
    }
    fn needs(&self) -> &'static [Need] {
        &[Need::Cotacao]
    }
    fn keywords(&self) -> &'static str {
        "mapa calor grade cores visão geral setor treemap tamanho lucro ajuste"
    }

    fn summary(&self, ctx: &Ctx) -> String {
        let n = ctx.portfolio.posicoes.len();
        match n {
            0 => "sem posições".into(),
            _ => format!("{n} ativos num olhar"),
        }
    }

    /// O cartão é sempre o mapa da **posição**: é o modo que não depende de nada buscado
    /// além do preço, e a home não pode disparar busca de série.
    fn widget(&self, ctx: &Ctx) -> Option<Pane> {
        if ctx.portfolio.posicoes.is_empty() {
            return None;
        }
        let (caixas, _) = caixas(ctx, Tamanho::Posicao);
        if caixas.is_empty() {
            return None;
        }
        Some(Pane::Grid {
            title: String::new(),
            // Sem `take`: quem decide quantas cabem é o desenho, que conhece o tamanho da
            // caixa. Cortar aqui num número fixo escondia papéis que cabiam numa tela
            // larga e ainda assim espremia os que sobravam numa estreita.
            cells: caixas
                .iter()
                .map(|c| celula(c, ctx, Tamanho::Posicao))
                .collect(),
            selected: None,
            legend: String::new(),
        })
    }

    fn open(&self, _ctx: &Ctx, _alvo: Option<&AssetId>) -> Box<dyn ModuleView> {
        Box::new(Vista {
            selecionado: 0,
            modo: Tamanho::Posicao,
        })
    }
}

struct Vista {
    selecionado: usize,
    modo: Tamanho,
}

impl ModuleView for Vista {
    fn title(&self) -> String {
        "Heatmap".into()
    }

    fn quer_mercado(&self) -> bool {
        true
    }

    fn layout(&self, ctx: &Ctx) -> Layout {
        let (caixas, de_fora) = caixas(ctx, self.modo);
        if caixas.is_empty() {
            return Layout::one(Pane::Empty {
                title: "Heatmap".into(),
                note: match self.modo {
                    Tamanho::Posicao => "Sem posições não há mapa. Comece por Posições.".into(),
                    Tamanho::Ajuste => "Nenhum ajuste a fazer — ou nenhuma carteira \
                                        recomendada com ativos ligados a ela. Ctrl+W \
                                        volta para o mapa da posição."
                        .into(),
                    _ if self.modo.quer_serie() && ctx.historico.pendentes() > 0 => {
                        "buscando a série de cada papel…".into()
                    }
                    _ => "Nenhum papel com esse número — falta preço médio informado ou \
                          série. Ctrl+W troca o que o tamanho mede."
                        .into(),
                },
            });
        }

        let total: f64 = caixas.iter().map(|c| c.valor.abs()).sum();
        let mut ressalvas = vec!["Ctrl+W troca o que o tamanho mede".to_string()];
        if de_fora > 0 {
            ressalvas.push(format!("{de_fora} sem esse número"));
        }
        let pendentes = ctx.historico.pendentes();
        if self.modo.quer_serie() && pendentes > 0 {
            ressalvas.push(format!("buscando {pendentes}"));
        }

        let legenda = format!(
            "−{}% ▓  −{}% ▒  0  +{}% ▒  +{}% ▓   ·   cinza = preço não ao vivo   ·   {}",
            DEGRAUS[3],
            DEGRAUS[1],
            DEGRAUS[1],
            DEGRAUS[3],
            ressalvas.join("   ·   ")
        );

        Layout::one(Pane::Grid {
            // O modo vai no **título**, e não só na legenda: é o que muda com a tecla,
            // e o que muda tem que estar onde o olho já está — a legenda é para consultar
            // uma vez, o título é para saber onde se está.
            title: format!(
                "Heatmap · tamanho: {} · {} · R$ {}",
                self.modo.label(),
                calc::plural(caixas.len(), "ativo", "ativos"),
                calc::moeda(total)
            ),
            cells: caixas.iter().map(|c| celula(c, ctx, self.modo)).collect(),
            selected: Some(self.selecionado),
            legend: legenda,
        })
    }

    fn key(&mut self, key: KeyEvent, ctx: &Ctx) -> Outcome {
        let (caixas, _) = caixas(ctx, self.modo);
        let n = caixas.len();
        match key.code {
            KeyCode::Right | KeyCode::Down if n > 0 => {
                self.selecionado = (self.selecionado + 1) % n;
                Outcome::Ok
            }
            KeyCode::Left | KeyCode::Up if n > 0 => {
                self.selecionado = (self.selecionado + n - 1) % n;
                Outcome::Ok
            }
            // O cursor volta para a primeira caixa: a lista muda de conteúdo **e** de
            // ordem entre os modos, e manter o índice apontaria para outro papel.
            KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.modo = self.modo.proximo();
                self.selecionado = 0;
                Outcome::Ok
            }
            KeyCode::Enter => Outcome::Abrir {
                modulo: "grafico",
                alvo: caixas.get(self.selecionado).map(|c| c.ativo.clone()),
            },
            _ => Outcome::Ignorada,
        }
    }

    fn escape(&mut self) -> Escape {
        Escape::Nao
    }

    fn hint(&self) -> String {
        hint(&[
            "setas andar",
            "Ctrl+W o que o tamanho mede",
            "Enter gráfico",
            "Esc sair",
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_escala_e_fixa_e_nao_relativa_ao_dia() {
        // A mesma variação tem que dar a mesma cor num dia calmo e num de crash.
        assert_eq!(tom(Some(2.0)), Tone::Bom);
        assert_eq!(tom(Some(-2.0)), Tone::Ruim);
        assert_eq!(
            tom(Some(0.1)),
            Tone::Normal,
            "abaixo do primeiro degrau é neutro"
        );
    }

    #[test]
    fn preco_nao_ao_vivo_fica_cinza() {
        assert_eq!(tom(None), Tone::Dim);
    }

    #[test]
    fn a_tecla_do_tamanho_da_a_volta_e_passa_por_todos() {
        let mut visto = vec![Tamanho::Posicao];
        let mut t = Tamanho::Posicao;
        for _ in 0..Tamanho::ALL.len() {
            t = t.proximo();
            visto.push(t);
        }
        assert_eq!(t, Tamanho::Posicao, "a volta tem que fechar");
        for modo in Tamanho::ALL {
            assert!(visto.contains(&modo), "{modo:?} não aparece na volta");
        }
    }

    /// Só o lucro do mês e o da semana dependem de rede além do preço — é o que decide
    /// se a tela pode dizer «buscando».
    #[test]
    fn so_as_janelas_pedem_serie() {
        assert!(!Tamanho::Posicao.quer_serie());
        assert!(!Tamanho::LucroTotal.quer_serie());
        assert!(!Tamanho::Ajuste.quer_serie());
        assert!(Tamanho::LucroMes.quer_serie());
        assert!(Tamanho::LucroSemana.quer_serie());
    }

    #[test]
    fn a_semana_comeca_na_segunda_e_o_mes_no_dia_um() {
        // Uma quinta-feira qualquer: 10/09/2026.
        let quinta = Data {
            ano: 2026,
            mes: 9,
            dia: 10,
        }
        .epoch_inicio(tempo::BRT_OFFSET)
            + 15 * 3600;
        let semana = Data::de_epoch(inicio_da_semana(quinta), tempo::BRT_OFFSET);
        assert_eq!(semana.dia_da_semana(), 1, "tem que cair numa segunda");
        assert_eq!(semana.dia, 7);
        let mes = Data::de_epoch(inicio_do_mes(quinta), tempo::BRT_OFFSET);
        assert_eq!((mes.ano, mes.mes, mes.dia), (2026, 9, 1));
    }

    /// Um domingo é o fim da semana que começou na segunda anterior, e não o começo de
    /// uma nova — é o erro que `(dow+6)%7` existe para não cometer.
    #[test]
    fn no_domingo_a_semana_ainda_e_a_que_passou() {
        let domingo = Data {
            ano: 2026,
            mes: 9,
            dia: 13,
        }
        .epoch_inicio(tempo::BRT_OFFSET)
            + 10 * 3600;
        let semana = Data::de_epoch(inicio_da_semana(domingo), tempo::BRT_OFFSET);
        assert_eq!((semana.mes, semana.dia), (9, 7));
    }
}
