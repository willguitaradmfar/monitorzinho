//! Importação: como a carteira entra sem digitação.
//!
//! Quatro regras, e as quatro vêm de `docs/invest/03`:
//!
//! 1. A chave é `(fonte, conta, ativo)` — importar a corretora X substitui as linhas dela
//!    e não toca em mais nada.
//! 2. **Idempotente.** O mesmo arquivo duas vezes dá o mesmo resultado que uma.
//! 3. O preço médio vem do arquivo e é gravado como informado. Nada é recalculado.
//! 4. **Nada é gravado antes da conferência.** É a regra que faz a diferença entre uma
//!    ferramenta em que se confia e uma que dá medo.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::invest::calc;
use crate::invest::csv::{Arquivo, Campo};
use crate::invest::model::{AssetId, Classe, Market, Moeda, Position};
use crate::invest::module::{
    Ctx, Edit, Escape, Field, Group, InvestModule, Layout, ModuleView, Outcome, Pane, Row, Tone,
};
use crate::invest::modules::comum::{Formulario, Lista, hint};

pub struct Importacao;

impl InvestModule for Importacao {
    fn id(&self) -> &'static str {
        "importacao"
    }
    fn name(&self) -> &'static str {
        "Importação"
    }
    fn description(&self) -> &'static str {
        "Lê o CSV da corretora, mostra o que vai mudar, e só grava depois de confirmado"
    }
    fn destaque(&self) -> u8 {
        1
    }
    fn group(&self) -> Group {
        Group::Operacao
    }
    fn keywords(&self) -> &'static str {
        "csv arquivo corretora b3 extrato planilha carregar"
    }

    fn summary(&self, ctx: &Ctx) -> String {
        match ctx.portfolio.fontes().len() {
            0 => "importe o CSV da sua corretora".into(),
            n => format!("{n} fonte(s) já importada(s)"),
        }
    }

    fn widget(&self, ctx: &Ctx) -> Option<Pane> {
        let fontes = ctx.portfolio.fontes();
        if fontes.is_empty() {
            return None;
        }
        // Quantas linhas vieram de cada corretora, e quantas ainda estão sem preço médio
        // — que é o que decide se falta importar a planilha de custo.
        Some(Pane::Facts {
            title: String::new(),
            rows: fontes
                .iter()
                .take(8)
                .map(|f| {
                    let linhas: Vec<_> = ctx
                        .portfolio
                        .posicoes
                        .iter()
                        .filter(|p| p.fonte == *f)
                        .collect();
                    let sem_pm = linhas.iter().filter(|p| p.preco_medio.is_none()).count();
                    (
                        f.clone(),
                        match sem_pm {
                            0 => calc::plural(linhas.len(), "linha", "linhas"),
                            n => format!(
                                "{} · {n} sem preço médio",
                                calc::plural(linhas.len(), "linha", "linhas")
                            ),
                        },
                        match sem_pm {
                            0 => Tone::Normal,
                            _ => Tone::Dim,
                        },
                    )
                })
                .collect(),
        })
    }

    fn open(&self, _ctx: &Ctx, _alvo: Option<&AssetId>) -> Box<dyn ModuleView> {
        Box::new(Vista {
            etapa: Etapa::Escolhendo,
            dir: dirs::download_dir()
                .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))),
            entradas: Vec::new(),
            lista: Lista::default(),
            arquivo: None,
            caminho: None,
            fonte: String::new(),
            excluidas: Vec::new(),
            form: None,
            erro: None,
        })
    }
}

enum Etapa {
    Escolhendo,
    Conferindo,
}

/// O que uma linha do arquivo faz com a carteira.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mudanca {
    Nova,
    Alterada,
    Igual,
    Problema,
}

impl Mudanca {
    fn label(&self) -> &'static str {
        match self {
            Mudanca::Nova => "nova",
            Mudanca::Alterada => "alterada",
            Mudanca::Igual => "igual",
            Mudanca::Problema => "problema",
        }
    }

    fn tone(&self) -> Tone {
        match self {
            Mudanca::Nova => Tone::Bom,
            Mudanca::Alterada => Tone::Aviso,
            Mudanca::Igual => Tone::Dim,
            Mudanca::Problema => Tone::Ruim,
        }
    }
}

