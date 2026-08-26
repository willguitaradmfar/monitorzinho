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

/// What colour a mark wears. Named rather than a raw RGB triple so the file stays
/// readable and so the terminal's own rendering stays the UI's business — `mark` knows
/// which marks are which, `ui` knows what each one looks like.
///
/// Colour is what makes several marks at once useful: with one star everywhere, a list
/// with four things followed in it says "four of these matter" and nothing more. With a
/// colour each, it says which is which without reading a single cell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MarkColor {
    /// The colour marks had before there was a choice — so a file written by an older
    /// version, whose marks have no colour at all, reads back exactly as it looked.
    #[default]
    Amarelo,
    Verde,
    Ciano,
    Azul,
    Roxo,
    Laranja,
    Vermelho,
}

impl MarkColor {
    pub const ALL: [MarkColor; 7] = [
        MarkColor::Amarelo,
        MarkColor::Verde,
        MarkColor::Ciano,
        MarkColor::Azul,
        MarkColor::Roxo,
        MarkColor::Laranja,
        MarkColor::Vermelho,
    ];

    pub fn label(self) -> &'static str {
        match self {
            MarkColor::Amarelo => "amarelo",
            MarkColor::Verde => "verde",
            MarkColor::Ciano => "ciano",
            MarkColor::Azul => "azul",
            MarkColor::Roxo => "roxo",
            MarkColor::Laranja => "laranja",
            MarkColor::Vermelho => "vermelho",
        }
    }

    /// The next colour along, wrapping — what ←/→ do on the colour field.
    pub fn cycled(self, delta: i32) -> Self {
        let count = Self::ALL.len() as i32;
        let index = Self::ALL.iter().position(|c| *c == self).unwrap_or(0) as i32;
        Self::ALL[(index + delta).rem_euclid(count) as usize]
    }
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
    /// Defaulted rather than required: every mark saved before colours existed keeps
    /// working, and reads back as the yellow it already was.
    #[serde(default)]
    pub color: MarkColor,
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

/// What the marks have to say about one row: which colour it wears, and whether the
/// mark that put it there reaches its children too.
#[derive(Clone, Copy)]
pub struct Hit {
    pub color: MarkColor,
    pub subtree: bool,
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
        // Never from a test. The list operations below are worth testing and every one
        // of them saves; writing over the marks of whoever is running `cargo test` is
        // not part of what they're checking.
        if cfg!(test) {
            return;
        }
        if let Ok(content) = serde_json::to_string_pretty(&self.all) {
            let _ = fs::write(path(), content);
        }
    }

    /// Every mark, in the order they were written — which is the order the list screen
    /// shows them in.
    pub fn all(&self) -> &[Mark] {
        &self.all
    }

    /// Adds a mark, replacing any identical one so marking the same thing twice is not
    /// two entries.
    pub fn add(&mut self, mark: Mark) {
        self.all.retain(|known| !known.same_subject(&mark));
        self.all.push(mark);
        self.save();
    }

    /// Rewrites the mark at `index` — what the list screen's editor saves. Any *other*
    /// mark that the edit turned into a duplicate goes, the same way `add` dedups.
    pub fn replace(&mut self, index: usize, mark: Mark) {
        if index >= self.all.len() {
            return;
        }
        self.all[index] = mark;
        let mut position = 0;
        let subject = self.all[index].clone();
        self.all.retain(|known| {
            let keep = position == index || !known.same_subject(&subject);
            position += 1;
            keep
        });
        self.save();
    }

    /// Drops one mark by position, which is what Del on the list screen means.
    pub fn remove(&mut self, index: usize) {
        if index < self.all.len() {
            self.all.remove(index);
            self.save();
        }
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

    /// What the marks say about this row, or `None` where none of them is about it.
    /// Where several match, the colour is the first one's — the list screen shows that
    /// order, so the tie is broken by something the user can see and reorder.
    pub fn hit(&self, table: &str, kinds: &[MarkKind], row: &TableRow) -> Option<Hit> {
        let mut hit: Option<Hit> = None;
        for mark in &self.all {
            if mark.matches(table, kinds, row) {
                match &mut hit {
                    Some(hit) => hit.subtree |= mark.subtree,
                    none => {
                        *none = Some(Hit {
                            color: mark.color,
                            subtree: mark.subtree,
                        });
                    }
                }
            }
        }
        hit
    }

    /// Applies the marks to a freshly sampled set of rows: each row that matches, plus
    /// the descendants of any match that asked for its subtree — those in the colour of
    /// the mark they were inherited from, so a build and everything it spawned read as
    /// one thing.
    pub fn apply(&self, table: &str, kinds: &[MarkKind], rows: &mut [TableRow]) {
        if self.all.is_empty() || kinds.is_empty() {
            for row in rows.iter_mut() {
                row.mark = None;
            }
            return;
        }
        let mut inherited: Vec<(u32, MarkColor)> = Vec::new();
        for row in rows.iter() {
            if let Some(hit) = self.hit(table, kinds, row)
                && hit.subtree
            {
                inherited.extend(row.descendant_pids.iter().map(|pid| (*pid, hit.color)));
            }
        }
        for row in rows.iter_mut() {
            row.mark = self
                .hit(table, kinds, row)
                .map(|hit| hit.color)
                .or_else(|| {
                    (row.pid != 0)
                        .then(|| {
                            inherited
                                .iter()
                                .find(|(pid, _)| *pid == row.pid)
                                .map(|(_, color)| *color)
                        })
                        .flatten()
                });
        }
    }
}

