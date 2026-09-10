//! Alocação: como o dinheiro está distribuído, e o quanto isso difere do que se queria.
//!
//! O peso por ativo já está em Posições. O que não está é o peso por **dimensão** — e é
//! aí que mora a surpresa: uma carteira com quinze ações e três FIIs pode ser 70% de um
//! setor só, e nenhuma linha de posição mostra isso.

use std::collections::BTreeMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::invest::calc;
use crate::invest::carteira::{self, Linha};
use crate::invest::model::AssetId;
use crate::invest::model::Classe;
use crate::invest::module::{
    Bar, Ctx, Edit, Escape, Field, Group, InvestModule, Layout, Marcavel, ModuleView, Need,
    Outcome, Pane, Tone,
};
use crate::invest::modules::comum::{Formulario, Lista, hint};

pub struct Alocacao;

/// Por que dimensão se está olhando.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Dimensao {
    Classe,
    Moeda,
    Corretora,
    Setor,
    Pais,
}

impl Dimensao {
    pub const ALL: [Dimensao; 5] = [
        Dimensao::Classe,
        Dimensao::Moeda,
        Dimensao::Corretora,
        Dimensao::Setor,
        Dimensao::Pais,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            Dimensao::Classe => "classe",
            Dimensao::Moeda => "moeda",
            Dimensao::Corretora => "corretora",
            Dimensao::Setor => "setor",
            Dimensao::Pais => "país",
        }
    }

    pub fn chave(&self, l: &Linha, ctx: &Ctx) -> String {
        match self {
            Dimensao::Classe => l.posicao.classe.label().to_string(),
            Dimensao::Moeda => l.posicao.moeda.code().to_string(),
            Dimensao::Corretora => l.posicao.fonte.clone(),
            // Informado, pelo mesmo raciocínio do preço médio: um setor errado vindo de
            // raspagem é pior que um setor em branco, porque leva a uma conclusão sobre
            // concentração.
            Dimensao::Setor => ctx
                .portfolio
                .setor(&l.posicao.ativo)
                .unwrap_or("sem setor")
                .to_string(),
            Dimensao::Pais => match l.posicao.ativo.market {
                crate::invest::model::Market::B3 | crate::invest::model::Market::Bcb => "Brasil",
                crate::invest::model::Market::Us => "Estados Unidos",
                crate::invest::model::Market::Binance => "global",
                _ => "outro",
            }
            .to_string(),
        }
    }

    /// Só a de classe tem alvo na v1 — é a única dimensão em que o arquivo guarda um.
    fn tem_alvo(&self) -> bool {
        matches!(self, Dimensao::Classe)
    }
}

impl InvestModule for Alocacao {
    fn id(&self) -> &'static str {
        "alocacao"
    }
    fn name(&self) -> &'static str {
        "Alocação"
    }
    fn description(&self) -> &'static str {
        "Por classe, moeda, corretora, setor e país — alvo contra real"
    }
    fn destaque(&self) -> u8 {
        3
    }
    fn group(&self) -> Group {
        Group::Carteira
    }

    /// Divide a lista de marcas dos ativos: as barras são classes, e classe é um dos tipos de marca da lista de ativos.
    fn marcavel(&self) -> Option<Marcavel> {
        Some(crate::invest::marcas::ATIVOS)
    }
    fn needs(&self) -> &'static [Need] {
        // Consolida em BRL: sem câmbio, um ativo estrangeiro fica fora da distribuição.
        &[Need::Posicoes, Need::Cambio]
    }
    fn keywords(&self) -> &'static str {
        "distribuição diversificação alvo desvio concentração peso"
    }

    fn summary(&self, ctx: &Ctx) -> String {
        if ctx.portfolio.posicoes.is_empty() {
            return "sem posições".into();
        }
        let linhas = carteira::linhas(ctx.portfolio, ctx.market, ctx.agora);
        let fatias = fatias(&linhas, Dimensao::Classe, ctx);
        match desvio_total(&fatias, &ctx.portfolio.alvos) {
            Some(d) => format!(
                "{} classes · desvio {}",
                fatias.len(),
                calc::pp(d).replace('+', "")
            ),
            None => format!("{} classes · sem alvo definido", fatias.len()),
        }
    }

    fn widget(&self, ctx: &Ctx) -> Option<Pane> {
        if ctx.portfolio.posicoes.is_empty() {
            return None;
        }
        let linhas = carteira::linhas(ctx.portfolio, ctx.market, ctx.agora);
        Some(Pane::Bars {
            title: String::new(),
            rows: fatias(&linhas, Dimensao::Classe, ctx)
                .into_iter()
                .take(8)
                .map(|(rotulo, _, peso)| Bar {
                    // A barra do alvo aparece aqui também: uma alocação sem o alvo ao
                    // lado é um número que não diz se está certo ou errado.
                    target: ctx.portfolio.alvos.get(&rotulo).copied(),
                    label: rotulo,
                    value: peso,
                    text: calc::pct_simples(peso),
                    tone: Tone::Normal,
                })
                .collect(),
            full: Some(100.0),
        })
    }

    fn open(&self, _ctx: &Ctx, _alvo: Option<&AssetId>) -> Box<dyn ModuleView> {
        Box::new(Vista {
            dimensao: Dimensao::Classe,
            lista: Lista::default(),
            form: None,
        })
    }
}

