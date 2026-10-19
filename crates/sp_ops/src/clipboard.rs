//! Clipboard: copy (visible rows of filtered ranges only), paste with tiling, Paste Special
//! (values / formulas / formats, arithmetic operations, skip blanks, transpose) and cut, which
//! is a move: the cells, their formats, notes, merges, validation and conditional formats go
//! to the destination and every reference to the moved cells anywhere in the workbook
//! follows them, while references to the cells pasted over become `#REF!`.

use crate::edit::{OpError, rewrite_all_formulas, shifted_formula};
use crate::history::Tx;
use sp_core::{CellAddr, CellRange, ErrorKind, MAX_CELLS_PER_OP, MAX_COLS, MAX_ROWS, Value};
use sp_formula::ast::{CellPart, RefKind};
use sp_formula::shift::shift_for_copy;
use sp_formula::{Expr, parse, to_formula};
use sp_model::{Cell, Comment, CondFormat, Content, DataValidation, Hyperlink, Sheet, StyleId};
use std::collections::HashMap;

/// Copied cells and the features that travel with them.
#[derive(Debug, Clone)]
pub struct ClipData {
    pub sheet: usize,
    /// The selected source range.
    pub range: CellRange,
    /// Cells by (row, col) offset inside the copied block.
    pub cells: Vec<(u32, u32, Cell)>,
    pub cut: bool,
    /// Source row of each block row: rows hidden by a filter are not copied, so the block
    /// can be shorter than `range`.
    pub src_rows: Vec<u32>,
    /// Merges, notes, validation and conditional formats inside the range (source
    /// coordinates; rule formulas are written for the rule's first range).
    pub merges: Vec<CellRange>,
    pub comments: Vec<Comment>,
    pub validations: Vec<DataValidation>,
    pub cond_formats: Vec<CondFormat>,
}

impl ClipData {
    /// Block size (rows, cols).
    pub fn size(&self) -> (u32, u32) {
        let rows = u32::try_from(self.src_rows.len()).unwrap_or(self.range.rows());
        (rows, self.range.cols())
    }

    /// Filtered-out rows were skipped, so source geometry does not map onto the block.
    pub fn compacted(&self) -> bool {
        self.src_rows.len() != self.range.rows() as usize
    }
}

fn check_size(r: &CellRange) -> Result<(), OpError> {
    let n = r.cell_count();
    if n > MAX_CELLS_PER_OP {
        Err(OpError::TooLarge(n))
    } else {
        Ok(())
    }
}

/// `r` minus `hole`: up to four rectangles.
pub fn subtract(r: CellRange, hole: CellRange) -> Vec<CellRange> {
    let Some(i) = r.intersect(&hole) else {
        return vec![r];
    };
    let mut out = Vec::new();
    if i.start.row > r.start.row {
        out.push(CellRange::new(
            r.start,
            CellAddr::new(i.start.row - 1, r.end.col),
        ));
    }
    if i.end.row < r.end.row {
        out.push(CellRange::new(
            CellAddr::new(i.end.row + 1, r.start.col),
            r.end,
        ));
    }
    if i.start.col > r.start.col {
        out.push(CellRange::new(
            CellAddr::new(i.start.row, r.start.col),
            CellAddr::new(i.end.row, i.start.col - 1),
        ));
    }
    if i.end.col < r.end.col {
        out.push(CellRange::new(
            CellAddr::new(i.start.row, i.end.col + 1),
            CellAddr::new(i.end.row, r.end.col),
        ));
    }
    out
}

fn contains_range(outer: &CellRange, inner: &CellRange) -> bool {
    outer.contains(inner.start) && outer.contains(inner.end)
}

/// Shift every formula text of a rule by (dr, dc) (rule formulas are relative to its anchor).
fn shift_texts(texts: Vec<&mut String>, dr: i64, dc: i64) {
    if dr == 0 && dc == 0 {
        return;
    }
    for t in texts {
        if t.trim().is_empty() {
            continue;
        }
        let new = shifted_formula(t, dr, dc);
        *t = new;
    }
}

fn dv_texts(dv: &mut DataValidation) -> Vec<&mut String> {
    vec![&mut dv.formula1, &mut dv.formula2]
}

fn cf_shift(cf: CondFormat, dr: i64, dc: i64) -> CondFormat {
    if dr == 0 && dc == 0 {
        return cf;
    }
    let mut tmp = Sheet::new("");
    tmp.cond_formats.push(cf);
    crate::table_shift::rewrite_formulas(&mut tmp, &|src| Some(shifted_formula(src, dr, dc)));
    tmp.cond_formats.pop().unwrap_or_else(|| CondFormat {
        ranges: vec![],
        rules: vec![],
    })
}

