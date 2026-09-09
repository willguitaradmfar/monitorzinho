//! Comparador: qual desses foi melhor, e por quanto.
//!
//! Normalizar a 100 é o módulo inteiro. Duas séries em escalas diferentes — uma ação a
//! R$ 38 e um índice a 142 mil — não se comparam no mesmo eixo: a de escala maior domina
//! o desenho e a outra vira uma linha reta no rodapé. Normalizadas, elas passam a mostrar
//! **retorno relativo**, que é o que se veio comparar.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::invest::calc;
use crate::invest::historico::{Estado, fechamentos};
use crate::invest::model::{AssetId, Market};
use crate::invest::module::{
    Ctx, Escape, Field, Group, InvestModule, Layout, ModuleView, Need, Outcome, Pane, Tone,
};
use crate::invest::modules::comum::{Formulario, hint};
use crate::invest::provider::Span;

pub struct Comparador;

/// Acima de seis séries o gráfico em blocos fica ilegível e a legenda não cabe. A sétima
/// é recusada **com essa explicação**, e não ignorada em silêncio.
const MAXIMO: usize = 6;

impl InvestModule for Comparador {
    fn id(&self) -> &'static str {
        "comparador"
    }
    fn name(&self) -> &'static str {
        "Comparador"
    }
    fn description(&self) -> &'static str {
        "N ativos normalizados a 100 na mesma escala, com a linha do CDI"
    }
    fn destaque(&self) -> u8 {
        1
    }
    fn group(&self) -> Group {
        Group::Analise
    }
    fn needs(&self) -> &'static [Need] {
        &[Need::Historico]
    }
    fn keywords(&self) -> &'static str {
        "comparar benchmark rendeu mais base 100 relativo cdi"
    }

    fn summary(&self, _ctx: &Ctx) -> String {
        "abra para comparar retorno relativo".into()
    }

    fn widget(&self, ctx: &Ctx) -> Option<Pane> {
        use crate::invest::historico::{Estado, fechamentos};
        let ativos = crate::invest::modules::risco::com_serie(ctx);
        if ativos.is_empty() {
            return None;
        }
        // Quem ganhou e quem perdeu no ano, lado a lado — que é o que o módulo faz em
        // tela cheia, com a linha do CDI junto.
        let mut linhas: Vec<(String, f64)> = ativos
            .iter()
            .filter_map(|a| match ctx.historico.get(ctx.providers, a, Span::Ano) {
                Estado::Pronta(velas) => {
                    let s = fechamentos(&velas);
                    let (primeiro, ultimo) = (*s.first()?, *s.last()?);
                    (primeiro > 0.0)
                        .then(|| (a.short().to_string(), (ultimo / primeiro - 1.0) * 100.0))
                }
                _ => None,
            })
            .collect();
        if linhas.is_empty() {
            return None;
        }
        linhas.sort_by(|a, b| b.1.total_cmp(&a.1));
        Some(Pane::Facts {
            title: String::new(),
            rows: linhas
                .into_iter()
                .take(6)
                .map(|(nome, pct)| {
                    (
                        nome,
                        format!("{} em 1 ano", calc::pct(pct)),
                        crate::invest::modules::heatmap::tom(Some(pct)),
                    )
                })
                .collect(),
        })
    }

    fn open(&self, ctx: &Ctx, alvo: Option<&AssetId>) -> Box<dyn ModuleView> {
        // O alvo entra primeiro e é o que dá o título ao gráfico: quem veio comparar
        // **a partir de** um ativo espera vê-lo na frente.
        let mut iniciais: Vec<AssetId> = alvo.cloned().into_iter().collect();
        for a in ctx
            .portfolio
            .posicoes
            .iter()
            .map(|p| p.ativo.clone())
            .chain(ctx.portfolio.watchlist.iter().cloned())
            .filter(|a| ctx.providers.tem_historico(a))
        {
            if iniciais.len() >= 3 {
                break;
            }
            if !iniciais.contains(&a) {
                iniciais.push(a);
            }
        }
        Box::new(Vista {
            ativos: iniciais,
            span: 4,
            com_cdi: true,
            form: None,
            erro: None,
        })
    }
}

struct Vista {
    ativos: Vec<AssetId>,
    span: usize,
    com_cdi: bool,
    form: Option<Formulario>,
    erro: Option<String>,
}

impl Vista {
    fn span(&self) -> Span {
        Span::ALL[self.span.min(Span::ALL.len() - 1)]
    }
}

impl ModuleView for Vista {
    fn title(&self) -> String {
        // Nomeia o primeiro da lista, que é o ativo com que se entrou: um título que só
        // diz «Comparador» não deixa saber a partir de quê se está comparando.
        match self.ativos.first() {
            Some(a) => format!("Comparador · {} · {}", a.short(), self.span().label()),
            None => format!("Comparador · {}", self.span().label()),
        }
    }

    fn quer_mercado(&self) -> bool {
        true
    }

    fn ativos(&self) -> Vec<AssetId> {
        match self.com_cdi {
            true => vec![AssetId::new(Market::Bcb, "CDI")],
            false => Vec::new(),
        }
    }

