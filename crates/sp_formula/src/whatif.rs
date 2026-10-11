//! Evaluation under substituted inputs (what-if data tables, `TABLE()`).
//!
//! [`SubstHost`] wraps another [`Host`]: chosen cells read as given values, and every formula
//! cell reached from the evaluated formula is re-evaluated against the substituted values
//! (memoized, depth- and budget-bounded), so a result reflects the inputs through any chain
//! of intermediate formulas without touching the workbook.

use crate::ast::Expr;
use crate::eval::{Ev, EvalValue, Host};
use crate::parse;
use sp_core::{CellAddr, CellRange, ErrorKind, Value};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::Arc;

/// Formula evaluations per host (each cell may be evaluated a few times while its
/// precedents are discovered).
const MAX_EVALS: usize = 500_000;

type Key = (usize, CellAddr);

thread_local! {
    /// `TABLE()` evaluations in progress on this thread (a data table whose formula reads
    /// another data table is not supported: `#CALC!` instead of unbounded nesting).
    static TABLE_DEPTH: Cell<usize> = const { Cell::new(0) };
}

pub struct SubstHost<'a> {
    base: &'a dyn Host,
    overrides: Vec<(Key, Value)>,
    memo: RefCell<HashMap<Key, Value>>,
    asts: RefCell<HashMap<Key, Option<Arc<Expr>>>>,
    /// Formula cells read before their value was known in the current evaluation.
    pending: RefCell<Vec<Key>>,
    evals: Cell<usize>,
}

impl<'a> SubstHost<'a> {
    pub fn new(base: &'a dyn Host, overrides: Vec<((usize, CellAddr), Value)>) -> Self {
        SubstHost {
            base,
            overrides,
            memo: RefCell::new(HashMap::new()),
            asts: RefCell::new(HashMap::new()),
            pending: RefCell::new(Vec::new()),
            evals: Cell::new(0),
        }
    }

    fn override_of(&self, k: Key) -> Option<&Value> {
        self.overrides
            .iter()
            .find(|(o, _)| *o == k)
            .map(|(_, v)| v)
    }

    fn ast(&self, k: Key) -> Option<Arc<Expr>> {
        if let Some(a) = self.asts.borrow().get(&k) {
            return a.clone();
        }
        let a = self
            .base
            .cell_formula(k.0, k.1)
            .and_then(|src| parse(&src).ok())
            .map(Arc::new);
        self.asts.borrow_mut().insert(k, a.clone());
        a
    }

    /// Read during an evaluation: known values, or blank + a note to compute `k` first.
    fn read(&self, k: Key) -> Value {
        if let Some(v) = self.override_of(k) {
            return v.clone();
        }
        if let Some(v) = self.memo.borrow().get(&k) {
            return v.clone();
        }
        if self.ast(k).is_none() {
            return self.base.cell_value(k.0, k.1);
        }
        self.pending.borrow_mut().push(k);
        Value::Empty
    }

    /// Value of `k` re-evaluated under the substitutions. Precedents are evaluated first
    /// with an explicit stack (no recursion), so long formula chains are fine.
    pub fn value(&self, k: Key) -> Value {
        if let Some(v) = self.override_of(k) {
            return v.clone();
        }
        if self.ast(k).is_none() {
            return self.base.cell_value(k.0, k.1);
        }
        let mut stack: Vec<Key> = vec![k];
        while let Some(&top) = stack.last() {
            if self.memo.borrow().contains_key(&top) {
                stack.pop();
                continue;
            }
            if self.evals.get() >= MAX_EVALS {
                // Out of budget: fall back to cached values for what is left.
                for x in stack.drain(..) {
                    let v = self.base.cell_value(x.0, x.1);
                    self.memo.borrow_mut().insert(x, v);
                }
                break;
            }
            self.evals.set(self.evals.get() + 1);
            self.pending.borrow_mut().clear();
            let v = match self.ast(top) {
                Some(expr) => {
                    let mut ev = Ev::new(self, top.0, top.1);
                    let r = ev.eval(&expr);
                    scalar(&ev, r)
                }
                None => self.base.cell_value(top.0, top.1),
            };
            let pending: Vec<Key> = std::mem::take(&mut *self.pending.borrow_mut());
            let mut pushed = false;
            for p in pending {
                if self.memo.borrow().contains_key(&p) {
                    continue;
                }
                if stack.contains(&p) {
                    // Circular: break the loop with the cached value.
                    let cv = self.base.cell_value(p.0, p.1);
                    self.memo.borrow_mut().insert(p, cv);
                    pushed = true;
                    continue;
                }
                stack.push(p);
                pushed = true;
            }
            if !pushed {
                self.memo.borrow_mut().insert(top, v);
                stack.pop();
            }
        }
        self.memo
            .borrow()
            .get(&k)
            .cloned()
            .unwrap_or_else(|| self.base.cell_value(k.0, k.1))
    }
}