fn delta(from: CellAddr, to: CellAddr) -> (i64, i64) {
    (
        i64::from(to.row) - i64::from(from.row),
        i64::from(to.col) - i64::from(from.col),
    )
}

/// A rule-like feature with ranges anchored at its first range.
trait Ranged: Clone {
    fn ranges_mut(&mut self) -> &mut Vec<CellRange>;
    fn ranges(&self) -> &[CellRange];
    /// Shift the relative references of the rule's formulas by (dr, dc).
    fn shifted(self, dr: i64, dc: i64) -> Self;
}

impl Ranged for DataValidation {
    fn ranges_mut(&mut self) -> &mut Vec<CellRange> {
        &mut self.ranges
    }
    fn ranges(&self) -> &[CellRange] {
        &self.ranges
    }
    fn shifted(mut self, dr: i64, dc: i64) -> Self {
        shift_texts(dv_texts(&mut self), dr, dc);
        self
    }
}

impl Ranged for CondFormat {
    fn ranges_mut(&mut self) -> &mut Vec<CellRange> {
        &mut self.ranges
    }
    fn ranges(&self) -> &[CellRange] {
        &self.ranges
    }
    fn shifted(self, dr: i64, dc: i64) -> Self {
        cf_shift(self, dr, dc)
    }
}

fn anchor_of(r: &[CellRange]) -> CellAddr {
    r.first().map(|r| r.start).unwrap_or_default()
}

/// Replace a rule's ranges, re-basing its formulas onto the new first range.
fn with_ranges<T: Ranged>(rule: &T, ranges: Vec<CellRange>) -> Option<T> {
    if ranges.is_empty() {
        return None;
    }
    let (dr, dc) = delta(anchor_of(rule.ranges()), anchor_of(&ranges));
    let mut n = rule.clone().shifted(dr, dc);
    *n.ranges_mut() = ranges;
    Some(n)
}

/// Remove `area` from every rule (rules left without cells disappear).
fn carve<T: Ranged>(rules: Vec<T>, area: CellRange) -> Vec<T> {
    rules
        .into_iter()
        .filter_map(|rule| {
            if !rule.ranges().iter().any(|r| r.intersect(&area).is_some()) {
                return Some(rule);
            }
            let ranges: Vec<CellRange> = rule
                .ranges()
                .iter()
                .flat_map(|r| subtract(*r, area))
                .collect();
            with_ranges(&rule, ranges)
        })
        .collect()
}

/// The part of each rule inside `area` (source coordinates, re-based onto the piece).
fn pieces<T: Ranged>(rules: &[T], area: CellRange) -> Vec<T> {
    rules
        .iter()
        .filter_map(|rule| {
            let ranges: Vec<CellRange> = rule
                .ranges()
                .iter()
                .filter_map(|r| r.intersect(&area))
                .collect();
            with_ranges(rule, ranges)
        })
        .collect()
}

/// Copy `r` of `sheet`. Rows hidden by the sheet's AutoFilter are left out (only visible
/// cells are copied from a filtered list); manually hidden rows are copied.
pub fn copy(
    wb: &sp_model::Workbook,
    sheet: usize,
    r: CellRange,
    cut: bool,
) -> Result<ClipData, OpError> {
    check_size(&r)?;
    let s = wb.sheet(sheet).ok_or(OpError::NoSheet)?;
    let filtered = |row: u32| {
        !cut && s.hidden_rows.contains(&row)
            && s.auto_filter
                .as_ref()
                .is_some_and(|af| af.range.start.row < row && row <= af.range.end.row)
    };
    let src_rows: Vec<u32> = (r.start.row..=r.end.row)
        .filter(|x| !filtered(*x))
        .collect();
    let row_pos: HashMap<u32, u32> = src_rows
        .iter()
        .enumerate()
        .filter_map(|(i, row)| Some((*row, u32::try_from(i).ok()?)))
        .collect();
    let mut cells = Vec::new();
    for (a, c) in s.cells_in(r) {
        let Some(dr) = row_pos.get(&a.row) else {
            continue;
        };
        // A spilled value travels as a constant unless its formula is copied too.
        let c = if c.is_spilled() {
            if s.spill_covering(a)
                .is_some_and(|sp| r.contains(sp.range.start))
            {
                Cell {
                    content: Content::Value(Value::Empty),
                    value: Value::Empty,
                    style: c.style,
                }
            } else {
                Cell {
                    content: Content::Value(c.value.clone()),
                    ..c.clone()
                }
            }
        } else {
            c.clone()
        };
        cells.push((*dr, a.col - r.start.col, c));
    }
    let compact = src_rows.len() != r.rows() as usize;
    let (merges, comments, validations, cond_formats) = if compact {
        (vec![], vec![], vec![], vec![])
    } else {
        (
            s.merges
                .iter()
                .filter(|m| contains_range(&r, m))
                .copied()
                .collect(),
            s.comments
                .iter()
                .filter(|c| r.contains(c.addr))
                .cloned()
                .collect(),
            pieces(&s.validations, r),
            pieces(&s.cond_formats, r),
        )
    };
    Ok(ClipData {
        sheet,
        range: r,
        cells,
        cut,
        src_rows,
        merges,
        comments,
        validations,
        cond_formats,
    })
}

