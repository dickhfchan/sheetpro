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
//!
//! Dynamic arrays: a formula whose result has more than one value *spills* into the cells
//! below/right of it. Spilled values live in those cells' `value` (their content stays
//! empty) and each area is recorded in `Sheet::spills`. A spill blocked by non-empty cells
//! makes the anchor `#SPILL!`. Reading a cell inside a recorded area waits for its anchor;
//! a spill that grows into cells nobody expected triggers another (incremental) round for
//! the dependents of the newly covered cells, until the areas are stable.

use crate::ast::Expr;
use crate::deps::{DepGraph, FxMap, FxSet};
use crate::eval::{Array2, Ev, EvalValue, Host, lambda_id};
use crate::parser::{ParseError, parse};
use sp_core::{CellAddr, CellRange, ErrorKind, MAX_COLS, MAX_ROWS, Value};
use sp_model::{Content, SpillArea, Workbook};
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

/// A computed formula's array output.
#[derive(Debug, Clone, PartialEq)]
enum SpillOut {
    /// Values for every cell of `range` (row-major, anchor included).
    Spilled(CellRange, Vec<Value>),
    Blocked(CellRange),
}

struct WbHost<'a> {
    wb: &'a Workbook,
    computed: RefCell<FxMap<Key, Value>>,
    /// Array outputs of formulas computed in this pass, plus an index of spilled areas.
    spills: RefCell<FxMap<Key, SpillOut>>,
    spill_index: RefCell<Vec<(usize, CellRange)>>,
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
            spills: RefCell::new(FxMap::default()),
            spill_index: RefCell::new(Vec::new()),
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

    /// Value of a cell without content of its own: a spilled value, or blank.
    fn spilled_value(&self, sheet: usize, a: CellAddr, stored: &Value) -> Value {
        if self.use_cached {
            return stored.clone();
        }
        // Spilled by a formula computed in this pass.
        let hit = self
            .spill_index
            .borrow()
            .iter()
            .find(|(s, r)| *s == sheet && r.contains(a))
            .map(|(_, r)| r.start);
        if let Some(anchor) = hit
            && let Some(SpillOut::Spilled(r, vals)) = self.spills.borrow().get(&(sheet, anchor))
        {
            let i =
                (a.row - r.start.row) as usize * r.cols() as usize + (a.col - r.start.col) as usize;
            return vals.get(i).cloned().unwrap_or(Value::Empty);
        }
        // Inside the area spilled last time: wait for that anchor.
        let Some(area) = self.wb.sheet(sheet).and_then(|s| s.spill_covering(a)) else {
            return Value::Empty;
        };
        let key = (sheet, area.range.start);
        if key.1 == a || self.computed.borrow().contains_key(&key) {
            return Value::Empty;
        }
        if self.dirty.is_some_and(|d| !d.contains(&key)) {
            return stored.clone();
        }
        let mut p = self.pending.borrow_mut();
        if p.len() < MAX_PENDING {
            p.push(key);
        }
        Value::Empty
    }

    /// Spill cells of this pass inside `range` that are not stored yet.
    fn unstored_spills(&self, sheet: usize, range: CellRange) -> Vec<(CellAddr, Value)> {
        let mut out = Vec::new();
        let idx = self.spill_index.borrow();
        if idx.is_empty() {
            return out;
        }
        let Some(s) = self.wb.sheet(sheet) else {
            return out;
        };
        let spills = self.spills.borrow();
        for (si, r) in idx.iter() {
            if *si != sheet {
                continue;
            }
            let Some(i) = r.intersect(&range) else {
                continue;
            };
            let Some(SpillOut::Spilled(_, vals)) = spills.get(&(sheet, r.start)) else {
                continue;
            };
            for a in i.iter() {
                if a == r.start || s.get(a).is_some() {
                    continue;
                }
                let k = (a.row - r.start.row) as usize * r.cols() as usize
                    + (a.col - r.start.col) as usize;
                if let Some(v) = vals.get(k) {
                    out.push((a, v.clone()));
                }
            }
        }
        out
    }
}