impl Mark {
    /// Whether two marks are about the same thing — same table, same kind, same value.
    /// Colour and subtree are how a mark is shown and how far it reaches, not what it
    /// is about, so changing either edits a mark instead of adding one.
    fn same_subject(&self, other: &Mark) -> bool {
        self.table == other.table && self.kind == other.kind && self.value == other.value
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

    fn watching(kind: &str, value: &str) -> Mark {
        Mark {
            table: "t".into(),
            kind: kind.into(),
            value: value.into(),
            subtree: false,
            color: MarkColor::Amarelo,
        }
    }

    #[test]
    fn text_marks_are_substrings_unless_they_look_like_patterns() {
        let mark = watching("comando", "rootlesskit");
        assert!(mark.matches("t", KINDS, &row(&["rootlesskit --state-dir=/run", "0"])));
        assert!(!mark.matches("t", KINDS, &row(&["dockerd", "0"])));
        // A different table's mark never applies, however well it matches.
        assert!(!mark.matches("outra", KINDS, &row(&["rootlesskit", "0"])));

        let pattern = watching("comando", "^ssh(d)?$");
        assert!(pattern.matches("t", KINDS, &row(&["sshd", "0"])));
        assert!(!pattern.matches("t", KINDS, &row(&["ssh-agent", "0"])));
    }

    #[test]
    fn a_port_is_a_number_and_not_a_substring() {
        let mark = watching("porta", "443");
        assert!(mark.matches("t", KINDS, &row(&["nginx", "443"])));
        // The whole point of comparing numbers: 4433 contains "443" and is not it.
        assert!(!mark.matches("t", KINDS, &row(&["nginx", "4433"])));
        // And a port inside an address is still that port.
        assert!(mark.matches("t", KINDS, &row(&["nginx", "10.0.0.5:443 → 1.1.1.1:80"])));
    }

    #[test]
    fn a_subtree_mark_reaches_the_children() {
        let mut marks = Marks::default();
        let mut make = watching("comando", "make");
        make.subtree = true;
        make.color = MarkColor::Verde;
        marks.all.push(make);
        let mut parent = row(&["make -j8", "0"]);
        parent.pid = 10;
        parent.descendant_pids = vec![11, 12];
        let mut child = row(&["cc1plus", "0"]);
        child.pid = 11;
        let mut stranger = row(&["firefox", "0"]);
        stranger.pid = 99;

        let mut rows = vec![parent, child, stranger];
        marks.apply("t", KINDS, &mut rows);
        assert_eq!(rows[0].mark, Some(MarkColor::Verde), "o próprio processo");
        // The child wears the colour it inherited, not the default one.
        assert_eq!(rows[1].mark, Some(MarkColor::Verde), "o filho, por herança");
        assert_eq!(rows[2].mark, None, "quem não tem nada a ver");
    }

    #[test]
    fn each_mark_keeps_its_own_colour() {
        let mut marks = Marks::default();
        let mut green = watching("comando", "postgres");
        green.color = MarkColor::Verde;
        let mut red = watching("comando", "firefox");
        red.color = MarkColor::Vermelho;
        marks.add(green);
        marks.add(red);

        let mut rows = vec![row(&["postgres -D /var", "0"]), row(&["firefox", "0"])];
        marks.apply("t", KINDS, &mut rows);
        assert_eq!(rows[0].mark, Some(MarkColor::Verde));
        assert_eq!(rows[1].mark, Some(MarkColor::Vermelho));
    }

    #[test]
    fn editing_a_mark_in_place_keeps_it_where_it_was() {
        let mut marks = Marks::default();
        marks.all.push(watching("comando", "a"));
        marks.all.push(watching("comando", "b"));
        marks.all.push(watching("comando", "c"));

        let mut edited = watching("comando", "c");
        edited.color = MarkColor::Azul;
        // Rewriting the middle one into a duplicate of the last leaves one of them, at
        // the position that was edited — the list must not grow while being edited.
        marks.replace(1, edited);
        assert_eq!(marks.all().len(), 2);
        assert_eq!(marks.all()[1].value, "c");
        assert_eq!(marks.all()[1].color, MarkColor::Azul);
        assert_eq!(marks.all()[0].value, "a");
    }
}