/// What Paste Special pastes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PasteWhat {
    #[default]
    All,
    Values,
    Formulas,
    Formats,
}

/// Paste Special arithmetic between the copied and the destination values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PasteOp {
    #[default]
    None,
    Add,
    Subtract,
    Multiply,
    Divide,
}

impl PasteOp {
    fn symbol(self) -> &'static str {
        match self {
            PasteOp::None | PasteOp::Add => "+",
            PasteOp::Subtract => "-",
            PasteOp::Multiply => "*",
            PasteOp::Divide => "/",
        }
    }

    fn apply(self, d: f64, s: f64) -> Value {
        let v = match self {
            PasteOp::None => s,
            PasteOp::Add => d + s,
            PasteOp::Subtract => d - s,
            PasteOp::Multiply => d * s,
            PasteOp::Divide => {
                if s == 0.0 {
                    return Value::Error(ErrorKind::Div0);
                }
                d / s
            }
        };
        if v.is_finite() {
            Value::Number(v)
        } else {
            Value::Error(ErrorKind::Num)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PasteOpts {
    pub what: PasteWhat,
    pub op: PasteOp,
    pub skip_blanks: bool,
    pub transpose: bool,
}

fn num_text(n: f64) -> String {
    let t = Value::Number(n).to_display();
    if n < 0.0 { format!("({t})") } else { t }
}

/// Combine destination content with pasted content under an operation (`None` = keep the
/// destination as it is). Text, logical and error values are not operated on: copied ones
/// replace the destination, destination ones stay.
fn combine(dest: &Content, src: &Content, op: PasteOp) -> Option<Content> {
    let blank_src = matches!(src, Content::Value(Value::Empty));
    let src_term = match src {
        Content::Value(Value::Number(n)) => Some(num_text(*n)),
        Content::Value(Value::Empty) => Some("0".to_string()),
        Content::Formula(f) => Some(format!("({f})")),
        Content::Value(_) => None,
    };
    let Some(src_term) = src_term else {
        return Some(src.clone());
    };
    if blank_src && matches!(op, PasteOp::Add | PasteOp::Subtract) {
        return None;
    }
    let src_num = match src {
        Content::Value(Value::Number(n)) => Some(*n),
        Content::Value(Value::Empty) => Some(0.0),
        _ => None,
    };
    match dest {
        Content::Value(Value::Empty) | Content::Value(Value::Number(_)) => {
            let d = match dest {
                Content::Value(Value::Number(d)) => *d,
                _ => 0.0,
            };
            if blank_src && matches!(dest, Content::Value(Value::Empty)) {
                return None;
            }
            match src_num {
                Some(s) => Some(Content::Value(op.apply(d, s))),
                None => Some(Content::Formula(format!(
                    "{}{}{src_term}",
                    num_text(d),
                    op.symbol()
                ))),
            }
        }
        Content::Formula(g) => Some(Content::Formula(format!("({g}){}{src_term}", op.symbol()))),
        Content::Value(_) => None,
    }
}

/// Rewrite references of a formula pasted transposed: fully relative references keep their
/// position relative to the block, transposed (row offset ↔ column offset); references with
/// an absolute part shift by the cell's own displacement like a normal paste.
fn transposed_formula(src: &str, from: CellAddr, to: CellAddr, cell_d: (i64, i64)) -> String {
    let Ok(mut e) = parse(src) else {
        return src.to_string();
    };
    let tr = |p: &CellPart| -> Option<CellPart> {
        if !p.row_abs && !p.col_abs {
            let r = i64::from(to.row) + i64::from(p.col) - i64::from(from.col);
            let c = i64::from(to.col) + i64::from(p.row) - i64::from(from.row);
            let ok = (0..i64::from(MAX_ROWS)).contains(&r) && (0..i64::from(MAX_COLS)).contains(&c);
            return ok.then_some(CellPart {
                row: r as u32,
                col: c as u32,
                ..*p
            });
        }
        let mut one = Expr::Ref(sp_formula::ast::RefExpr {
            sheet: None,
            kind: RefKind::Cell(*p),
        });
        shift_for_copy(&mut one, cell_d.0, cell_d.1);
        match one {
            Expr::Ref(sp_formula::ast::RefExpr {
                kind: RefKind::Cell(q),
                ..
            }) => Some(q),
            _ => None,
        }
    };
    e.walk_mut(&mut |node| {
        let Expr::Ref(r) = node else { return };
        let new = match &r.kind {
            RefKind::Cell(p) => tr(p).map(RefKind::Cell),
            RefKind::Area(a, b) => match (tr(a), tr(b)) {
                (Some(a), Some(b)) => {
                    let (r0, r1) = (a.row.min(b.row), a.row.max(b.row));
                    let (c0, c1) = (a.col.min(b.col), a.col.max(b.col));
                    Some(RefKind::Area(
                        CellPart {
                            row: r0,
                            col: c0,
                            ..a
                        },
                        CellPart {
                            row: r1,
                            col: c1,
                            ..b
                        },
                    ))
                }
                _ => None,
            },
            _ => {
                let mut one = Expr::Ref(r.clone());
                shift_for_copy(&mut one, cell_d.0, cell_d.1);
                match one {
                    Expr::Ref(x) => Some(x.kind),
                    _ => None,
                }
            }
        };
        match new {
            Some(k) => r.kind = k,
            None => *node = Expr::Error(ErrorKind::Ref),
        }
    });
    to_formula(&e)
}

fn refuse_partial_merges(
    s: &Sheet,
    out: CellRange,
    ignore: Option<CellRange>,
) -> Result<(), OpError> {
    let bad = s.merges.iter().any(|m| {
        m.intersect(&out).is_some()
            && !contains_range(&out, m)
            && !ignore.is_some_and(|i| contains_range(&i, m))
    });
    if bad {
        return Err(OpError::Invalid(
            "We can't do that to a merged cell.".into(),
        ));
    }
    Ok(())
}

/// Paste `clip` into `dest`. A destination that is a whole multiple of the copied block
/// receives tiled copies; otherwise one copy goes at its top-left. Cut data is moved
/// (see [`move_range`]). Returns the range written.
pub fn paste(
    tx: &mut Tx<'_>,
    clip: &ClipData,
    sheet: usize,
    dest: CellRange,
    opts: PasteOpts,
) -> Result<CellRange, OpError> {
    tx.wb.sheet(sheet).ok_or(OpError::NoSheet)?;
    if clip.cut {
        if opts != PasteOpts::default() {
            return Err(OpError::Invalid(
                "Cut cells can only be pasted as they are (not with Paste Special).".into(),
            ));
        }
        return move_range(tx, clip.sheet, clip.range, sheet, dest.start);
    }
    let (h, w) = if opts.transpose {
        let (r, c) = clip.size();
        (c, r)
    } else {
        clip.size()
    };
    if h == 0 || w == 0 {
        return Ok(CellRange::single(dest.start));
    }
    let (tr, tc) = if dest.rows().is_multiple_of(h) && dest.cols().is_multiple_of(w) {
        (dest.rows() / h, dest.cols() / w)
    } else {
        (1, 1)
    };
    let end = dest
        .start
        .offset(
            i64::from(h) * i64::from(tr) - 1,
            i64::from(w) * i64::from(tc) - 1,
        )
        .ok_or_else(|| OpError::Invalid("paste area extends beyond the sheet".into()))?;
    let out = CellRange::new(dest.start, end);
    check_size(&out)?;
    let s = tx.wb.sheet(sheet).ok_or(OpError::NoSheet)?;
    refuse_partial_merges(s, out, None)?;

    let features = matches!(opts.what, PasteWhat::All | PasteWhat::Formats) && !clip.compacted();
    let dest_has_features = s.merges.iter().any(|m| m.intersect(&out).is_some())
        || s.comments.iter().any(|c| out.contains(c.addr))
        || s.validations
            .iter()
            .any(|v| v.ranges.iter().any(|r| r.intersect(&out).is_some()))
        || s.cond_formats
            .iter()
            .any(|v| v.ranges.iter().any(|r| r.intersect(&out).is_some()));
    let clip_has_features = !clip.merges.is_empty()
        || !clip.comments.is_empty()
        || !clip.validations.is_empty()
        || !clip.cond_formats.is_empty();
    if features && (dest_has_features || clip_has_features) {
        tx.touch_sheet(sheet);
    }

    // Block offset (r, c) of each copied cell, in destination orientation.
    let mut src_at: HashMap<(u32, u32), (&Cell, CellAddr)> = HashMap::new();
    for (dr, dc, c) in &clip.cells {
        let row = clip
            .src_rows
            .get(*dr as usize)
            .copied()
            .unwrap_or(clip.range.start.row + dr);
        let at = CellAddr::new(row, clip.range.start.col + dc);
        let key = if opts.transpose {
            (*dc, *dr)
        } else {
            (*dr, *dc)
        };
        src_at.insert(key, (c, at));
    }
    let mut writes: Vec<(CellAddr, Option<Cell>)> = Vec::new();
    for ti in 0..tr {
        for tj in 0..tc {
            let Some(origin) = out.start.offset(i64::from(ti * h), i64::from(tj * w)) else {
                continue;
            };
            let Some(tile_end) = origin.offset(i64::from(h) - 1, i64::from(w) - 1) else {
                continue;
            };
            let tile = CellRange::new(origin, tile_end);
            let s = tx.wb.sheet(sheet).ok_or(OpError::NoSheet)?;
            // Every block position holding a copied cell or an existing destination cell.
            let mut keys: Vec<(u32, u32)> = src_at.keys().copied().collect();
            for (a, _) in s.cells_in(tile) {
                let k = (a.row - origin.row, a.col - origin.col);
                if !src_at.contains_key(&k) {
                    keys.push(k);
                }
            }
            for k in keys {
                let Some(at) = origin.offset(i64::from(k.0), i64::from(k.1)) else {
                    continue;
                };
                let existing = s.get(at);
                let src = src_at.get(&k);
                let blank =
                    src.is_none_or(|(c, _)| matches!(c.content, Content::Value(Value::Empty)));
                if opts.skip_blanks && blank {
                    continue;
                }
                let src_content = match src {
                    None => Content::Value(Value::Empty),
                    Some((c, from)) => match (&c.content, opts.what) {
                        (Content::Formula(_), PasteWhat::Values) => Content::Value(c.value.clone()),
                        (Content::Formula(f), _) => {
                            let d = delta(*from, at);
                            Content::Formula(if opts.transpose {
                                transposed_formula(f, clip.range.start, origin, d)
                            } else {
                                shifted_formula(f, d.0, d.1)
                            })
                        }
                        (other, _) => other.clone(),
                    },
                };
                let dest_content = existing.map(|c| c.content.clone()).unwrap_or_default();
                let content = match opts.what {
                    PasteWhat::Formats => dest_content.clone(),
                    _ if opts.op != PasteOp::None => {
                        match combine(&dest_content, &src_content, opts.op) {
                            Some(c) => c,
                            None => dest_content.clone(),
                        }
                    }
                    _ => src_content,
                };
                let style = match opts.what {
                    PasteWhat::All | PasteWhat::Formats => {
                        src.map_or(StyleId::DEFAULT, |(c, _)| c.style)
                    }
                    PasteWhat::Values | PasteWhat::Formulas => {
                        existing.map_or(StyleId::DEFAULT, |c| c.style)
                    }
                };
                let unchanged = match existing {
                    Some(e) => e.content == content && e.style == style,
                    None => {
                        matches!(content, Content::Value(Value::Empty)) && style == StyleId::DEFAULT
                    }
                };
                if unchanged {
                    continue;
                }
                let value = match &content {
                    Content::Value(v) => v.clone(),
                    Content::Formula(_) => Value::Empty,
                };
                let cell = Cell {
                    content,
                    value,
                    style,
                };
                writes.push((at, (!cell.is_blank()).then_some(cell)));
            }
        }
    }
    for (a, c) in writes {
        tx.set_cell(sheet, a, c);
    }
    if features {
        paste_features(tx, clip, sheet, out, (h, w), (tr, tc), opts)?;
    }
    Ok(out)
}

/// Merges, notes, validation and conditional formats for Paste (All) / Paste Formatting.
fn paste_features(
    tx: &mut Tx<'_>,
    clip: &ClipData,
    sheet: usize,
    out: CellRange,
    (h, w): (u32, u32),
    (tr, tc): (u32, u32),
    opts: PasteOpts,
) -> Result<(), OpError> {
    let o = clip.range.start;
    // Map a source range to its copy in tile (ti, tj).
    let place = |r: CellRange, ti: u32, tj: u32| -> Option<CellRange> {
        let rel = |a: CellAddr| -> (i64, i64) {
            let (dr, dc) = delta(o, a);
            if opts.transpose { (dc, dr) } else { (dr, dc) }
        };
        let (a0, b0) = (rel(r.start), rel(r.end));
        let base = out.start.offset(i64::from(ti * h), i64::from(tj * w))?;
        let a = base.offset(a0.0.min(b0.0), a0.1.min(b0.1))?;
        let b = base.offset(a0.0.max(b0.0), a0.1.max(b0.1))?;
        Some(CellRange::new(a, b))
    };
    let tiles: Vec<(u32, u32)> = (0..tr).flat_map(|i| (0..tc).map(move |j| (i, j))).collect();
    let s = tx.wb.sheet_mut(sheet).ok_or(OpError::NoSheet)?;
    s.merges.retain(|m| !contains_range(&out, m));
    for m in &clip.merges {
        for (i, j) in &tiles {
            if let Some(r) = place(*m, *i, *j) {
                s.merges.push(r);
            }
        }
    }
    s.cond_formats = carve(std::mem::take(&mut s.cond_formats), out);
    for cf in &clip.cond_formats {
        let ranges: Vec<CellRange> = tiles
            .iter()
            .flat_map(|(i, j)| cf.ranges.iter().filter_map(|r| place(*r, *i, *j)))
            .collect();
        let (dr, dc) = delta(anchor_of(&cf.ranges), anchor_of(&ranges));
        let mut n = cf.clone().shifted(dr, dc);
        n.ranges = ranges;
        if !n.ranges.is_empty() {
            s.cond_formats.push(n);
        }
    }
    if opts.what == PasteWhat::All {
        s.comments.retain(|c| !out.contains(c.addr));
        for c in &clip.comments {
            for (i, j) in &tiles {
                if let Some(r) = place(CellRange::single(c.addr), *i, *j) {
                    s.comments.push(Comment {
                        addr: r.start,
                        ..c.clone()
                    });
                }
            }
        }
        s.comments.sort_by_key(|c| (c.addr.row, c.addr.col));
        s.validations = carve(std::mem::take(&mut s.validations), out);
        for dv in &clip.validations {
            let ranges: Vec<CellRange> = tiles
                .iter()
                .flat_map(|(i, j)| dv.ranges.iter().filter_map(|r| place(*r, *i, *j)))
                .collect();
            let (dr, dc) = delta(anchor_of(&dv.ranges), anchor_of(&ranges));
            let mut n = dv.clone().shifted(dr, dc);
            n.ranges = ranges;
            if !n.ranges.is_empty() {
                s.validations.push(n);
            }
        }
    }
    Ok(())
}

/// Rewrite one formula for a move of `src` (on sheet `src_name`) to `out` (on `dst_name`).
/// `resolve` is the sheet unqualified references point at, `home` the sheet the formula
/// will live on. References entirely inside `src` follow the cells (absolute or not);
/// references inside `out` (the cells pasted over) become `#REF!`.
struct Move<'a> {
    src_name: &'a str,
    dst_name: &'a str,
    src: CellRange,
    out: CellRange,
}

impl Move<'_> {
    fn apply(&self, e: &mut Expr, resolve: &str, home: &str) {
        let (dr, dc) = delta(self.src.start, self.out.start);
        e.walk_mut(&mut |node| {
            let Expr::Ref(r) = node else { return };
            let target = r.sheet.clone().unwrap_or_else(|| resolve.to_string());
            let span = match &r.kind {
                RefKind::Cell(p) => CellRange::single(CellAddr::new(p.row, p.col)),
                RefKind::Area(a, b) => {
                    CellRange::new(CellAddr::new(a.row, a.col), CellAddr::new(b.row, b.col))
                }
                _ => {
                    // Whole rows/columns never move with a cut; just keep pointing at the
                    // same sheet.
                    if r.sheet.is_none() && !resolve.eq_ignore_ascii_case(home) {
                        r.sheet = Some(resolve.to_string());
                    }
                    return;
                }
            };
            if target.eq_ignore_ascii_case(self.src_name) && contains_range(&self.src, &span) {
                let mv = |p: &CellPart| CellPart {
                    row: (i64::from(p.row) + dr) as u32,
                    col: (i64::from(p.col) + dc) as u32,
                    ..*p
                };
                r.kind = match &r.kind {
                    RefKind::Cell(p) => RefKind::Cell(mv(p)),
                    RefKind::Area(a, b) => RefKind::Area(mv(a), mv(b)),
                    k => k.clone(),
                };
                r.sheet = if self.dst_name.eq_ignore_ascii_case(home) {
                    r.sheet.as_ref().map(|_| self.dst_name.to_string())
                } else {
                    Some(self.dst_name.to_string())
                };
                return;
            }
            if target.eq_ignore_ascii_case(self.dst_name) && contains_range(&self.out, &span) {
                *node = Expr::Error(ErrorKind::Ref);
                return;
            }
            if r.sheet.is_none() && !resolve.eq_ignore_ascii_case(home) {
                r.sheet = Some(resolve.to_string());
            }
        });
    }

    fn text(&self, src: &str, resolve: &str, home: &str) -> String {
        match parse(src) {
            Ok(mut e) => {
                let orig = e.clone();
                self.apply(&mut e, resolve, home);
                if e == orig {
                    src.to_string()
                } else {
                    to_formula(&e)
                }
            }
            Err(_) => src.to_string(),
        }
    }
}

