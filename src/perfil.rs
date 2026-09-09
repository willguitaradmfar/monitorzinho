//! Escolher com qual banco o programa vai trabalhar, antes de ele existir.
//!
//! Um perfil é um arquivo `.db` em `~/.local/share/monitorzinho/db/`. Ter mais de um é
//! ter mais de uma vida separada no mesmo programa — a carteira de casa e a da empresa,
//! a máquina de trabalho e o servidor que se acompanha, um banco de verdade e um de
//! brincadeira para mexer sem medo. Eles não conversam: cada um tem o seu histórico, as
//! suas ferramentas, as suas marcas e a sua carteira.
//!
//! A regra de quando perguntar é a que faz o caso comum não custar nada:
//!
//! * **Nenhum perfil** — cria `padrao` e entra. Quem nunca ouviu falar disto nunca vê a
//!   tela.
//! * **Um perfil** — entra nele direto. Ter um só é não ter escolha a fazer.
//! * **Mais de um** — pergunta, obrigatoriamente. Adivinhar aqui é abrir a carteira
//!   errada, e «o último que você usou» é exatamente o palpite que erra no dia em que a
//!   pessoa criou o segundo perfil para não misturar as coisas.
//!
//! `--perfil` força a tela mesmo com um só, que é como se cria o segundo; `--perfil
//! <nome>` vai direto, que é como se põe o monitorzinho num script ou num atalho.

use std::io;
use std::path::{Path, PathBuf};

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::db::{self, Perfil};
use crate::ui::palette;

/// O que a linha de comando pediu sobre o perfil.
pub enum Pedido {
    /// Sem `--perfil`: só pergunta se houver mais de um.
    Automatico,
    /// `--perfil` sem nome: pergunta sempre, que é como se cria um perfil novo.
    Escolher,
    /// `--perfil <nome>`: vai direto, criando se não existir.
    Nomeado(String),
}

/// Decide o perfil e o abre. `None` quando a pessoa desistiu na tela — e aí o programa
/// termina sem ter aberto nada, que é o que Esc numa tela de escolha significa.
pub fn abrir(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    pedido: Pedido,
) -> io::Result<Option<Aberto>> {
    let existentes = db::perfis();
    let caminho = match pedido {
        Pedido::Nomeado(nome) => db::caminho_de(&nome),
        Pedido::Escolher => match escolher(terminal, existentes)? {
            Some(caminho) => caminho,
            None => return Ok(None),
        },
        Pedido::Automatico => match existentes.len() {
            0 => db::caminho_de(db::PADRAO),
            1 => existentes[0].caminho.clone(),
            _ => match escolher(terminal, existentes)? {
                Some(caminho) => caminho,
                None => return Ok(None),
            },
        },
    };

    match concluir(&caminho) {
        Ok(legado) => Ok(Some(Aberto::Pronto { legado })),
        Err(erro) => Ok(Some(Aberto::Falhou { caminho, erro })),
    }
}

/// Abre o arquivo e traz o que houver da versão anterior. O caminho por onde toda
/// abertura passa — com tela ou sem —, para que nenhuma delas possa esquecer a metade
/// que a outra faz.
fn concluir(caminho: &Path) -> Result<Option<crate::legado::Resumo>, String> {
    let novo = db::abrir(caminho)?;
    // Só no nascimento do primeiro perfil: um banco que já existe já passou por isto, e
    // um segundo perfil é justamente para começar limpo.
    Ok(
        match novo && db::perfis().len() == 1 && crate::legado::existe_algo() {
            true => Some(crate::legado::importar()),
            false => None,
        },
    )
}

/// O resultado de abrir — separado de «desistiu» porque um banco que não abre é uma
/// mensagem que alguém precisa ler, e não um programa que some.
pub enum Aberto {
    Pronto {
        legado: Option<crate::legado::Resumo>,
    },
    Falhou {
        caminho: PathBuf,
        erro: String,
    },
}

/// Abre o perfil que uma execução sem flag abriria, sem tela nenhuma. É o que o
/// `--bench` usa: ele mede uma execução normal e não pode parar para perguntar.
///
/// Com mais de um perfil ele não adivinha a carteira certa — pega o `padrao` se houver, e
/// o primeiro em ordem alfabética se não. Para medir outro, `--perfil <nome>`.
pub fn abrir_sem_tela() -> Result<(), String> {
    let existentes = db::perfis();
    let caminho = match existentes.len() {
        0 => db::caminho_de(db::PADRAO),
        1 => existentes[0].caminho.clone(),
        _ => existentes
            .iter()
            .find(|p| p.nome == db::PADRAO)
            .unwrap_or(&existentes[0])
            .caminho
            .clone(),
    };
    concluir(&caminho).map(|_| ())
}

