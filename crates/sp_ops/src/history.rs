//! Transactions and undo/redo.
//!
//! Every mutation runs inside a `Tx`, which records the *before* state of whatever it
//! touches (individual cells, whole sheets, or the whole workbook for structural edits).
//! Committing produces matching *after* patches, so undo and redo are both patch replays.

use sp_core::CellAddr;
use sp_model::{Cell, Sheet, Workbook};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
pub enum Patch {
    Cell {
        sheet: usize,
        addr: CellAddr,
        cell: Option<Cell>,
    },
    Sheet {
        index: usize,
        sheet: Box<Sheet>,
    },
    Book(Box<Workbook>),
}

fn apply(wb: &mut Workbook, p: &Patch) {
    match p {
        Patch::Cell { sheet, addr, cell } => {
            if let Some(s) = wb.sheet_mut(*sheet) {
                match cell {
                    Some(c) => {
                        s.set(*addr, c.clone());
                    }
                    None => {
                        s.remove(*addr);
                    }
                }
            }
        }
        Patch::Sheet { index, sheet } => {
            if let Some(s) = wb.sheet_mut(*index) {
                *s = (**sheet).clone();
            }
        }
        Patch::Book(b) => *wb = (**b).clone(),
    }
}

#[derive(Debug, Clone)]
pub struct Txn {
    pub label: String,
    undo: Vec<Patch>,
    redo: Vec<Patch>,
}

impl Txn {
    /// Undo this transaction without recording it (used to roll back a failed command).
    pub fn revert(self, wb: &mut Workbook) {
        for p in self.undo.iter().rev() {
            apply(wb, p);
        }
    }
}

/// Recording transaction over a workbook.
pub struct Tx<'a> {
    pub wb: &'a mut Workbook,
    undo: Vec<Patch>,
    cells: HashSet<(usize, CellAddr)>,
    sheets: HashSet<usize>,
    book: bool,
}

impl<'a> Tx<'a> {
    pub fn new(wb: &'a mut Workbook) -> Self {
        Tx {
            wb,
            undo: Vec::new(),
            cells: HashSet::new(),
            sheets: HashSet::new(),
            book: false,
        }
    }

    /// Record a cell's current state before it is changed (idempotent).
    pub fn touch_cell(&mut self, sheet: usize, addr: CellAddr) {
        if self.book || self.sheets.contains(&sheet) || !self.cells.insert((sheet, addr)) {
            return;
        }
        let before = self.wb.sheet(sheet).and_then(|s| s.get(addr)).cloned();
        self.undo.push(Patch::Cell {
            sheet,
            addr,
            cell: before,
        });
    }

    /// Record a whole sheet before a bulk change (sort, insert rows…).
    pub fn touch_sheet(&mut self, sheet: usize) {
        if self.book || !self.sheets.insert(sheet) {
            return;
        }
        if let Some(s) = self.wb.sheet(sheet) {
            self.undo.push(Patch::Sheet {
                index: sheet,
                sheet: Box::new(s.clone()),
            });
        }
    }

    /// Record the whole workbook before a structural change (sheet add/remove/reorder, styles).
    pub fn touch_book(&mut self) {
        if self.book {
            return;
        }
        self.book = true;
        self.undo.push(Patch::Book(Box::new(self.wb.clone())));
    }