/// Dois preços médios são «o mesmo» quando ambos faltam, ou quando batem. `None` e
/// `Some` são diferentes: passar a informar um custo é uma mudança de verdade.
/// O preço médio na tela: o número, ou um traço quando não foi informado.
fn pm_texto(pm: Option<f64>) -> String {
    pm.map(calc::preco).unwrap_or_else(|| "—".to_string())
}

fn mesmo_pm(a: Option<f64>, b: Option<f64>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => (x - y).abs() < 1e-9,
        _ => false,
    }
}

/// Uma linha lida, já conferida contra a carteira.
struct Lida {
    posicao: Option<Position>,
    mudanca: Mudanca,
    antes: Option<Position>,
    motivo: String,
}

struct Vista {
    etapa: Etapa,
    dir: PathBuf,
    entradas: Vec<PathBuf>,
    lista: Lista,
    arquivo: Option<Arquivo>,
    caminho: Option<PathBuf>,
    fonte: String,
    /// Índices de linhas que o usuário tirou da importação. Um arquivo com uma linha ruim
    /// não pode obrigar a escolher entre importar o erro e não importar nada.
    excluidas: Vec<usize>,
    form: Option<Formulario>,
    erro: Option<String>,
}

/// Extensões que valem a pena mostrar. Digitar o caminho inteiro à mão é como se desiste
/// de usar a funcionalidade, então há um navegador — e ele filtra.
/// `json` está aqui pelo extrato de posição consolidada da B3, que é a única fonte
/// deste programa que não é texto separado por vírgula — ver `invest/b3.rs`.
const EXTENSOES: [&str; 5] = ["csv", "txt", "ofx", "tsv", "json"];

impl Vista {
    fn listar(&mut self) {
        self.entradas.clear();
        // O pai primeiro, para dar sempre um caminho de volta.
        if self.dir.parent().is_some() {
            self.entradas.push(self.dir.join(".."));
        }
        let Ok(leitura) = std::fs::read_dir(&self.dir) else {
            self.erro = Some(format!("não consigo ler {}", self.dir.display()));
            return;
        };
        let mut dirs = Vec::new();
        let mut arquivos = Vec::new();
        for entrada in leitura.flatten() {
            let caminho = entrada.path();
            let nome = entrada.file_name().to_string_lossy().to_string();
            if nome.starts_with('.') {
                continue;
            }
            match caminho.is_dir() {
                true => dirs.push(caminho),
                false => {
                    let ext = caminho
                        .extension()
                        .map(|e| e.to_string_lossy().to_lowercase())
                        .unwrap_or_default();
                    if EXTENSOES.contains(&ext.as_str()) {
                        arquivos.push(caminho);
                    }
                }
            }
        }
        dirs.sort();
        arquivos.sort();
        self.entradas.extend(dirs);
        self.entradas.extend(arquivos);
        self.lista.selecionado = 0;
    }