/// A formula result as a cell value (like recalc's finalize).
fn scalar(ev: &Ev<'_>, v: EvalValue) -> Value {
    let v = match v {
        EvalValue::Ref(r) => ev.host.cell_value(r.sheet, r.range.start),
        EvalValue::Refs(rs) => match rs.first() {
            Some(r) if rs.len() == 1 => ev.host.cell_value(r.sheet, r.range.start),
            _ => Value::Error(ErrorKind::Value),
        },
        other => ev.to_scalar(other),
    };
    match v {
        Value::Empty => Value::Number(0.0),
        Value::Number(n) if !n.is_finite() => Value::Error(ErrorKind::Num),
        other => other,
    }
}

impl Host for SubstHost<'_> {
    fn cell_value(&self, sheet: usize, a: CellAddr) -> Value {
        self.read((sheet, a))
    }

    fn for_each_stored(&self, sheet: usize, range: CellRange, f: &mut dyn FnMut(CellAddr, &Value)) {
        let mut items: Vec<(CellAddr, Value)> = Vec::new();
        self.base
            .for_each_stored(sheet, range, &mut |a, v| items.push((a, v.clone())));
        for ((s, a), v) in &self.overrides {
            if *s == sheet && range.contains(*a) && !items.iter().any(|(x, _)| x == a) {
                items.push((*a, v.clone()));
            }
        }
        items.sort_by_key(|(a, _)| (a.row, a.col));
        for (a, v) in items {
            let nv = if self.override_of((sheet, a)).is_some() || self.ast((sheet, a)).is_some() {
                self.read((sheet, a))
            } else {
                v
            };
            f(a, &nv);
        }
    }

    fn cell_formula(&self, sheet: usize, a: CellAddr) -> Option<String> {
        self.base.cell_formula(sheet, a)
    }
    fn sheet_index(&self, name: &str) -> Option<usize> {
        self.base.sheet_index(name)
    }
    fn sheet_name(&self, i: usize) -> Option<String> {
        self.base.sheet_name(i)
    }
    fn sheet_count(&self) -> usize {
        self.base.sheet_count()
    }
    fn used_range(&self, sheet: usize) -> Option<CellRange> {
        self.base.used_range(sheet)
    }
    fn defined_name(&self, name: &str, sheet: usize) -> Option<Arc<Expr>> {
        self.base.defined_name(name, sheet)
    }
    fn row_hidden(&self, sheet: usize, row: u32) -> bool {
        self.base.row_hidden(sheet, row)
    }
    fn row_filtered(&self, sheet: usize, row: u32) -> bool {
        self.base.row_filtered(sheet, row)
    }
    fn is_subtotal_cell(&self, sheet: usize, a: CellAddr) -> bool {
        self.base.is_subtotal_cell(sheet, a)
    }
    fn now(&self) -> f64 {
        self.base.now()
    }
    fn random(&self) -> f64 {
        self.base.random()
    }
    fn spill_range(&self, sheet: usize, a: CellAddr) -> Option<CellRange> {
        self.base.spill_range(sheet, a)
    }
    fn workbook(&self) -> Option<&sp_model::Workbook> {
        self.base.workbook()
    }
}

/// Whether a formula is a data-table cell (`TABLE(…)`).
pub fn is_table_formula(src: &str) -> bool {
    src.trim_start()
        .get(..6)
        .is_some_and(|p| p.eq_ignore_ascii_case("TABLE("))
}

