//! Metas: estou fazendo o que combinei comigo mesmo?
//!
//! Duas metas, e a distinção não é cosmética. A de **aporte** é inteiramente controlável,
//! e o progresso nela é um fato sobre disciplina. A de **patrimônio** depende de retorno,
//! que não se controla — e apresentar as duas do mesmo jeito ensina a pessoa a se sentir
//! mal por um mercado ruim ou bem por um mercado bom.
//!
//! Este módulo **não julga**. Nada de «você está atrasado», nada de vermelho por não bater
//! a meta. O tom de um programa que cobra é o tom que faz a pessoa parar de abri-lo.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::invest::calc;
use crate::invest::carteira;
use crate::invest::model::{AssetId, Meta, TipoLancamento, TipoMeta};
use crate::invest::module::{
    Bar, Ctx, Edit, Escape, Field, Group, InvestModule, Layout, ModuleView, Need, Outcome, Pane,
    Tone,
};
use crate::invest::modules::comum::{Formulario, Lista, hint};
use crate::invest::tempo::{self, Data};

pub struct Metas;

impl InvestModule for Metas {
    fn id(&self) -> &'static str {
        "metas"
    }
    fn name(&self) -> &'static str {
        "Metas"
    }
    fn description(&self) -> &'static str {
        "Aporte mensal e patrimônio — o plano, e o quanto dele foi cumprido"
    }
    fn destaque(&self) -> u8 {
        3
    }
    fn group(&self) -> Group {
        Group::Operacao
    }
    fn needs(&self) -> &'static [Need] {
        // Aporte só se mede onde ele foi registrado.
        &[Need::Posicoes, Need::Lancamentos]
    }
    fn keywords(&self) -> &'static str {
        "objetivo plano progresso disciplina independência"
    }

    fn summary(&self, ctx: &Ctx) -> String {
        match ctx.portfolio.metas.len() {
            0 => "nenhuma meta definida".into(),
            n => format!("{n} meta(s)"),
        }
    }

    fn widget(&self, ctx: &Ctx) -> Option<Pane> {
        if ctx.portfolio.metas.is_empty() {
            return None;
        }
        let linhas = carteira::linhas(ctx.portfolio, ctx.market, ctx.agora);
        let patrimonio = carteira::totais(&linhas).mercado;
        let mes = Data::de_epoch(ctx.agora, tempo::BRT_OFFSET).chave_mes();
        Some(Pane::Bars {
            title: String::new(),
            rows: ctx
                .portfolio
                .metas
                .iter()
                .take(6)
                .map(|meta| {
                    // Cada tipo de meta mede uma coisa diferente: a de aporte mede o que
                    // depende de você, a de patrimônio mede você mais o mercado.
                    let (rotulo, feito) = match meta.tipo {
                        TipoMeta::AporteMensal => {
                            ("aporte do mês".to_string(), aportado_no_mes(ctx, &mes))
                        }
                        TipoMeta::Patrimonio => ("patrimônio".to_string(), patrimonio),
                    };
                    let pct = match meta.valor > 0.0 {
                        true => (feito / meta.valor * 100.0).clamp(0.0, 100.0),
                        false => 0.0,
                    };
                    Bar {
                        label: rotulo,
                        value: pct,
                        target: Some(100.0),
                        text: format!(
                            "R$ {} de R$ {}  {}",
                            calc::moeda(feito),
                            calc::moeda(meta.valor),
                            calc::pct_simples(pct)
                        ),
                        tone: match pct >= 100.0 {
                            true => Tone::Bom,
                            false => Tone::Normal,
                        },
                    }
                })
                .collect(),
            full: Some(100.0),
        })
    }

    fn open(&self, _ctx: &Ctx, _alvo: Option<&AssetId>) -> Box<dyn ModuleView> {
        Box::new(Vista {
            lista: Lista::default(),
            form: None,
        })
    }
}

/// Quanto foi aportado num mês, dos lançamentos.
pub fn aportado_no_mes(ctx: &Ctx, mes: &str) -> f64 {
    ctx.portfolio
        .lancamentos
        .iter()
        .filter(|l| l.tipo == TipoLancamento::Aporte)
        .filter(|l| Data::de_epoch(l.em, tempo::BRT_OFFSET).chave_mes() == mes)
        .map(|l| l.valor)
        .sum()
}

struct Vista {
    lista: Lista,
    form: Option<Formulario>,
}

