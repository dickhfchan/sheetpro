//! Editing algorithms: clear, fill, sort, insert/delete rows & columns, clipboard.
//!
//! All functions take a recording `Tx` so they are undoable, and return `OpError` instead
//! of panicking on any input.

use crate::history::Tx;
use sp_core::{CellAddr, CellRange, MAX_CELLS_PER_OP, MAX_COLS, MAX_ROWS, Value};
use sp_formula::eval::compare;
use sp_formula::shift::{Axis, StructEdit, adjust_for_struct_edit, shift_for_copy};
use sp_formula::{parse, to_formula};
use sp_model::features::LineShift;
use sp_model::{Cell, Content, StyleId, Workbook};
use std::cmp::Ordering;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OpError {
    #[error("no such sheet")]
    NoSheet,
    #[error("operation too large ({0} cells)")]
    TooLarge(u64),
    #[error("{0}")]
    Invalid(String),
}

fn check_size(r: &CellRange) -> Result<(), OpError> {
    let n = r.cell_count();
    if n > MAX_CELLS_PER_OP {
        Err(OpError::TooLarge(n))
    } else {
        Ok(())
    }
}

/// Formula text moved by (dr, dc), as copy/paste/fill do.
pub fn shifted_formula(src: &str, dr: i64, dc: i64) -> String {
    match parse(src) {
        Ok(mut e) => {
            shift_for_copy(&mut e, dr, dc);
            to_formula(&e)
        }
        Err(_) => src.to_string(),
    }
}

