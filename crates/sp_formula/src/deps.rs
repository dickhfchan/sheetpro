//! Dependency graph for incremental recalculation.
//!
//! Maps precedents to the formula cells that read them: single cells through a hash index,
//! narrow ranges through per-column interval lists, wide ranges (and whole rows) through a
//! per-sheet list. Whole-row/column references also depend on the sheet's used range
//! (the evaluator clamps them to it), so they are re-dirtied whenever that range moves.
//! Volatile formulas (`Expr::is_volatile`, which covers INDIRECT and OFFSET) and formulas
//! whose references cannot be resolved statically are always dirty.
//!
//! Index entries are invalidated lazily: each formula node carries a generation, and an
//! entry is live only while its generation matches. Stale entries are compacted in bulk.

use crate::ast::{BinOp, Expr, RefKind};
use crate::parser::parse;
use sp_core::{CellAddr, CellRange, MAX_COLS, MAX_ROWS};
use sp_model::Workbook;
use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::Arc;

/// Small fast non-cryptographic hasher (FxHash scheme) for the cell-keyed maps of recalc.
#[derive(Debug, Default, Clone, Copy)]
pub struct FxHasher {
    hash: u64,
}

impl FxHasher {
    #[inline]
    fn add(&mut self, w: u64) {
        self.hash = (self.hash.rotate_left(5) ^ w).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
}

impl Hasher for FxHasher {
    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut b = [0u8; 8];
            for (d, s) in b.iter_mut().zip(chunk) {
                *d = *s;
            }
            self.add(u64::from_le_bytes(b));
        }
    }
    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.add(u64::from(i));
    }
    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.add(u64::from(i));
    }
    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.add(i);
    }
    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.add(i as u64);
    }
    #[inline]
    fn finish(&self) -> u64 {
        self.hash
    }
}

pub type FxBuild = BuildHasherDefault<FxHasher>;
pub type FxMap<K, V> = HashMap<K, V, FxBuild>;
pub type FxSet<K> = HashSet<K, FxBuild>;

pub type Key = (usize, CellAddr);

/// Ranges spanning more columns than this go to the per-sheet wide list.
const WIDE_COLS: u32 = 32;
/// Nested defined-name expansion limit; deeper chains are treated as always dirty.
const MAX_NAME_DEPTH: u32 = 16;
/// Expression nodes visited while collecting one formula's precedents (name blow-up guard).
const MAX_COLLECT_NODES: usize = 100_000;
/// Range-index probe work allowed per propagation before falling back to a full recalc.
const MAX_PROBE_WORK: u64 = 50_000_000;

/// Statically known precedents of one formula.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Precedents {
    pub cells: Vec<Key>,
    pub ranges: Vec<(usize, CellRange)>,
    /// Recompute on every recalc (volatile, dynamic reference, or unresolvable).
    pub always: bool,
}

type NameMap = HashMap<(String, Option<usize>), Arc<Expr>>;

struct Collector<'a> {
    wb: &'a Workbook,
    names: &'a NameMap,
    sheet: usize,
    /// The formula's cell, for unqualified table references (`[@Col]`).
    cell: Option<CellAddr>,
    /// Shared-formula offset applied to relative reference parts (see `Ev::with_origin`).
    delta: (i64, i64),
    visited: usize,
}