/// Uma tela de texto que espera uma tecla. As duas coisas que acontecem antes de o
/// programa existir — a importação dos arquivos antigos e um banco que não abre — são
/// coisas que alguém precisa **ler**, e um aviso no rodapé de um monitor some no
/// primeiro tick.
fn avisar(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    titulo: &str,
    cor: ratatui::style::Color,
    linhas: Vec<String>,
) -> io::Result<()> {
    terminal.draw(|frame| {
        let area = frame.area();
        frame.render_widget(Clear, area);
        let bloco = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(cor))
            .title(format!(" {titulo} "));
        let dentro = bloco.inner(area);
        frame.render_widget(bloco, area);
        let mut texto: Vec<Line> = vec![Line::raw("")];
        texto.extend(linhas.iter().map(|l| Line::raw(format!(" {l}"))));
        texto.push(Line::raw(""));
        texto.push(Line::styled(
            " qualquer tecla continua",
            Style::default().fg(palette::DIM),
        ));
        frame.render_widget(Paragraph::new(texto), dentro);
    })?;
    // Só uma tecla, e não um `Enter`: a pessoa não pediu esta tela, e sair dela tem que
    // ser a coisa mais fácil que existe.
    loop {
        if let Event::Key(_) = event::read()? {
            return Ok(());
        }
    }
}

/// O que a importação dos arquivos da versão anterior fez. Acontece uma vez na vida da
/// instalação e move a carteira de alguém de lugar — então diz o que moveu e para onde.
pub fn mostrar_legado(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    resumo: &crate::legado::Resumo,
) -> io::Result<()> {
    let mut linhas = vec![
        "Esta versão guarda tudo num arquivo SQLite por perfil, em vez de vários".to_string(),
        "arquivos JSON soltos. O que já existia foi trazido para dentro dele:".to_string(),
        String::new(),
    ];
    for nome in &resumo.importados {
        linhas.push(format!("  ✓ {nome}"));
    }
    for nome in &resumo.ilegiveis {
        linhas.push(format!("  ✗ {nome} — ilegível, ficou onde estava"));
    }
    if let Some(dir) = &resumo.guardados_em {
        linhas.push(String::new());
        linhas.push(format!("Os originais estão guardados em {}", dir.display()));
        linhas.push("— nada foi apagado, e dá para conferir.".to_string());
    }
    linhas.push(String::new());
    linhas.push(format!(
        "Daqui em diante o que vale é {}",
        db::caminho_de(&db::perfil_atual()).display()
    ));
    avisar(
        terminal,
        "o que estava em disco veio junto",
        palette::GREEN,
        linhas,
    )
}

/// O banco não abriu. Dizer qual arquivo e por quê, em vez de um programa que some.
pub fn mostrar_falha(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    caminho: &Path,
    erro: &str,
) -> io::Result<()> {
    avisar(
        terminal,
        "não deu para abrir este perfil",
        palette::RED,
        vec![
            caminho.display().to_string(),
            String::new(),
            erro.to_string(),
            String::new(),
            "Nada foi alterado no arquivo. Um perfil diferente abre com".to_string(),
            "  monitorzinho --perfil <nome>".to_string(),
        ],
    )
}

/// A tela. Devolve o caminho escolhido, ou `None` se a pessoa saiu.
fn escolher(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    perfis: Vec<Perfil>,
) -> io::Result<Option<PathBuf>> {
    let mut tela = Tela {
        perfis,
        cursor: 0,
        novo: None,
    };

    loop {
        terminal.draw(|frame| desenhar(frame.area(), frame, &tela))?;

        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Ok(None);
        }

        // Com a caixa de nome aberta, cada letra é o nome — inclusive as que fora dela
        // seriam atalhos.
        if let Some(nome) = &mut tela.novo {
            match key.code {
                KeyCode::Esc => tela.novo = None,
                KeyCode::Backspace => {
                    nome.pop();
                }
                KeyCode::Enter => {
                    let nome = db::sanear(nome);
                    return Ok(Some(db::caminho_de(&nome)));
                }
                KeyCode::Char(c) => nome.push(c),
                _ => {}
            }
            continue;
        }

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Ok(None),
            KeyCode::Up | KeyCode::Char('k') => {
                tela.cursor = tela.cursor.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                tela.cursor = (tela.cursor + 1).min(tela.perfis.len().saturating_sub(1));
            }
            KeyCode::Char('n') => tela.novo = Some(String::new()),
            KeyCode::Enter => {
                if let Some(perfil) = tela.perfis.get(tela.cursor) {
                    return Ok(Some(perfil.caminho.clone()));
                }
            }
            _ => {}
        }
    }
}

struct Tela {
    perfis: Vec<Perfil>,
    cursor: usize,
    /// O nome sendo digitado, quando a caixa do perfil novo está aberta.
    novo: Option<String>,
}