    fn abrir(&mut self, caminho: PathBuf, ctx: &Ctx) {
        let Ok(bytes) = std::fs::read(&caminho) else {
            self.erro = Some(format!("não consigo ler {}", caminho.display()));
            return;
        };
        if bytes.is_empty() {
            self.erro = Some("o arquivo está vazio — nada foi tocado".into());
            return;
        }
        // O nome do arquivo é um bom palpite de fonte, e o mapeamento salvo é procurado
        // por ela: da segunda importação em diante, não se pergunta nada.
        let fonte = caminho
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "importado".into())
            .split(['-', '_'])
            .next()
            .unwrap_or("importado")
            .to_lowercase();
        let salvo = ctx.portfolio.mapeamentos.get(&fonte);
        self.arquivo = Some(Arquivo::ler(&bytes, salvo));
        self.caminho = Some(caminho);
        self.fonte = fonte;
        self.excluidas.clear();
        self.erro = None;
        self.lista = Lista::default();
        self.etapa = Etapa::Conferindo;
    }

    /// As linhas do navegador de arquivos, na mesma ordem em que a tela as mostra — é o
    /// que permite resolver o cursor filtrado para a entrada certa.
    fn linhas_arquivos(&self) -> Vec<Row> {
        self.entradas
            .iter()
            .map(|p| {
                let nome = match p.ends_with("..") {
                    true => "..".to_string(),
                    false => p
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default(),
                };
                let (tipo, tamanho) = match p.is_dir() || p.ends_with("..") {
                    true => ("pasta".to_string(), String::new()),
                    false => (
                        "arquivo".to_string(),
                        std::fs::metadata(p)
                            .map(|m| crate::format::human_bytes(m.len() as f64))
                            .unwrap_or_default(),
                    ),
                };
                Row::new(vec![nome, tipo, tamanho])
            })
            .collect()
    }

    /// As linhas da conferência que correspondem a **linhas do arquivo** — as mesmas que
    /// `excluidas` indexa. As linhas de remoção que a tela acrescenta no fim não entram:
    /// elas não vêm do arquivo e não se excluem.
    fn linhas_conferencia(&self, ctx: &Ctx) -> Vec<Row> {
        self.ler(ctx)
            .iter()
            .map(|l| {
                let ativo = l
                    .posicao
                    .as_ref()
                    .map(|p| p.ativo.short().to_string())
                    .unwrap_or_else(|| "—".into());
                Row::new(vec![ativo, l.mudanca.label().to_string()])
            })
            .collect()
    }

    /// Lê as linhas do arquivo e confere cada uma contra a carteira.
    fn ler(&self, ctx: &Ctx) -> Vec<Lida> {
        let Some(arquivo) = &self.arquivo else {
            return Vec::new();
        };
        arquivo
            .linhas
            .iter()
            .map(|linha| {
                let bruto_ativo = arquivo.coluna(linha, Campo::Ativo).unwrap_or_default();
                // Um símbolo sem mercado é o caso normal num CSV de corretora: ela escreve
                // «PETR4».
                //
                // Antes de assumir B3, o símbolo é procurado **na carteira**: quem tem
                // «TESOURO PREFIXADO 2031» em `OUTRO` escreve isso na planilha dele e não
                // `OUTRO/TESOURO-PREFIXADO-2031`, e sem esta busca a linha viraria um papel
                // da B3 novo em folha em vez de casar com a posição que já existe. O espaço
                // vira hífen porque é assim que o símbolo foi gravado.
                let ativo = match AssetId::parse(&bruto_ativo) {
                    Ok(a) => Some(a),
                    Err(_) if !bruto_ativo.trim().is_empty() => {
                        let simbolo = bruto_ativo.trim().replace(' ', "-").to_uppercase();
                        ctx.portfolio
                            .posicoes
                            .iter()
                            .map(|p| &p.ativo)
                            .find(|a| a.symbol == simbolo)
                            .cloned()
                            .or_else(|| Some(AssetId::new(Market::B3, &simbolo)))
                    }
                    Err(_) => None,
                };
                let Some(ativo) = ativo else {
                    return Lida {
                        posicao: None,
                        mudanca: Mudanca::Problema,
                        antes: None,
                        motivo: "sem ativo".into(),
                    };
                };
                let quantidade = arquivo.numero(linha, Campo::Quantidade);
                // O preço médio é opcional: um arquivo que não o traz ainda produz uma
                // posição de verdade, só sem P&L. Exigi-lo recusava importações inteiras
                // por uma coluna que a corretora não exporta.
                let pm = arquivo.numero(linha, Campo::PrecoMedio);
                let Some(quantidade) = quantidade else {
                    return Lida {
                        posicao: None,
                        mudanca: Mudanca::Problema,
                        antes: None,
                        motivo: "quantidade ilegível".into(),
                    };
                };
                let classe = arquivo
                    .coluna(linha, Campo::Classe)
                    .and_then(|v| Classe::parse(&v))
                    .unwrap_or_else(|| Classe::guess(&ativo));
                let moeda = arquivo
                    .coluna(linha, Campo::Moeda)
                    .and_then(|v| Moeda::parse(&v))
                    .or_else(|| ativo.market.moeda())
                    .unwrap_or(Moeda::Brl);
                let preco_manual = arquivo.numero(linha, Campo::PrecoAtual);

                let posicao = Position {
                    // A coluna manda quando existe: o extrato consolidado da B3 traz
                    // várias corretoras num arquivo só, e o nome do arquivo não sabe
                    // dizer de qual delas é cada linha.
                    fonte: arquivo
                        .coluna(linha, Campo::Fonte)
                        .map(|f| f.trim().to_string())
                        .filter(|f| !f.is_empty())
                        .unwrap_or_else(|| self.fonte.clone()),
                    conta: arquivo.coluna(linha, Campo::Conta).unwrap_or_default(),
                    ativo,
                    classe,
                    quantidade,
                    preco_medio: pm,
                    moeda,
                    preco_manual,
                    preco_manual_em: None,
                    atualizado_em: 0,
                    carteira: None,
                };
                if let Err(e) = posicao.validate() {
                    return Lida {
                        posicao: None,
                        mudanca: Mudanca::Problema,
                        antes: None,
                        motivo: e,
                    };
                }

                let chave = posicao.key();
                let antes = ctx
                    .portfolio
                    .posicoes
                    .iter()
                    .find(|p| p.key() == chave)
                    .cloned();
                let mudanca = match &antes {
                    None => Mudanca::Nova,
                    Some(a)
                        if (a.quantidade - posicao.quantidade).abs() < 1e-9
                            && mesmo_pm(a.preco_medio, posicao.preco_medio) =>
                    {
                        Mudanca::Igual
                    }
                    Some(_) => Mudanca::Alterada,
                };
                Lida {
                    posicao: Some(posicao),
                    mudanca,
                    antes,
                    motivo: String::new(),
                }
            })
            .collect()
    }
}

