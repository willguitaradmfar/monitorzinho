//! Following a file the way `tail -f` does, into the same log every other execution
//! here writes to.
//!
//! The reason this is a tool and not a suggestion to open another terminal is the
//! viewer: search as you type, jump between hits, hide everything that doesn't match,
//! read it as hex when the file isn't text, scroll back through thousands of lines that
//! are still there an hour later. `tail -f | grep` gives you one of those and takes the
//! rest away.
//!
//! Rotation is handled, because a log worth following is a log something rotates out
//! from under you. The file is checked by identity — device and inode, not name — so a
//! `logrotate` that renames the file and creates a fresh one is noticed and the new one
//! is picked up from its start, with a line in the log saying so rather than the output
//! silently stopping.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;

use super::{EventKind, Execution, ParamSpec, Recorder, Tool};

/// How often the file is checked for new bytes. A log is read by a person, and a
/// quarter of a second is under what anyone notices.
const POLL: Duration = Duration::from_millis(250);
/// Lines shown from the end of the file when the execution starts, so it opens with
/// context instead of with an empty screen and a promise.
const TAIL_LINES: usize = 200;
/// A single line longer than this is truncated in the log. Some programs write a whole
/// JSON document per line; the viewer is not the place to hold a megabyte of it.
const MAX_LINE: usize = 8 * 1024;

const FROM: &[&str] = &["fim do arquivo", "começo do arquivo"];

/// Como cada linha do arquivo deve ser lida.
///
/// «Como está» é o padrão e é o que um arquivo de log costuma ser. A outra opção existe
/// porque uma família inteira de programas escreve um documento JSON por linha — os
/// runtimes de container entre eles — e aí a mensagem que interessa está enterrada num
/// envelope. Mostrar o envelope é mostrar a verdade do arquivo, e é ilegível.
const SHAPE: &[&str] = &["como está", "JSON por linha"];

/// Os campos que carregam a mensagem num JSON por linha, na ordem em que são procurados.
/// Cobrem o que os runtimes de container e as bibliotecas de log mais usadas escrevem.
const MESSAGE_FIELDS: [&str; 4] = ["log", "message", "msg", "text"];

pub struct TailTool;

impl Tool for TailTool {
    fn id(&self) -> &'static str {
        "tail"
    }

    fn name(&self) -> &'static str {
        "Seguir arquivo"
    }

    fn description(&self) -> &'static str {
        "Acompanha um arquivo linha a linha, com busca, filtro e rolagem — e sobrevive a quem rotaciona o arquivo"
    }

    fn params(&self) -> Vec<ParamSpec> {
        vec![
            ParamSpec::text(
                "caminho",
                "Arquivo",
                "/var/log/syslog",
                "Caminho do arquivo a seguir. Precisa existir e ser legível por você",
            ),
            ParamSpec::choice(
                "inicio",
                "Começar do",
                FROM,
                "«fim» mostra as últimas linhas e segue daí; «começo» lê o arquivo inteiro antes de seguir",
            ),
            ParamSpec::choice(
                "formato",
                "Formato das linhas",
                SHAPE,
                "«JSON por linha» mostra só a mensagem de dentro do envelope — é o formato que os runtimes de container escrevem",
            ),
            ParamSpec::text(
                "contendo",
                "Só linhas contendo",
                "",
                "Vazio segue tudo. Preenchido, só as linhas com esse texto entram no log — filtro na origem, diferente da busca do visualizador",
            ),
        ]
    }

    fn summarize(&self, params: &HashMap<&'static str, String>) -> String {
        let get = |key| params.get(key).map(String::as_str).unwrap_or("").trim();
        let filter = match get("contendo") {
            "" => String::new(),
            text => format!("  ·  contendo «{text}»"),
        };
        format!("{}{filter}", get("caminho"))
    }

    fn columns(&self, execution: &Execution) -> (String, String) {
        let stats = &execution.stats;
        let lines = stats.connections.load(Ordering::Relaxed);
        let noun = if lines == 1 { "linha" } else { "linhas" };
        (
            format!("{lines} {noun}"),
            format!(
                "{} lidos",
                crate::format::human_bytes(stats.to_target.load(Ordering::Relaxed) as f64)
            ),
        )
    }

    fn start(&self, id: u64, params: &HashMap<&'static str, String>) -> Result<Execution, String> {
        let get = |key| params.get(key).map(String::as_str).unwrap_or("").trim();
        let path = PathBuf::from(get("caminho"));
        if get("caminho").is_empty() {
            return Err("informe o caminho do arquivo".to_string());
        }
        // Opened here, in front of the person who typed the path: a file that doesn't
        // exist or that we can't read is a mistake to fix on the form, not a thread
        // that dies quietly two seconds later.
        let file =
            File::open(&path).map_err(|e| format!("não consegui abrir {}: {e}", path.display()))?;
        let metadata = file
            .metadata()
            .map_err(|e| format!("não consegui ler {}: {e}", path.display()))?;
        if metadata.is_dir() {
            return Err(format!("{} é um diretório", path.display()));
        }

        let plan = Plan {
            path,
            from_start: get("inicio") == FROM[1],
            filter: get("contendo").to_string(),
            json: get("formato") == SHAPE[1],
        };
        let (execution, recorder) = Execution::new(id, self.name(), self.summarize(params));
        let finished = execution.finish_flag();
        thread::spawn(move || {
            follow(plan, file, &recorder);
            finished.store(true, Ordering::Relaxed);
        });
        Ok(execution)
    }
}