    pub fn set_cell(&mut self, sheet: usize, addr: CellAddr, cell: Option<Cell>) {
        self.touch_cell(sheet, addr);
        if let Some(s) = self.wb.sheet_mut(sheet) {
            match cell {
                Some(c) => {
                    s.set(addr, c);
                }
                None => {
                    s.remove(addr);
                }
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.undo.is_empty()
    }

    /// Finish: build redo patches from the current state of everything touched.
    pub fn commit(self, label: &str) -> Option<Txn> {
        if self.undo.is_empty() {
            return None;
        }
        let redo: Vec<Patch> = if self.book {
            vec![Patch::Book(Box::new(self.wb.clone()))]
        } else {
            let mut r = Vec::new();
            for p in &self.undo {
                match p {
                    Patch::Cell { sheet, addr, .. } => r.push(Patch::Cell {
                        sheet: *sheet,
                        addr: *addr,
                        cell: self.wb.sheet(*sheet).and_then(|s| s.get(*addr)).cloned(),
                    }),
                    Patch::Sheet { index, .. } => {
                        if let Some(s) = self.wb.sheet(*index) {
                            r.push(Patch::Sheet {
                                index: *index,
                                sheet: Box::new(s.clone()),
                            });
                        }
                    }
                    Patch::Book(_) => {}
                }
            }
            r
        };
        // Cell patches recorded before a sheet snapshot of the same sheet are redundant on
        // undo (the snapshot restores them), and harmless to replay in reverse order.
        Some(Txn {
            label: label.to_string(),
            undo: self.undo,
            redo,
        })
    }
}

#[derive(Debug, Default)]
pub struct History {
    done: Vec<Txn>,
    undone: Vec<Txn>,
    limit: usize,
}

impl History {
    pub fn new(limit: usize) -> Self {
        History {
            done: Vec::new(),
            undone: Vec::new(),
            limit: limit.max(1),
        }
    }

    pub fn push(&mut self, t: Txn) {
        self.done.push(t);
        self.undone.clear();
        if self.done.len() > self.limit {
            self.done.remove(0);
        }
    }

    pub fn undo(&mut self, wb: &mut Workbook) -> Option<String> {
        let t = self.done.pop()?;
        for p in t.undo.iter().rev() {
            apply(wb, p);
        }
        let label = t.label.clone();
        self.undone.push(t);
        Some(label)
    }

    pub fn redo(&mut self, wb: &mut Workbook) -> Option<String> {
        let t = self.undone.pop()?;
        for p in &t.redo {
            apply(wb, p);
        }
        let label = t.label.clone();
        self.done.push(t);
        Some(label)
    }

    pub fn can_undo(&self) -> bool {
        !self.done.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.undone.is_empty()
    }

    pub fn undo_label(&self) -> Option<&str> {
        self.done.last().map(|t| t.label.as_str())
    }

    pub fn clear(&mut self) {
        self.done.clear();
        self.undone.clear();
    }

    /// Summary for debugging: label → number of patches.
    pub fn summary(&self) -> HashMap<String, usize> {
        self.done
            .iter()
            .map(|t| (t.label.clone(), t.undo.len()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sp_core::Value;

    #[test]
    fn undo_redo_cells_and_book() {
        let mut wb = Workbook::new();
        let mut h = History::new(100);
        let a = CellAddr::new(0, 0);
        let mut tx = Tx::new(&mut wb);
        tx.set_cell(0, a, Some(Cell::constant(Value::Number(1.0))));
        tx.set_cell(0, a, Some(Cell::constant(Value::Number(2.0))));
        h.push(tx.commit("set").unwrap());
        assert_eq!(wb.sheets[0].value(a), &Value::Number(2.0));

        let mut tx = Tx::new(&mut wb);
        tx.touch_book();
        tx.wb.sheets.push(Sheet::new("S2"));
        h.push(tx.commit("add sheet").unwrap());
        assert_eq!(wb.sheets.len(), 2);

        assert_eq!(h.undo(&mut wb).as_deref(), Some("add sheet"));
        assert_eq!(wb.sheets.len(), 1);
        assert_eq!(h.undo(&mut wb).as_deref(), Some("set"));
        assert!(wb.sheets[0].get(a).is_none());
        assert!(h.undo(&mut wb).is_none());
        h.redo(&mut wb);
        assert_eq!(wb.sheets[0].value(a), &Value::Number(2.0));
        h.redo(&mut wb);
        assert_eq!(wb.sheets.len(), 2);
    }

    #[test]
    fn empty_tx_commits_nothing() {
        let mut wb = Workbook::new();
        assert!(Tx::new(&mut wb).commit("noop").is_none());
    }
}