impl Host for WbHost<'_> {
    fn cell_value(&self, sheet: usize, a: CellAddr) -> Value {
        let Some(c) = self.wb.sheet(sheet).and_then(|s| s.get(a)) else {
            return Value::Empty;
        };
        match &c.content {
            Content::Formula(_) => self.formula_value((sheet, a), &c.value),
            Content::Value(Value::Empty) => self.spilled_value(sheet, a, &c.value),
            Content::Value(v) => v.clone(),
        }
    }

    fn for_each_stored(&self, sheet: usize, range: CellRange, f: &mut dyn FnMut(CellAddr, &Value)) {
        let Some(s) = self.wb.sheet(sheet) else {
            return;
        };
        let extra = self.unstored_spills(sheet, range);
        let value_of = |addr: CellAddr, c: &sp_model::Cell| -> Option<Value> {
            match &c.content {
                Content::Formula(_) => Some(self.formula_value((sheet, addr), &c.value)),
                Content::Value(Value::Empty) => Some(self.spilled_value(sheet, addr, &c.value)),
                Content::Value(_) => None,
            }
        };
        if extra.is_empty() {
            for (addr, c) in s.cells_in(range) {
                match value_of(addr, c) {
                    Some(v) => f(addr, &v),
                    None => {
                        if let Content::Value(v) = &c.content {
                            f(addr, v);
                        }
                    }
                }
            }
            return;
        }
        let mut all: Vec<(CellAddr, Value)> = extra;
        for (addr, c) in s.cells_in(range) {
            let v = match value_of(addr, c) {
                Some(v) => v,
                None => match &c.content {
                    Content::Value(v) => v.clone(),
                    Content::Formula(_) => Value::Empty,
                },
            };
            all.push((addr, v));
        }
        all.sort_unstable_by_key(|(a, _)| (a.row, a.col));
        for (a, v) in &all {
            f(*a, v);
        }
    }

    fn spill_range(&self, sheet: usize, a: CellAddr) -> Option<CellRange> {
        let s = self.wb.sheet(sheet)?;
        let c = s.get(a)?;
        c.formula_src()?;
        let model = || match s.spill_at_anchor(a) {
            Some(area) if area.blocked => None,
            Some(area) => Some(area.range),
            None => Some(CellRange::single(a)),
        };
        if self.use_cached {
            return model();
        }
        let key = (sheet, a);
        let _ = self.formula_value(key, &c.value);
        if self.computed.borrow().contains_key(&key) {
            return match self.spills.borrow().get(&key) {
                Some(SpillOut::Spilled(r, _)) => Some(*r),
                Some(SpillOut::Blocked(_)) => None,
                None => Some(CellRange::single(a)),
            };
        }
        if self.dirty.is_some_and(|d| !d.contains(&key)) {
            return model();
        }
        None
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

    fn is_subtotal_cell(&self, sheet: usize, a: CellAddr) -> bool {
        self.wb
            .sheet(sheet)
            .and_then(|s| s.get(a))
            .and_then(|c| c.formula_src())
            .is_some_and(crate::functions::math::is_subtotal_formula)
    }

    fn row_filtered(&self, sheet: usize, row: u32) -> bool {
        self.wb.sheet(sheet).is_some_and(|s| {
            s.hidden_rows.contains(&row)
                && s.auto_filter
                    .as_ref()
                    .is_some_and(|f| row > f.range.start.row && row <= f.range.end.row)
        })
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
        a @ EvalValue::Array(_) => ev.to_scalar(a),
        EvalValue::Scalar(v) => v,
    };
    match v {
        Value::Empty => Value::Number(0.0),
        Value::Number(n) if !n.is_finite() => Value::Error(ErrorKind::Num),
        other => other,
    }
}

/// Largest array a formula may spill.
const MAX_SPILL_CELLS: u64 = 1_000_000;
/// Rounds of re-evaluation after spills grow before giving up on further propagation.
const MAX_SPILL_ROUNDS: usize = 16;

fn normalize(v: Value) -> Value {
    match v {
        Value::Empty => Value::Number(0.0),
        Value::Number(n) if !n.is_finite() => Value::Error(ErrorKind::Num),
        other => other,
    }
}

/// Final value of the formula at `key`, and its array output: a legacy array formula fills
/// its fixed area; a dynamic one spills a multi-valued result (or is blocked).
fn finalize_formula(
    ev: &Ev<'_>,
    wb: &Workbook,
    key: Key,
    v: EvalValue,
) -> (Value, Option<SpillOut>) {
    let sheet = wb.sheet(key.0);
    let fixed = sheet
        .and_then(|s| s.spill_at_anchor(key.1))
        .filter(|a| a.fixed)
        .map(|a| a.range);
    if lambda_id(&v).is_some() {
        return (Value::Error(ErrorKind::Calc), None);
    }
    let multi = match &v {
        EvalValue::Array(a) => a.rows * a.cols != 1,
        EvalValue::Ref(r) => !r.range.is_single(),
        EvalValue::Refs(rs) => rs.len() == 1 && rs.iter().any(|r| !r.range.is_single()),
        EvalValue::Scalar(_) => false,
    };
    if let Some(f) = fixed {
        let arr = match ev.to_array(v) {
            Ok(a) => a,
            Err(e) => Array2::scalar(Value::Error(e)),
        };
        let vals: Vec<Value> = (0..f.rows() as usize)
            .flat_map(|r| (0..f.cols() as usize).map(move |c| (r, c)))
            .map(|(r, c)| normalize(arr.get_broadcast(r, c)))
            .collect();
        let first = vals.first().cloned().unwrap_or(Value::Number(0.0));
        return (first, Some(SpillOut::Spilled(f, vals)));
    }
    if !multi {
        return (finalize(ev, v), None);
    }
    let arr = match ev.to_array(v) {
        Ok(a) => a,
        Err(e) => return (Value::Error(e), None),
    };
    if arr.rows == 0 || arr.cols == 0 {
        return (Value::Error(ErrorKind::Calc), None);
    }
    let a = key.1;
    let end_row = u64::from(a.row) + arr.rows as u64 - 1;
    let end_col = u64::from(a.col) + arr.cols as u64 - 1;
    let range = CellRange::new(
        a,
        CellAddr::new(
            end_row.min(u64::from(MAX_ROWS - 1)) as u32,
            end_col.min(u64::from(MAX_COLS - 1)) as u32,
        ),
    );
    let off_sheet = end_row >= u64::from(MAX_ROWS) || end_col >= u64::from(MAX_COLS);
    let blocked = off_sheet
        || (arr.rows as u64) * (arr.cols as u64) > MAX_SPILL_CELLS
        || sheet.is_some_and(|s| {
            s.cells_in(range)
                .any(|(p, c)| p != a && !matches!(c.content, Content::Value(Value::Empty)))
                || s.merges.iter().any(|m| m.intersect(&range).is_some())
        });
    if blocked {
        return (
            Value::Error(ErrorKind::Spill),
            Some(SpillOut::Blocked(range)),
        );
    }
    let vals: Vec<Value> = arr.data.into_iter().map(normalize).collect();
    let first = vals.first().cloned().unwrap_or(Value::Number(0.0));
    (first, Some(SpillOut::Spilled(range, vals)))
}