struct Plan {
    path: PathBuf,
    from_start: bool,
    filter: String,
    /// Se cada linha é um documento JSON do qual só a mensagem interessa.
    json: bool,
}

/// Device and inode: what makes a file *that* file, whatever it is called at the
/// moment. Comparing names would miss the rename that rotation is.
#[derive(PartialEq, Eq, Clone, Copy)]
struct Identity(u64, u64);

fn identity(path: &Path) -> Option<Identity> {
    let metadata = std::fs::metadata(path).ok()?;
    Some(Identity(metadata.dev(), metadata.ino()))
}

fn follow(plan: Plan, file: File, rec: &Recorder) {
    rec.record(
        0,
        EventKind::Note(format!(
            "seguindo {}{}",
            plan.path.display(),
            match plan.filter.as_str() {
                "" => String::new(),
                text => format!(" — só linhas com «{text}»"),
            }
        )),
    );

    let mut reader = BufReader::new(file);
    if plan.from_start {
        // Already at the start; nothing to do but read it all.
    } else if let Err(e) = seek_to_tail(&mut reader) {
        rec.record(
            0,
            EventKind::Error(format!("não consegui posicionar no fim: {e}")),
        );
    }

    let mut current = identity(&plan.path);
    loop {
        if rec.stopping() {
            break;
        }
        let read = drain(&mut reader, &plan, rec);
        if read == 0 {
            // Nothing new. Rotation is only worth checking for when the file has gone
            // quiet, which is exactly when it happens.
            match identity(&plan.path) {
                fresh if fresh != current && fresh.is_some() => {
                    rec.record(
                        0,
                        EventKind::Note(
                            "o arquivo foi rotacionado — seguindo o novo desde o começo"
                                .to_string(),
                        ),
                    );
                    match File::open(&plan.path) {
                        Ok(file) => {
                            reader = BufReader::new(file);
                            current = fresh;
                        }
                        Err(e) => rec.record(
                            0,
                            EventKind::Error(format!("não consegui reabrir o arquivo: {e}")),
                        ),
                    }
                }
                _ => thread::sleep(POLL),
            }
        }
    }
    rec.record(0, EventKind::Note("parou de seguir".to_string()));
}

/// Moves to where the last `TAIL_LINES` lines begin, by walking back from the end in
/// blocks and counting newlines — the same thing `tail` does, and the reason it doesn't
/// have to read a two-gigabyte file to show ten lines of it.
fn seek_to_tail(reader: &mut BufReader<File>) -> std::io::Result<()> {
    const BLOCK: i64 = 64 * 1024;
    let end = reader.seek(SeekFrom::End(0))? as i64;
    let mut position = end;
    let mut newlines = 0usize;
    let mut buffer = vec![0u8; BLOCK as usize];

    while position > 0 && newlines <= TAIL_LINES {
        let step = BLOCK.min(position);
        position -= step;
        reader.seek(SeekFrom::Start(position as u64))?;
        let slice = &mut buffer[..step as usize];
        std::io::Read::read_exact(reader, slice)?;
        newlines += slice.iter().filter(|byte| **byte == b'\n').count();
        if newlines > TAIL_LINES {
            // Walk forward over the newlines we overshot, so the first line shown is a
            // whole line rather than the tail of one.
            let mut extra = newlines - TAIL_LINES;
            let offset = slice
                .iter()
                .position(|byte| {
                    if *byte == b'\n' {
                        extra -= 1;
                    }
                    *byte == b'\n' && extra == 0
                })
                .map(|at| at + 1)
                .unwrap_or(0);
            position += offset as i64;
        }
    }
    reader.seek(SeekFrom::Start(position.max(0) as u64))?;
    Ok(())
}

