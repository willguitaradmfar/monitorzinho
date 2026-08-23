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