impl Collector<'_> {
    /// A reference part moved by the shared-formula offset (`None`: off the sheet).
    fn part(&self, c: &crate::ast::CellPart) -> Option<CellAddr> {
        let (dr, dc) = self.delta;
        let mv = |v: u32, abs: bool, d: i64, max: u32| -> Option<u32> {
            if abs || d == 0 {
                return Some(v);
            }
            let n = i64::from(v).checked_add(d)?;
            (0..i64::from(max)).contains(&n).then_some(n as u32)
        };
        Some(CellAddr::new(
            mv(c.row, c.row_abs, dr, MAX_ROWS)?,
            mv(c.col, c.col_abs, dc, MAX_COLS)?,
        ))
    }

    fn add(&self, out: &mut Precedents, sheet: usize, rg: CellRange) {
        if rg.is_single() {
            out.cells.push((sheet, rg.start));
        } else {
            out.ranges.push((sheet, rg));
        }
    }

    fn collect(&mut self, e: &Expr, depth: u32, out: &mut Precedents) {
        self.visited += 1;
        if self.visited > MAX_COLLECT_NODES {
            out.always = true;
            return;
        }
        match e {
            Expr::Ref(r) => {
                let sheet = match &r.sheet {
                    None => Some(self.sheet),
                    Some(s) => self.wb.sheet_index(s),
                };
                let Some(sheet) = sheet else {
                    out.always = true;
                    return;
                };
                let rg = match &r.kind {
                    // Off the sheet: the formula evaluates to #REF!, depending on nothing.
                    RefKind::Cell(c) => match self.part(c) {
                        Some(a) => CellRange::single(a),
                        None => return,
                    },
                    RefKind::Area(a, b) => match (self.part(a), self.part(b)) {
                        (Some(a), Some(b)) => CellRange::new(a, b),
                        _ => return,
                    },
                    RefKind::Cols { c0, c1, .. } => {
                        CellRange::new(CellAddr::new(0, *c0), CellAddr::new(MAX_ROWS - 1, *c1))
                    }
                    RefKind::Rows { r0, r1, .. } => {
                        CellRange::new(CellAddr::new(*r0, 0), CellAddr::new(*r1, MAX_COLS - 1))
                    }
                };
                self.add(out, sheet, rg);
            }
            Expr::Table(t) => {
                let cell = self.cell.unwrap_or_default();
                let found = crate::structref::find_table(self.wb, t, self.sheet, cell);
                match found.map(|(s, tb)| (s, crate::structref::resolve(tb, t, None))) {
                    Some((s, Ok(rg))) => self.add(out, s, rg),
                    Some((_, Err(_))) => {}
                    None => out.always = true,
                }
            }
            Expr::Name(n) => {
                let k = n.to_lowercase();
                let found = self
                    .names
                    .get(&(k.clone(), Some(self.sheet)))
                    .or_else(|| self.names.get(&(k, None)))
                    .cloned();
                if let Some(expr) = found {
                    if depth >= MAX_NAME_DEPTH || expr.is_volatile() {
                        out.always = true;
                        return;
                    }
                    // A name's formula is not moved by the shared-formula offset.
                    let saved = std::mem::take(&mut self.delta);
                    self.collect(&expr, depth + 1, out);
                    self.delta = saved;
                }
            }
            Expr::Binary(BinOp::Range, a, b) => {
                // `x:y` spans the bounding box of both operands' references.
                let mut inner = Precedents::default();
                self.collect(a, depth, &mut inner);
                self.collect(b, depth, &mut inner);
                let mut bbox: Option<(usize, CellRange)> = None;
                let all = inner
                    .cells
                    .iter()
                    .map(|(s, a)| (*s, CellRange::single(*a)))
                    .chain(inner.ranges.iter().copied());
                for (s, r) in all {
                    bbox = match bbox {
                        None => Some((s, r)),
                        Some((bs, br)) if bs == s => Some((
                            bs,
                            CellRange::new(
                                CellAddr::new(
                                    br.start.row.min(r.start.row),
                                    br.start.col.min(r.start.col),
                                ),
                                CellAddr::new(br.end.row.max(r.end.row), br.end.col.max(r.end.col)),
                            ),
                        )),
                        Some(other) => {
                            out.always = true;
                            Some(other)
                        }
                    };
                }
                out.always |= inner.always;
                out.cells.extend(inner.cells);
                out.ranges.extend(inner.ranges);
                if let Some((s, r)) = bbox {
                    self.add(out, s, r);
                }
            }
            Expr::Unary(_, a) | Expr::Spill(a) | Expr::Paren(a) | Expr::Implicit(a) => {
                self.collect(a, depth, out)
            }
            Expr::Invoke(callee, args) => {
                self.collect(callee, depth, out);
                for a in args {
                    self.collect(a, depth, out);
                }
            }
            Expr::Binary(_, a, b) => {
                self.collect(a, depth, out);
                self.collect(b, depth, out);
            }
            Expr::Call(name, args) => {
                // A defined name holding a LAMBDA, called by name.
                if !self.names.is_empty() && crate::functions::registry().get(name).is_none() {
                    self.collect(&Expr::Name(name.clone()), depth, out);
                }
                for a in args {
                    self.collect(a, depth, out);
                }
            }
            Expr::Array(rows) => {
                for a in rows.iter().flatten() {
                    self.collect(a, depth, out);
                }
            }
            Expr::Number(_) | Expr::Text(_) | Expr::Bool(_) | Expr::Error(_) | Expr::Missing => {}
        }
    }
}

