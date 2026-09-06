//! As chamadas ao binário, e o parsing do que elas respondem.
//!
//! O único arquivo do programa que cria processos. A justificativa está no cabeçalho de
//! `tmux/mod.rs`; aqui ficam as três regras práticas que ela impõe:
//!
//! * **O binário é resolvido uma vez, absoluto.** Uma mudança no `PATH` no meio da
//!   execução não pode trocar o programa embaixo de nós.
//! * **O socket é dito, quando se sabe qual é.** Numa máquina com mais de um servidor,
//!   falar com o padrão enquanto se vive em outro é responder sobre outras sessões.
//! * **O separador é uma tabulação.** Não por ser improvável nos dados — um nome de
//!   janela é texto livre —, mas porque o tmux **escapa os caracteres de controle que
//!   aparecem nos valores** e não os que estão no texto do formato. Uma janela chamada
//!   com uma tabulação dentro sai como `a\tb`, em quatro caracteres visíveis; a
//!   tabulação que nós pusemos entre os campos sai como ela mesma. Conferido: é o que
//!   torna a divisão por tabulação exata, e não só provável.
//!
//!   Foi a segunda tentativa. A primeira usou `\x1f`, o separador de unidade do ASCII, no
//!   raciocínio de que ninguém o digita — e o tmux o escapou junto com todo o resto,
//!   devolvendo `\037` em texto e uma linha de um campo só.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// O separador entre campos de uma linha do `-F`. Ver o cabeçalho: é seguro porque o
/// tmux escapa a tabulação que estiver *num valor* e não a que está no formato.
const SEP: char = '\t';

/// Os campos que uma volta pede, na ordem em que são lidos de volta.
///
/// Todos numa chamada a `list-panes -a`: os formatos do tmux dão os campos da janela e da
/// sessão de cada painel na mesma linha, e como toda sessão tem ao menos uma janela e
/// toda janela ao menos um painel, isto reconstrói a árvore inteira de uma vez.
const FIELDS: &[&str] = &[
    "#{session_name}",
    "#{session_id}",
    "#{session_created}",
    "#{session_attached}",
    "#{session_windows}",
    "#{session_path}",
    "#{window_index}",
    "#{window_name}",
    "#{window_active}",
    "#{window_panes}",
    "#{pane_index}",
    "#{pane_id}",
    "#{pane_active}",
    "#{pane_pid}",
    "#{pane_width}",
    "#{pane_height}",
    "#{pane_current_command}",
    "#{pane_current_path}",
];

/// O tmux instalado nesta máquina, se houver.
///
/// Varre o `PATH` procurando um arquivo executável — filesystem puro, sem criar processo
/// nenhum para descobrir se existe um programa. É esta função que decide se a aba entra
/// na barra.
pub fn find_binary() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("tmux"))
        .find(|candidate| executable(candidate))
}

#[cfg(unix)]
fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn executable(path: &Path) -> bool {
    path.is_file()
}

pub fn version(bin: &Path) -> Result<String, String> {
    let output = run(bin, None, &["-V"])?;
    // `tmux 3.4` — a versão é o que vem depois do nome dele.
    Ok(output
        .trim()
        .strip_prefix("tmux ")
        .unwrap_or(output.trim())
        .to_string())
}

/// Roda um comando e devolve o que ele escreveu na saída padrão.
///
/// Um erro traz o que o tmux disse no erro padrão, sem tradução. Só quando ele não disse
/// nada é que inventamos uma frase, porque uma falha silenciosa não é resposta.
pub fn run(bin: &Path, socket: Option<&str>, args: &[&str]) -> Result<String, String> {
    let mut command = Command::new(bin);
    if let Some(socket) = socket {
        command.arg("-S").arg(socket);
    }
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = command
        .output()
        .map_err(|error| format!("não consegui rodar o tmux: {error}"))?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    let said = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(if said.is_empty() {
        format!("tmux {} falhou", args.first().copied().unwrap_or("?"))
    } else {
        said
    })
}

/// Se uma resposta de erro quer dizer «não há servidor» em vez de «deu errado».
///
/// Sem servidor não há sessão nenhuma, e isso é um estado legítimo — é o que se vê numa
/// máquina que tem o tmux instalado e ainda não abriu nada. Uma tabela vazia é a resposta
/// certa; uma linha vermelha dizendo que algo falhou seria mentira.
fn no_server(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    error.contains("no server running")
        || error.contains("error connecting to")
        || error.contains("no current session")
}

/// A árvore inteira numa chamada.
pub fn snapshot(bin: &Path, socket: Option<&str>) -> Result<super::Snapshot, String> {
    let format = FIELDS.join(&SEP.to_string());
    let output = match run(bin, socket, &["list-panes", "-a", "-F", &format]) {
        Ok(output) => output,
        Err(error) if no_server(&error) => return Ok(super::Snapshot::default()),
        Err(error) => return Err(error),
    };
    Ok(parse(&output))
}