/// Values and array outputs of one evaluation pass.
struct Computed {
    values: FxMap<Key, Value>,
    spills: FxMap<Key, SpillOut>,
}

/// Evaluate `order` (formula key + source) demand-driven; returns each formula's value.
fn compute(
    wb: &Workbook,
    cache: &mut AstCache,
    order: &[(Key, String)],
    dirty: Option<&FxSet<Key>>,
    opts: RecalcOptions,
    stats: &mut RecalcStats,
) -> Computed {
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
                    let (v, spill) = finalize_formula(&ev, wb, top, r);
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
                    if let Some(sp) = spill {
                        if let SpillOut::Spilled(r, _) = &sp {
                            host.spill_index.borrow_mut().push((top.0, *r));
                        }
                        host.spills.borrow_mut().insert(top, sp);
                    }
                    v
                }
            };
            host.computed.borrow_mut().insert(top, value);
        }
    }
    Computed {
        values: host.computed.into_inner(),
        spills: host.spills.into_inner(),
    }
}

/// Clear the spilled values of `area` (cells without content of their own), except `keep`.
fn clear_area(s: &mut sp_model::Sheet, area: CellRange, keep: Option<CellRange>) {
    let cells: Vec<CellAddr> = s
        .cells_in(area)
        .filter(|(a, c)| {
            *a != area.start
                && matches!(c.content, Content::Value(Value::Empty))
                && !keep.is_some_and(|k| k.contains(*a))
        })
        .map(|(a, _)| a)
        .collect();
    for a in cells {
        if let Some(c) = s.get_mut(a) {
            c.value = Value::Empty;
            if c.is_blank() {
                s.remove(a);
            }
        }
    }
}

fn set_area(s: &mut sp_model::Sheet, area: SpillArea) {
    s.spills.retain(|x| x.range.start != area.range.start);
    let at = s.spills.partition_point(|x| {
        (x.range.start.row, x.range.start.col) < (area.range.start.row, area.range.start.col)
    });
    s.spills.insert(at, area);
}

/// Write computed values and spills into the workbook. Returns cells newly covered by a
/// spill (readers evaluated earlier may have seen them blank).
fn write_back(wb: &mut Workbook, results: Computed) -> Vec<Key> {
    let mut grown = Vec::new();
    for (&(si, a), v) in &results.values {
        let Some(s) = wb.sheet_mut(si) else { continue };
        let Some(c) = s.get_mut(a) else { continue };
        c.value = v.clone();
        if c.formula_src().is_none() {
            continue;
        }
        let old = s.spill_at_anchor(a).copied();
        let old_live = old.filter(|o| !o.blocked).map(|o| o.range);
        match results.spills.get(&(si, a)) {
            None => {
                if let Some(o) = old {
                    if let Some(r) = old_live {
                        clear_area(s, r, None);
                    }
                    s.spills.retain(|x| x.range.start != o.range.start);
                }
            }
            Some(SpillOut::Blocked(r)) => {
                if let Some(o) = old_live {
                    clear_area(s, o, None);
                }
                set_area(
                    s,
                    SpillArea {
                        range: *r,
                        fixed: false,
                        blocked: true,
                    },
                );
            }
            Some(SpillOut::Spilled(r, vals)) => {
                if let Some(o) = old_live {
                    clear_area(s, o, Some(*r));
                }
                let cols = r.cols() as usize;
                for (i, v) in vals.iter().enumerate() {
                    let p = CellAddr::new(
                        r.start.row + (i / cols) as u32,
                        r.start.col + (i % cols) as u32,
                    );
                    if p == a {
                        continue;
                    }
                    if !old_live.is_some_and(|o| o.contains(p)) {
                        grown.push((si, p));
                    }
                    let cell = s.entry(p);
                    if matches!(cell.content, Content::Value(Value::Empty)) {
                        cell.value = v.clone();
                    }
                }
                set_area(
                    s,
                    SpillArea {
                        range: *r,
                        fixed: old.is_some_and(|o| o.fixed),
                        blocked: false,
                    },
                );
            }
        }
    }
    grown
}

