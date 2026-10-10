//! Workbook recalculation.
//!
//! Demand-driven with an explicit work stack (no deep recursion): evaluating a formula that
//! reads a not-yet-computed formula cell records it as *pending*; the pending cells are
//! computed first and the formula is retried. Cells found on the stack while pending form a
//! cycle and evaluate to 0 (the target's behaviour with iterative calculation off).
//!
//! `recalc` recomputes every formula and (re)builds the dependency graph kept in the
//! `AstCache`; `recalc_dirty` recomputes only edited cells and their transitive dependents,
//! reading every other formula's cached value. It falls back to a full recalc whenever the
//! graph is missing or stale, the workbook has circular references, or propagation blows up,
//! so its results always equal a full recalc's (volatile random draws aside).

use crate::ast::Expr;
use crate::deps::{DepGraph, FxMap, FxSet};
use crate::eval::{Ev, EvalValue, Host};
use crate::parser::{ParseError, parse};
use sp_core::{CellAddr, CellRange, ErrorKind, Value};
use sp_model::{Content, Workbook};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::Arc;

pub use crate::deps::Key;

/// Parsed-formula cache keyed by cell; entries are re-parsed when the source text changes.
/// Also holds the dependency graph built by the last full recalc (for `recalc_dirty`).
#[derive(Default)]
pub struct AstCache {
    map: FxMap<Key, (String, Result<Arc<Expr>, ParseError>)>,
    deps: Option<DepGraph>,
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
        self.deps = None;
    }

    /// Drop the dependency graph (the workbook changed without a recalc); the next
    /// `recalc_dirty` then runs a full recalc, which rebuilds it.
    pub fn invalidate_deps(&mut self) {
        self.deps = None;
    }

    pub fn deps(&self) -> Option<&DepGraph> {
        self.deps.as_ref()
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
    /// Recomputed only dirty cells (`formulas` then counts the dirty formulas).
    pub incremental: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RecalcOptions {
    /// Fixed NOW() for deterministic tests; `None` = wall clock.
    pub now: Option<f64>,
    pub seed: u64,
}

struct WbHost<'a> {
    wb: &'a Workbook,
    computed: RefCell<FxMap<Key, Value>>,
    pending: RefCell<Vec<Key>>,
    /// Incremental mode: formulas outside this set read their cached value.
    dirty: Option<&'a FxSet<Key>>,
    names: HashMap<(String, Option<usize>), Arc<Expr>>,
    /// Read formula cells' cached values instead of tracking pending work.
    use_cached: bool,
    now: Option<f64>,
    rng: Cell<u64>,
}

const MAX_PENDING: usize = 1_000_000;