/// Most cells scanned to find the edges of a data table.
const MAX_TABLE_SCAN: u32 = 100_000;

/// `TABLE(row_input, col_input)`: the value of the data table's formula with the inputs
/// replaced by this cell's row/column header values.
pub fn table_value(
    host: &dyn Host,
    sheet: usize,
    at: CellAddr,
    row_in: Option<(usize, CellAddr)>,
    col_in: Option<(usize, CellAddr)>,
) -> Value {
    let is_table = |a: CellAddr| {
        host.cell_formula(sheet, a)
            .is_some_and(|f| is_table_formula(&f))
    };
    // The block's top row / left column are the first non-TABLE cells above / to the left.
    let mut r0 = at.row;
    let mut n = 0;
    while r0 > 0 && n < MAX_TABLE_SCAN && is_table(CellAddr::new(r0 - 1, at.col)) {
        r0 -= 1;
        n += 1;
    }
    let mut c0 = at.col;
    n = 0;
    while c0 > 0 && n < MAX_TABLE_SCAN && is_table(CellAddr::new(at.row, c0 - 1)) {
        c0 -= 1;
        n += 1;
    }
    if r0 == 0 && row_in.is_some() || c0 == 0 && col_in.is_some() {
        return Value::Error(ErrorKind::Ref);
    }
    let top = r0.saturating_sub(1);
    let left = c0.saturating_sub(1);
    let (formula_at, overrides) = match (row_in, col_in) {
        (Some(ri), Some(ci)) => (
            CellAddr::new(top, left),
            vec![
                (ri, host.cell_value(sheet, CellAddr::new(top, at.col))),
                (ci, host.cell_value(sheet, CellAddr::new(at.row, left))),
            ],
        ),
        (None, Some(ci)) => (
            CellAddr::new(top, at.col),
            vec![(ci, host.cell_value(sheet, CellAddr::new(at.row, left)))],
        ),
        (Some(ri), None) => (
            CellAddr::new(at.row, left),
            vec![(ri, host.cell_value(sheet, CellAddr::new(top, at.col)))],
        ),
        (None, None) => return Value::Error(ErrorKind::Value),
    };
    if host.cell_formula(sheet, formula_at).is_none() {
        // A constant (or blank) in the formula position: shown as is.
        return match host.cell_value(sheet, formula_at) {
            Value::Empty => Value::Number(0.0),
            v => v,
        };
    }
    let depth = TABLE_DEPTH.with(Cell::get);
    if depth >= 1 {
        return Value::Error(ErrorKind::Calc);
    }
    TABLE_DEPTH.with(|d| d.set(depth + 1));
    let sub = SubstHost::new(host, overrides);
    let v = sub.value((sheet, formula_at));
    TABLE_DEPTH.with(|d| d.set(depth));
    v
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use crate::recalc::{AstCache, RecalcOptions, recalc};
    use sp_core::{CellAddr, Value};
    use sp_model::{Cell, Workbook};

    fn book(cells: &[(&str, &str)]) -> Workbook {
        let mut wb = Workbook::new();
        for (a, input) in cells {
            let addr = CellAddr::parse(a).unwrap();
            let cell = match input.strip_prefix('=') {
                Some(f) => Cell::formula(f),
                None => match input.parse::<f64>() {
                    Ok(n) => Cell::constant(Value::Number(n)),
                    Err(_) => Cell::constant(Value::text(*input)),
                },
            };
            wb.sheets[0].set(addr, cell);
        }
        let mut cache = AstCache::default();
        recalc(&mut wb, &mut cache, RecalcOptions::default());
        wb
    }

    fn v(wb: &Workbook, a: &str) -> Value {
        wb.sheets[0].value(CellAddr::parse(a).unwrap()).clone()
    }

    #[test]
    fn one_variable_column_table() {
        // B1 = rate input, B2 = B1*100 via an intermediate; table D2:E5.
        let wb = book(&[
            ("B1", "1"),
            ("B3", "=B1*10"),
            ("B2", "=B3*10"),
            ("E2", "=B2+1"),
            ("D3", "2"),
            ("D4", "3"),
            ("D5", "4"),
            ("E3", "=TABLE(,B1)"),
            ("E4", "=TABLE(,B1)"),
            ("E5", "=TABLE(,B1)"),
        ]);
        assert_eq!(v(&wb, "E2"), Value::Number(101.0));
        assert_eq!(v(&wb, "E3"), Value::Number(201.0));
        assert_eq!(v(&wb, "E4"), Value::Number(301.0));
        assert_eq!(v(&wb, "E5"), Value::Number(401.0));
        // Inputs untouched.
        assert_eq!(v(&wb, "B2"), Value::Number(100.0));
    }

    #[test]
    fn two_variable_and_row_tables() {
        let wb = book(&[
            ("A1", "2"),
            ("A2", "3"),
            ("C3", "=A1*A2"),
            ("D3", "10"),
            ("E3", "20"),
            ("C4", "1"),
            ("C5", "5"),
            ("D4", "=TABLE(A1,A2)"),
            ("E4", "=TABLE(A1,A2)"),
            ("D5", "=TABLE(A1,A2)"),
            ("E5", "=TABLE(A1,A2)"),
            // Row-oriented: inputs along row 8, formula in G9.
            ("H8", "7"),
            ("I8", "8"),
            ("G9", "=A1+100"),
            ("H9", "=TABLE(A1,)"),
            ("I9", "=TABLE(A1,)"),
        ]);
        assert_eq!(v(&wb, "D4"), Value::Number(10.0));
        assert_eq!(v(&wb, "E4"), Value::Number(20.0));
        assert_eq!(v(&wb, "D5"), Value::Number(50.0));
        assert_eq!(v(&wb, "E5"), Value::Number(100.0));
        assert_eq!(v(&wb, "H9"), Value::Number(107.0));
        assert_eq!(v(&wb, "I9"), Value::Number(108.0));
    }

    #[test]
    fn table_recalcs_when_model_changes() {
        let mut wb = book(&[
            ("A1", "1"),
            ("A2", "5"),
            ("C1", "=A1+A2"),
            ("B2", "10"),
            ("C2", "=TABLE(,A1)"),
        ]);
        assert_eq!(v(&wb, "C2"), Value::Number(15.0));
        wb.sheets[0].set(CellAddr::parse("A2").unwrap(), Cell::constant(Value::Number(6.0)));
        let mut cache = AstCache::default();
        recalc(&mut wb, &mut cache, RecalcOptions::default());
        let changed = [(0usize, CellAddr::parse("A2").unwrap())];
        crate::recalc::recalc_dirty(&mut wb, &mut cache, &changed, RecalcOptions::default());
        assert_eq!(v(&wb, "C2"), Value::Number(16.0));
    }

    #[test]
    fn long_chain_is_exact() {
        let mut wb = Workbook::new();
        let s = &mut wb.sheets[0];
        s.set(CellAddr::new(0, 0), Cell::constant(Value::Number(1.0)));
        s.set(CellAddr::new(0, 1), Cell::formula("A1+1"));
        for r in 1..2000u32 {
            s.set(CellAddr::new(r, 1), Cell::formula(format!("B{r}+1")));
        }
        s.set(CellAddr::new(0, 3), Cell::formula("B2000"));
        s.set(CellAddr::new(1, 2), Cell::constant(Value::Number(100.0)));
        s.set(CellAddr::new(1, 3), Cell::formula("TABLE(,A1)"));
        let mut cache = AstCache::default();
        recalc(&mut wb, &mut cache, RecalcOptions::default());
        assert_eq!(v(&wb, "D1"), Value::Number(2001.0));
        assert_eq!(v(&wb, "D2"), Value::Number(2100.0));
    }

    #[test]
    fn self_referencing_table_does_not_hang() {
        let wb = book(&[("A1", "1"), ("B1", "=B2+A1"), ("A2", "3"), ("B2", "=TABLE(,A1)")]);
        // Pathological: the formula reads the table itself; must terminate.
        let _ = v(&wb, "B2");
    }
}
