//! Workbook recalculation.
//!
//! Demand-driven with an explicit work stack (no deep recursion): evaluating a formula that
//! reads a not-yet-computed formula cell records it as *pending*; the pending cells are
//! computed first and the formula is retried. Cells found on the stack while pending form a
//! cycle and evaluate to 0 (the target's behaviour with iterative calculation off).

use crate::ast::Expr;
use crate::eval::{Ev, EvalValue, Host};
use crate::parser::{ParseError, parse};
use sp_core::{CellAddr, CellRange, ErrorKind, Value};
use sp_model::{Content, Workbook};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub type Key = (usize, CellAddr);

/// Parsed-formula cache keyed by cell; entries are re-parsed when the source text changes.
#[derive(Default)]
pub struct AstCache {
    map: HashMap<Key, (String, Result<Arc<Expr>, ParseError>)>,
}

impl AstCache {
    pub fn get(&mut self, key: Key, src: &str) -> Result<Arc<Expr>, ParseError> {
        if let Some((s, r)) = self.map.get(&key)
            && s == src
        {
            return r.clone();
        }
        let r = parse(src).map(Arc::new);
        self.map.insert(key, (src.to_string(), r.clone()));
        r
    }

    pub fn clear(&mut self) {
        self.map.clear();
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecalcStats {
    pub formulas: usize,
    pub evaluations: usize,
    pub cycles: usize,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RecalcOptions {
    /// Fixed NOW() for deterministic tests; `None` = wall clock.
    pub now: Option<f64>,
    pub seed: u64,
}

struct WbHost<'a> {
    wb: &'a Workbook,
    computed: RefCell<HashMap<Key, Value>>,
    pending: RefCell<Vec<Key>>,
    names: HashMap<(String, Option<usize>), Arc<Expr>>,
    /// Read formula cells' cached values instead of tracking pending work.
    use_cached: bool,
    now: Option<f64>,
    rng: Cell<u64>,
}

const MAX_PENDING: usize = 1_000_000;

impl<'a> WbHost<'a> {
    fn new(wb: &'a Workbook, use_cached: bool, opts: RecalcOptions) -> Self {
        let mut names = HashMap::new();
        for n in &wb.names {
            if let Ok(e) = parse(&n.formula) {
                names.insert((n.name.to_lowercase(), n.scope), Arc::new(e));
            }
        }
        WbHost {
            wb,
            computed: RefCell::new(HashMap::new()),
            pending: RefCell::new(Vec::new()),
            names,
            use_cached,
            now: opts.now,
            rng: Cell::new(opts.seed.max(1) ^ 0x9E37_79B9_7F4A_7C15),
        }
    }

    fn formula_value(&self, key: Key, cached: &Value) -> Value {
        if self.use_cached {
            return cached.clone();
        }
        if let Some(v) = self.computed.borrow().get(&key) {
            return v.clone();
        }
        let mut p = self.pending.borrow_mut();
        if p.len() < MAX_PENDING {
            p.push(key);
        }
        Value::Empty
    }
}

impl Host for WbHost<'_> {
    fn cell_value(&self, sheet: usize, a: CellAddr) -> Value {
        let Some(c) = self.wb.sheet(sheet).and_then(|s| s.get(a)) else {
            return Value::Empty;
        };
        match &c.content {
            Content::Formula(_) => self.formula_value((sheet, a), &c.value),
            Content::Value(v) => v.clone(),
        }
    }

    fn for_each_stored(&self, sheet: usize, range: CellRange, f: &mut dyn FnMut(CellAddr, &Value)) {
        let Some(s) = self.wb.sheet(sheet) else {
            return;
        };
        for (addr, c) in s.cells_in(range) {
            match &c.content {
                Content::Formula(_) => {
                    let v = self.formula_value((sheet, addr), &c.value);
                    f(addr, &v);
                }
                Content::Value(v) => f(addr, v),
            }
        }
    }

    fn cell_formula(&self, sheet: usize, a: CellAddr) -> Option<String> {
        self.wb
            .sheet(sheet)?
            .get(a)?
            .formula_src()
            .map(str::to_string)
    }

    fn sheet_index(&self, name: &str) -> Option<usize> {
        self.wb.sheet_index(name)
    }

    fn sheet_name(&self, i: usize) -> Option<String> {
        self.wb.sheet(i).map(|s| s.name.clone())
    }

    fn sheet_count(&self) -> usize {
        self.wb.sheets.len()
    }

    fn used_range(&self, sheet: usize) -> Option<CellRange> {
        self.wb.sheet(sheet)?.used_range()
    }

    fn defined_name(&self, name: &str, sheet: usize) -> Option<Arc<Expr>> {
        let k = name.to_lowercase();
        self.names
            .get(&(k.clone(), Some(sheet)))
            .or_else(|| self.names.get(&(k, None)))
            .cloned()
    }

    fn row_hidden(&self, sheet: usize, row: u32) -> bool {
        self.wb
            .sheet(sheet)
            .is_some_and(|s| s.hidden_rows.contains(&row))
    }

    fn now(&self) -> f64 {
        self.now.unwrap_or_else(sp_core::date::now_serial)
    }

    fn random(&self) -> f64 {
        // xorshift64*
        let mut x = self.rng.get();
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng.set(x);
        (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Final cell value of a formula result: references read their (top-left) cell, arrays
/// their top-left element (the spill anchor), and blanks become 0.
fn finalize(ev: &Ev<'_>, v: EvalValue) -> Value {
    let v = match v {
        EvalValue::Ref(r) => ev.host.cell_value(r.sheet, r.range.start),
        EvalValue::Refs(rs) => match rs.first() {
            Some(r) if rs.len() == 1 => ev.host.cell_value(r.sheet, r.range.start),
            _ => Value::Error(ErrorKind::Value),
        },
        EvalValue::Array(a) => a.get(0, 0).clone(),
        EvalValue::Scalar(v) => v,
    };
    match v {
        Value::Empty => Value::Number(0.0),
        Value::Number(n) if !n.is_finite() => Value::Error(ErrorKind::Num),
        other => other,
    }
}

/// Recalculate every formula in the workbook, writing results into each cell's `value`.
pub fn recalc(wb: &mut Workbook, cache: &mut AstCache, opts: RecalcOptions) -> RecalcStats {
    let mut stats = RecalcStats::default();
    let mut formulas: Vec<(Key, String)> = Vec::new();
    for (si, s) in wb.sheets.iter().enumerate() {
        for (a, c) in s.iter() {
            if let Some(src) = c.formula_src() {
                formulas.push(((si, a), src.to_string()));
            }
        }
    }
    stats.formulas = formulas.len();
    let sources: HashMap<Key, String> = formulas.iter().cloned().collect();
    let results: HashMap<Key, Value> = {
        let host = WbHost::new(wb, false, opts);
        let mut on_stack: HashSet<Key> = HashSet::new();
        for (key, _) in &formulas {
            if host.computed.borrow().contains_key(key) {
                continue;
            }
            let mut stack = vec![*key];
            on_stack.insert(*key);
            while let Some(&top) = stack.last() {
                if host.computed.borrow().contains_key(&top) {
                    stack.pop();
                    on_stack.remove(&top);
                    continue;
                }
                let Some(src) = sources.get(&top) else {
                    stack.pop();
                    on_stack.remove(&top);
                    continue;
                };
                let value = match cache.get(top, src) {
                    Err(_) => Value::Error(ErrorKind::Name),
                    Ok(expr) => {
                        host.pending.borrow_mut().clear();
                        let mut ev = Ev::new(&host, top.0, top.1);
                        let r = ev.eval(&expr);
                        stats.evaluations += 1;
                        let pending: Vec<Key> = std::mem::take(&mut *host.pending.borrow_mut());
                        if !pending.is_empty() {
                            let mut seen = HashSet::new();
                            for p in pending {
                                if !seen.insert(p) || host.computed.borrow().contains_key(&p) {
                                    continue;
                                }
                                if on_stack.contains(&p) {
                                    stats.cycles += 1;
                                    host.computed.borrow_mut().insert(p, Value::Number(0.0));
                                } else {
                                    stack.push(p);
                                    on_stack.insert(p);
                                }
                            }
                            continue;
                        }
                        finalize(&ev, r)
                    }
                };
                host.computed.borrow_mut().insert(top, value);
            }
        }
        host.computed.into_inner()
    };
    for ((si, a), v) in results {
        if let Some(c) = wb.sheet_mut(si).and_then(|s| s.get_mut(a)) {
            c.value = v;
        }
    }
    stats
}

/// Evaluate a standalone formula against a workbook whose cached values are current.
pub fn eval_formula(wb: &Workbook, sheet: usize, at: CellAddr, src: &str) -> Value {
    match parse(src) {
        Err(_) => Value::Error(ErrorKind::Name),
        Ok(expr) => {
            let host = WbHost::new(wb, true, RecalcOptions::default());
            let mut ev = Ev::new(&host, sheet, at);
            let r = ev.eval(&expr);
            finalize(&ev, r)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sp_model::Cell as MCell;

    fn wb_with(cells: &[(&str, &str)]) -> Workbook {
        let mut wb = Workbook::new();
        for (a, input) in cells {
            let addr = CellAddr::parse(a).unwrap();
            let cell = match input.strip_prefix('=') {
                Some(f) => MCell::formula(f),
                None => match input.parse::<f64>() {
                    Ok(n) => MCell::constant(Value::Number(n)),
                    Err(_) => MCell::constant(Value::text(*input)),
                },
            };
            wb.sheets[0].set(addr, cell);
        }
        let mut cache = AstCache::default();
        recalc(
            &mut wb,
            &mut cache,
            RecalcOptions {
                now: Some(45306.5),
                seed: 7,
            },
        );
        wb
    }

    fn v(wb: &Workbook, a: &str) -> Value {
        wb.sheets[0].value(CellAddr::parse(a).unwrap()).clone()
    }

    #[test]
    fn chain_out_of_order() {
        let wb = wb_with(&[("A1", "=A2+1"), ("A2", "=A3*2"), ("A3", "5")]);
        assert_eq!(v(&wb, "A1"), Value::Number(11.0));
    }

    #[test]
    fn long_reverse_chain_does_not_overflow() {
        let mut cells: Vec<(String, String)> = Vec::new();
        for r in 1..20_000 {
            cells.push((format!("A{r}"), format!("=A{}+1", r + 1)));
        }
        cells.push(("A20000".into(), "1".into()));
        let refs: Vec<(&str, &str)> = cells
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let wb = wb_with(&refs);
        assert_eq!(v(&wb, "A1"), Value::Number(20_000.0));
    }

    #[test]
    fn cycles_are_zero() {
        let wb = wb_with(&[("A1", "=B1+1"), ("B1", "=A1+1")]);
        assert!(matches!(v(&wb, "A1"), Value::Number(_)));
    }

    #[test]
    fn functions_end_to_end() {
        let wb = wb_with(&[
            ("A1", "1"),
            ("A2", "2"),
            ("A3", "3"),
            ("B1", "=SUM(A1:A3)"),
            ("B2", "=AVERAGE(A:A)"),
            ("B3", "=IF(B1>5,\"big\",\"small\")"),
            ("B4", "=VLOOKUP(2,A1:B3,2,FALSE)"),
            ("B5", "=SUMPRODUCT((A1:A3>1)*A1:A3)"),
            ("B6", "=COUNTIF(A1:A3,\">=2\")"),
            ("B7", "=TEXT(0.25,\"0%\")"),
            ("B8", "=1/0"),
            ("B9", "=IFERROR(1/0,\"x\")"),
            ("B10", "=DATE(2024,1,15)"),
            ("B11", "=NOSUCHFN(1)"),
            ("B12", "=LET(x,2,y,3,x*y)"),
            ("B13", "=INDEX(A1:A3,2)+MATCH(3,A1:A3,0)"),
        ]);
        assert_eq!(v(&wb, "B1"), Value::Number(6.0));
        assert_eq!(v(&wb, "B2"), Value::Number(2.0));
        assert_eq!(v(&wb, "B3"), Value::text("big"));
        assert_eq!(v(&wb, "B4"), Value::Number(2.0)); // B2 = AVERAGE(A:A)
        assert_eq!(v(&wb, "B5"), Value::Number(5.0));
        assert_eq!(v(&wb, "B6"), Value::Number(2.0));
        assert_eq!(v(&wb, "B7"), Value::text("25%"));
        assert_eq!(v(&wb, "B8"), Value::Error(ErrorKind::Div0));
        assert_eq!(v(&wb, "B9"), Value::text("x"));
        assert_eq!(v(&wb, "B10"), Value::Number(45306.0));
        assert_eq!(v(&wb, "B11"), Value::Error(ErrorKind::Name));
        assert_eq!(v(&wb, "B12"), Value::Number(6.0));
        assert_eq!(v(&wb, "B13"), Value::Number(5.0));
    }

    #[test]
    fn empty_ref_is_zero() {
        let wb = wb_with(&[("A1", "=Z99")]);
        assert_eq!(v(&wb, "A1"), Value::Number(0.0));
    }
}