/// As fatias de uma dimensão: rótulo, valor em BRL e peso percentual.
pub fn fatias(linhas: &[Linha], dim: Dimensao, ctx: &Ctx) -> Vec<(String, f64, f64)> {
    let agrupado = carteira::agrupar(linhas, |l| dim.chave(l, ctx));
    let total: f64 = agrupado.iter().map(|(_, v)| v).sum();
    agrupado
        .into_iter()
        .map(|(rotulo, v)| {
            let peso = match total > 0.0 {
                true => v / total * 100.0,
                false => 0.0,
            };
            (rotulo, v, peso)
        })
        .collect()
}

/// O alvo de uma fatia, quando há um.
fn alvo_de(rotulo: &str, alvos: &BTreeMap<String, f64>) -> Option<f64> {
    Classe::ALL
        .iter()
        .find(|c| c.label() == rotulo)
        .and_then(|c| alvos.get(c.code()).copied())
}

/// A soma dos desvios em módulo, dividida por dois — que é o quanto do patrimônio teria
/// de mudar de lugar para a carteira ficar no alvo. `None` sem alvo nenhum.
pub fn desvio_total(fatias: &[(String, f64, f64)], alvos: &BTreeMap<String, f64>) -> Option<f64> {
    if alvos.is_empty() {
        return None;
    }
    let soma: f64 = fatias
        .iter()
        .map(|(rotulo, _, peso)| (peso - alvo_de(rotulo, alvos).unwrap_or(0.0)).abs())
        .sum();
    Some(soma / 2.0)
}

struct Vista {
    dimensao: Dimensao,
    lista: Lista,
    form: Option<Formulario>,
}

impl ModuleView for Vista {
    fn title(&self) -> String {
        format!("Alocação · por {}", self.dimensao.label())
    }

    fn quer_mercado(&self) -> bool {
        true
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
        if ctx.portfolio.posicoes.is_empty() {
            return Layout::one(Pane::Empty {
                title: "Alocação".into(),
                note: "Sem posições, não há o que distribuir.\n\nComece por Posições.".into(),
            });
        }

        let linhas = carteira::linhas(ctx.portfolio, ctx.market, ctx.agora);
        let totais = carteira::totais(&linhas);
        let fatias = fatias(&linhas, self.dimensao, ctx);
        let com_alvo = self.dimensao.tem_alvo() && !ctx.portfolio.alvos.is_empty();

        let barras: Vec<Bar> = fatias
            .iter()
            .map(|(rotulo, valor, peso)| {
                let alvo = com_alvo
                    .then(|| alvo_de(rotulo, &ctx.portfolio.alvos))
                    .flatten();
                let desvio = alvo.map(|a| peso - a);
                Bar {
                    label: rotulo.clone(),
                    value: *peso,
                    target: alvo,
                    text: match desvio {
                        Some(d) => format!(
                            "{}  alvo {}  {}  R$ {}",
                            calc::pct_simples(*peso),
                            calc::pct_simples(alvo.unwrap_or(0.0)),
                            calc::pp(d),
                            calc::moeda(*valor)
                        ),
                        None => format!("{}  R$ {}", calc::pct_simples(*peso), calc::moeda(*valor)),
                    },
                    tone: match desvio {
                        Some(d) if d.abs() > 5.0 => Tone::Aviso,
                        _ => Tone::Normal,
                    },
                }
            })
            .collect();

        let mut fatos = Vec::new();
        match desvio_total(&fatias, &ctx.portfolio.alvos).filter(|_| com_alvo) {
            Some(d) => {
                fatos.push((
                    "desvio total".into(),
                    format!("{} do patrimônio fora do lugar", calc::pct_simples(d)),
                    match d > 5.0 {
                        true => Tone::Aviso,
                        false => Tone::Bom,
                    },
                ));
                // Alvos que não somam 100 **não são normalizados calado**: normalizar
                // mudaria os números que a pessoa escreveu, sem avisar.
                let soma: f64 = ctx.portfolio.alvos.values().sum();
                if (soma - 100.0).abs() > 0.01 {
                    fatos.push((
                        "seus alvos somam".into(),
                        format!(
                            "{} — não 100%. Nada foi ajustado por você",
                            calc::pct_simples(soma)
                        ),
                        Tone::Aviso,
                    ));
                }
            }
            None if self.dimensao.tem_alvo() => fatos.push((
                "sem alvo".into(),
                "Ctrl+A define quanto você quer em cada classe".into(),
                Tone::Dim,
            )),
            None => fatos.push((
                "sem alvo nesta dimensão".into(),
                "alvos existem por classe; aqui só a distribuição real".into(),
                Tone::Dim,
            )),
        }
        if !totais.ressalva().is_empty() {
            fatos.push(("atenção".into(), totais.ressalva(), Tone::Aviso));
        }

        Layout::rows(vec![
            (
                3,
                Layout::one(Pane::Bars {
                    title: format!("Alocação · por {}", self.dimensao.label()),
                    rows: barras,
                    // Sempre contra 100%, e não contra a maior fatia: uma barra que
                    // reescala sozinha faz 40% parecer «cheio» num dia e «metade» noutro.
                    full: Some(100.0),
                }),
            ),
            (
                1,
                Layout::one(Pane::Facts {
                    title: "Leitura".into(),
                    rows: fatos,
                }),
            ),
        ])
    }