fn moved_rule<T: Ranged>(
    rule: T,
    (dr, dc): (i64, i64),
    mv: &Move<'_>,
    texts: impl Fn(&mut T) -> Vec<&mut String>,
) -> T {
    let mut n = rule;
    for r in n.ranges_mut() {
        if let (Some(a), Some(b)) = (r.start.offset(dr, dc), r.end.offset(dr, dc)) {
            *r = CellRange::new(a, b);
        }
    }
    for t in texts(&mut n) {
        if !t.trim().is_empty() {
            *t = mv.text(t, mv.src_name, mv.dst_name);
        }
    }
    n
}

fn cf_texts(cf: &mut CondFormat) -> Vec<&mut String> {
    use sp_model::{CfKind, CfvoKind};
    let mut out: Vec<&mut String> = Vec::new();
    for r in &mut cf.rules {
        match &mut r.kind {
            CfKind::CellIs { f1, f2, .. } => {
                out.push(f1);
                out.push(f2);
            }
            CfKind::Expression(f) => out.push(f),
            CfKind::DataBar { min, max, .. } => {
                for v in [min, max] {
                    if v.kind == CfvoKind::Formula {
                        out.push(&mut v.value);
                    }
                }
            }
            CfKind::ColorScale { points } => {
                for (v, _) in points {
                    if v.kind == CfvoKind::Formula {
                        out.push(&mut v.value);
                    }
                }
            }
            CfKind::IconSet { points, .. } => {
                for v in points {
                    if v.kind == CfvoKind::Formula {
                        out.push(&mut v.value);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Cut + paste: move `src` on sheet `src_sheet` so its top-left lands on `dest`.
pub fn move_range(
    tx: &mut Tx<'_>,
    src_sheet: usize,
    src: CellRange,
    dst_sheet: usize,
    dest: CellAddr,
) -> Result<CellRange, OpError> {
    check_size(&src)?;
    let end = dest
        .offset(i64::from(src.rows()) - 1, i64::from(src.cols()) - 1)
        .ok_or_else(|| OpError::Invalid("paste area extends beyond the sheet".into()))?;
    let out = CellRange::new(dest, end);
    let src_name = tx.wb.sheet(src_sheet).ok_or(OpError::NoSheet)?.name.clone();
    let dst = tx.wb.sheet(dst_sheet).ok_or(OpError::NoSheet)?;
    let dst_name = dst.name.clone();
    let same = src_sheet == dst_sheet;
    refuse_partial_merges(dst, out, same.then_some(src))?;
    if let Some(s) = tx.wb.sheet(src_sheet) {
        refuse_partial_merges(s, src, None)?;
    }
    if same && src == out {
        return Ok(out);
    }
    let d = delta(src.start, out.start);
    tx.touch_book();
    // 1. Take everything out of the source.
    let s = tx.wb.sheet_mut(src_sheet).ok_or(OpError::NoSheet)?;
    let addrs: Vec<CellAddr> = s.cells_in(src).map(|(a, _)| a).collect();
    let mut cells: Vec<(CellAddr, Cell)> = Vec::with_capacity(addrs.len());
    for a in addrs {
        if let Some(c) = s.remove(a) {
            cells.push((a, c));
        }
    }
    let (comments, rest): (Vec<Comment>, Vec<Comment>) = std::mem::take(&mut s.comments)
        .into_iter()
        .partition(|c| src.contains(c.addr));
    s.comments = rest;
    let (links, rest): (Vec<Hyperlink>, Vec<Hyperlink>) = std::mem::take(&mut s.hyperlinks)
        .into_iter()
        .partition(|h| contains_range(&src, &h.range));
    s.hyperlinks = rest;
    let (merges, rest): (Vec<CellRange>, Vec<CellRange>) = std::mem::take(&mut s.merges)
        .into_iter()
        .partition(|m| contains_range(&src, m));
    s.merges = rest;
    let dvs = pieces(&s.validations, src);
    s.validations = carve(std::mem::take(&mut s.validations), src);
    let cfs = pieces(&s.cond_formats, src);
    s.cond_formats = carve(std::mem::take(&mut s.cond_formats), src);
    // 2. Clear the destination.
    let s = tx.wb.sheet_mut(dst_sheet).ok_or(OpError::NoSheet)?;
    let addrs: Vec<CellAddr> = s.cells_in(out).map(|(a, _)| a).collect();
    for a in addrs {
        s.remove(a);
    }
    s.comments.retain(|c| !out.contains(c.addr));
    s.hyperlinks.retain(|h| h.range.intersect(&out).is_none());
    s.merges.retain(|m| !contains_range(&out, m));
    s.validations = carve(std::mem::take(&mut s.validations), out);
    s.cond_formats = carve(std::mem::take(&mut s.cond_formats), out);
    // 3. Every other reference in the workbook follows the move.
    let mv = Move {
        src_name: &src_name,
        dst_name: &dst_name,
        src,
        out,
    };
    rewrite_all_formulas(tx.wb, &|e, sheet| mv.apply(e, sheet, sheet));
    // 4. Put the moved content down; its own references are rewritten the same way, read
    //    from the source sheet and written for the destination sheet.
    let s = tx.wb.sheet_mut(dst_sheet).ok_or(OpError::NoSheet)?;
    for (a, mut c) in cells {
        let Some(at) = a.offset(d.0, d.1) else {
            continue;
        };
        if let Content::Formula(f) = &c.content {
            c.content = Content::Formula(mv.text(f, &src_name, &dst_name));
        }
        s.set(at, c);
    }
    for mut c in comments {
        if let Some(at) = c.addr.offset(d.0, d.1) {
            c.addr = at;
            s.comments.push(c);
        }
    }
    s.comments.sort_by_key(|c| (c.addr.row, c.addr.col));
    for mut h in links {
        if let (Some(a), Some(b)) = (h.range.start.offset(d.0, d.1), h.range.end.offset(d.0, d.1)) {
            h.range = CellRange::new(a, b);
            if !h.location.is_empty() {
                h.location = mv.text(&h.location, &src_name, &dst_name);
            }
            s.hyperlinks.push(h);
        }
    }
    for m in merges {
        if let (Some(a), Some(b)) = (m.start.offset(d.0, d.1), m.end.offset(d.0, d.1)) {
            s.merges.push(CellRange::new(a, b));
        }
    }
    for dv in dvs {
        s.validations.push(moved_rule(dv, d, &mv, dv_texts));
    }
    for cf in cfs {
        s.cond_formats.push(moved_rule(cf, d, &mv, cf_texts));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(s: &str) -> CellRange {
        CellRange::parse(s).unwrap()
    }

    #[test]
    fn subtract_pieces() {
        assert_eq!(subtract(r("A1:C3"), r("E5")), vec![r("A1:C3")]);
        assert_eq!(subtract(r("A1:C3"), r("A1:C3")), Vec::<CellRange>::new());
        let p = subtract(r("A1:C3"), r("B2"));
        assert_eq!(p, vec![r("A1:C1"), r("A3:C3"), r("A2"), r("C2")]);
        assert_eq!(p.iter().map(CellRange::cell_count).sum::<u64>(), 8);
    }

    #[test]
    fn transpose_refs() {
        let t = |f: &str| transposed_formula(f, CellAddr::new(0, 0), CellAddr::new(0, 4), (1, 3));
        assert_eq!(t("B1*2"), "E2*2");
        assert_eq!(t("SUM(A1:C1)"), "SUM(E1:E3)");
        assert_eq!(t("$A$1"), "$A$1");
    }

    #[test]
    fn combine_ops() {
        let n = |x: f64| Content::Value(Value::Number(x));
        assert_eq!(combine(&n(10.0), &n(2.0), PasteOp::Divide), Some(n(5.0)));
        assert_eq!(
            combine(&n(1.0), &n(0.0), PasteOp::Divide),
            Some(Content::Value(Value::Error(ErrorKind::Div0)))
        );
        assert_eq!(
            combine(&Content::Formula("A1".into()), &n(-3.0), PasteOp::Subtract),
            Some(Content::Formula("(A1)-(-3)".into()))
        );
        assert_eq!(
            combine(
                &Content::Value(Value::Empty),
                &Content::Value(Value::Empty),
                PasteOp::Multiply
            ),
            None
        );
    }
}
