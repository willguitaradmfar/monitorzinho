//! Marks: keeping an eye on something in a list that keeps reordering itself.
//!
//! Every table here re-ranks on every tick, which is right — the busiest process should
//! be at the top — and makes following one particular row impossible. You find the
//! connection you care about, look away, and it has moved. A mark pins it *visually*:
//! the row keeps whatever position the ranking gives it, and wears a star wherever it
//! lands.
//!
//! What a mark can be about differs by table, and the difference is the point. A port is
//! a number, a process is a command line, a session is a person: matching all three with
//! the same "type a regex" would work and would be worse, because a mark is something
//! you set in a hurry, from the row already under the cursor, and the right default
//! matters more than the general case.
//!
//! Marks are per machine and survive restarts — the whole reason they exist is to
//! outlast the list.

use std::fs;

use serde::{Deserialize, Serialize};

use super::TableRow;
use crate::history;

/// One thing a table can be asked to watch for.
///
/// `column` is which cell the value is tested against and `numeric` how: a port is
/// compared as a number against every number in the cell — so `443` matches the port
/// column and not the `4433` beside it — while everything else is a substring, or a
/// regular expression when the value looks like one.
pub struct MarkKind {
    pub name: &'static str,
    pub column: usize,
    pub numeric: bool,
    pub help: &'static str,
}

/// A saved mark. `table` is `TableMonitor::id`, so renaming a panel's title never
/// orphans what somebody asked to watch.
#[derive(Clone, Serialize, Deserialize)]
pub struct Mark {
    pub table: String,
    pub kind: String,
    pub value: String,
    /// For a tree: whether the children of a matching row are marked with it. A build
    /// that matters is a build plus everything it spawned.
    #[serde(default)]
    pub subtree: bool,
}

impl Mark {
    /// Whether this mark is about `row` of `table`.
    pub fn matches(&self, table: &str, kinds: &[MarkKind], row: &TableRow) -> bool {
        if self.table != table || self.value.trim().is_empty() {
            return false;
        }
        let Some(kind) = kinds.iter().find(|kind| kind.name == self.kind) else {
            return false;
        };
        let Some(cell) = row.cells.get(kind.column) else {
            return false;
        };
        if kind.numeric {
            return numbers_in(cell).any(|number| number == self.value.trim());
        }
        matches_text(cell, &self.value)
    }
}

/// Every run of digits in a cell, so a number is compared as a number: the port column
/// of `127.0.0.1:8080 → 10.0.0.5:443` holds four of them and `443` is one.
fn numbers_in(cell: &str) -> impl Iterator<Item = &str> {
    cell.split(|c: char| !c.is_ascii_digit())
        .filter(|part| !part.is_empty())
}

/// Substring, case-insensitive — or a regular expression, when the value contains
/// something only a regular expression would mean. Typing `postgres` should not require
/// knowing what a regex is, and typing `^ssh|^sshd$` should not be taken literally.
fn matches_text(cell: &str, value: &str) -> bool {
    let value = value.trim();
    if value.chars().any(|c| "^$*+?[]()|\\".contains(c))
        && let Ok(regex) = regex::Regex::new(value)
    {
        return regex.is_match(cell);
    }
    crate::format::contains_ci(cell, value)
}

/// The marks for this machine.
#[derive(Default)]
pub struct Marks {
    all: Vec<Mark>,
}