impl<'a> WbHost<'a> {
    fn new(
        wb: &'a Workbook,
        use_cached: bool,
        dirty: Option<&'a FxSet<Key>>,
        opts: RecalcOptions,
    ) -> Self {
        let mut names = HashMap::new();
        for n in &wb.names {
            if let Ok(e) = parse(&n.formula) {
                names.insert((n.name.to_lowercase(), n.scope), Arc::new(e));
            }
        }
        WbHost {
            wb,
            computed: RefCell::new(FxMap::default()),
            pending: RefCell::new(Vec::new()),
            dirty,
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
        if self.dirty.is_some_and(|d| !d.contains(&key)) {
            return cached.clone();
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

/// Evaluate `order` (formula key + source) demand-driven; returns each formula's value.
fn compute(
    wb: &Workbook,
    cache: &mut AstCache,
    order: &[(Key, String)],
    dirty: Option<&FxSet<Key>>,
    opts: RecalcOptions,
    stats: &mut RecalcStats,
) -> FxMap<Key, Value> {
    let sources: FxMap<Key, &str> = order.iter().map(|(k, s)| (*k, s.as_str())).collect();
    let host = WbHost::new(wb, false, dirty, opts);
    // Formulas evaluated at least once and waiting for pending precedents. They are exactly
    // the current evaluation path, so reaching one again is a genuine circular reference
    // (cells merely queued on the stack are not: they are pushed again and evaluated first).
    let mut waiting: FxSet<Key> = FxSet::default();
    for (key, _) in order {
        if host.computed.borrow().contains_key(key) {
            continue;
        }
        let mut stack = vec![*key];
        while let Some(&top) = stack.last() {
            if host.computed.borrow().contains_key(&top) {
                stack.pop();
                waiting.remove(&top);
                continue;
            }
            let Some(src) = sources.get(&top) else {
                stack.pop();
                continue;
            };
            let value = match cache.get(top, src) {
                Err(_) => Value::Error(ErrorKind::Name),
                Ok(expr) => {
                    host.pending.borrow_mut().clear();
                    let mut ev = Ev::new(&host, top.0, top.1);
                    let r = ev.eval(&expr);
                    // Finalizing may read a returned reference's cell: do it before checking
                    // for pending precedents so those reads are tracked too.
                    let v = finalize(&ev, r);
                    stats.evaluations += 1;
                    let pending: Vec<Key> = std::mem::take(&mut *host.pending.borrow_mut());
                    if !pending.is_empty() {
                        waiting.insert(top);
                        let mut seen = FxSet::default();
                        for p in pending {
                            if !seen.insert(p) || host.computed.borrow().contains_key(&p) {
                                continue;
                            }
                            if waiting.contains(&p) {
                                stats.cycles += 1;
                                host.computed.borrow_mut().insert(p, Value::Number(0.0));
                            } else {
                                stack.push(p);
                            }
                        }
                        continue;
                    }
                    v
                }
            };
            host.computed.borrow_mut().insert(top, value);
        }
    }
    host.computed.into_inner()
}

fn write_back(wb: &mut Workbook, results: FxMap<Key, Value>) {
    for ((si, a), v) in results {
        if let Some(c) = wb.sheet_mut(si).and_then(|s| s.get_mut(a)) {
            c.value = v;
        }
    }
}

/// Recalculate every formula in the workbook, writing results into each cell's `value`,
/// and rebuild the dependency graph used by `recalc_dirty`.
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
    let results = compute(wb, cache, &formulas, None, opts, &mut stats);
    write_back(wb, results);
    let mut graph = DepGraph::new(wb);
    for (key, src) in &formulas {
        let expr = cache.get(*key, src).ok();
        graph.insert(wb, *key, expr.as_deref());
    }
    graph.has_cycles = stats.cycles > 0;
    cache.deps = Some(graph);
    stats
}

/// Recalculate after `changed` cells were edited (content replaced, cleared or inserted):
/// only those cells, always-dirty (volatile) formulas and their transitive dependents are
/// recomputed. Every other formula keeps its cached value. Results equal `recalc`'s; falls
/// back to it when there is no current dependency graph or the workbook has cycles.
pub fn recalc_dirty(
    wb: &mut Workbook,
    cache: &mut AstCache,
    changed: &[Key],
    opts: RecalcOptions,
) -> RecalcStats {
    let Some(mut graph) = cache.deps.take() else {
        return recalc(wb, cache, opts);
    };
    if graph.has_cycles || !graph.matches(wb) {
        return recalc(wb, cache, opts);
    }
    for &key in changed {
        let src = wb
            .sheet(key.0)
            .and_then(|s| s.get(key.1))
            .and_then(|c| c.formula_src())
            .map(str::to_string);
        match src {
            Some(src) => {
                let expr = cache.get(key, &src).ok();
                graph.insert(wb, key, expr.as_deref());
            }
            None => graph.remove(key),
        }
    }
    let Some(dirty) = graph.dirty_from(wb, changed) else {
        return recalc(wb, cache, opts);
    };
    let mut order: Vec<(Key, String)> = dirty
        .iter()
        .filter_map(|k| {
            let src = wb.sheet(k.0)?.get(k.1)?.formula_src()?;
            Some((*k, src.to_string()))
        })
        .collect();
    // Same visiting order as a full recalc (sheet, then row-major).
    order.sort_unstable_by_key(|(k, _)| *k);
    let mut stats = RecalcStats {
        formulas: order.len(),
        incremental: true,
        ..RecalcStats::default()
    };
    let results = compute(wb, cache, &order, Some(&dirty), opts, &mut stats);
    if stats.cycles > 0 {
        // A new circular reference: resolve it exactly as a full recalc would.
        return recalc(wb, cache, opts);
    }
    write_back(wb, results);
    cache.deps = Some(graph);
    stats
}

/// Evaluate a standalone formula against a workbook whose cached values are current.
pub fn eval_formula(wb: &Workbook, sheet: usize, at: CellAddr, src: &str) -> Value {
    match parse(src) {
        Err(_) => Value::Error(ErrorKind::Name),
        Ok(expr) => {
            let host = WbHost::new(wb, true, None, RecalcOptions::default());
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

    const OPTS: RecalcOptions = RecalcOptions {
        now: Some(45306.5),
        seed: 7,
    };

    fn parse_input(input: &str) -> Option<MCell> {
        let cell = match input.strip_prefix('=') {
            Some(f) => MCell::formula(f),
            None if input.is_empty() => return None,
            None => match input.parse::<f64>() {
                Ok(n) => MCell::constant(Value::Number(n)),
                Err(_) => MCell::constant(Value::text(input)),
            },
        };
        Some(cell)
    }

    /// Apply edits, recalc incrementally, and check against a cold full recalc.
    fn edit_and_compare(wb: &mut Workbook, cache: &mut AstCache, edits: &[(usize, &str, &str)]) {
        let mut changed = Vec::new();
        for (si, a, input) in edits {
            let addr = CellAddr::parse(a).unwrap();
            match parse_input(input) {
                Some(c) => {
                    wb.sheets[*si].set(addr, c);
                }
                None => {
                    wb.sheets[*si].remove(addr);
                }
            }
            changed.push((*si, addr));
        }
        recalc_dirty(wb, cache, &changed, OPTS);
        let mut full = wb.clone();
        recalc(&mut full, &mut AstCache::default(), OPTS);
        for (si, (a, b)) in wb.sheets.iter().zip(&full.sheets).enumerate() {
            for (addr, c) in b.iter() {
                assert_eq!(
                    a.get(addr),
                    Some(c),
                    "sheet {si} {} differs after {edits:?}\n{}",
                    addr.to_a1(),
                    b.iter()
                        .map(|(a, c)| format!("{}={:?}->{:?}", a.to_a1(), c.content, c.value))
                        .collect::<Vec<_>>()
                        .join("\n")
                );
            }
        }
        assert_eq!(*wb, full, "incremental != full after {edits:?}");
    }

    fn base(cells: &[(&str, &str)]) -> (Workbook, AstCache) {
        let mut wb = Workbook::new();
        for (a, input) in cells {
            if let Some(c) = parse_input(input) {
                wb.sheets[0].set(CellAddr::parse(a).unwrap(), c);
            }
        }
        let mut cache = AstCache::default();
        recalc(&mut wb, &mut cache, OPTS);
        (wb, cache)
    }

    #[test]
    fn incremental_chain_recomputes_only_dependents() {
        let mut cells: Vec<(String, String)> = vec![("A1".into(), "1".into())];
        for r in 2..=5000 {
            cells.push((format!("A{r}"), format!("=A{}+1", r - 1)));
            cells.push((format!("B{r}"), format!("=C{r}*2")));
        }
        let refs: Vec<(&str, &str)> = cells
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let (mut wb, mut cache) = base(&refs);
        let k = |a: &str| (0, CellAddr::parse(a).unwrap());
        wb.sheets[0].set(
            CellAddr::parse("A1").unwrap(),
            MCell::constant(Value::Number(10.0)),
        );
        let st = recalc_dirty(&mut wb, &mut cache, &[k("A1")], OPTS);
        assert!(st.incremental);
        assert_eq!(st.formulas, 4999);
        assert_eq!(v(&wb, "A5000"), Value::Number(5009.0));
        wb.sheets[0].set(
            CellAddr::parse("C7").unwrap(),
            MCell::constant(Value::Number(4.0)),
        );
        let st = recalc_dirty(&mut wb, &mut cache, &[k("C7")], OPTS);
        assert_eq!((st.formulas, st.evaluations), (1, 1));
        assert_eq!(v(&wb, "B7"), Value::Number(8.0));
        // Reverse chain entered mid-way.
        edit_and_compare(&mut wb, &mut cache, &[(0, "A3000", "=B7+A2999")]);
        edit_and_compare(&mut wb, &mut cache, &[(0, "A1", "")]);
    }

    #[test]
    fn incremental_ranges_and_whole_columns() {
        let (mut wb, mut cache) = base(&[
            ("A1", "1"),
            ("A2", "2"),
            ("B1", "=SUM(A1:A5)"),
            ("B2", "=SUM(A:A)"),
            ("B3", "=COUNTBLANK(A:A)"),
            ("B4", "=SUM(2:2)"),
            ("B5", "=(A1):(A3)"),
            ("B6", "=SUM((A1):(A9))"),
            ("C1", "=B1+B2"),
            ("D1", "=Total*2"),
        ]);
        wb.names.push(sp_model::DefinedName {
            name: "Total".into(),
            formula: "Sheet1!$A$1:$A$9".into(),
            scope: None,
        });
        recalc(&mut wb, &mut cache, OPTS);
        edit_and_compare(&mut wb, &mut cache, &[(0, "A4", "10")]);
        edit_and_compare(&mut wb, &mut cache, &[(0, "A8", "5")]);
        edit_and_compare(&mut wb, &mut cache, &[(0, "A100", "7")]);
        edit_and_compare(&mut wb, &mut cache, &[(0, "Z2", "3")]);
        // Used range grows: COUNTBLANK(A:A) changes though column A did not.
        edit_and_compare(&mut wb, &mut cache, &[(0, "F500", "1")]);
        edit_and_compare(&mut wb, &mut cache, &[(0, "F500", "")]);
        edit_and_compare(&mut wb, &mut cache, &[(0, "A1", "=SUM(Total)")]); // cycle
        edit_and_compare(&mut wb, &mut cache, &[(0, "A1", "=SUM(A2:A9)")]);
        edit_and_compare(&mut wb, &mut cache, &[(0, "A3", "100")]);
        assert_eq!(v(&wb, "B5"), Value::Number(117.0));
    }

    #[test]
    fn incremental_cycles_match_full() {
        let (mut wb, mut cache) = base(&[("A1", "1"), ("B1", "=A1+1"), ("C1", "=B1+1")]);
        edit_and_compare(&mut wb, &mut cache, &[(0, "A1", "=C1+1")]);
        assert!(cache.deps().is_some_and(|g| g.has_cycles));
        edit_and_compare(&mut wb, &mut cache, &[(0, "B1", "5")]);
        edit_and_compare(&mut wb, &mut cache, &[(0, "A1", "2")]);
        assert!(cache.deps().is_some_and(|g| !g.has_cycles));
        edit_and_compare(&mut wb, &mut cache, &[(0, "D1", "=D1")]);
    }

    #[test]
    fn incremental_volatile_always_recomputed() {
        let (mut wb, mut cache) = base(&[
            ("A1", "1"),
            ("B1", "=NOW()+A2"),
            ("C1", "=B1+1"),
            ("D1", "=INDIRECT(\"A\"&(A1+1))"),
            ("E1", "=OFFSET(A1,A1,0)"),
            ("F1", "=RAND()"),
            ("G1", "=F1*0"),
        ]);
        let k = |a: &str| (0, CellAddr::parse(a).unwrap());
        wb.sheets[0].set(
            CellAddr::parse("A2").unwrap(),
            MCell::constant(Value::Number(9.0)),
        );
        let opts = RecalcOptions { seed: 99, ..OPTS };
        let before = v(&wb, "F1");
        let st = recalc_dirty(&mut wb, &mut cache, &[k("A2")], opts);
        assert!(st.incremental);
        assert_eq!(st.formulas, 6);
        assert_eq!(v(&wb, "C1"), Value::Number(45306.5 + 9.0 + 1.0));
        assert_eq!(v(&wb, "D1"), Value::Number(9.0));
        assert_eq!(v(&wb, "E1"), Value::Number(9.0));
        assert_ne!(v(&wb, "F1"), before);
        assert_eq!(v(&wb, "G1"), Value::Number(0.0));
    }

    #[test]
    fn incremental_matches_full_on_random_edits() {
        let mut rng: u64 = 0x2545_F491_4F6C_DD1D;
        let mut next = move |n: u64| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng % n
        };
        let cell = |r: u64, c: u64| format!("{}{}", (b'A' + c as u8) as char, r + 1);
        let mut wb = Workbook::new();
        wb.sheets.push(sp_model::Sheet::new("Two"));
        wb.names.push(sp_model::DefinedName {
            name: "Rate".into(),
            formula: "Two!$B$2".into(),
            scope: None,
        });
        let mut cache = AstCache::default();
        recalc(&mut wb, &mut cache, OPTS);
        let mut incremental = 0;
        for _ in 0..400 {
            let n_edits = 1 + next(3);
            let mut edits: Vec<(usize, String, String)> = Vec::new();
            for _ in 0..n_edits {
                // Formulas only read columns left of their own, so most states are acyclic
                // (whole-row refs and the occasional self-column range still make cycles).
                let (r, c) = (next(12), next(6));
                let r2 = next(12);
                let c2 = if c == 0 { 0 } else { next(c) };
                let c3 = if c == 0 { 0 } else { next(c) };
                let input = match if c == 0 { next(3) } else { next(16) } {
                    0 | 1 => format!("{}", next(100)),
                    2 => String::new(),
                    3 => format!("={}+{}", cell(r2, c2), cell(next(12), c3)),
                    4 => format!("=SUM({}:{})", cell(r2, c2), cell(r2 + next(4), c2)),
                    5 => format!("=SUM({}:{})", cell(0, c2), cell(r2, c3)),
                    6 => format!("=SUM({0}:{0})", (b'A' + c2 as u8) as char),
                    7 => format!("=IF({}>50,{},1)", cell(r2, c2), cell(next(12), c3)),
                    8 => format!("=Two!{}*2", cell(r2, c2)),
                    9 if c > 1 => "=Rate+1".into(),
                    10 => format!("=COUNTBLANK({0}:{0})", (b'A' + c2 as u8) as char),
                    11 => format!("=INDEX({}:{},2)", cell(r2, c2), cell(r2 + 3, c2)),
                    12 => format!("=ISFORMULA({})", cell(r2, c2)),
                    13 if next(8) == 0 => format!("=SUM({}:{})", r2 + 1, r2 + 2),
                    14 => format!("=SUM(({}):({}))", cell(r2, c2), cell(next(12), c3)),
                    _ => format!("=LET(x,{},x*3)", cell(r2, c2)),
                };
                edits.push((next(2) as usize, cell(r, c), input));
            }
            let refs: Vec<(usize, &str, &str)> = edits
                .iter()
                .map(|(s, a, i)| (*s, a.as_str(), i.as_str()))
                .collect();
            edit_and_compare(&mut wb, &mut cache, &refs);
            incremental += usize::from(cache.deps().is_some_and(|g| !g.has_cycles));
        }
        assert!(incremental > 0);
    }

    #[test]
    fn stale_graph_falls_back_to_full() {
        let (mut wb, mut cache) = base(&[("A1", "1"), ("B1", "=A1*2")]);
        // A sheet added behind the graph's back: signature mismatch → full recalc.
        wb.sheets.push(sp_model::Sheet::new("New"));
        wb.sheets[0].set(CellAddr::parse("C1").unwrap(), MCell::formula("New!A1+B1"));
        let st = recalc_dirty(&mut wb, &mut cache, &[], OPTS);
        assert!(!st.incremental);
        assert_eq!(v(&wb, "C1"), Value::Number(2.0));
        cache.invalidate_deps();
        let st = recalc_dirty(&mut wb, &mut cache, &[], OPTS);
        assert!(!st.incremental);
        assert!(cache.deps().is_some());
    }

    #[test]
    fn queued_precedent_is_not_a_cycle() {
        // A1 queues B1 and C1; C1 then needs B1 while it is still only queued.
        let mut wb = Workbook::new();
        for (a, f) in [("A1", "B1+C1"), ("B1", "2+3"), ("C1", "B1+1")] {
            wb.sheets[0].set(CellAddr::parse(a).unwrap(), MCell::formula(f));
        }
        let st = recalc(&mut wb, &mut AstCache::default(), OPTS);
        assert_eq!(st.cycles, 0);
        assert_eq!(v(&wb, "A1"), Value::Number(11.0));
    }

    #[test]
    fn returned_reference_to_later_formula() {
        let wb = wb_with(&[("A1", "=INDEX(B1:B3,2)"), ("B2", "=3+4"), ("C1", "=B1:B2")]);
        assert_eq!(v(&wb, "A1"), Value::Number(7.0));
    }

    #[test]
    fn empty_ref_is_zero() {
        let wb = wb_with(&[("A1", "=Z99")]);
        assert_eq!(v(&wb, "A1"), Value::Number(0.0));
    }
}
