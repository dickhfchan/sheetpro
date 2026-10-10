//! Reference rewriting: copy/fill offsets, structural edits, sheet renames.

use crate::ast::{CellPart, Expr, RefExpr, RefKind};
use sp_core::{ErrorKind, MAX_COLS, MAX_ROWS};

fn moved(v: u32, abs: bool, d: i64, max: u32) -> Option<u32> {
    if abs {
        return Some(v);
    }
    let n = i64::from(v).checked_add(d)?;
    (0..i64::from(max)).contains(&n).then_some(n as u32)
}

fn shift_part(c: &CellPart, dr: i64, dc: i64) -> Option<CellPart> {
    Some(CellPart {
        row: moved(c.row, c.row_abs, dr, MAX_ROWS)?,
        col: moved(c.col, c.col_abs, dc, MAX_COLS)?,
        ..*c
    })
}

/// Rewrite relative references as if the formula moved by (dr, dc), as copy/paste and fill
/// do. References pushed off the sheet become `#REF!`.
pub fn shift_for_copy(e: &mut Expr, dr: i64, dc: i64) {
    e.walk_mut(&mut |node| {
        let Expr::Ref(r) = node else { return };
        let new_kind = match &r.kind {
            RefKind::Cell(c) => shift_part(c, dr, dc).map(RefKind::Cell),
            RefKind::Area(a, b) => match (shift_part(a, dr, dc), shift_part(b, dr, dc)) {
                (Some(a), Some(b)) => Some(RefKind::Area(a, b)),
                _ => None,
            },
            RefKind::Cols {
                c0,
                c0_abs,
                c1,
                c1_abs,
            } => {
                match (
                    moved(*c0, *c0_abs, dc, MAX_COLS),
                    moved(*c1, *c1_abs, dc, MAX_COLS),
                ) {
                    (Some(a), Some(b)) => Some(RefKind::Cols {
                        c0: a,
                        c0_abs: *c0_abs,
                        c1: b,
                        c1_abs: *c1_abs,
                    }),
                    _ => None,
                }
            }
            RefKind::Rows {
                r0,
                r0_abs,
                r1,
                r1_abs,
            } => {
                match (
                    moved(*r0, *r0_abs, dr, MAX_ROWS),
                    moved(*r1, *r1_abs, dr, MAX_ROWS),
                ) {
                    (Some(a), Some(b)) => Some(RefKind::Rows {
                        r0: a,
                        r0_abs: *r0_abs,
                        r1: b,
                        r1_abs: *r1_abs,
                    }),
                    _ => None,
                }
            }
        };
        match new_kind {
            Some(k) => r.kind = k,
            None => *node = Expr::Error(ErrorKind::Ref),
        }
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Rows,
    Cols,
}

/// A structural edit on one sheet: insert (`count > 0`) or delete (`count < 0`) `|count|`
/// rows/cols starting at index `at`.
#[derive(Debug, Clone, Copy)]
pub struct StructEdit<'a> {
    pub sheet: &'a str,
    pub axis: Axis,
    pub at: u32,
    pub count: i64,
}

/// Adjust one index for an insert/delete. For deletes, `None` = index was removed.
fn adjust_index(v: u32, ed: &StructEdit<'_>, max: u32) -> Option<u32> {
    let v64 = i64::from(v);
    let at = i64::from(ed.at);
    if ed.count >= 0 {
        if v64 >= at {
            let n = v64 + ed.count;
            (n < i64::from(max)).then_some(n as u32)
        } else {
            Some(v)
        }
    } else {
        let del = -ed.count;
        if v64 < at {
            Some(v)
        } else if v64 < at + del {
            None
        } else {
            Some((v64 - del) as u32)
        }
    }
}

/// Adjust a span [lo, hi] for a structural edit; spans shrink on partial deletes and
/// vanish (`None`) when fully deleted.
fn adjust_span(lo: u32, hi: u32, ed: &StructEdit<'_>, max: u32) -> Option<(u32, u32)> {
    if ed.count >= 0 {
        let lo2 = adjust_index(lo, ed, max)?;
        // An insert strictly inside the span grows it.
        let hi2 = adjust_index(hi, ed, max).unwrap_or(max - 1);
        return Some((lo2, hi2));
    }
    let del = -ed.count;
    let (at, end) = (i64::from(ed.at), i64::from(ed.at) + del - 1);
    let (l, h) = (i64::from(lo), i64::from(hi));
    if l >= at && h <= end {
        return None;
    }
    let new_lo = if l < at {
        l
    } else if l <= end {
        at
    } else {
        l - del
    };
    let new_hi = if h < at {
        h
    } else if h <= end {
        at - 1
    } else {
        h - del
    };
    (new_lo <= new_hi && new_lo >= 0).then_some((new_lo as u32, new_hi as u32))
}

/// Rewrite references after inserting/deleting rows or columns on `ed.sheet`. `formula_sheet`
/// is the name of the sheet holding this formula (unqualified refs point there).
pub fn adjust_for_struct_edit(e: &mut Expr, formula_sheet: &str, ed: &StructEdit<'_>) {
    e.walk_mut(&mut |node| {
        let Expr::Ref(r) = node else { return };
        let target = r.sheet.as_deref().unwrap_or(formula_sheet);
        if !target.eq_ignore_ascii_case(ed.sheet) {
            return;
        }
        let max = if ed.axis == Axis::Rows {
            MAX_ROWS
        } else {
            MAX_COLS
        };
        let new_kind = match (&r.kind, ed.axis) {
            (RefKind::Cell(c), Axis::Rows) => {
                adjust_index(c.row, ed, max).map(|row| RefKind::Cell(CellPart { row, ..*c }))
            }
            (RefKind::Cell(c), Axis::Cols) => {
                adjust_index(c.col, ed, max).map(|col| RefKind::Cell(CellPart { col, ..*c }))
            }
            (RefKind::Area(a, b), Axis::Rows) => {
                adjust_span(a.row, b.row, ed, max).map(|(lo, hi)| {
                    RefKind::Area(CellPart { row: lo, ..*a }, CellPart { row: hi, ..*b })
                })
            }
            (RefKind::Area(a, b), Axis::Cols) => {
                adjust_span(a.col, b.col, ed, max).map(|(lo, hi)| {
                    RefKind::Area(CellPart { col: lo, ..*a }, CellPart { col: hi, ..*b })
                })
            }
            (
                RefKind::Cols {
                    c0,
                    c0_abs,
                    c1,
                    c1_abs,
                },
                Axis::Cols,
            ) => adjust_span(*c0, *c1, ed, max).map(|(lo, hi)| RefKind::Cols {
                c0: lo,
                c0_abs: *c0_abs,
                c1: hi,
                c1_abs: *c1_abs,
            }),
            (
                RefKind::Rows {
                    r0,
                    r0_abs,
                    r1,
                    r1_abs,
                },
                Axis::Rows,
            ) => adjust_span(*r0, *r1, ed, max).map(|(lo, hi)| RefKind::Rows {
                r0: lo,
                r0_abs: *r0_abs,
                r1: hi,
                r1_abs: *r1_abs,
            }),
            (k, _) => Some(k.clone()),
        };
        match new_kind {
            Some(k) => r.kind = k,
            None => *node = Expr::Error(ErrorKind::Ref),
        }
    });
}

/// Rename sheet references `old` → `new`.
pub fn rename_sheet(e: &mut Expr, old: &str, new: &str) {
    e.walk_mut(&mut |node| {
        if let Expr::Ref(RefExpr { sheet: Some(s), .. }) = node
            && s.eq_ignore_ascii_case(old)
        {
            *s = new.to_string();
        }
    });
}

/// Replace references to a deleted sheet with `#REF!`.
pub fn invalidate_sheet(e: &mut Expr, deleted: &str) {
    e.walk_mut(&mut |node| {
        if let Expr::Ref(RefExpr { sheet: Some(s), .. }) = node
            && s.eq_ignore_ascii_case(deleted)
        {
            *node = Expr::Error(ErrorKind::Ref);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{parse, to_formula};

    fn copy(src: &str, dr: i64, dc: i64) -> String {
        let mut e = parse(src).unwrap();
        shift_for_copy(&mut e, dr, dc);
        to_formula(&e)
    }

    fn edit(src: &str, axis: Axis, at: u32, count: i64) -> String {
        let mut e = parse(src).unwrap();
        adjust_for_struct_edit(
            &mut e,
            "Sheet1",
            &StructEdit {
                sheet: "Sheet1",
                axis,
                at,
                count,
            },
        );
        to_formula(&e)
    }

    #[test]
    fn copy_shift() {
        assert_eq!(copy("A1+$B$1+C$1+$D1", 1, 1), "B2+$B$1+D$1+$D2");
        assert_eq!(copy("SUM(A1:B2)", 2, 0), "SUM(A3:B4)");
        assert_eq!(copy("A1", -1, 0), "#REF!");
        assert_eq!(copy("A:A", 0, 2), "C:C");
    }

    #[test]
    fn insert_delete() {
        assert_eq!(edit("A5+A1", Axis::Rows, 2, 3), "A8+A1");
        assert_eq!(edit("SUM(A1:A10)", Axis::Rows, 4, 2), "SUM(A1:A12)");
        assert_eq!(edit("A5", Axis::Rows, 4, -1), "#REF!");
        assert_eq!(edit("SUM(A1:A10)", Axis::Rows, 2, -3), "SUM(A1:A7)");
        assert_eq!(edit("SUM(A3:A5)", Axis::Rows, 2, -3), "SUM(#REF!)");
        assert_eq!(edit("C1", Axis::Cols, 0, 1), "D1");
        assert_eq!(edit("Other!A5", Axis::Rows, 0, 1), "Other!A5");
    }

    #[test]
    fn renames() {
        let mut e = parse("Old!A1+'old'!B2+A1").unwrap();
        rename_sheet(&mut e, "Old", "New Name");
        assert_eq!(to_formula(&e), "'New Name'!A1+'New Name'!B2+A1");
    }
}