/// One line per table: structured references resolve against these.
fn table_sig(wb: &Workbook) -> Vec<String> {
    let mut v = Vec::new();
    for (i, s) in wb.sheets.iter().enumerate() {
        for t in &s.tables {
            let cols: Vec<&str> = t.columns.iter().map(|c| c.name.as_str()).collect();
            v.push(format!(
                "{i}|{}|{}|{}{}|{}",
                t.name,
                t.range.to_a1(),
                u8::from(t.header_row),
                u8::from(t.totals_row),
                cols.join("\u{1}")
            ));
        }
    }
    v
}

fn name_map(wb: &Workbook) -> NameMap {
    let mut names = HashMap::new();
    for n in &wb.names {
        if let Ok(e) = parse(&n.formula) {
            names.insert((n.name.to_lowercase(), n.scope), Arc::new(e));
        }
    }
    names
}

fn collect_with(
    wb: &Workbook,
    names: &NameMap,
    sheet: usize,
    cell: Option<CellAddr>,
    origin: Option<CellAddr>,
    expr: &Expr,
) -> Precedents {
    let delta = match (cell, origin) {
        (Some(c), Some(o)) => (
            i64::from(c.row) - i64::from(o.row),
            i64::from(c.col) - i64::from(o.col),
        ),
        _ => (0, 0),
    };
    let mut out = Precedents {
        always: expr.is_volatile(),
        ..Precedents::default()
    };
    let mut c = Collector {
        wb,
        names,
        sheet,
        cell,
        delta,
        visited: 0,
    };
    c.collect(expr, 0, &mut out);
    out.cells.sort_unstable();
    out.cells.dedup();
    out.ranges
        .sort_unstable_by_key(|(s, r)| (*s, r.start, r.end));
    out.ranges.dedup();
    out
}

/// Precedents of a formula on `sheet`, with defined names resolved against `wb`.
pub fn precedents(wb: &Workbook, sheet: usize, expr: &Expr) -> Precedents {
    collect_with(wb, &name_map(wb), sheet, None, None, expr)
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    dep: Key,
    generation: u64,
}

/// Dependents of one precedent cell; most cells feed a single formula.
#[derive(Debug, Clone)]
enum Deps {
    One(Entry),
    Many(Vec<Entry>),
}

impl Deps {
    fn as_slice(&self) -> &[Entry] {
        match self {
            Deps::One(e) => std::slice::from_ref(e),
            Deps::Many(v) => v,
        }
    }

    fn push(&mut self, e: Entry) {
        match self {
            Deps::One(first) => *self = Deps::Many(vec![*first, e]),
            Deps::Many(v) => v.push(e),
        }
    }