/// Reads every complete line available right now, returning how many bytes were taken.
/// A trailing partial line is left in the file for the next round: half a line in the
/// log would be a line the reader has to mentally repair.
/// A mensagem de dentro de uma linha JSON, ou a linha inteira quando ela não é JSON.
///
/// Cair de volta na linha crua é deliberado: um arquivo que mistura formatos, ou uma
/// linha truncada no meio de uma escrita, continua legível em vez de virar um buraco. E
/// o carimbo de tempo vem junto quando existe, porque num log de container ele é a
/// única coisa que diz quando a linha aconteceu.
fn unwrap_json(line: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
        return line.to_string();
    };
    let Some(message) = MESSAGE_FIELDS
        .iter()
        .find_map(|field| value.get(field)?.as_str())
    else {
        return line.to_string();
    };
    let message = message.trim_end();
    // Só a hora do carimbo, e não o instante inteiro em nanossegundos: trinta caracteres
    // de precisão que ninguém lê empurrariam a mensagem para fora da tela. Vale a pena
    // manter porque um programa que não carimba as próprias linhas não tem outra fonte
    // de «quando» — o relógio do visualizador conta desde que a execução começou, o que
    // não diz nada sobre uma linha escrita ontem.
    match value
        .get("time")
        .or_else(|| value.get("timestamp"))
        .and_then(|time| time.as_str())
        .and_then(clock_of)
    {
        Some(clock) => format!("{clock} {message}"),
        None => message.to_string(),
    }
}

/// `2026-08-22T14:46:02.498335304Z` → `14:46:02`.
fn clock_of(stamp: &str) -> Option<&str> {
    let time = stamp.split('T').nth(1)?;
    (time.len() >= 8).then(|| &time[..8])
}

/// Tira as sequências de escape ANSI de uma linha.
///
/// O visualizador não pinta cor: são células de um `ratatui`, não um terminal cru. Deixar
/// os bytes passar não colore nada — só deixa o esqueleto da sequência (`[38;5;160m`) no
/// meio da frase, que é pior que não ter cor nenhuma. Um log de container vem colorido
/// com muito mais frequência que um arquivo de texto qualquer, e foi ali que apareceu.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        // CSI: ESC [ …parâmetros… letra-final. Qualquer outra sequência de escape é de
        // dois caracteres, e o segundo vai embora com ela.
        match chars.next() {
            Some('[') => {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() || c == '~' {
                        break;
                    }
                }
            }
            _ => continue,
        }
    }
    out
}

fn drain(reader: &mut BufReader<File>, plan: &Plan, rec: &Recorder) -> usize {
    let mut taken = 0usize;
    loop {
        if rec.stopping() {
            return taken;
        }
        let mut line = Vec::new();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) => return taken,
            Ok(read) => {
                if !line.ends_with(b"\n") {
                    // Incomplete: rewind so it's read whole next time round.
                    let _ = reader.seek(SeekFrom::Current(-(read as i64)));
                    return taken;
                }
                taken += read;
                let text = String::from_utf8_lossy(&line);
                // Desembrulhado antes do filtro: quem digitou «erro» quer procurar na
                // mensagem, não no envelope que a carrega.
                let text = if plan.json {
                    unwrap_json(&text)
                } else {
                    text.into_owned()
                };
                let text = strip_ansi(&text);
                if plan.filter.is_empty() || crate::format::contains_ci(&text, &plan.filter) {
                    rec.stats.connections.fetch_add(1, Ordering::Relaxed);
                    rec.stats
                        .to_target
                        .fetch_add(read as u64, Ordering::Relaxed);
                    // One line of the file is one line of the log. A relay records
                    // chunks, and a chunk deserves the size header the viewer puts above
                    // it; a line of text is not a chunk, and two rows per line would
                    // turn a screenful of log into half a screenful.
                    let mut text = text.trim_end().to_string();
                    text.truncate(MAX_LINE);
                    rec.record(0, EventKind::Note(text));
                }
            }
            Err(e) => {
                rec.record(0, EventKind::Error(format!("erro ao ler: {e}")));
                return taken;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unwraps_a_container_log_line() {
        let line =
            r#"{"log":"iniciando\n","stream":"stdout","time":"2026-08-22T14:46:02.498335304Z"}"#;
        assert_eq!(unwrap_json(line), "14:46:02 iniciando");
    }

    #[test]
    fn a_line_that_is_not_json_survives_whole() {
        // Um arquivo que mistura formatos, ou uma linha truncada no meio de uma escrita,
        // continua legível em vez de virar um buraco.
        assert_eq!(unwrap_json("apenas texto"), "apenas texto");
        assert_eq!(
            unwrap_json(r#"{"sem":"mensagem"}"#),
            r#"{"sem":"mensagem"}"#
        );
    }

    #[test]
    fn other_message_fields_are_understood() {
        assert_eq!(unwrap_json(r#"{"msg":"oi"}"#), "oi");
        assert_eq!(unwrap_json(r#"{"message":"oi"}"#), "oi");
    }

    #[test]
    fn ansi_sequences_leave_no_skeleton_behind() {
        assert_eq!(strip_ansi("\u{1b}[38;5;160merro\u{1b}[0m"), "erro");
        assert_eq!(strip_ansi("sem cor"), "sem cor");
        // Uma sequência cortada no fim do buffer não pode levar a linha junto.
        assert_eq!(strip_ansi("fim\u{1b}["), "fim");
    }
}