impl ModuleView for Vista {
    fn tick(&mut self, _ctx: &Ctx) {
        if matches!(self.etapa, Etapa::Escolhendo) && self.entradas.is_empty() {
            self.listar();
        }
    }

    fn layout(&self, ctx: &Ctx) -> Layout {
        if let Some(form) = &self.form {
            return Layout::one(Pane::Form {
                title: form.titulo.clone(),
                fields: form.campos.clone(),
                selected: form.selecionado,
                error: form.erro.clone(),
                hint: hint(&[
                    "↑/↓ coluna",
                    "←/→ significado",
                    "Enter aplicar",
                    "Esc cancelar",
                ]),
            });
        }

        match self.etapa {
            Etapa::Escolhendo => {
                let rows = self.linhas_arquivos();
                Layout::one(Pane::Table {
                    title: format!("Escolher arquivo · {}", self.dir.display()),
                    headers: vec!["Nome".into(), "Tipo".into(), "Tamanho".into()],
                    rows,
                    selected: Some(self.lista.selecionado),
                    query: self.lista.busca.clone(),
                    note: self.erro.clone().map(|e| (e, Tone::Aviso)).or(Some((
                        "CSV, TSV, TXT, OFX e o JSON de posição da B3 · Enter entra na pasta ou abre · Ctrl+R relista"
                            .into(),
                        Tone::Dim,
                    ))),
                })
            }
            Etapa::Conferindo => {
                let Some(arquivo) = &self.arquivo else {
                    return Layout::one(Pane::Empty {
                        title: "Importação".into(),
                        note: "nada aberto".into(),
                    });
                };
                if let Some(falta) = arquivo.falta() {
                    return Layout::one(Pane::Empty {
                        title: format!("Importação · {}", self.fonte),
                        note: format!(
                            "{falta}.\n\n\
                             As colunas do arquivo são: {}\n\n\
                             Ctrl+M abre o mapeamento para você dizer o que cada uma é. \n\
                             Ele fica salvo para esta fonte, e da próxima vez não se \n\
                             pergunta nada.",
                            arquivo.cabecalho.join(", ")
                        ),
                    });
                }

                let lidas = self.ler(ctx);
                let mut rows: Vec<Row> = Vec::new();
                let (mut novas, mut alteradas, mut iguais, mut problemas) = (0, 0, 0, 0);

                for (i, l) in lidas.iter().enumerate() {
                    let excluida = self.excluidas.contains(&i);
                    match l.mudanca {
                        Mudanca::Nova => novas += 1,
                        Mudanca::Alterada => alteradas += 1,
                        Mudanca::Igual => iguais += 1,
                        Mudanca::Problema => problemas += 1,
                    }
                    let marca = match (excluida, l.mudanca) {
                        (true, _) => "○",
                        (false, Mudanca::Problema) => "⚠",
                        _ => "●",
                    };
                    let (ativo, qtd, pm) = match &l.posicao {
                        Some(p) => (
                            p.ativo.short().to_string(),
                            match &l.antes {
                                Some(a) if (a.quantidade - p.quantidade).abs() > 1e-9 => format!(
                                    "{} (era {})",
                                    calc::preco(p.quantidade),
                                    calc::preco(a.quantidade)
                                ),
                                _ => calc::preco(p.quantidade),
                            },
                            match &l.antes {
                                Some(a) if !mesmo_pm(a.preco_medio, p.preco_medio) => format!(
                                    "{} (era {})",
                                    pm_texto(p.preco_medio),
                                    pm_texto(a.preco_medio)
                                ),
                                _ => pm_texto(p.preco_medio),
                            },
                        ),
                        None => ("—".into(), "—".into(), "—".into()),
                    };
                    rows.push(
                        Row::new(vec![
                            format!("{marca} {ativo}"),
                            qtd,
                            pm,
                            match l.motivo.is_empty() {
                                true => l.mudanca.label().to_string(),
                                false => format!("{} — {}", l.mudanca.label(), l.motivo),
                            },
                        ])
                        .with_cell_tones(vec![
                            match excluida {
                                true => Tone::Dim,
                                false => Tone::Normal,
                            },
                            Tone::Normal,
                            Tone::Normal,
                            l.mudanca.tone(),
                        ]),
                    );
                }

                // Uma remoção é a mudança mais perigosa que uma importação faz, e a que um
                // arquivo exportado pela metade produz. Ela é destacada.
                let no_arquivo: Vec<AssetId> = lidas
                    .iter()
                    .filter_map(|l| l.posicao.as_ref().map(|p| p.ativo.clone()))
                    .collect();
                let removidas: Vec<&Position> = ctx
                    .portfolio
                    .posicoes
                    .iter()
                    .filter(|p| p.fonte == self.fonte && !no_arquivo.contains(&p.ativo))
                    .collect();
                for p in &removidas {
                    rows.push(Row::tinted(
                        vec![
                            format!("● {}", p.ativo.short()),
                            format!("— (era {})", calc::preco(p.quantidade)),
                            "—".into(),
                            "REMOVIDA — não está no arquivo".into(),
                        ],
                        Tone::Ruim,
                    ));
                }

                let dialeto = format!(
                    "separador «{}» · {} · ",
                    match arquivo.dialeto.separador {
                        '\t' => "tab".to_string(),
                        c => c.to_string(),
                    },
                    match arquivo.dialeto.tem_cabecalho {
                        true => "com cabeçalho",
                        false => "sem cabeçalho",
                    }
                );
                let resumo = dialeto
                    + &format!(
                        "{novas} nova(s) · {alteradas} alterada(s) · {iguais} igual(is) · {} removida(s) · {problemas} com problema · {} excluída(s) por você",
                        removidas.len(),
                        self.excluidas.len()
                    );

                Layout::rows(vec![
                    (
                        4,
                        Layout::one(Pane::Table {
                            title: format!(
                                "Conferir · {} · {} linha(s)",
                                self.caminho
                                    .as_ref()
                                    .and_then(|c| c.file_name())
                                    .map(|n| n.to_string_lossy().to_string())
                                    .unwrap_or_default(),
                                lidas.len()
                            ),
                            headers: vec![
                                "Ativo".into(),
                                "Quantidade".into(),
                                "Preço médio".into(),
                                "O que muda".into(),
                            ],
                            rows,
                            selected: Some(self.lista.selecionado),
                            query: self.lista.busca.clone(),
                            note: Some((resumo, Tone::Aviso)),
                        }),
                    ),
                    (
                        1,
                        Layout::one(Pane::Facts {
                            title: "Antes de gravar".into(),
                            rows: vec![
                                (
                                    "fonte".into(),
                                    format!(
                                        "{} — só as linhas desta fonte serão tocadas",
                                        self.fonte
                                    ),
                                    Tone::Normal,
                                ),
                                (
                                    "preço médio".into(),
                                    "vem do arquivo e é gravado como informado — nada é recalculado"
                                        .into(),
                                    Tone::Dim,
                                ),
                                (
                                    "nada foi gravado ainda".into(),
                                    "Enter aplica · Esc sai sem tocar em nada".into(),
                                    Tone::Aviso,
                                ),
                            ],
                        }),
                    ),
                ])
            }
        }
    }