    /// Keep live entries; false if none remain.
    fn retain(&mut self, live: impl Fn(&Entry) -> bool) -> bool {
        match self {
            Deps::One(e) => live(e),
            Deps::Many(v) => {
                v.retain(|e| live(e));
                !v.is_empty()
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct RangeEntry {
    range: CellRange,
    dep: Key,
    generation: u64,
}

#[derive(Debug)]
struct Node {
    generation: u64,
    entries: usize,
    always: bool,
    full_sheets: Vec<usize>,
}

/// Precedent → dependents index over every formula of a workbook.
#[derive(Debug, Default)]
pub struct DepGraph {
    nodes: FxMap<Key, Node>,
    cells: FxMap<Key, Deps>,
    cols: FxMap<(usize, u32), Vec<RangeEntry>>,
    wide: FxMap<usize, Vec<RangeEntry>>,
    always: FxSet<Key>,
    /// Formulas with whole-row/column refs into a sheet (sensitive to its used range).
    full: FxMap<usize, FxSet<Key>>,
    used: Vec<Option<CellRange>>,
    sheet_names: Vec<String>,
    /// Table names, places and columns the graph was built against.
    tables: Vec<String>,
    defined: Vec<(String, String, Option<usize>)>,
    names: NameMap,
    next_generation: u64,
    live: usize,
    stale: usize,
    /// The last recalculation met a circular reference; incremental recalc defers to full.
    pub has_cycles: bool,
}

impl DepGraph {
    /// Empty graph bound to `wb`'s current sheets, names and used ranges.
    pub fn new(wb: &Workbook) -> Self {
        DepGraph {
            used: wb.sheets.iter().map(sp_model::Sheet::used_range).collect(),
            sheet_names: wb.sheets.iter().map(|s| s.name.clone()).collect(),
            tables: table_sig(wb),
            defined: wb
                .names
                .iter()
                .map(|n| (n.name.clone(), n.formula.clone(), n.scope))
                .collect(),
            names: name_map(wb),
            ..DepGraph::default()
        }
    }

    /// True if sheets and defined names are those the graph was built against.
    pub fn matches(&self, wb: &Workbook) -> bool {
        self.sheet_names.len() == wb.sheets.len()
            && self
                .sheet_names
                .iter()
                .zip(&wb.sheets)
                .all(|(a, s)| *a == s.name)
            && self.defined.len() == wb.names.len()
            && self
                .defined
                .iter()
                .zip(&wb.names)
                .all(|(d, n)| d.0 == n.name && d.1 == n.formula && d.2 == n.scope)
            && self.tables == table_sig(wb)
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn contains(&self, key: Key) -> bool {
        self.nodes.contains_key(&key)
    }

    fn is_live(&self, dep: Key, generation: u64) -> bool {
        self.nodes
            .get(&dep)
            .is_some_and(|n| n.generation == generation)
    }

    /// Register formula `key`. `expr` is `None` for a formula that failed to parse.
    pub fn insert(&mut self, wb: &Workbook, key: Key, expr: Option<&Expr>) {
        self.insert_at(wb, key, expr, None);
    }

    /// Register formula `key` whose tree was written for a formula at `origin` (a shared
    /// tree; `None`: written for `key` itself).
    pub fn insert_at(
        &mut self,
        wb: &Workbook,
        key: Key,
        expr: Option<&Expr>,
        origin: Option<CellAddr>,
    ) {
        self.remove(key);
        let p = match expr {
            Some(e) => collect_with(wb, &self.names, key.0, Some(key.1), origin, e),
            None => Precedents::default(),
        };
        self.next_generation += 1;
        let generation = self.next_generation;
        let mut entries = 0usize;
        for c in &p.cells {
            let e = Entry {
                dep: key,
                generation,
            };
            self.cells
                .entry(*c)
                .and_modify(|d| d.push(e))
                .or_insert(Deps::One(e));
            entries += 1;
        }
        let mut full_sheets = Vec::new();
        for (s, r) in &p.ranges {
            let e = RangeEntry {
                range: *r,
                dep: key,
                generation,
            };
            if r.is_full_cols() || r.is_full_rows() {
                full_sheets.push(*s);
            }
            if r.cols() > WIDE_COLS {
                self.wide.entry(*s).or_default().push(e);
                entries += 1;
            } else {
                for c in r.start.col..=r.end.col {
                    self.cols.entry((*s, c)).or_default().push(e);
                    entries += 1;
                }
            }
        }
        full_sheets.sort_unstable();
        full_sheets.dedup();
        for s in &full_sheets {
            self.full.entry(*s).or_default().insert(key);
        }
        if p.always {
            self.always.insert(key);
        }
        self.live += entries;
        self.nodes.insert(
            key,
            Node {
                generation,
                entries,
                always: p.always,
                full_sheets,
            },
        );
    }

    /// Forget formula `key` (no-op if it is not registered).
    pub fn remove(&mut self, key: Key) {
        let Some(n) = self.nodes.remove(&key) else {
            return;
        };
        if n.always {
            self.always.remove(&key);
        }
        for s in &n.full_sheets {
            if let Some(set) = self.full.get_mut(s) {
                set.remove(&key);
            }
        }
        self.live = self.live.saturating_sub(n.entries);
        self.stale += n.entries;
        if self.stale > self.live + 65_536 {
            self.compact();
        }
    }

    /// Drop stale index entries.
    fn compact(&mut self) {
        let nodes = &self.nodes;
        let live = |dep: &Key, g: u64| nodes.get(dep).is_some_and(|n| n.generation == g);
        self.cells
            .retain(|_, d| d.retain(|e| live(&e.dep, e.generation)));
        self.cols.retain(|_, v| {
            v.retain(|e| live(&e.dep, e.generation));
            !v.is_empty()
        });
        self.wide.retain(|_, v| {
            v.retain(|e| live(&e.dep, e.generation));
            !v.is_empty()
        });
        self.stale = 0;
    }

    /// Formula cells that must be recomputed after `changed` cells were edited: changed
    /// formulas, always-dirty formulas, and everything transitively depending on them.
    /// `None` if propagation got too expensive (caller should recalc in full).
    pub fn dirty_from(&mut self, wb: &Workbook, changed: &[Key]) -> Option<FxSet<Key>> {
        let mut dirty: FxSet<Key> = FxSet::default();
        let mut queue: Vec<Key> = Vec::new();
        // Spill areas: when an anchor is recomputed, its spilled cells change too.
        let areas: FxMap<Key, CellRange> = wb
            .sheets
            .iter()
            .enumerate()
            .flat_map(|(si, s)| {
                s.spills
                    .iter()
                    .filter(|x| !x.blocked)
                    .map(move |x| ((si, x.range.start), x.range))
            })
            .collect();
        for &k in changed {
            if self.nodes.contains_key(&k) {
                dirty.insert(k);
            }
            queue.push(k);
        }
        for &k in &self.always {
            if dirty.insert(k) {
                queue.push(k);
            }
        }
        // Whole-row/column refs are clamped to the used range: re-dirty them when it moves.
        let mut sheets: Vec<usize> = changed.iter().map(|k| k.0).collect();
        sheets.sort_unstable();
        sheets.dedup();
        for s in sheets {
            let now = wb.sheet(s).and_then(sp_model::Sheet::used_range);
            let Some(slot) = self.used.get_mut(s) else {
                continue;
            };
            if *slot != now {
                *slot = now;
                if let Some(set) = self.full.get(&s) {
                    for &k in set {
                        if dirty.insert(k) {
                            queue.push(k);
                        }
                    }
                }
            }
        }

        let mut work: u64 = 0;
        loop {
            // Phase 1: exhaust single-cell edges, recording positions for range probes.
            let mut col_probes: FxMap<(usize, u32), Vec<u32>> = FxMap::default();
            let mut sheet_probes: FxMap<usize, Vec<CellAddr>> = FxMap::default();
            while let Some(p) = queue.pop() {
                if let Some(r) = areas.get(&p) {
                    work += r.cell_count();
                    if work > MAX_PROBE_WORK {
                        return None;
                    }
                    queue.extend(r.iter().filter(|a| *a != p.1).map(|a| (p.0, a)));
                }
                if let Some(d) = self.cells.get(&p) {
                    for e in d.as_slice() {
                        work += 1;
                        if self.is_live(e.dep, e.generation) && dirty.insert(e.dep) {
                            queue.push(e.dep);
                        }
                    }
                }
                if self.cols.contains_key(&(p.0, p.1.col)) {
                    col_probes.entry((p.0, p.1.col)).or_default().push(p.1.row);
                }
                if self.wide.contains_key(&p.0) {
                    sheet_probes.entry(p.0).or_default().push(p.1);
                }
            }
            // Phase 2: probe range lists in bulk against the positions changed this round.
            for ((s, c), mut rows) in col_probes {
                rows.sort_unstable();
                rows.dedup();
                let Some(v) = self.cols.get(&(s, c)) else {
                    continue;
                };
                work += v.len() as u64;
                for e in v {
                    if dirty.contains(&e.dep) || !self.is_live(e.dep, e.generation) {
                        continue;
                    }
                    let i = rows.partition_point(|r| *r < e.range.start.row);
                    if rows.get(i).is_some_and(|r| *r <= e.range.end.row) && dirty.insert(e.dep) {
                        queue.push(e.dep);
                    }
                }
            }
            for (s, addrs) in sheet_probes {
                let Some(v) = self.wide.get(&s) else {
                    continue;
                };
                for e in v {
                    if dirty.contains(&e.dep) || !self.is_live(e.dep, e.generation) {
                        continue;
                    }
                    work += addrs.len() as u64;
                    if addrs.iter().any(|a| e.range.contains(*a)) && dirty.insert(e.dep) {
                        queue.push(e.dep);
                    }
                }
            }
            if work > MAX_PROBE_WORK {
                return None;
            }
            if queue.is_empty() {
                break;
            }
        }
        Some(dirty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sp_model::Cell;

    fn key(a: &str) -> Key {
        (0, CellAddr::parse(a).unwrap())
    }

    fn graph(cells: &[(&str, &str)]) -> (Workbook, DepGraph) {
        let mut wb = Workbook::new();
        for (a, f) in cells {
            wb.sheets[0].set(CellAddr::parse(a).unwrap(), Cell::formula(*f));
        }
        let mut g = DepGraph::new(&wb);
        for (a, f) in cells {
            let e = parse(f).ok();
            g.insert(&wb, key(a), e.as_ref());
        }
        (wb, g)
    }

    fn dirty(wb: &Workbook, g: &mut DepGraph, changed: &[&str]) -> Vec<String> {
        let ks: Vec<Key> = changed.iter().map(|a| key(a)).collect();
        let mut v: Vec<Key> = g.dirty_from(wb, &ks).unwrap().into_iter().collect();
        v.sort_unstable();
        v.into_iter().map(|(_, a)| a.to_a1()).collect()
    }

    #[test]
    fn precedents_cover_refs_ranges_and_names() {
        let mut wb = Workbook::new();
        wb.sheets.push(sp_model::Sheet::new("Data"));
        wb.names.push(sp_model::DefinedName {
            name: "Rate".into(),
            formula: "Data!$B$2".into(),
            scope: None,
        });
        let e = parse("A1+SUM(B1:B3)+Rate+C:C+(D1:E2):F5").unwrap();
        let p = precedents(&wb, 0, &e);
        assert!(p.cells.contains(&(0, CellAddr::parse("A1").unwrap())));
        assert!(p.cells.contains(&(1, CellAddr::parse("B2").unwrap())));
        assert!(p.ranges.contains(&(0, CellRange::parse("B1:B3").unwrap())));
        assert!(p.ranges.contains(&(0, CellRange::parse("C:C").unwrap())));
        assert!(p.ranges.contains(&(0, CellRange::parse("D1:F5").unwrap())));
        assert!(!p.always);
        assert!(precedents(&wb, 0, &parse("INDIRECT(\"A1\")").unwrap()).always);
        assert!(precedents(&wb, 0, &parse("OFFSET(A1,1,1)").unwrap()).always);
        assert!(precedents(&wb, 0, &parse("NoSheet!A1").unwrap()).always);
    }

    #[test]
    fn propagation_follows_cells_ranges_and_columns() {
        let (wb, mut g) = graph(&[
            ("B1", "A1+1"),
            ("C1", "B1*2"),
            ("D1", "SUM(A1:A10)"),
            ("E1", "SUM(A:A)"),
            ("F1", "SUM(1:1)"),
            ("G1", "Z99"),
            ("H1", "RAND()"),
            ("I1", "H1+1"),
        ]);
        // Row 1 holds every formula, so anything dirty also dirties F1 = SUM(1:1).
        assert_eq!(
            dirty(&wb, &mut g, &["A5"]),
            vec!["D1", "E1", "F1", "H1", "I1"]
        );
        assert_eq!(
            dirty(&wb, &mut g, &["A1"]),
            vec!["B1", "C1", "D1", "E1", "F1", "H1", "I1"]
        );
        assert_eq!(dirty(&wb, &mut g, &["Z99"]), vec!["F1", "G1", "H1", "I1"]);
        assert_eq!(dirty(&wb, &mut g, &["B2"]), vec!["F1", "H1", "I1"]);
    }

    #[test]
    fn removed_formulas_stop_propagating() {
        let (wb, mut g) = graph(&[("B1", "A1+1"), ("C1", "SUM(A1:A3)")]);
        g.remove(key("B1"));
        g.remove(key("C1"));
        assert!(dirty(&wb, &mut g, &["A1"]).is_empty());
        g.insert(&wb, key("C1"), parse("A2").ok().as_ref());
        assert!(dirty(&wb, &mut g, &["A1"]).is_empty());
        assert_eq!(dirty(&wb, &mut g, &["A2"]), vec!["C1"]);
    }

    #[test]
    fn many_removals_compact() {
        let mut wb = Workbook::new();
        wb.sheets[0].set(CellAddr::new(0, 1), Cell::formula("A1"));
        let mut g = DepGraph::new(&wb);
        let e = parse("SUM($A$1:$A$5)").unwrap();
        for i in 0..200_000u32 {
            let k = (0, CellAddr::new(i % 100, 2));
            g.insert(&wb, k, Some(&e));
        }
        assert_eq!(g.len(), 100);
        assert!(g.stale <= g.live + 65_536);
        assert_eq!(dirty(&wb, &mut g, &["A3"]).len(), 100);
    }
}