    fn key(&mut self, key: KeyEvent, ctx: &Ctx) -> Outcome {
        if let Some(form) = &mut self.form {
            if form.tecla(key) {
                let mut alvos = BTreeMap::new();
                for classe in Classe::ALL {
                    let bruto = form.valor(classe.label());
                    if bruto.trim().is_empty() {
                        continue;
                    }
                    match calc::ler_numero(&bruto) {
                        Some(v) if v >= 0.0 => {
                            alvos.insert(classe.code().to_string(), v);
                        }
                        _ => {
                            form.erro =
                                Some(format!("«{}» em {} não é um número", bruto, classe.label()));
                            return Outcome::Ok;
                        }
                    }
                }
                self.form = None;
                return Outcome::Editar(vec![Edit::SetAlvos(alvos)]);
            }
            return Outcome::Ok;
        }

        match key.code {
            KeyCode::Left | KeyCode::Right => {
                let n = Dimensao::ALL.len() as i32;
                let atual = Dimensao::ALL
                    .iter()
                    .position(|d| *d == self.dimensao)
                    .unwrap_or(0) as i32;
                let passo = if key.code == KeyCode::Right { 1 } else { -1 };
                self.dimensao = Dimensao::ALL[(atual + passo).rem_euclid(n) as usize];
                self.lista.selecionado = 0;
                Outcome::Ok
            }
            // `Ctrl+A` de alvos — o `Ctrl+E` que era virou a tecla de marcar, em toda
            // tela do programa.
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.form = Some(Formulario::novo(
                    "Alvos de alocação (%)",
                    Classe::ALL
                        .iter()
                        .map(|c| {
                            Field::text(
                                c.label(),
                                ctx.portfolio
                                    .alvos
                                    .get(c.code())
                                    .map(|v| calc::pct_simples(*v).replace('%', ""))
                                    .unwrap_or_default(),
                                "Quanto você quer nesta classe. Vazio = sem alvo",
                            )
                        })
                        .collect(),
                ));
                Outcome::Ok
            }
            KeyCode::Char('r') => Outcome::Abrir {
                modulo: "rebalanceamento",
                alvo: None,
            },
            _ => match self.lista.tecla(key, Dimensao::ALL.len()) {
                true => Outcome::Ok,
                false => Outcome::Ignorada,
            },
        }
    }

    fn escape(&mut self) -> Escape {
        if self.form.take().is_some() {
            return Escape::Consumido;
        }
        self.lista.escape()
    }

    fn hint(&self) -> String {
        hint(&["←/→ dimensão", "Ctrl+A alvos", "r rebalancear", "Esc sair"])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desvio_total_e_o_quanto_teria_de_mudar_de_lugar() {
        let fatias = vec![
            ("ação".to_string(), 4400.0, 44.0),
            ("FII".to_string(), 1800.0, 18.0),
            ("cripto".to_string(), 1400.0, 14.0),
            ("renda fixa".to_string(), 2400.0, 24.0),
        ];
        let mut alvos = BTreeMap::new();
        alvos.insert("acao".into(), 40.0);
        alvos.insert("fii".into(), 20.0);
        alvos.insert("cripto".into(), 10.0);
        alvos.insert("renda_fixa".into(), 30.0);
        // |4| + |−2| + |4| + |−6| = 16, dividido por dois = 8 p.p.
        let d = desvio_total(&fatias, &alvos).unwrap();
        assert!((d - 8.0).abs() < 1e-9, "deu {d}");
    }

    #[test]
    fn sem_alvo_nao_ha_desvio_e_isso_nao_e_zero() {
        let fatias = vec![("ação".to_string(), 100.0, 100.0)];
        // `None` significa «não há alvo»; `Some(0.0)` significaria «está no alvo».
        assert_eq!(desvio_total(&fatias, &BTreeMap::new()), None);
    }

    #[test]
    fn carteira_no_alvo_tem_desvio_zero() {
        let fatias = vec![
            ("ação".to_string(), 100.0, 60.0),
            ("FII".to_string(), 100.0, 40.0),
        ];
        let mut alvos = BTreeMap::new();
        alvos.insert("acao".into(), 60.0);
        alvos.insert("fii".into(), 40.0);
        assert_eq!(desvio_total(&fatias, &alvos), Some(0.0));
    }

    #[test]
    fn dimensoes_ciclam_sem_buraco() {
        assert_eq!(Dimensao::ALL.len(), 5);
        for d in Dimensao::ALL {
            assert!(!d.label().is_empty());
        }
    }
}