/// A cell copied to a new position: formulas shift, values and style carry over.
pub fn moved_cell(c: &Cell, dr: i64, dc: i64) -> Cell {
    match &c.content {
        Content::Formula(f) => Cell {
            content: Content::Formula(shifted_formula(f, dr, dc)),
            value: Value::Empty,
            style: c.style,
        },
        Content::Value(_) => c.clone(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClearWhat {
    Contents,
    Formats,
    All,
}

/// Clear part or all of each cell in `r` (only stored cells are visited).
pub fn clear(
    tx: &mut Tx<'_>,
    sheet: usize,
    r: CellRange,
    what: ClearWhat,
) -> Result<usize, OpError> {
    let s = tx.wb.sheet(sheet).ok_or(OpError::NoSheet)?;
    let addrs: Vec<CellAddr> = s.cells_in(r).map(|(a, _)| a).collect();
    for a in &addrs {
        let cur = tx.wb.sheet(sheet).and_then(|s| s.get(*a)).cloned();
        let Some(mut c) = cur else { continue };
        match what {
            ClearWhat::All => {
                tx.set_cell(sheet, *a, None);
                continue;
            }
            ClearWhat::Contents => {
                c.content = Content::Value(Value::Empty);
                c.value = Value::Empty;
            }
            ClearWhat::Formats => c.style = StyleId::DEFAULT,
        }
        tx.set_cell(sheet, *a, Some(c));
    }
    Ok(addrs.len())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillDir {
    Down,
    Right,
    Up,
    Left,
}

/// Fill a selection from its first row/column (Down/Right) or last (Up/Left).
pub fn fill(tx: &mut Tx<'_>, sheet: usize, r: CellRange, dir: FillDir) -> Result<usize, OpError> {
    check_size(&r)?;
    let s = tx.wb.sheet(sheet).ok_or(OpError::NoSheet)?;
    let mut writes: Vec<(CellAddr, Option<Cell>)> = Vec::new();
    match dir {
        FillDir::Down | FillDir::Up => {
            let src_row = if dir == FillDir::Down {
                r.start.row
            } else {
                r.end.row
            };
            for col in r.start.col..=r.end.col {
                let src = s.get(CellAddr::new(src_row, col)).cloned();
                for row in r.start.row..=r.end.row {
                    if row == src_row {
                        continue;
                    }
                    let dr = i64::from(row) - i64::from(src_row);
                    writes.push((
                        CellAddr::new(row, col),
                        src.as_ref().map(|c| moved_cell(c, dr, 0)),
                    ));
                }
            }
        }
        FillDir::Right | FillDir::Left => {
            let src_col = if dir == FillDir::Right {
                r.start.col
            } else {
                r.end.col
            };
            for row in r.start.row..=r.end.row {
                let src = s.get(CellAddr::new(row, src_col)).cloned();
                for col in r.start.col..=r.end.col {
                    if col == src_col {
                        continue;
                    }
                    let dc = i64::from(col) - i64::from(src_col);
                    writes.push((
                        CellAddr::new(row, col),
                        src.as_ref().map(|c| moved_cell(c, 0, dc)),
                    ));
                }
            }
        }
    }
    let n = writes.len();
    for (a, c) in writes {
        tx.set_cell(sheet, a, c);
    }
    Ok(n)
}

/// AutoFill: extend `src` into `dest` (which contains `src` and extends it down or right).
/// Numeric runs continue their linear trend; single numbers and text repeat; formulas shift.
pub fn autofill(
    tx: &mut Tx<'_>,
    sheet: usize,
    src: CellRange,
    dest: CellRange,
) -> Result<usize, OpError> {
    check_size(&dest)?;
    let s = tx.wb.sheet(sheet).ok_or(OpError::NoSheet)?;
    let vertical =
        dest.cols() == src.cols() && dest.start.col == src.start.col && dest.rows() > src.rows();
    let horizontal =
        dest.rows() == src.rows() && dest.start.row == src.start.row && dest.cols() > src.cols();
    if !(vertical || horizontal) || dest.start != src.start {
        return Err(OpError::Invalid(
            "fill range must extend the source down or right".into(),
        ));
    }
    let mut writes = Vec::new();
    let lines = if vertical { src.cols() } else { src.rows() };
    let len = if vertical { src.rows() } else { src.cols() };
    let total = if vertical { dest.rows() } else { dest.cols() };
    for line in 0..lines {
        let at = |k: u32| {
            if vertical {
                CellAddr::new(src.start.row + k, src.start.col + line)
            } else {
                CellAddr::new(src.start.row + line, src.start.col + k)
            }
        };
        let cells: Vec<Option<Cell>> = (0..len).map(|k| s.get(at(k)).cloned()).collect();
        let nums: Vec<f64> = cells
            .iter()
            .filter_map(|c| match c.as_ref().map(|c| &c.content) {
                Some(Content::Value(Value::Number(n))) => Some(*n),
                _ => None,
            })
            .collect();
        let series = nums.len() == len as usize && len >= 2;
        let (slope, intercept) = if series {
            let n = len as f64;
            let mx = (n - 1.0) / 2.0;
            let my = nums.iter().sum::<f64>() / n;
            let sxx: f64 = (0..len).map(|k| (f64::from(k) - mx).powi(2)).sum();
            let sxy: f64 = nums
                .iter()
                .enumerate()
                .map(|(k, y)| (k as f64 - mx) * (y - my))
                .sum();
            let b = if sxx == 0.0 { 0.0 } else { sxy / sxx };
            (b, my - b * mx)
        } else {
            (0.0, 0.0)
        };
        for k in len..total {
            let dest_addr = if vertical {
                CellAddr::new(dest.start.row + k, dest.start.col + line)
            } else {
                CellAddr::new(dest.start.row + line, dest.start.col + k)
            };
            let src_k = (k % len) as usize;
            let base = cells.get(src_k).cloned().flatten();
            let cell = if series {
                let style = base.as_ref().map_or(StyleId::DEFAULT, |c| c.style);
                let v = Value::Number(intercept + slope * f64::from(k));
                Some(Cell {
                    content: Content::Value(v.clone()),
                    value: v,
                    style,
                })
            } else {
                let off = i64::from(k) - src_k as i64;
                base.map(|c| {
                    if vertical {
                        moved_cell(&c, off, 0)
                    } else {
                        moved_cell(&c, 0, off)
                    }
                })
            };
            writes.push((dest_addr, cell));
        }
    }
    let n = writes.len();
    for (a, c) in writes {
        tx.set_cell(sheet, a, c);
    }
    Ok(n)
}

#[derive(Debug, Clone, Copy)]
pub struct SortKey {
    /// Column offset within the range.
    pub col: u32,
    pub ascending: bool,
}

/// Sort the rows of `r` by `keys`. Blanks always sort last; formulas move with their row
/// (relative references shift by the row delta).
pub fn sort_rows(
    tx: &mut Tx<'_>,
    sheet: usize,
    r: CellRange,
    keys: &[SortKey],
    has_header: bool,
) -> Result<(), OpError> {
    check_size(&r)?;
    if keys.is_empty() || keys.iter().any(|k| k.col >= r.cols()) {
        return Err(OpError::Invalid("sort key outside range".into()));
    }
    let body = if has_header {
        if r.rows() < 2 {
            return Ok(());
        }
        CellRange::new(CellAddr::new(r.start.row + 1, r.start.col), r.end)
    } else {
        r
    };
    tx.touch_sheet(sheet);
    let s = tx.wb.sheet(sheet).ok_or(OpError::NoSheet)?;
    let rows: Vec<u32> = (body.start.row..=body.end.row).collect();
    let mut order: Vec<u32> = rows.clone();
    order.sort_by(|a, b| {
        for k in keys {
            let va = s.value(CellAddr::new(*a, body.start.col + k.col));
            let vb = s.value(CellAddr::new(*b, body.start.col + k.col));
            let o = match (va.is_empty(), vb.is_empty()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                _ => {
                    let o = compare(va, vb);
                    if k.ascending { o } else { o.reverse() }
                }
            };
            if o != Ordering::Equal {
                return o;
            }
        }
        Ordering::Equal
    });
    let snapshot: Vec<Vec<Option<Cell>>> = order
        .iter()
        .map(|src_row| {
            (body.start.col..=body.end.col)
                .map(|c| s.get(CellAddr::new(*src_row, c)).cloned())
                .collect()
        })
        .collect();
    for (i, (dest_row, src_row)) in rows.iter().zip(order.iter()).enumerate() {
        let dr = i64::from(*dest_row) - i64::from(*src_row);
        for (j, col) in (body.start.col..=body.end.col).enumerate() {
            let cell = snapshot
                .get(i)
                .and_then(|r| r.get(j))
                .cloned()
                .flatten()
                .map(|c| moved_cell(&c, dr, 0));
            tx.set_cell(sheet, CellAddr::new(*dest_row, col), cell);
        }
    }
    Ok(())
}

/// Re-parse `src` and apply `f`; `None` when unparseable or unchanged.
fn rewritten(src: &str, f: &dyn Fn(&mut sp_formula::Expr)) -> Option<String> {
    let orig = parse(src).ok()?;
    let mut e = orig.clone();
    f(&mut e);
    (e != orig).then(|| to_formula(&e))
}

/// Rewrite every formula in the workbook with `f(expr, sheet the formula lives on)`: cells,
/// data-validation rules, in-workbook hyperlink targets and defined names (sheet-scoped names resolve unqualified refs on
/// their own sheet).
fn rewrite_all_formulas(wb: &mut Workbook, f: &dyn Fn(&mut sp_formula::Expr, &str)) {
    let names: Vec<String> = wb.sheets.iter().map(|s| s.name.clone()).collect();
    for sheet in &mut wb.sheets {
        let name = sheet.name.clone();
        let g = |e: &mut sp_formula::Expr| f(e, &name);
        for row in sheet.cells.values_mut() {
            for cell in row.values_mut() {
                if let Content::Formula(src) = &cell.content
                    && let Some(new) = rewritten(src, &g)
                {
                    cell.content = Content::Formula(new);
                }
            }
        }
        let texts = sheet
            .validations
            .iter_mut()
            .flat_map(|dv| [&mut dv.formula1, &mut dv.formula2])
            .chain(
                sheet
                    .hyperlinks
                    .iter_mut()
                    .filter(|h| !h.location.is_empty())
                    .map(|h| &mut h.location),
            );
        for src in texts {
            if let Some(new) = rewritten(src, &g) {
                *src = new;
            }
        }
        crate::table_shift::rewrite_formulas(sheet, &|src| rewritten(src, &g));
    }
    for n in &mut wb.names {
        let home = n
            .scope
            .and_then(|i| names.get(i))
            .map_or("", String::as_str);
        if let Some(new) = rewritten(&n.formula, &|e| f(e, home)) {
            n.formula = new;
        }
    }
}

/// Move or drop every per-cell feature of `s` for a row/column insert/delete: comments,
/// hyperlinks, merges, validations, AutoFilter, page breaks, outline levels, array areas
/// and allow-edit ranges.
fn shift_sheet_features(s: &mut sp_model::Sheet, sh: &LineShift) {
    s.comments = std::mem::take(&mut s.comments)
        .into_iter()
        .filter_map(|mut c| {
            c.addr = sh.addr(c.addr)?;
            Some(c)
        })
        .collect();
    s.hyperlinks = std::mem::take(&mut s.hyperlinks)
        .into_iter()
        .filter_map(|mut h| {
            h.range = sh.range(h.range)?;
            Some(h)
        })
        .collect();
    s.merges = s
        .merges
        .iter()
        .filter_map(|m| sh.range(*m).filter(|r| !r.is_single()))
        .collect();
    s.validations = std::mem::take(&mut s.validations)
        .into_iter()
        .filter_map(|mut dv| {
            let old_anchor = dv.anchor();
            dv.ranges = dv.ranges.iter().filter_map(|r| sh.range(*r)).collect();
            if dv.ranges.is_empty() {
                return None;
            }
            // Relative refs are written for the anchor; if the old anchor was deleted the
            // first surviving cell becomes the anchor, so re-base them onto it.
            if sh.addr(old_anchor).is_none() {
                let del = sh.count.unsigned_abs();
                let (dr, dc) = if sh.rows {
                    (i64::from(sh.at) + del as i64 - i64::from(old_anchor.row), 0)
                } else {
                    (0, i64::from(sh.at) + del as i64 - i64::from(old_anchor.col))
                };
                for f in [&mut dv.formula1, &mut dv.formula2] {
                    if let Some(new) = rewritten(f, &|e| shift_for_copy(e, dr, dc)) {
                        *f = new;
                    }
                }
            }
            Some(dv)
        })
        .collect();
    s.auto_filter = s.auto_filter.as_ref().and_then(|af| af.shifted(sh));
    s.page_setup.shift_breaks(sh);
    s.outline.shift(sh);
    s.spills = std::mem::take(&mut s.spills)
        .into_iter()
        .filter_map(|mut a| {
            sh.addr(a.range.start)?;
            a.range = sh.range(a.range)?;
            Some(a)
        })
        .collect();
    s.allow_edit_ranges = std::mem::take(&mut s.allow_edit_ranges)
        .into_iter()
        .filter_map(|mut r| {
            r.ranges = r.ranges.iter().filter_map(|x| sh.range(*x)).collect();
            (!r.ranges.is_empty()).then_some(r)
        })
        .collect();
}

/// Insert (`count > 0`) or delete (`count < 0`) rows/columns on one sheet, fixing every
/// formula and defined name in the workbook that refers to the moved cells and moving the
/// sheet's comments, links, validations, filter, page breaks, outline and array areas.
/// Cutting through part of a legacy (fixed) array formula is refused.
pub fn insert_delete(
    tx: &mut Tx<'_>,
    sheet: usize,
    axis: Axis,
    at: u32,
    count: i64,
) -> Result<(), OpError> {
    let max = if axis == Axis::Rows {
        MAX_ROWS
    } else {
        MAX_COLS
    };
    if count == 0 || at >= max || count.unsigned_abs() > u64::from(max) {
        return Err(OpError::Invalid("bad insert/delete position".into()));
    }
    let sh = LineShift {
        rows: axis == Axis::Rows,
        at,
        count,
        max,
    };
    let name = {
        let s = tx.wb.sheet(sheet).ok_or(OpError::NoSheet)?;
        if s.spills.iter().any(|a| a.fixed && sh.splits(a.range)) {
            return Err(OpError::Invalid(
                "You can't change part of an array.".into(),
            ));
        }
        s.name.clone()
    };
    tx.touch_book();
    // Spilled values of dynamic arrays cut by the edit are stale: clear them (recalc
    // re-spills from the anchor).
    if let Some(s) = tx.wb.sheet_mut(sheet) {
        let stale: Vec<CellRange> = s
            .spills
            .iter()
            .filter(|a| {
                !a.fixed && !a.blocked && (sh.splits(a.range) || sh.addr(a.range.start).is_none())
            })
            .map(|a| a.range)
            .collect();
        for r in stale {
            let spilled: Vec<CellAddr> = s
                .cells_in(r)
                .filter(|(a, c)| *a != r.start && c.is_spilled())
                .map(|(a, _)| a)
                .collect();
            for a in spilled {
                if let Some(c) = s.get_mut(a) {
                    c.value = Value::Empty;
                    if c.is_blank() {
                        s.remove(a);
                    }
                }
            }
        }
    }
    let losses;
    {
        let s = tx.wb.sheet_mut(sheet).ok_or(OpError::NoSheet)?;
        match axis {
            Axis::Rows => {
                s.cells = sh.keys(&s.cells);
                s.row_heights = sh.keys(&s.row_heights);
                s.hidden_rows = sh.set(&s.hidden_rows);
            }
            Axis::Cols => {
                for row in s.cells.values_mut() {
                    *row = sh.keys(row);
                }
                s.cells.retain(|_, r| !r.is_empty());
                s.col_widths = sh.keys(&s.col_widths);
                s.hidden_cols = sh.set(&s.hidden_cols);
            }
        }
        shift_sheet_features(s, &sh);
        losses = crate::table_shift::shift_cf_and_tables(s, &sh);
    }
    let ed_name = name.clone();
    rewrite_all_formulas(tx.wb, &move |e, formula_sheet| {
        let ed = StructEdit {
            sheet: &ed_name,
            axis,
            at,
            count,
        };
        adjust_for_struct_edit(e, formula_sheet, &ed);
    });
    crate::table_shift::invalidate_refs(tx.wb, &losses);
    Ok(())
}

/// Copied cells, relative to the copied range's top-left.
#[derive(Debug, Clone)]
pub struct ClipData {
    pub sheet: usize,
    pub range: CellRange,
    pub cells: Vec<(u32, u32, Cell)>,
    pub cut: bool,
}

pub fn copy(wb: &Workbook, sheet: usize, r: CellRange, cut: bool) -> Result<ClipData, OpError> {
    check_size(&r)?;
    let s = wb.sheet(sheet).ok_or(OpError::NoSheet)?;
    let cells = s
        .cells_in(r)
        .map(|(a, c)| (a.row - r.start.row, a.col - r.start.col, c.clone()))
        .collect();
    Ok(ClipData {
        sheet,
        range: r,
        cells,
        cut,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteMode {
    All,
    Values,
    Formulas,
    Formats,
    Transpose,
}

/// Paste at `dest` (top-left). Returns the destination range written.
pub fn paste(
    tx: &mut Tx<'_>,
    clip: &ClipData,
    sheet: usize,
    dest: CellAddr,
    mode: PasteMode,
) -> Result<CellRange, OpError> {
    tx.wb.sheet(sheet).ok_or(OpError::NoSheet)?;
    let (rows, cols) = if mode == PasteMode::Transpose {
        (clip.range.cols(), clip.range.rows())
    } else {
        (clip.range.rows(), clip.range.cols())
    };
    let end = dest
        .offset(i64::from(rows) - 1, i64::from(cols) - 1)
        .ok_or_else(|| OpError::Invalid("paste area extends beyond the sheet".into()))?;
    let out = CellRange::new(dest, end);
    // Clear the destination first (values modes keep destination formats).
    if matches!(mode, PasteMode::All | PasteMode::Transpose) {
        clear(tx, sheet, out, ClearWhat::All)?;
    }
    for (dr, dc, c) in &clip.cells {
        let (r, col) = if mode == PasteMode::Transpose {
            (*dc, *dr)
        } else {
            (*dr, *dc)
        };
        let Some(at) = dest.offset(i64::from(r), i64::from(col)) else {
            continue;
        };
        let src_at = CellAddr::new(clip.range.start.row + dr, clip.range.start.col + dc);
        let (sr, sc) = (
            i64::from(at.row) - i64::from(src_at.row),
            i64::from(at.col) - i64::from(src_at.col),
        );
        let existing = tx
            .wb
            .sheet(sheet)
            .and_then(|s| s.get(at))
            .cloned()
            .unwrap_or_default();
        let new = match mode {
            PasteMode::All | PasteMode::Transpose => moved_cell(c, sr, sc),
            PasteMode::Formulas => Cell {
                style: existing.style,
                ..moved_cell(c, sr, sc)
            },
            PasteMode::Values => Cell {
                content: Content::Value(c.value.clone()),
                value: c.value.clone(),
                style: existing.style,
            },
            PasteMode::Formats => Cell {
                style: c.style,
                ..existing
            },
        };
        tx.set_cell(sheet, at, Some(new));
    }
    if clip.cut && mode == PasteMode::All {
        let moved_into: Vec<CellAddr> = out.iter().collect();
        let src = tx
            .wb
            .sheet(clip.sheet)
            .map(|s| s.cells_in(clip.range).map(|(a, _)| a).collect::<Vec<_>>())
            .unwrap_or_default();
        for a in src {
            if !(clip.sheet == sheet && moved_into.contains(&a)) {
                tx.set_cell(clip.sheet, a, None);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::History;

    fn num(n: f64) -> Cell {
        Cell::constant(Value::Number(n))
    }

    fn a(s: &str) -> CellAddr {
        CellAddr::parse(s).unwrap()
    }

    fn src(wb: &Workbook, s: &str) -> String {
        wb.sheets[0]
            .get(a(s))
            .map(|c| c.input_text())
            .unwrap_or_default()
    }

    #[test]
    fn fill_down_shifts_formulas() {
        let mut wb = Workbook::new();
        wb.sheets[0].set(a("B1"), Cell::formula("A1*2"));
        let mut tx = Tx::new(&mut wb);
        fill(
            &mut tx,
            0,
            CellRange::parse("B1:B3").unwrap(),
            FillDir::Down,
        )
        .unwrap();
        tx.commit("fill");
        assert_eq!(src(&wb, "B3"), "=A3*2");
    }

    #[test]
    fn autofill_series() {
        let mut wb = Workbook::new();
        wb.sheets[0].set(a("A1"), num(1.0));
        wb.sheets[0].set(a("A2"), num(3.0));
        let mut tx = Tx::new(&mut wb);
        autofill(
            &mut tx,
            0,
            CellRange::parse("A1:A2").unwrap(),
            CellRange::parse("A1:A5").unwrap(),
        )
        .unwrap();
        assert_eq!(src(&wb, "A5"), "9");
    }

    #[test]
    fn sort_with_blanks_last() {
        let mut wb = Workbook::new();
        for (i, v) in [3.0, 1.0, 2.0].iter().enumerate() {
            wb.sheets[0].set(CellAddr::new(i as u32, 0), num(*v));
        }
        let mut tx = Tx::new(&mut wb);
        sort_rows(
            &mut tx,
            0,
            CellRange::parse("A1:A4").unwrap(),
            &[SortKey {
                col: 0,
                ascending: false,
            }],
            false,
        )
        .unwrap();
        assert_eq!(
            (src(&wb, "A1"), src(&wb, "A3"), src(&wb, "A4")),
            ("3".into(), "1".into(), "".into())
        );
    }

    #[test]
    fn insert_rows_fixes_refs_and_undoes() {
        let mut wb = Workbook::new();
        wb.sheets[0].set(a("A5"), num(5.0));
        wb.sheets[0].set(a("B1"), Cell::formula("SUM(A1:A5)"));
        wb.sheets.push(sp_model::Sheet::new("Other"));
        wb.sheets[1].set(a("A1"), Cell::formula("Sheet1!A5"));
        let mut h = History::new(10);
        let mut tx = Tx::new(&mut wb);
        insert_delete(&mut tx, 0, Axis::Rows, 2, 2).unwrap();
        h.push(tx.commit("insert").unwrap());
        assert_eq!(src(&wb, "A7"), "5");
        assert_eq!(src(&wb, "B1"), "=SUM(A1:A7)");
        assert_eq!(
            wb.sheets[1].get(a("A1")).unwrap().input_text(),
            "=Sheet1!A7"
        );
        h.undo(&mut wb);
        assert_eq!(src(&wb, "A5"), "5");
        assert_eq!(src(&wb, "B1"), "=SUM(A1:A5)");
    }

    #[test]
    fn delete_cols_makes_ref_errors() {
        let mut wb = Workbook::new();
        wb.sheets[0].set(a("A1"), Cell::formula("C1+D1"));
        let mut tx = Tx::new(&mut wb);
        insert_delete(&mut tx, 0, Axis::Cols, 2, -1).unwrap();
        assert_eq!(src(&wb, "A1"), "=#REF!+C1");
    }

    #[test]
    fn copy_paste_and_cut() {
        let mut wb = Workbook::new();
        wb.sheets[0].set(a("A1"), num(1.0));
        wb.sheets[0].set(a("B1"), Cell::formula("A1+1"));
        let clip = copy(&wb, 0, CellRange::parse("A1:B1").unwrap(), true).unwrap();
        let mut tx = Tx::new(&mut wb);
        paste(&mut tx, &clip, 0, a("A3"), PasteMode::All).unwrap();
        assert_eq!(src(&wb, "B3"), "=A3+1");
        assert_eq!(src(&wb, "A1"), "");
        let mut tx = Tx::new(&mut wb);
        assert!(
            paste(
                &mut tx,
                &clip,
                0,
                CellAddr::new(MAX_ROWS - 1, 0),
                PasteMode::Transpose
            )
            .is_err()
        );
    }

    fn r(s: &str) -> CellRange {
        CellRange::parse(s).unwrap()
    }

    /// A sheet carrying one of every per-cell feature, for insert/delete tests.
    fn featured() -> Workbook {
        use sp_model::{
            AllowEditRange, AutoFilter, Comment, DataValidation, FilterColumn, FilterCriteria,
            Hyperlink, SpillArea, ValidationKind,
        };
        let mut wb = Workbook::new();
        let s = &mut wb.sheets[0];
        s.comments.push(Comment {
            addr: a("B5"),
            kind: Default::default(),
            author: "me".into(),
            text: "note".into(),
            replies: vec![],
            visible: false,
            resolved: false,
        });
        s.hyperlinks.push(Hyperlink {
            range: r("C5:C6"),
            target: "https://example.com".into(),
            location: String::new(),
            tooltip: String::new(),
            display: String::new(),
        });
        s.validations.push(DataValidation {
            ranges: vec![r("D3:D8")],
            kind: ValidationKind::List,
            formula1: "$H$1:$H$4".into(),
            ..Default::default()
        });
        s.auto_filter = Some(AutoFilter {
            range: r("A2:E10"),
            columns: vec![FilterColumn {
                col: 3,
                criteria: FilterCriteria::Values {
                    values: vec!["x".into()],
                    blanks: false,
                },
            }],
        });
        s.page_setup.row_breaks.insert(6);
        s.page_setup.col_breaks.insert(3);
        for row in 3..=6 {
            s.outline.rows.insert(row, 1);
        }
        s.outline.cols.insert(2, 1);
        s.allow_edit_ranges.push(AllowEditRange {
            title: "r".into(),
            ranges: vec![r("E4:E6")],
            password: None,
        });
        s.set(a("G12"), Cell::formula("{1;2}"));
        s.spills.push(SpillArea {
            range: r("G12:G13"),
            fixed: true,
            blocked: false,
        });
        wb.names.push(sp_model::DefinedName {
            name: "_xlnm.Print_Area".into(),
            formula: "Sheet1!$A$1:$E$10".into(),
            scope: Some(0),
        });
        wb.names.push(sp_model::DefinedName {
            name: "_xlnm.Print_Titles".into(),
            formula: "Sheet1!$2:$2".into(),
            scope: Some(0),
        });
        wb
    }

    #[test]
    fn insert_rows_moves_features_and_undo_restores() {
        let mut wb = featured();
        let before = wb.clone();
        let mut h = History::new(10);
        let mut tx = Tx::new(&mut wb);
        insert_delete(&mut tx, 0, Axis::Rows, 3, 2).unwrap(); // above row 4
        h.push(tx.commit("insert").unwrap());
        let s = &wb.sheets[0];
        assert_eq!(s.comments[0].addr, a("B7"));
        assert_eq!(s.hyperlinks[0].range, r("C7:C8"));
        assert_eq!(s.validations[0].ranges, vec![r("D3:D10")]);
        assert_eq!(s.validations[0].formula1, "$H$1:$H$6");
        let af = s.auto_filter.as_ref().unwrap();
        assert_eq!(af.range, r("A2:E12"));
        assert_eq!(af.columns[0].col, 3);
        assert!(s.page_setup.row_breaks.contains(&8));
        // Inserted above the group's first row: the group just moves.
        assert_eq!(
            s.outline.rows.keys().copied().collect::<Vec<_>>(),
            vec![5, 6, 7, 8]
        );
        assert_eq!(s.allow_edit_ranges[0].ranges, vec![r("E6:E8")]);
        assert_eq!(s.spills[0].range, r("G14:G15"));
        assert!(s.get(a("G14")).is_some());
        assert_eq!(wb.names[0].formula, "Sheet1!$A$1:$E$12");
        assert_eq!(wb.names[1].formula, "Sheet1!$2:$2");
        h.undo(&mut wb);
        assert_eq!(wb, before);
    }

    #[test]
    fn delete_rows_shrinks_and_drops_features() {
        let mut wb = featured();
        let mut tx = Tx::new(&mut wb);
        insert_delete(&mut tx, 0, Axis::Rows, 4, -2).unwrap(); // rows 5:6
        let s = &wb.sheets[0];
        assert!(s.comments.is_empty());
        assert!(s.hyperlinks.is_empty());
        assert_eq!(s.validations[0].ranges, vec![r("D3:D6")]);
        assert_eq!(s.auto_filter.as_ref().unwrap().range, r("A2:E8"));
        assert!(s.page_setup.row_breaks.contains(&4));
        assert_eq!(
            s.outline.rows.keys().copied().collect::<Vec<_>>(),
            vec![3, 4]
        );
        assert_eq!(s.allow_edit_ranges[0].ranges, vec![r("E4")]);
        assert_eq!(s.spills[0].range, r("G10:G11"));
        // Deleting the filter header row removes the filter; a fully deleted validation vanishes.
        let mut tx = Tx::new(&mut wb);
        insert_delete(&mut tx, 0, Axis::Rows, 1, -6).unwrap();
        let s = &wb.sheets[0];
        assert!(s.auto_filter.is_none());
        assert!(s.validations.is_empty());
        assert!(s.allow_edit_ranges.is_empty());
    }

    #[test]
    fn insert_inside_outline_group_joins_it() {
        let mut wb = featured();
        let mut tx = Tx::new(&mut wb);
        insert_delete(&mut tx, 0, Axis::Rows, 5, 1).unwrap();
        let s = &wb.sheets[0];
        assert_eq!(
            s.outline.rows.keys().copied().collect::<Vec<_>>(),
            vec![3, 4, 5, 6, 7]
        );
    }

    #[test]
    fn insert_delete_cols_moves_features() {
        let mut wb = featured();
        let mut tx = Tx::new(&mut wb);
        insert_delete(&mut tx, 0, Axis::Cols, 1, 1).unwrap(); // before B
        let s = &wb.sheets[0];
        assert_eq!(s.comments[0].addr, a("C5"));
        assert_eq!(s.validations[0].ranges, vec![r("E3:E8")]);
        assert_eq!(s.validations[0].formula1, "$I$1:$I$4");
        let af = s.auto_filter.as_ref().unwrap();
        assert_eq!((af.range, af.columns[0].col), (r("A2:F10"), 4));
        assert!(s.page_setup.col_breaks.contains(&4));
        assert_eq!(s.outline.cols.get(&3), Some(&1));
        assert_eq!(s.spills[0].range, r("H12:H13"));
        assert_eq!(wb.names[0].formula, "Sheet1!$A$1:$F$10");
        // Deleting the filtered column drops its criteria.
        let mut tx = Tx::new(&mut wb);
        insert_delete(&mut tx, 0, Axis::Cols, 4, -1).unwrap();
        let s = &wb.sheets[0];
        let af = s.auto_filter.as_ref().unwrap();
        assert_eq!(af.range, r("A2:E10"));
        assert!(af.columns.is_empty());
        assert!(s.validations.is_empty());
        assert!(s.page_setup.col_breaks.is_empty());
    }

    #[test]
    fn validation_on_other_sheet_follows_source() {
        let mut wb = featured();
        wb.sheets.push(sp_model::Sheet::new("Other"));
        wb.sheets[1].validations.push(sp_model::DataValidation {
            ranges: vec![r("A1")],
            kind: sp_model::ValidationKind::List,
            formula1: "Sheet1!$H$1:$H$4".into(),
            ..Default::default()
        });
        wb.sheets[1].validations.push(sp_model::DataValidation {
            ranges: vec![r("A2")],
            kind: sp_model::ValidationKind::List,
            formula1: "\"a,b,c\"".into(),
            ..Default::default()
        });
        wb.sheets[1].hyperlinks.push(sp_model::Hyperlink {
            range: r("C1"),
            target: String::new(),
            location: "Sheet1!B5".into(),
            tooltip: String::new(),
            display: String::new(),
        });
        let mut tx = Tx::new(&mut wb);
        insert_delete(&mut tx, 0, Axis::Rows, 0, 1).unwrap();
        assert_eq!(wb.sheets[1].hyperlinks[0].location, "Sheet1!B6");
        assert_eq!(wb.sheets[1].validations[0].formula1, "Sheet1!$H$2:$H$5");
        assert_eq!(wb.sheets[1].validations[1].formula1, "\"a,b,c\"");
    }

    #[test]
    fn validation_anchor_deleted_rebases_relative_refs() {
        let mut wb = Workbook::new();
        wb.sheets[0].validations.push(sp_model::DataValidation {
            ranges: vec![r("B2:B6")],
            kind: sp_model::ValidationKind::Custom,
            formula1: "B2>A2".into(),
            ..Default::default()
        });
        let mut tx = Tx::new(&mut wb);
        insert_delete(&mut tx, 0, Axis::Rows, 1, -2).unwrap(); // rows 2:3
        let dv = &wb.sheets[0].validations[0];
        assert_eq!(dv.ranges, vec![r("B2:B4")]);
        // Old B4 (now B2) checked B4>A4 before; it still checks itself against column A.
        assert_eq!(dv.formula1, "B2>A2");
    }

    #[test]
    fn splitting_fixed_array_is_refused() {
        let mut wb = featured();
        let before = wb.clone();
        let mut tx = Tx::new(&mut wb);
        assert!(insert_delete(&mut tx, 0, Axis::Rows, 12, 1).is_err());
        assert!(insert_delete(&mut tx, 0, Axis::Rows, 12, -1).is_err());
        drop(tx);
        assert_eq!(wb, before);
        // Deleting the whole array's rows is fine.
        let mut tx = Tx::new(&mut wb);
        insert_delete(&mut tx, 0, Axis::Rows, 11, -2).unwrap();
        assert!(wb.sheets[0].spills.is_empty());
    }

    #[test]
    fn dynamic_spill_cut_clears_stale_values() {
        let mut wb = Workbook::new();
        let s = &mut wb.sheets[0];
        s.set(a("A1"), Cell::formula("SEQUENCE(3)"));
        for row in 1..3 {
            s.entry(CellAddr::new(row, 0)).value = Value::Number(f64::from(row + 1));
        }
        s.spills.push(sp_model::SpillArea {
            range: r("A1:A3"),
            fixed: false,
            blocked: false,
        });
        let mut tx = Tx::new(&mut wb);
        insert_delete(&mut tx, 0, Axis::Rows, 1, 1).unwrap();
        let s = &wb.sheets[0];
        assert_eq!(s.cell_count(), 1);
        assert_eq!(s.spills[0].range, r("A1:A4"));
    }
}