/// Drop array areas whose anchor no longer holds a formula (clearing their values); the
/// cleared cells are returned so their dependents can be recomputed.
fn drop_dead_areas(wb: &mut Workbook, only: Option<&[Key]>) -> Vec<Key> {
    let mut cleared = Vec::new();
    for (si, s) in wb.sheets.iter_mut().enumerate() {
        if s.spills.is_empty() {
            continue;
        }
        let dead: Vec<SpillArea> = s
            .spills
            .iter()
            .filter(|x| {
                only.is_none_or(|o| o.contains(&(si, x.range.start)))
                    && s.get(x.range.start).and_then(|c| c.formula_src()).is_none()
            })
            .copied()
            .collect();
        for d in dead {
            if !d.blocked {
                cleared.extend(
                    s.cells_in(d.range)
                        .filter(|(a, c)| *a != d.range.start && c.is_spilled())
                        .map(|(a, _)| (si, a)),
                );
                clear_area(s, d.range, None);
            }
            s.spills.retain(|x| x.range.start != d.range.start);
        }
    }
    cleared
}

/// Re-evaluate dependents of cells newly covered by spills until the areas settle.
fn settle_spills(
    wb: &mut Workbook,
    cache: &mut AstCache,
    graph: &mut DepGraph,
    mut grown: Vec<Key>,
    opts: RecalcOptions,
    stats: &mut RecalcStats,
) {
    let mut rounds = 0;
    while !grown.is_empty() && rounds < MAX_SPILL_ROUNDS {
        rounds += 1;
        let Some(dirty) = graph.dirty_from(wb, &grown) else {
            return;
        };
        let mut order: Vec<(Key, String)> = dirty
            .iter()
            .filter_map(|k| {
                let src = wb.sheet(k.0)?.get(k.1)?.formula_src()?;
                Some((*k, src.to_string()))
            })
            .collect();
        if order.is_empty() {
            return;
        }
        order.sort_unstable_by_key(|(k, _)| *k);
        let results = compute(wb, cache, &order, Some(&dirty), opts, stats);
        grown = write_back(wb, results);
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
    drop_dead_areas(wb, None);
    let results = compute(wb, cache, &formulas, None, opts, &mut stats);
    let grown = write_back(wb, results);
    let mut graph = DepGraph::new(wb);
    for (key, src) in &formulas {
        let expr = cache.get(*key, src).ok();
        graph.insert(wb, *key, expr.as_deref());
    }
    graph.has_cycles = stats.cycles > 0;
    if !grown.is_empty() && !graph.has_cycles {
        settle_spills(wb, cache, &mut graph, grown, opts, &mut stats);
    }
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
    // Array areas: an edit inside one re-evaluates its anchor (to block or unblock it); a
    // removed anchor's spilled values disappear.
    let mut changed: Vec<Key> = changed.to_vec();
    let cleared = drop_dead_areas(wb, Some(&changed));
    let mut anchors: Vec<Key> = Vec::new();
    for &(si, a) in &changed {
        if let Some(s) = wb.sheet(si) {
            anchors.extend(
                s.spills
                    .iter()
                    .filter(|x| x.range.start != a && x.range.contains(a))
                    .map(|x| (si, x.range.start)),
            );
        }
    }
    changed.extend(cleared);
    changed.extend(anchors);
    let Some(dirty) = graph.dirty_from(wb, &changed) else {
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
    let grown = write_back(wb, results);
    settle_spills(wb, cache, &mut graph, grown, opts, &mut stats);
    if stats.cycles > 0 {
        return recalc(wb, cache, opts);
    }
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
            ("B5", "=INDEX((A1):(A3),1)"),
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

    fn n(x: f64) -> Value {
        Value::Number(x)
    }

    #[test]
    fn spill_fills_cells_and_spill_ref() {
        let wb = wb_with(&[
            ("A1", "=SEQUENCE(3)"),
            ("B1", "=SUM(A1#)"),
            ("C1", "=A3*10"),
            ("D1", "=ROWS(A1#)"),
            ("E1", "=SUM(A1:A3)"),
        ]);
        assert_eq!(v(&wb, "A2"), n(2.0));
        assert_eq!(v(&wb, "A3"), n(3.0));
        assert_eq!(v(&wb, "B1"), n(6.0));
        assert_eq!(v(&wb, "C1"), n(30.0));
        assert_eq!(v(&wb, "D1"), n(3.0));
        assert_eq!(v(&wb, "E1"), n(6.0));
        let s = &wb.sheets[0];
        assert!(s.get(CellAddr::parse("A2").unwrap()).unwrap().is_spilled());
        assert_eq!(s.spills.len(), 1);
        assert_eq!(s.spills[0].range.to_a1(), "A1:A3");
        // `A1#` on a cell that is not an anchor is #REF!.
        let wb = wb_with(&[("A1", "5"), ("B1", "=SUM(A1#)")]);
        assert_eq!(v(&wb, "B1"), Value::Error(ErrorKind::Ref));
    }

    #[test]
    fn blocked_spill_and_unblock() {
        let (mut wb, mut cache) = base(&[("A1", "=SEQUENCE(3)"), ("A3", "x"), ("B1", "=A2")]);
        assert_eq!(v(&wb, "A1"), Value::Error(ErrorKind::Spill));
        assert_eq!(v(&wb, "A2"), Value::Empty);
        assert_eq!(v(&wb, "B1"), n(0.0));
        assert!(wb.sheets[0].spills[0].blocked);
        // Clearing the blocker lets it spill (incremental).
        edit_and_compare(&mut wb, &mut cache, &[(0, "A3", "")]);
        assert_eq!(v(&wb, "A1"), n(1.0));
        assert_eq!(v(&wb, "B1"), n(2.0));
        // Typing into the spill area blocks it again.
        edit_and_compare(&mut wb, &mut cache, &[(0, "A2", "7")]);
        assert_eq!(v(&wb, "A1"), Value::Error(ErrorKind::Spill));
        assert_eq!(v(&wb, "A3"), Value::Empty);
        assert_eq!(v(&wb, "B1"), n(7.0));
        // Off the sheet edge.
        let wb = wb_with(&[("A1048575", "=SEQUENCE(3)")]);
        assert_eq!(v(&wb, "A1048575"), Value::Error(ErrorKind::Spill));
    }

    #[test]
    fn spill_resizes_incrementally() {
        let (mut wb, mut cache) = base(&[
            ("A1", "2"),
            ("B1", "=SEQUENCE(A1)"),
            ("C1", "=B4+1"),
            ("D1", "=SUM(B1#)"),
            ("E1", "=COUNT(B:B)"),
        ]);
        assert_eq!(v(&wb, "D1"), n(3.0));
        assert_eq!(v(&wb, "C1"), n(1.0));
        edit_and_compare(&mut wb, &mut cache, &[(0, "A1", "5")]);
        assert_eq!(v(&wb, "B5"), n(5.0));
        assert_eq!(v(&wb, "C1"), n(5.0));
        assert_eq!(v(&wb, "D1"), n(15.0));
        edit_and_compare(&mut wb, &mut cache, &[(0, "A1", "1")]);
        assert_eq!(v(&wb, "B2"), Value::Empty);
        assert_eq!(v(&wb, "C1"), n(1.0));
        assert_eq!(v(&wb, "E1"), n(1.0));
        // Spill feeding a spill.
        edit_and_compare(&mut wb, &mut cache, &[(0, "A1", "4"), (0, "F1", "=B1#*2")]);
        assert_eq!(v(&wb, "F4"), n(8.0));
        edit_and_compare(&mut wb, &mut cache, &[(0, "A1", "2")]);
        assert_eq!(v(&wb, "F3"), Value::Empty);
        // Removing the anchor clears its spill.
        edit_and_compare(&mut wb, &mut cache, &[(0, "B1", "")]);
        assert_eq!(v(&wb, "B2"), Value::Empty);
        assert!(wb.sheets[0].spills.iter().all(|s| s.range.start.col != 1));
    }

    #[test]
    fn random_spill_edits_match_full() {
        let (mut wb, mut cache) = base(&[("A1", "3")]);
        let inputs = [
            ("B1", "=SEQUENCE(A1)"),
            ("C2", "=B1#+1"),
            ("B3", "x"),
            ("B3", ""),
            ("A1", "6"),
            ("D1", "=FILTER(B1:B9,B1:B9>2,\"none\")"),
            ("E1", "=SUM(D1#)"),
            ("A1", "1"),
            ("C3", "=TRANSPOSE(B1:B4)"),
            ("A1", "5"),
            ("C2", ""),
        ];
        for (a, i) in inputs {
            edit_and_compare(&mut wb, &mut cache, &[(0, a, i)]);
        }
    }

    #[test]
    fn implicit_intersection() {
        let wb = wb_with(&[
            ("A1", "1"),
            ("A2", "2"),
            ("A3", "3"),
            ("B2", "=@A1:A3"),
            ("C2", "=@A1:A3*10"),
            ("D5", "=@A1:A3"),
            ("E2", "=SUM(@A:A)"),
            ("F2", "=@SEQUENCE(3)"),
        ]);
        assert_eq!(v(&wb, "B2"), n(2.0));
        assert_eq!(v(&wb, "C2"), n(20.0));
        assert_eq!(v(&wb, "D5"), Value::Error(ErrorKind::Value));
        assert_eq!(v(&wb, "E2"), n(2.0));
        assert_eq!(v(&wb, "F2"), n(1.0));
        assert_eq!(v(&wb, "F3"), Value::Empty);
        // Legacy rewriting adds `@` only where a value could be multi-cell.
        let legacy = |s: &str| crate::to_formula(&parse(s).unwrap().into_legacy());
        assert_eq!(legacy("A1:A3"), "@A1:A3");
        assert_eq!(legacy("A1:A3*2+B1"), "@A1:A3*2+B1");
        assert_eq!(legacy("SUM(A1:A3)"), "SUM(A1:A3)");
        assert_eq!(legacy("ABS(A1:A3)"), "@ABS(A1:A3)");
        assert_eq!(legacy("IF(A1,1,2)"), "IF(A1,1,2)");
        let e = parse("@A1:A3*2").unwrap();
        assert_eq!(crate::to_formula(&e.from_legacy().unwrap()), "A1:A3*2");
        assert!(parse("SEQUENCE(3)").unwrap().from_legacy().is_none());
    }

    #[test]
    fn fixed_array_area_broadcasts() {
        let mut wb = Workbook::new();
        let s = &mut wb.sheets[0];
        for r in 0..6 {
            s.set(CellAddr::new(r, 0), MCell::constant(n(f64::from(r) + 1.0)));
        }
        s.set(CellAddr::new(0, 1), MCell::formula("A1:A6"));
        s.set(CellAddr::new(0, 2), MCell::formula("123"));
        s.set(CellAddr::new(0, 3), MCell::formula("SUM(C1#)"));
        for (a, b) in [("B1:B3", true), ("C1:C4", true)] {
            s.spills.push(sp_model::SpillArea {
                range: CellRange::parse(a).unwrap(),
                fixed: b,
                blocked: false,
            });
        }
        recalc(&mut wb, &mut AstCache::default(), OPTS);
        assert_eq!(v(&wb, "B3"), n(3.0));
        assert_eq!(v(&wb, "B4"), Value::Empty);
        assert_eq!(v(&wb, "C4"), n(123.0));
        assert_eq!(v(&wb, "D1"), n(492.0));
    }

    #[test]
    fn array_functions() {
        let wb = wb_with(&[
            ("A1", "3"),
            ("A2", "1"),
            ("A3", "2"),
            ("A4", "1"),
            ("B1", "c"),
            ("B2", "a"),
            ("B3", "b"),
            ("B4", "A"),
            ("C1", "=TEXTJOIN(\",\",,SORT(A1:A4))"),
            ("C2", "=TEXTJOIN(\",\",,SORT(A1:A4,1,-1))"),
            ("C3", "=TEXTJOIN(\",\",,UNIQUE(A1:A4))"),
            ("C4", "=TEXTJOIN(\",\",,UNIQUE(B1:B4))"),
            ("C5", "=TEXTJOIN(\",\",,FILTER(B1:B4,A1:A4>1))"),
            ("C6", "=FILTER(A1:A4,A1:A4>9,\"none\")"),
            ("C7", "=FILTER(A1:A4,A1:A4>9)"),
            ("C8", "=TEXTJOIN(\",\",,SORTBY(B1:B4,A1:A4,-1))"),
            ("C9", "=TEXTJOIN(\",\",,TAKE(A1:A4,2))"),
            ("C10", "=TEXTJOIN(\",\",,TAKE(A1:A4,-1))"),
            ("C11", "=TEXTJOIN(\",\",,DROP(A1:A4,3))"),
            ("C12", "=DROP(A1:A4,4)"),
            (
                "C13",
                "=ROWS(VSTACK(A1:A2,A3:A4))*10+COLUMNS(HSTACK(A1:A4,B1:B4))",
            ),
            ("C14", "=TEXTJOIN(\",\",,TOROW(A1:B2))"),
            ("C15", "=TEXTJOIN(\",\",,TOCOL(A1:B2,,TRUE))"),
            ("C16", "=TEXTJOIN(\",\",FALSE,WRAPROWS(A1:A4,3,\"-\"))"),
            ("C17", "=ROWS(WRAPCOLS(A1:A4,3))"),
            ("C18", "=TEXTJOIN(\",\",,CHOOSECOLS(A1:B1,-1,1))"),
            ("C19", "=TEXTJOIN(\",\",,CHOOSEROWS(A1:A4,2,-1))"),
            ("C20", "=COUNTA(EXPAND(A1:A2,3,2,0))"),
            ("C21", "=ROWS(TRIMRANGE(A:A))"),
            ("C22", "=ARRAYTOTEXT(A1:B2,1)"),
            ("C23", "=ARRAYTOTEXT(A1:B2)"),
            ("C24", "=VALUETOTEXT(\"x\",1)"),
            ("C25", "=MDETERM(MUNIT(3)*4)"),
            ("C26", "=SUM(MMULT(MINVERSE({4,7;2,6}),{4,7;2,6}))"),
            ("C27", "=MINVERSE({1,2;2,4})"),
            ("C28", "=COUNT(RANDARRAY(2,3))"),
            ("C29", "=SUM(RANDARRAY(4,4,5,5,TRUE))"),
            ("E1", "=VSTACK(1,{2,3})"),
            ("C31", "=TAKE(A1:A4,0)"),
        ]);
        let t = |a: &str| v(&wb, a);
        assert_eq!(t("C1"), Value::text("1,1,2,3"));
        assert_eq!(t("C2"), Value::text("3,2,1,1"));
        assert_eq!(t("C3"), Value::text("3,1,2"));
        assert_eq!(t("C4"), Value::text("c,a,b"));
        assert_eq!(t("C5"), Value::text("c,b"));
        assert_eq!(t("C6"), Value::text("none"));
        assert_eq!(t("C7"), Value::Error(ErrorKind::Calc));
        assert_eq!(t("C8"), Value::text("c,b,a,A"));
        assert_eq!(t("C9"), Value::text("3,1"));
        assert_eq!(t("C10"), Value::text("1"));
        assert_eq!(t("C11"), Value::text("1"));
        assert_eq!(t("C12"), Value::Error(ErrorKind::Calc));
        assert_eq!(t("C13"), n(42.0));
        assert_eq!(t("C14"), Value::text("3,c,1,a"));
        assert_eq!(t("C15"), Value::text("3,1,c,a"));
        assert_eq!(t("C16"), Value::text("3,1,2,1,-,-"));
        assert_eq!(t("C17"), n(3.0));
        assert_eq!(t("C18"), Value::text("c,3"));
        assert_eq!(t("C19"), Value::text("1,1"));
        assert_eq!(t("C20"), n(6.0));
        assert_eq!(t("C21"), n(4.0));
        assert_eq!(t("C22"), Value::text("{3,\"c\";1,\"a\"}"));
        assert_eq!(t("C23"), Value::text("3, c, 1, a"));
        assert_eq!(t("C24"), Value::text("\"x\""));
        assert_eq!(t("C25"), n(64.0));
        match t("C26") {
            Value::Number(x) => assert!((x - 2.0).abs() < 1e-9),
            other => panic!("{other:?}"),
        }
        assert_eq!(t("C27"), Value::Error(ErrorKind::Num));
        assert_eq!(t("C28"), n(6.0));
        assert_eq!(t("C29"), n(80.0));
        assert_eq!(t("F1"), Value::Error(ErrorKind::NA));
        assert_eq!(t("F2"), n(3.0));
        assert_eq!(t("C31"), Value::Error(ErrorKind::Calc));
    }

    #[test]
    fn lambdas() {
        let mut wb = Workbook::new();
        wb.names.push(sp_model::DefinedName {
            name: "Hyp".into(),
            formula: "LAMBDA(x,y,SQRT(x*x+y*y))".into(),
            scope: None,
        });
        for (a, input) in [
            ("A1", "1"),
            ("A2", "2"),
            ("A3", "3"),
            ("B1", "=LAMBDA(x,x+1)(5)"),
            ("B2", "=LET(f,LAMBDA(a,b,a*b),f(6,7))"),
            ("B3", "=Hyp(3,4)"),
            ("B4", "=LAMBDA(x,y,IF(ISOMITTED(y),\"none\",x+y))(1,)"),
            ("B5", "=LAMBDA(x,y,IF(ISOMITTED(y),x*2,x+y))(4)"),
            ("B6", "=TEXTJOIN(\",\",,MAP(A1:A3,LAMBDA(v,v*v)))"),
            ("B7", "=REDUCE(0,A1:A3,LAMBDA(acc,v,acc+v))"),
            ("B8", "=TEXTJOIN(\",\",,SCAN(0,A1:A3,LAMBDA(acc,v,acc+v)))"),
            ("B9", "=TEXTJOIN(\",\",,BYROW({1,2;3,4},LAMBDA(r,SUM(r))))"),
            ("B10", "=TEXTJOIN(\",\",,BYCOL({1,2;3,4},LAMBDA(c,MAX(c))))"),
            ("B11", "=SUM(MAKEARRAY(3,3,LAMBDA(r,c,r*c)))"),
            ("B12", "=LAMBDA(x,x)"),
            ("B13", "=MAP(A1:A3,LAMBDA(a,b,a))"),
            ("B14", "=TEXTJOIN(\",\",,BYROW({1,2;3,4},SUM))"),
            ("B15", "=LET(k,10,f,LAMBDA(x,x*k),f(2))"),
            ("B16", "=LAMBDA(x,x)(1,2)"),
            ("B17", "=ISOMITTED(A1)"),
        ] {
            let addr = CellAddr::parse(a).unwrap();
            let cell = match input.strip_prefix('=') {
                Some(f) => MCell::formula(f),
                None => MCell::constant(n(input.parse().unwrap())),
            };
            wb.sheets[0].set(addr, cell);
        }
        recalc(&mut wb, &mut AstCache::default(), OPTS);
        let t = |a: &str| v(&wb, a);
        assert_eq!(t("B1"), n(6.0));
        assert_eq!(t("B2"), n(42.0));
        assert_eq!(t("B3"), n(5.0));
        assert_eq!(t("B4"), Value::text("none"));
        assert_eq!(t("B5"), n(8.0));
        assert_eq!(t("B6"), Value::text("1,4,9"));
        assert_eq!(t("B7"), n(6.0));
        assert_eq!(t("B8"), Value::text("1,3,6"));
        assert_eq!(t("B9"), Value::text("3,7"));
        assert_eq!(t("B10"), Value::text("3,4"));
        assert_eq!(t("B11"), n(36.0));
        assert_eq!(t("B12"), Value::Error(ErrorKind::Calc));
        assert_eq!(t("B13"), Value::Error(ErrorKind::Value));
        assert_eq!(t("B14"), Value::text("3,7"));
        assert_eq!(t("B15"), n(20.0));
        assert_eq!(t("B16"), Value::Error(ErrorKind::Value));
        assert_eq!(t("B17"), Value::Bool(false));
    }

    #[test]
    fn runaway_lambda_recursion_terminates() {
        let mut wb = Workbook::new();
        wb.names.push(sp_model::DefinedName {
            name: "Loop".into(),
            formula: "LAMBDA(x,Loop(x+1))".into(),
            scope: None,
        });
        wb.sheets[0].set(CellAddr::new(0, 0), MCell::formula("Loop(1)"));
        wb.sheets[0].set(
            CellAddr::new(1, 0),
            MCell::formula("MAKEARRAY(100000,100000,LAMBDA(r,c,1))"),
        );
        recalc(&mut wb, &mut AstCache::default(), OPTS);
        assert!(matches!(v(&wb, "A1"), Value::Error(_)));
        assert!(matches!(v(&wb, "A2"), Value::Error(_)));
    }

    #[test]
    fn subtotal_skips_nested_and_hidden_rows() {
        let mut wb = wb_with(&[
            ("A1", "Head"),
            ("A2", "1"),
            ("A3", "2"),
            ("A4", "=SUBTOTAL(9,A2:A3)"),
            ("A5", "10"),
            ("A6", "20"),
            ("A7", "=SUBTOTAL(9,A5:A6)"),
            ("A8", "=SUBTOTAL(9,A2:A7)"),
            ("B1", "=SUBTOTAL(109,A2:A7)"),
            ("B2", "=SUBTOTAL(3,A1:A7)"),
            ("B3", "=SUBTOTAL(103,A1:A7)"),
            ("B4", "=SUBTOTAL(1,A2:A7)"),
            ("B5", "=SUBTOTAL(9,A2:A3,A5)"),
            ("B6", "=AGGREGATE(9,0,A2:A8)"),
            ("B7", "=SUBTOTAL(4,C1:C3)"),
            ("B8", "=SUBTOTAL(1,C1:C3)"),
        ]);
        assert_eq!(v(&wb, "A8"), Value::Number(33.0));
        assert_eq!(v(&wb, "B2"), Value::Number(5.0));
        assert_eq!(v(&wb, "B4"), Value::Number(8.25));
        assert_eq!(v(&wb, "B5"), Value::Number(13.0));
        assert_eq!(v(&wb, "B6"), Value::Number(33.0));
        assert_eq!(v(&wb, "B7"), Value::Number(0.0));
        assert_eq!(v(&wb, "B8"), Value::Error(ErrorKind::Div0));
        // Manually hidden row 3 and 6: 1xx codes skip them, 1–11 do not.
        wb.sheets[0].hidden_rows.extend([2, 5]);
        let opts = RecalcOptions {
            now: Some(45306.5),
            seed: 7,
        };
        recalc(&mut wb, &mut AstCache::default(), opts);
        assert_eq!(v(&wb, "A8"), Value::Number(33.0));
        assert_eq!(v(&wb, "B1"), Value::Number(11.0));
        assert_eq!(v(&wb, "B3"), Value::Number(3.0));
        // Inside an AutoFilter range hidden rows are filtered: every code skips them.
        wb.sheets[0].auto_filter = Some(sp_model::AutoFilter {
            range: CellRange::parse("A1:A7").unwrap(),
            columns: vec![],
        });
        recalc(&mut wb, &mut AstCache::default(), opts);
        assert_eq!(v(&wb, "A8"), Value::Number(11.0));
        assert_eq!(v(&wb, "B2"), Value::Number(3.0));
    }
}