    fn layout(&self, ctx: &Ctx) -> Layout {
        if let Some(form) = &self.form {
            return Layout::one(Pane::Form {
                title: form.titulo.clone(),
                fields: form.campos.clone(),
                selected: form.selecionado,
                error: form.erro.clone(),
                hint: hint(&["Enter adicionar", "Esc cancelar"]),
            });
        }
        if self.ativos.is_empty() {
            return Layout::one(Pane::Empty {
                title: "Comparador".into(),
                note: "Nada para comparar ainda.\n\nCtrl+A adiciona um ativo com série \
                       (Binance ou câmbio)."
                    .into(),
            });
        }

        let mut series: Vec<(String, Vec<f64>)> = Vec::new();
        // As datas do eixo saem da primeira série que chega: todas cobrem a mesma janela,
        // e é a primeira que o gráfico desenha.
        let mut rotulos: Vec<String> = Vec::new();
        for a in &self.ativos {
            if let Estado::Pronta(c) = ctx.historico.get(ctx.providers, a, self.span()) {
                if rotulos.is_empty() {
                    rotulos = crate::invest::historico::rotulos(&c, self.span());
                }
                series.push((a.short().to_string(), calc::base_100(&fechamentos(&c))));
            }
        }
        // O CDI acumulado é a referência que responde «isso rendeu mais que deixar
        // parado?» — a pergunta que importa no Brasil, e ela vem de graça.
        if self.com_cdi
            && let Estado::Pronta(c) = ctx.historico.get(
                ctx.providers,
                &AssetId::new(Market::Bcb, "CDI"),
                self.span(),
            )
        {
            let taxas: Vec<f64> = c.iter().map(|p| p.fechamento).collect();
            let mut acumulado = vec![100.0];
            for t in &taxas {
                let ultimo = *acumulado.last().unwrap_or(&100.0);
                acumulado.push(ultimo * (1.0 + t / 100.0));
            }
            series.push(("CDI".into(), acumulado));
        }

        if series.is_empty() {
            return Layout::one(Pane::Empty {
                title: "Comparador".into(),
                note: format!("buscando {} série(s)…", ctx.historico.pendentes().max(1)),
            });
        }

        // Uma janela só, a interseção: comparar retornos sobre janelas diferentes é
        // comparar coisas diferentes.
        let n = series.iter().map(|(_, s)| s.len()).min().unwrap_or(0);
        let mut fatos: Vec<(String, String, Tone)> = series
            .iter()
            .map(|(nome, s)| {
                let janela = &s[s.len() - n..];
                let retorno = match janela.first() {
                    Some(p) if *p > 0.0 => (janela.last().unwrap_or(p) / p - 1.0) * 100.0,
                    _ => 0.0,
                };
                (
                    nome.clone(),
                    calc::pct(retorno),
                    match retorno >= 0.0 {
                        true => Tone::Bom,
                        false => Tone::Ruim,
                    },
                )
            })
            .collect();
        fatos.push((
            "base".into(),
            format!("100 no início do período · {n} pontos na interseção"),
            Tone::Dim,
        ));
        for a in &self.ativos {
            if matches!(ctx.historico.peek(a, self.span()), Some(Estado::SemFonte)) {
                fatos.push((
                    a.short().to_string(),
                    "sem série — fica na lista, mas não desenha".into(),
                    Tone::Aviso,
                ));
            }
        }
        if let Some(e) = &self.erro {
            fatos.push(("recusado".into(), e.clone(), Tone::Aviso));
        }

        // O `Pane::Chart` desenha uma série. Com várias, a primeira é o desenho e as
        // outras são a legenda numerada — é o que o vocabulário atual permite sem
        // inventar um visual novo para este módulo.
        let principal = series.first().cloned().unwrap_or_default();
        Layout::rows(vec![
            (
                3,
                Layout::one(Pane::Chart {
                    title: format!("{} · base 100 · {}", principal.0, self.span().label()),
                    series: principal.1,
                    format: |v| format!("{v:.1}").replace('.', ","),
                    marks: vec![(100.0, "base".into())],
                    x_labels: rotulos,
                    note: None,
                }),
            ),
            (
                2,
                Layout::one(Pane::Facts {
                    title: "Retorno no período".into(),
                    rows: fatos,
                }),
            ),
        ])
    }

    fn key(&mut self, key: KeyEvent, _ctx: &Ctx) -> Outcome {
        if let Some(form) = &mut self.form {
            if form.tecla(key) {
                match AssetId::parse(&form.valor("ativo")) {
                    Ok(a) if self.ativos.len() >= MAXIMO => {
                        let _ = a;
                        form.erro = Some(format!(
                            "já há {MAXIMO} séries — acima disso o gráfico fica ilegível e a legenda não cabe"
                        ));
                    }
                    Ok(a) => {
                        if !self.ativos.contains(&a) {
                            self.ativos.push(a);
                        }
                        self.form = None;
                    }
                    Err(e) => form.erro = Some(e),
                }
            }
            return Outcome::Ok;
        }

        match key.code {
            KeyCode::Left => {
                self.span = (self.span + Span::ALL.len() - 1) % Span::ALL.len();
                Outcome::Ok
            }
            KeyCode::Right => {
                self.span = (self.span + 1) % Span::ALL.len();
                Outcome::Ok
            }
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.erro = None;
                self.form = Some(Formulario::novo(
                    "Comparar com",
                    vec![Field::text(
                        "ativo",
                        "BINANCE/",
                        "BINANCE/BTCBRL, FX/USDBRL",
                    )],
                ));
                Outcome::Ok
            }
            KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.com_cdi = !self.com_cdi;
                Outcome::Ok
            }
            KeyCode::Delete => {
                self.ativos.pop();
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
        hint(&[
            "←/→ período",
            "Ctrl+A adicionar",
            "Ctrl+B CDI",
            "Del tirar",
            "Esc sair",
        ])
    }
}