impl Marks {
    pub fn load() -> Self {
        let all = match fs::read_to_string(path()) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
            Err(_) => Vec::new(),
        };
        Self { all }
    }

    fn save(&self) {
        if let Ok(content) = serde_json::to_string_pretty(&self.all) {
            let _ = fs::write(path(), content);
        }
    }

    /// Adds a mark, replacing any identical one so marking the same thing twice is not
    /// two entries.
    pub fn add(&mut self, mark: Mark) {
        self.all.retain(|known| {
            !(known.table == mark.table && known.kind == mark.kind && known.value == mark.value)
        });
        self.all.push(mark);
        self.save();
    }

    /// Drops every mark that matches `row`, which is what pressing the key again on a
    /// marked row means.
    pub fn remove_matching(&mut self, table: &str, kinds: &[MarkKind], row: &TableRow) {
        let before = self.all.len();
        self.all.retain(|mark| !mark.matches(table, kinds, row));
        if self.all.len() != before {
            self.save();
        }
    }

    /// Whether any mark is about this row, and whether one of them extends to its
    /// children.
    pub fn hit(&self, table: &str, kinds: &[MarkKind], row: &TableRow) -> Option<bool> {
        let mut found = false;
        let mut subtree = false;
        for mark in &self.all {
            if mark.matches(table, kinds, row) {
                found = true;
                subtree |= mark.subtree;
            }
        }
        found.then_some(subtree)
    }

    /// Applies the marks to a freshly sampled set of rows: each row that matches, plus
    /// the descendants of any match that asked for its subtree.
    pub fn apply(&self, table: &str, kinds: &[MarkKind], rows: &mut [TableRow]) {
        if self.all.is_empty() || kinds.is_empty() {
            return;
        }
        let mut inherited: Vec<u32> = Vec::new();
        for row in rows.iter() {
            if let Some(true) = self.hit(table, kinds, row) {
                inherited.extend(row.descendant_pids.iter().copied());
            }
        }
        for row in rows.iter_mut() {
            row.marked = self.hit(table, kinds, row).is_some()
                || (row.pid != 0 && inherited.contains(&row.pid));
        }
    }
}

fn path() -> std::path::PathBuf {
    history::data_file("marks.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(cells: &[&str]) -> TableRow {
        TableRow::leaf(cells.iter().map(|c| c.to_string()).collect(), 0)
    }

    const KINDS: &[MarkKind] = &[
        MarkKind {
            name: "comando",
            column: 0,
            numeric: false,
            help: "",
        },
        MarkKind {
            name: "porta",
            column: 1,
            numeric: true,
            help: "",
        },
    ];

    #[test]
    fn text_marks_are_substrings_unless_they_look_like_patterns() {
        let mark = Mark {
            table: "t".into(),
            kind: "comando".into(),
            value: "rootlesskit".into(),
            subtree: false,
        };
        assert!(mark.matches("t", KINDS, &row(&["rootlesskit --state-dir=/run", "0"])));
        assert!(!mark.matches("t", KINDS, &row(&["dockerd", "0"])));
        // A different table's mark never applies, however well it matches.
        assert!(!mark.matches("outra", KINDS, &row(&["rootlesskit", "0"])));

        let pattern = Mark {
            table: "t".into(),
            kind: "comando".into(),
            value: "^ssh(d)?$".into(),
            subtree: false,
        };
        assert!(pattern.matches("t", KINDS, &row(&["sshd", "0"])));
        assert!(!pattern.matches("t", KINDS, &row(&["ssh-agent", "0"])));
    }

    #[test]
    fn a_port_is_a_number_and_not_a_substring() {
        let mark = Mark {
            table: "t".into(),
            kind: "porta".into(),
            value: "443".into(),
            subtree: false,
        };
        assert!(mark.matches("t", KINDS, &row(&["nginx", "443"])));
        // The whole point of comparing numbers: 4433 contains "443" and is not it.
        assert!(!mark.matches("t", KINDS, &row(&["nginx", "4433"])));
        // And a port inside an address is still that port.
        assert!(mark.matches("t", KINDS, &row(&["nginx", "10.0.0.5:443 → 1.1.1.1:80"])));
    }

    #[test]
    fn a_subtree_mark_reaches_the_children() {
        let mut marks = Marks::default();
        marks.all.push(Mark {
            table: "t".into(),
            kind: "comando".into(),
            value: "make".into(),
            subtree: true,
        });
        let mut parent = row(&["make -j8", "0"]);
        parent.pid = 10;
        parent.descendant_pids = vec![11, 12];
        let mut child = row(&["cc1plus", "0"]);
        child.pid = 11;
        let mut stranger = row(&["firefox", "0"]);
        stranger.pid = 99;

        let mut rows = vec![parent, child, stranger];
        marks.apply("t", KINDS, &mut rows);
        assert!(rows[0].marked, "o próprio processo");
        assert!(rows[1].marked, "o filho, por herança");
        assert!(!rows[2].marked, "quem não tem nada a ver");
    }
}
