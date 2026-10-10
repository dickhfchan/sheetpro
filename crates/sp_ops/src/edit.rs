//! Editing algorithms: clear, fill, sort, insert/delete rows & columns, clipboard.
//!
//! All functions take a recording `Tx` so they are undoable, and return `OpError` instead
//! of panicking on any input.

use crate::history::Tx;
use sp_core::{CellAddr, CellRange, MAX_CELLS_PER_OP, MAX_COLS, MAX_ROWS, Value};
use sp_formula::eval::compare;
use sp_formula::shift::{Axis, StructEdit, adjust_for_struct_edit, shift_for_copy};
use sp_formula::{parse, to_formula};
use sp_model::{Cell, Content, StyleId, Workbook};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

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

fn rewrite_all_formulas(wb: &mut Workbook, f: &dyn Fn(&mut sp_formula::Expr, &str)) {
    for si in 0..wb.sheets.len() {
        let Some(sheet) = wb.sheets.get_mut(si) else {
            continue;
        };
        let name = sheet.name.clone();
        for row in sheet.cells.values_mut() {
            for cell in row.values_mut() {
                if let Content::Formula(src) = &cell.content
                    && let Ok(mut e) = parse(src)
                {
                    f(&mut e, &name);
                    let new = to_formula(&e);
                    if new != *src {
                        cell.content = Content::Formula(new);
                    }
                }
            }
        }
    }
    for n in &mut wb.names {
        if let Ok(mut e) = parse(&n.formula) {
            f(&mut e, "");
            n.formula = to_formula(&e);
        }
    }
}

fn shift_keys<T: Clone>(m: &BTreeMap<u32, T>, at: u32, count: i64, max: u32) -> BTreeMap<u32, T> {
    let mut out = BTreeMap::new();
    for (k, v) in m {
        let k64 = i64::from(*k);
        if count < 0 && k64 >= i64::from(at) && k64 < i64::from(at) - count {
            continue;
        }
        let nk = if k64 >= i64::from(at) {
            k64 + count
        } else {
            k64
        };
        if (0..i64::from(max)).contains(&nk) {
            out.insert(nk as u32, v.clone());
        }
    }
    out
}

fn shift_set(s: &BTreeSet<u32>, at: u32, count: i64, max: u32) -> BTreeSet<u32> {
    let m: BTreeMap<u32, ()> = s.iter().map(|k| (*k, ())).collect();
    shift_keys(&m, at, count, max).into_keys().collect()
}

/// Insert (`count > 0`) or delete (`count < 0`) rows/columns on one sheet, fixing every
/// formula and defined name in the workbook that refers to the moved cells.
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
    tx.touch_book();
    let name = tx.wb.sheet(sheet).ok_or(OpError::NoSheet)?.name.clone();
    {
        let s = tx.wb.sheet_mut(sheet).ok_or(OpError::NoSheet)?;
        match axis {
            Axis::Rows => {
                s.cells = shift_keys(&s.cells, at, count, max);
                s.row_heights = shift_keys(&s.row_heights, at, count, max);
                s.hidden_rows = shift_set(&s.hidden_rows, at, count, max);
            }
            Axis::Cols => {
                for row in s.cells.values_mut() {
                    *row = shift_keys(row, at, count, max);
                }
                s.cells.retain(|_, r| !r.is_empty());
                s.col_widths = shift_keys(&s.col_widths, at, count, max);
                s.hidden_cols = shift_set(&s.hidden_cols, at, count, max);
            }
        }
        let ed = StructEdit {
            sheet: &name,
            axis,
            at,
            count,
        };
        s.merges = s
            .merges
            .iter()
            .filter_map(|m| {
                let mut e = parse(&m.to_a1()).ok()?;
                adjust_for_struct_edit(&mut e, &name, &ed);
                CellRange::parse(&to_formula(&e))
                    .ok()
                    .filter(|r| !r.is_single())
            })
            .collect();
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
}