    fn key(&mut self, key: KeyEvent, ctx: &Ctx) -> Outcome {
        if let Some(form) = &mut self.form {
            if form.tecla(key) {
                if let Some(arquivo) = &mut self.arquivo {
                    for (i, nome) in arquivo.cabecalho.clone().iter().enumerate() {
                        let escolhido = form.valor(nome);
                        if let Some(campo) = Campo::ALL.into_iter().find(|c| c.label() == escolhido)
                        {
                            arquivo.mapa[i] = campo;
                        }
                    }
                }
                self.form = None;
            }
            return Outcome::Ok;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match (&self.etapa, key.code) {
            (Etapa::Escolhendo, KeyCode::Char('r')) if ctrl => {
                // Relista à mão: a pasta é lida uma vez ao abrir, e não a cada tique —
                // um `read_dir` duas vezes por segundo seria I/O por nada.
                self.entradas.clear();
                self.lista = Lista::default();
                Outcome::Ok
            }
            (Etapa::Escolhendo, KeyCode::Enter) => {
                // Pelo índice **visível**: com uma busca ativa, o cursor anda pela lista
                // filtrada, e usar o número dele na lista original abre outra pasta.
                let rows = self.linhas_arquivos();
                let Some(escolhido) = self
                    .lista
                    .atual(&rows)
                    .and_then(|i| self.entradas.get(i))
                    .cloned()
                else {
                    return Outcome::Ok;
                };
                if escolhido.ends_with("..") {
                    if let Some(pai) = self.dir.parent() {
                        self.dir = pai.to_path_buf();
                        self.listar();
                    }
                    return Outcome::Ok;
                }
                if escolhido.is_dir() {
                    self.dir = escolhido;
                    self.listar();
                    return Outcome::Ok;
                }
                self.abrir(escolhido, ctx);
                Outcome::Ok
            }
            (Etapa::Escolhendo, _) => match self
                .lista
                .tecla(key, self.linhas_arquivos().len().min(self.entradas.len()))
            {
                true => Outcome::Ok,
                false => Outcome::Ignorada,
            },

            (Etapa::Conferindo, KeyCode::Char('m')) if ctrl => {
                if let Some(arquivo) = &self.arquivo {
                    let opcoes: Vec<String> =
                        Campo::ALL.iter().map(|c| c.label().to_string()).collect();
                    self.form = Some(Formulario::novo(
                        "O que é cada coluna",
                        arquivo
                            .cabecalho
                            .iter()
                            .zip(&arquivo.mapa)
                            .map(|(nome, campo)| {
                                Field::choice(
                                    nome,
                                    campo.label(),
                                    opcoes.clone(),
                                    "←/→ escolhe. Fica salvo para esta fonte",
                                )
                            })
                            .collect(),
                    ));
                }
                Outcome::Ok
            }
            (Etapa::Conferindo, KeyCode::Char(' ')) => {
                // Pelo índice visível: excluir pelo número do cursor filtrado tiraria
                // outra linha da importação.
                let rows = self.linhas_conferencia(ctx);
                let Some(i) = self.lista.atual(&rows) else {
                    return Outcome::Ok;
                };
                match self.excluidas.iter().position(|x| *x == i) {
                    Some(p) => {
                        self.excluidas.remove(p);
                    }
                    None => self.excluidas.push(i),
                }
                Outcome::Ok
            }
            // Só a coluna do preço médio, nas posições que já existem.
            //
            // É o caso de quem tem duas fontes de informação: o extrato da B3 diz quanto
            // se tem e em qual corretora, e não diz o custo; a planilha de quem investe diz
            // o custo, e não sabe de corretora. Importar a planilha como fonte duplicaria a
            // carteira; isto escreve só o que falta, casando por ativo.
            (Etapa::Conferindo, KeyCode::Char('p'))
                if key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                let edits: Vec<Edit> = self
                    .ler(ctx)
                    .into_iter()
                    .enumerate()
                    .filter(|(i, l)| !self.excluidas.contains(i) && l.posicao.is_some())
                    .filter_map(|(_, l)| {
                        let p = l.posicao?;
                        // Uma linha sem preço médio não apaga o que já está lá: ela não
                        // tem o que dizer sobre custo.
                        Some(Edit::PrecoMedio(p.ativo, p.preco_medio?))
                    })
                    .collect();
                if edits.is_empty() {
                    self.erro = Some(
                        "nenhuma linha traz preço médio — nada foi tocado. Confira o \
                         mapeamento com Ctrl+M."
                            .into(),
                    );
                    return Outcome::Ok;
                }
                self.etapa = Etapa::Escolhendo;
                self.arquivo = None;
                self.entradas.clear();
                Outcome::Editar(edits)
            }
            (Etapa::Conferindo, KeyCode::Enter) => {
                let lidas = self.ler(ctx);
                let posicoes: Vec<Position> = lidas
                    .iter()
                    .enumerate()
                    .filter(|(i, l)| !self.excluidas.contains(i) && l.posicao.is_some())
                    .filter_map(|(_, l)| l.posicao.clone())
                    .collect();
                // Um arquivo de que não sobrou nada não «sincroniza» a carteira para
                // vazia: isso apagaria tudo.
                if posicoes.is_empty() {
                    self.erro = Some(
                        "nenhuma linha válida — a importação foi recusada e nada foi tocado".into(),
                    );
                    return Outcome::Ok;
                }
                // **Uma substituição por corretora.** Com um arquivo de corretora única
                // isto dá exatamente o que dava antes; com o extrato da B3, que traz
                // várias, é o que impede duas coisas: juntar todas numa fonte só, e —
                // pior — uma fonte do arquivo apagar as linhas de outra.
                let mut por_fonte: std::collections::BTreeMap<String, Vec<Position>> =
                    Default::default();
                for p in posicoes {
                    por_fonte.entry(p.fonte.clone()).or_default().push(p);
                }
                let mut edits: Vec<Edit> = por_fonte
                    .into_iter()
                    .map(|(fonte, posicoes)| Edit::SubstituirFonte { fonte, posicoes })
                    .collect();
                // O mapeamento aprendido, para a próxima vez não perguntar nada. É o que
                // transforma isto de uma tarefa chata numa que se usa todo mês.
                if let Some(arquivo) = &self.arquivo {
                    let mapa = arquivo.para_salvar();
                    if !mapa.is_empty() {
                        edits.push(Edit::SetMapeamento(self.fonte.clone(), mapa));
                    }
                }
                self.etapa = Etapa::Escolhendo;
                self.arquivo = None;
                self.entradas.clear();
                Outcome::Editar(edits)
            }
            (Etapa::Conferindo, _) => {
                let total = self.linhas_conferencia(ctx).len();
                match self.lista.tecla(key, total) {
                    true => Outcome::Ok,
                    false => Outcome::Ignorada,
                }
            }
        }
    }

    fn escape(&mut self) -> Escape {
        if self.form.take().is_some() {
            return Escape::Consumido;
        }
        // Sair da conferência volta para a escolha, e **nada foi gravado**.
        if matches!(self.etapa, Etapa::Conferindo) {
            self.etapa = Etapa::Escolhendo;
            self.arquivo = None;
            self.entradas.clear();
            return Escape::Consumido;
        }
        self.lista.escape()
    }

    fn hint(&self) -> String {
        match self.etapa {
            Etapa::Escolhendo => hint(&["↑/↓ andar", "Enter abrir", "Esc sair"]),
            Etapa::Conferindo => hint(&[
                "↑/↓ andar",
                "espaço incluir/excluir",
                "Ctrl+M mapear colunas",
                "Ctrl+P só o preço médio",
                "Enter importar",
                "Esc cancelar",
            ]),
        }
    }
}