impl ModuleView for Vista {
    fn title(&self) -> String {
        "Metas".into()
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
                hint: hint(&["↑/↓ campo", "←/→ tipo", "Enter gravar", "Esc cancelar"]),
            });
        }
        if ctx.portfolio.metas.is_empty() {
            return Layout::one(Pane::Empty {
                title: "Metas".into(),
                note: "Nenhuma meta ainda.\n\n\
                       Ctrl+A cria uma. Há duas, e elas são diferentes:\n\
                       • Aporte mensal — depende só de você.\n\
                       • Patrimônio até uma data — depende de você e do mercado.\n\n\
                       A segunda sempre mostra quanto do progresso foi aporte e quanto \n\
                       foi mercado, para que um ano bom não vire mérito nem um ruim, culpa."
                    .into(),
            });
        }

        let linhas = carteira::linhas(ctx.portfolio, ctx.market, ctx.agora);
        let totais = carteira::totais(&linhas);
        let hoje = Data::de_epoch(ctx.agora, tempo::BRT_OFFSET);

        let mut barras = Vec::new();
        let mut fatos = Vec::new();

        for meta in &ctx.portfolio.metas {
            match meta.tipo {
                TipoMeta::AporteMensal => {
                    // Os últimos seis meses, do mais recente para trás.
                    let mut mes = hoje;
                    for _ in 0..6 {
                        let chave = mes.chave_mes();
                        let feito = aportado_no_mes(ctx, &chave);
                        let pct = match meta.valor > 0.0 {
                            true => feito / meta.valor * 100.0,
                            false => 0.0,
                        };
                        barras.push(Bar {
                            label: format!("{}/{}", tempo::nome_mes(mes.mes), mes.ano % 100),
                            value: pct.min(100.0),
                            target: None,
                            text: format!(
                                "R$ {} de R$ {}  {}",
                                calc::moeda(feito),
                                calc::moeda(meta.valor),
                                calc::pct_simples(pct)
                            ),
                            // Sem vermelho por não bater: o tom de um programa que cobra é
                            // o tom que faz a pessoa parar de abri-lo.
                            tone: match pct >= 100.0 {
                                true => Tone::Bom,
                                false => Tone::Normal,
                            },
                        });
                        mes = mes.mes_anterior();
                    }
                    if ctx.portfolio.lancamentos.is_empty() {
                        fatos.push((
                            "sem lançamentos".into(),
                            "não dá para medir aporte sem registrá-lo — veja Lançamentos".into(),
                            Tone::Aviso,
                        ));
                    }
                }
                TipoMeta::Patrimonio => {
                    let pct = match meta.valor > 0.0 {
                        true => totais.mercado / meta.valor * 100.0,
                        false => 0.0,
                    };
                    barras.push(Bar {
                        label: "patrimônio".into(),
                        value: pct.min(100.0),
                        target: None,
                        text: format!(
                            "R$ {} de R$ {}  {}",
                            calc::moeda(totais.mercado),
                            calc::moeda(meta.valor),
                            calc::pct_simples(pct)
                        ),
                        tone: Tone::Destaque,
                    });
                    // A única projeção do módulo, e ela usa o ritmo **observado** — não um
                    // retorno suposto. É o que a diferencia do Simulador.
                    let aporte_medio = {
                        let mut soma = 0.0;
                        let mut mes = hoje;
                        for _ in 0..6 {
                            soma += aportado_no_mes(ctx, &mes.chave_mes());
                            mes = mes.mes_anterior();
                        }
                        soma / 6.0
                    };
                    let falta = (meta.valor - totais.mercado).max(0.0);
                    fatos.push((
                        "no ritmo atual".into(),
                        match aporte_medio > 0.0 {
                            true => {
                                let meses = (falta / aporte_medio).ceil() as u32;
                                let mut alvo = hoje;
                                for _ in 0..meses.min(1200) {
                                    alvo = alvo.mes_seguinte();
                                }
                                format!(
                                    "{}/{} — {} meses no aporte médio de R$ {}",
                                    tempo::nome_mes(alvo.mes),
                                    alvo.ano,
                                    meses,
                                    calc::moeda(aporte_medio)
                                )
                            }
                            false => "sem aporte registrado, não há ritmo a projetar".into(),
                        },
                        Tone::Dim,
                    ));
                    fatos.push((
                        "falta".into(),
                        format!("R$ {}", calc::moeda(falta)),
                        Tone::Normal,
                    ));
                }
            }
        }

        Layout::rows(vec![
            (
                3,
                Layout::one(Pane::Bars {
                    title: "Progresso".into(),
                    rows: barras,
                    full: Some(100.0),
                }),
            ),
            (
                2,
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
                let Some(valor) = calc::ler_numero(&form.valor("valor")) else {
                    form.erro = Some("valor não é um número".into());
                    return Outcome::Ok;
                };
                if valor <= 0.0 {
                    form.erro = Some("a meta tem que ser maior que zero".into());
                    return Outcome::Ok;
                }
                let tipo = match form.valor("tipo").as_str() {
                    "patrimônio" => TipoMeta::Patrimonio,
                    _ => TipoMeta::AporteMensal,
                };
                let mut metas = ctx.portfolio.metas.clone();
                metas.retain(|m| m.tipo != tipo);
                metas.push(Meta {
                    tipo,
                    valor,
                    prazo: None,
                });
                self.form = None;
                return Outcome::Editar(vec![Edit::SetMetas(metas)]);
            }
            return Outcome::Ok;
        }

        match key.code {
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.form = Some(Formulario::novo(
                    "Nova meta",
                    vec![
                        Field::choice(
                            "tipo",
                            "aporte mensal",
                            vec!["aporte mensal".into(), "patrimônio".into()],
                            "aporte depende só de você; patrimônio depende também do mercado",
                        ),
                        Field::text("valor", "", "Quanto por mês, ou quanto no total"),
                    ],
                ));
                Outcome::Ok
            }
            KeyCode::Delete => {
                let mut metas = ctx.portfolio.metas.clone();
                if self.lista.selecionado < metas.len() {
                    metas.remove(self.lista.selecionado);
                    return Outcome::Editar(vec![Edit::SetMetas(metas)]);
                }
                Outcome::Ok
            }
            // Esta tela desenha barras e não uma tabela, então não há lista para filtrar —
            // e uma busca invisível que desloca o cursor é pior que busca nenhuma.
            KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown => {
                match self.lista.tecla(key, ctx.portfolio.metas.len()) {
                    true => Outcome::Ok,
                    false => Outcome::Ignorada,
                }
            }
            _ => Outcome::Ignorada,
        }
    }

    fn escape(&mut self) -> Escape {
        match self.form.take().is_some() {
            true => Escape::Consumido,
            false => self.lista.escape(),
        }
    }

    fn hint(&self) -> String {
        hint(&["Ctrl+A nova meta", "Del apagar", "Esc sair"])
    }
}
