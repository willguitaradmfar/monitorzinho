use std::collections::{HashMap, VecDeque};

use crate::db;

// Sized to still fill a wide fullscreened panel (each sample is one column) rather than
// just the overview grid's narrower one-of-three-columns panels.
pub const CAPACITY: usize = 300;

pub type HistoryMap = HashMap<String, Vec<f64>>;

#[derive(Debug, Clone)]
pub struct History {
    buf: VecDeque<f64>,
    capacity: usize,
}

impl History {
    pub fn new(capacity: usize) -> Self {
        Self {
            buf: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    pub fn from_saved(values: Vec<f64>, capacity: usize) -> Self {
        let mut buf: VecDeque<f64> = values.into_iter().collect();
        while buf.len() > capacity {
            buf.pop_front();
        }
        Self { buf, capacity }
    }

    pub fn push(&mut self, value: f64) {
        if self.buf.len() == self.capacity {
            self.buf.pop_front();
        }
        self.buf.push_back(value);
    }

    pub fn last(&self) -> Option<f64> {
        self.buf.back().copied()
    }

    /// Peak value within the currently retained window (not all-time — old samples
    /// fall off as the ring buffer fills).
    pub fn max(&self) -> Option<f64> {
        self.buf.iter().copied().fold(None, |acc, v| match acc {
            Some(m) => Some(v.max(m)),
            None => Some(v),
        })
    }

    pub fn values(&self) -> Vec<f64> {
        self.buf.iter().copied().collect()
    }
}

/// The saved series, as they were left. A series the current build no longer draws
/// stays in the table on purpose — downgrading and coming back shouldn't have thrown
/// away the history of a panel that still exists in the other version.
pub fn load_all() -> HistoryMap {
    db::ler(HistoryMap::new(), |conn| {
        let mut stmt = conn.prepare("SELECT serie, valor FROM historico ORDER BY serie, pos")?;
        let mut linhas = stmt.query([])?;
        let mut map = HistoryMap::new();
        while let Some(linha) = linhas.next()? {
            let serie: String = linha.get(0)?;
            let valor: f64 = linha.get(1)?;
            map.entry(serie).or_default().push(valor);
        }
        Ok(map)
    })
}

/// One row per sample, the whole table rewritten. It is a few thousand rows every ten
/// seconds inside one transaction — a millisecond or two — and what it buys is that
/// `SELECT` sees the same shape the panels do, instead of an opaque array of numbers.
pub fn save_all(map: &HistoryMap) {
    db::escrever(|conn| {
        db::limpar(conn, "historico")?;
        let mut stmt =
            conn.prepare("INSERT INTO historico (serie, pos, valor) VALUES (?1, ?2, ?3)")?;
        for (serie, valores) in map {
            for (pos, valor) in valores.iter().enumerate() {
                // A NaN in a chart is a gap, and SQLite has no NaN: it would come back
                // as NULL and fail the read of every series after it.
                if !valor.is_finite() {
                    continue;
                }
                stmt.execute((serie, pos as i64, *valor))?;
            }
        }
        Ok(())
    });
}