fn desenhar(area: Rect, frame: &mut ratatui::Frame, tela: &Tela) {
    frame.render_widget(Clear, area);

    let linhas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .split(area);

    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(
                format!(" monitorzinho v{}", env!("CARGO_PKG_VERSION")),
                Style::default()
                    .fg(palette::CYAN)
                    .add_modifier(Modifier::BOLD),
            ),
            Line::styled(
                format!(" {}", db::perfis_dir().display()),
                Style::default().fg(palette::DIM),
            ),
        ]),
        linhas[0],
    );

    let bloco = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(palette::DIM))
        .title(" qual perfil ");
    let dentro = bloco.inner(linhas[1]);
    frame.render_widget(bloco, linhas[1]);

    let mut corpo: Vec<Line> = Vec::new();
    if tela.perfis.is_empty() {
        corpo.push(Line::styled(
            "  não há nenhum ainda — n cria o primeiro",
            Style::default().fg(palette::DIM),
        ));
    }
    // A coluna do nome tem a largura do maior deles, para tamanho e idade ficarem
    // alinhados: são números que se comparam com o olho, e uma coluna serrilhada é uma
    // comparação que não se faz.
    let largura_nome = tela
        .perfis
        .iter()
        .map(|p| p.nome.chars().count())
        .max()
        .unwrap_or(0)
        .max(8);
    for (i, perfil) in tela.perfis.iter().enumerate() {
        let escolhido = i == tela.cursor && tela.novo.is_none();
        let estilo = match escolhido {
            true => Style::default()
                .fg(palette::CYAN)
                .add_modifier(Modifier::BOLD),
            false => Style::default(),
        };
        corpo.push(Line::from(vec![
            Span::styled(
                match escolhido {
                    true => "  ▸ ",
                    false => "    ",
                },
                estilo,
            ),
            Span::styled(
                format!(
                    "{:<largura_nome$}",
                    perfil.nome,
                    largura_nome = largura_nome
                ),
                estilo,
            ),
            Span::styled(
                format!(
                    "   {:>10}   {}",
                    crate::format::human_bytes(perfil.bytes as f64),
                    idade(perfil.modificado_em)
                ),
                Style::default().fg(palette::DIM),
            ),
        ]));
    }
    frame.render_widget(Paragraph::new(corpo), dentro);

    let rodape = match &tela.novo {
        Some(_) => " Enter cria e abre · Esc cancela ".to_string(),
        None => " ↑/↓ escolhe · Enter abre · n novo perfil · Esc sai ".to_string(),
    };
    frame.render_widget(
        Paragraph::new(Line::styled(rodape, Style::default().fg(palette::DIM)))
            .alignment(Alignment::Right),
        linhas[2],
    );

    if let Some(nome) = &tela.novo {
        caixa_de_nome(frame, area, nome);
    }
}

/// A caixa do perfil novo. Um perfil novo nasce **vazio** — não é uma cópia do que está
/// aberto —, e o texto diz isso, porque «novo» num programa que já tem uma carteira
/// dentro é ambíguo o bastante para alguém esperar o contrário.
fn caixa_de_nome(frame: &mut ratatui::Frame, area: Rect, nome: &str) {
    let largura = 52.min(area.width.saturating_sub(4));
    let caixa = Rect {
        x: area.x + (area.width.saturating_sub(largura)) / 2,
        y: area.y + area.height / 3,
        width: largura,
        height: 6,
    };
    frame.render_widget(Clear, caixa);
    let bloco = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(palette::CYAN))
        .title(" perfil novo ");
    let dentro = bloco.inner(caixa);
    frame.render_widget(bloco, caixa);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::raw(" nome: "),
                Span::styled(
                    format!("{nome}_"),
                    Style::default()
                        .fg(palette::CYAN)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::raw(""),
            Line::styled(
                " começa vazio, sem nada do perfil de agora",
                Style::default().fg(palette::DIM),
            ),
        ]),
        dentro,
    );
}

/// «há 3 dias», para a lista dizer qual foi usado por último sem uma coluna de data.
fn idade(em: u64) -> String {
    if em == 0 {
        return String::new();
    }
    let agora = db::agora();
    let segundos = agora.saturating_sub(em);
    match segundos {
        s if s < 90 => "agora há pouco".to_string(),
        s if s < 3600 => format!("há {} min", s / 60),
        s if s < 86400 => format!("há {} h", s / 3600),
        s => format!("há {} dias", s / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_idade_diz_a_unidade_que_cabe() {
        let agora = db::agora();
        assert_eq!(idade(agora), "agora há pouco");
        assert_eq!(idade(agora - 600), "há 10 min");
        assert_eq!(idade(agora - 7200), "há 2 h");
        assert_eq!(idade(agora - 3 * 86400), "há 3 dias");
        // Sem carimbo não se inventa um: a coluna fica vazia.
        assert_eq!(idade(0), "");
    }
}