/// Monta sessões, janelas e painéis a partir das linhas de painel.
///
/// Público para o teste, que é onde a forma da resposta do tmux fica registrada — sem
/// isso, a única maneira de conferir o parsing seria ter um servidor rodando.
pub fn parse(output: &str) -> super::Snapshot {
    let mut snapshot = super::Snapshot::default();
    for line in output.lines() {
        let fields: Vec<&str> = line.split(SEP).collect();
        if fields.len() < FIELDS.len() {
            continue;
        }
        let session_name = fields[0].to_string();
        let window_index = fields[6].parse().unwrap_or(0);

        if snapshot.session_named(&session_name).is_none() {
            snapshot.sessions.push(super::Session {
                name: session_name.clone(),
                id: fields[1].to_string(),
                created: fields[2].parse().unwrap_or(0),
                attached: fields[3].parse().unwrap_or(0),
                windows: fields[4].parse().unwrap_or(0),
                path: fields[5].to_string(),
            });
        }
        if !snapshot
            .windows
            .iter()
            .any(|w| w.session == session_name && w.index == window_index)
        {
            snapshot.windows.push(super::Window {
                session: session_name.clone(),
                index: window_index,
                name: fields[7].to_string(),
                active: fields[8] == "1",
                panes: fields[9].parse().unwrap_or(0),
            });
        }
        snapshot.panes.push(super::Pane {
            session: session_name,
            window_index,
            index: fields[10].parse().unwrap_or(0),
            id: fields[11].to_string(),
            active: fields[12] == "1",
            pid: fields[13].parse().unwrap_or(0),
            width: fields[14].parse().unwrap_or(0),
            height: fields[15].parse().unwrap_or(0),
            command: fields[16].to_string(),
            path: fields[17].to_string(),
        });
    }
    // O tmux lista na ordem dele; a tabela quer a ordem que uma pessoa espera ler.
    snapshot.sessions.sort_by(|a, b| a.name.cmp(&b.name));
    snapshot
        .windows
        .sort_by(|a, b| (&a.session, a.index).cmp(&(&b.session, b.index)));
    snapshot.panes.sort_by(|a, b| {
        (&a.session, a.window_index, a.index).cmp(&(&b.session, b.window_index, b.index))
    });
    snapshot
}

/// Entrega o terminal ao tmux e espera ele acabar.
///
/// `status()` e não `output()`: o filho tem que herdar o terminal de verdade — é o que
/// faz o tmux desenhar na tela em que estamos e receber o teclado direto, sem ninguém no
/// meio copiando bytes.
///
/// `nested` apaga o `$TMUX` do ambiente do filho, que é literalmente o que a mensagem do
/// tmux manda fazer quando recusa («unset $TMUX to force»). Sem isso ele se recusa a
/// anexar dentro de um painel do próprio servidor.
pub fn attach(
    bin: &Path,
    socket: Option<&str>,
    session: &str,
    nested: bool,
) -> Result<bool, String> {
    let mut command = Command::new(bin);
    if let Some(socket) = socket {
        command.arg("-S").arg(socket);
    }
    command.args(["attach-session", "-t", session]);
    if nested {
        command.env_remove("TMUX");
    }
    command
        .status()
        .map(|status| status.success())
        .map_err(|error| format!("não consegui rodar o tmux: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(fields: &[&str]) -> String {
        fields.join(&SEP.to_string())
    }

    #[test]
    fn one_pane_per_line_rebuilds_the_whole_tree() {
        let output = [
            line(&[
                "work",
                "$1",
                "1700000000",
                "1",
                "2",
                "/home/u",
                "0",
                "editor",
                "1",
                "2",
                "0",
                "%0",
                "1",
                "111",
                "80",
                "24",
                "nvim",
                "/home/u/src",
            ]),
            line(&[
                "work",
                "$1",
                "1700000000",
                "1",
                "2",
                "/home/u",
                "0",
                "editor",
                "1",
                "2",
                "1",
                "%1",
                "0",
                "112",
                "80",
                "24",
                "bash",
                "/home/u",
            ]),
            line(&[
                "work",
                "$1",
                "1700000000",
                "1",
                "2",
                "/home/u",
                "1",
                "logs",
                "0",
                "1",
                "0",
                "%2",
                "1",
                "113",
                "80",
                "24",
                "tail",
                "/var/log",
            ]),
        ]
        .join("\n");

        let snapshot = parse(&output);
        assert_eq!(snapshot.sessions.len(), 1, "a sessão aparece uma vez só");
        assert_eq!(snapshot.windows.len(), 2, "duas janelas, sem repetição");
        assert_eq!(snapshot.panes.len(), 3);
        assert_eq!(snapshot.sessions[0].attached, 1);
        assert_eq!(snapshot.panes_of("work", 0).count(), 2);
    }

    #[test]
    fn a_name_with_a_pipe_in_it_survives() {
        // Por isto o separador não é `|`: um nome de janela é texto livre, e uma barra
        // vertical dentro dele partiria a linha num campo a mais.
        let output = line(&[
            "a|b", "$2", "0", "0", "1", "/", "0", "x|y", "1", "1", "0", "%0", "1", "1", "80", "24",
            "bash", "/tmp/a|b",
        ]);
        let snapshot = parse(&output);
        assert_eq!(snapshot.sessions[0].name, "a|b");
        assert_eq!(snapshot.windows[0].name, "x|y");
        assert_eq!(snapshot.panes[0].path, "/tmp/a|b");
    }

    #[test]
    fn a_tab_inside_a_value_arrives_already_escaped_and_does_not_split_the_line() {
        // O que o tmux de fato devolve para uma janela chamada com uma tabulação dentro:
        // quatro caracteres visíveis, `\` `t` entre o `a` e o `b`. A tabulação de verdade
        // só existe onde nós a pusemos. É esta diferença que faz o separador funcionar, e
        // é por isso que ela está escrita num teste e não só num comentário.
        let output = line(&[
            "s", "$1", "0", "0", "1", "/", "0", r"a\tb", "1", "1", "0", "%0", "1", "1", "80", "24",
            "bash", "/tmp",
        ]);
        let snapshot = parse(&output);
        assert_eq!(snapshot.windows.len(), 1);
        assert_eq!(snapshot.windows[0].name, r"a\tb");
        assert_eq!(snapshot.panes[0].command, "bash");
    }

    #[test]
    fn a_dead_server_is_an_empty_list_not_a_failure() {
        assert!(no_server("no server running on /tmp/tmux-1000/default"));
        assert!(!no_server("duplicate session: work"));
    }
}
