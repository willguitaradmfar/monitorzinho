//! Remembering which executions were running, so a restart picks them back up.
//!
//! Only the *configuration* is stored — which tool, and what it was given. Logs and
//! counters are deliberately not: they describe one run, and restoring them alongside a
//! freshly started thread would present stale traffic as if it were live.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::db;

/// Everything needed to recreate one execution. `BTreeMap` so the parameter order is
/// stable between writes instead of shuffling with the hash seed on every save.
#[derive(Clone, Serialize, Deserialize)]
pub struct ExecutionSpec {
    /// `Tool::id`, not its display name — the name is free to change, this isn't.
    pub tool: String,
    pub params: BTreeMap<String, String>,
    /// Whether it should be running. Switched off stays switched off across restarts —
    /// the row is kept on purpose, and coming back up doing the very thing somebody
    /// turned off would be the opposite of what they asked for.
    ///
    /// Defaulted, so a file written by an older version reads as "on", which is what
    /// every execution in it was.
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
}

fn enabled_by_default() -> bool {
    true
}

/// The saved executions, or nothing at all if they can't be read — a broken row costs
/// the saved list, never the app starting.
pub fn load() -> Vec<ExecutionSpec> {
    db::ler(Vec::new(), |conn| {
        let mut execucoes =
            conn.prepare("SELECT id, ferramenta, ligada FROM execucao ORDER BY id")?;
        let cabecas: Vec<(i64, String, bool)> = execucoes
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get::<_, i64>(2)? != 0))
            })?
            .collect::<rusqlite::Result<_>>()?;

        let mut params =
            conn.prepare("SELECT chave, valor FROM execucao_param WHERE execucao_id = ?1")?;
        let mut out = Vec::with_capacity(cabecas.len());
        for (id, tool, enabled) in cabecas {
            let pares: BTreeMap<String, String> = params
                .query_map([id], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            out.push(ExecutionSpec {
                tool,
                params: pares,
                enabled,
            });
        }
        Ok(out)
    })
}

/// The whole list, rewritten. `id` carries the order the tab shows them in, and the
/// parameters go with it: `ON DELETE CASCADE` means clearing the parent table can't
/// leave a row of parameters behind pointing at an execution that no longer exists.
pub fn save(specs: &[ExecutionSpec]) {
    db::escrever(|conn| {
        db::limpar(conn, "execucao")?;
        db::limpar(conn, "execucao_param")?;
        let mut cabeca =
            conn.prepare("INSERT INTO execucao (id, ferramenta, ligada) VALUES (?1, ?2, ?3)")?;
        let mut param = conn.prepare(
            "INSERT INTO execucao_param (execucao_id, chave, valor) VALUES (?1, ?2, ?3)",
        )?;
        for (i, spec) in specs.iter().enumerate() {
            let id = i as i64 + 1;
            cabeca.execute((id, &spec.tool, spec.enabled as i64))?;
            for (chave, valor) in &spec.params {
                param.execute((id, chave, valor))?;
            }
        }
        Ok(())
    });
}
