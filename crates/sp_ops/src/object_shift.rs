//! Row/column insert and delete for pictures, shapes and text boxes, by placement:
//! - move and size: both corners follow their cells (like charts);
//! - move only: the top-left corner follows its cell and the bottom-right keeps the same
//!   distance in lines from it (exact for uniform row heights / column widths);
//! - fixed, and absolute anchors: unchanged.
//!
//! A one-cell anchor moves its top-left corner and keeps its size.

use sp_core::CellAddr;
use sp_model::Sheet;
use sp_model::chart::Anchor;
use sp_model::features::LineShift;
use sp_model::objects::{AnchorKind, Placement};

/// Line `v` after the edit; a deleted line snaps to the edit position.
fn line(sh: &LineShift, v: u32) -> (u32, bool) {
    match sh.index(v) {
        Some(n) => (n, true),
        None => (sh.at.min(sh.max.saturating_sub(1)), false),
    }
}

fn get(a: &Anchor, rows: bool) -> (u32, f64, u32, f64) {
    if rows {
        (a.from.row, a.from_off.1, a.to.row, a.to_off.1)
    } else {
        (a.from.col, a.from_off.0, a.to.col, a.to_off.0)
    }
}

fn set(a: &mut Anchor, rows: bool, f: (u32, f64), t: (u32, f64)) {
    if rows {
        a.from = CellAddr::new(f.0, a.from.col);
        a.to = CellAddr::new(t.0, a.to.col);
        a.from_off.1 = f.1;
        a.to_off.1 = t.1;
    } else {
        a.from = CellAddr::new(a.from.row, f.0);
        a.to = CellAddr::new(a.to.row, t.0);
        a.from_off.0 = f.1;
        a.to_off.0 = t.1;
    }
}

fn move_and_size(a: &mut Anchor, sh: &LineShift) {
    let (fv, fo, tv, to) = get(a, sh.rows);
    let (f, fk) = line(sh, fv);
    let (t, tk) = line(sh, tv);
    set(
        a,
        sh.rows,
        (f, if fk { fo } else { 0.0 }),
        (t, if tk { to } else { 0.0 }),
    );
}

fn move_only(a: &mut Anchor, sh: &LineShift) {
    let (fv, fo, tv, to) = get(a, sh.rows);
    let (f, fk) = line(sh, fv);
    let span = tv.saturating_sub(fv);
    let t = f.saturating_add(span).min(sh.max.saturating_sub(1));
    set(a, sh.rows, (f, if fk { fo } else { 0.0 }), (t, to));
}

/// Move the objects of the edited sheet.
pub fn shift_objects(s: &mut Sheet, sh: &LineShift) {
    for o in &mut s.objects {
        let a = &mut o.anchor;
        match a.kind {
            AnchorKind::Absolute => {}
            AnchorKind::OneCell => move_only(&mut a.cells, sh),
            AnchorKind::TwoCell => match a.placement {
                Placement::MoveAndSize => move_and_size(&mut a.cells, sh),
                Placement::Move => move_only(&mut a.cells, sh),
                Placement::Fixed => {}
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::edit::insert_delete;
    use crate::history::Tx;
    use sp_core::CellAddr;
    use sp_formula::shift::Axis;
    use sp_model::Workbook;
    use sp_model::chart::Anchor;
    use sp_model::objects::{AnchorKind, DrawObject, ObjectAnchor, Placement};

    fn obj(kind: AnchorKind, placement: Placement) -> DrawObject {
        DrawObject {
            anchor: ObjectAnchor {
                kind,
                placement,
                cells: Anchor {
                    from: CellAddr::new(2, 2),
                    from_off: (4.0, 5.0),
                    to: CellAddr::new(6, 5),
                    to_off: (6.0, 7.0),
                },
                ext: (100.0, 50.0),
                pos: (30.0, 40.0),
            },
            ..DrawObject::default()
        }
    }

    fn book() -> Workbook {
        let mut wb = Workbook::new();
        let s = &mut wb.sheets[0];
        s.objects
            .push(obj(AnchorKind::TwoCell, Placement::MoveAndSize));
        s.objects.push(obj(AnchorKind::TwoCell, Placement::Move));
        s.objects.push(obj(AnchorKind::TwoCell, Placement::Fixed));
        s.objects.push(obj(AnchorKind::OneCell, Placement::Move));
        s.objects.push(obj(AnchorKind::Absolute, Placement::Fixed));
        wb
    }

    fn edit(wb: &mut Workbook, axis: Axis, at: u32, count: i64) {
        let mut tx = Tx::new(wb);
        insert_delete(&mut tx, 0, axis, at, count).unwrap();
        tx.commit("edit");
    }

    fn rows(wb: &Workbook, i: usize) -> (u32, u32) {
        let a = wb.sheets[0].objects[i].anchor.cells;
        (a.from.row, a.to.row)
    }

    #[test]
    fn insert_above_moves_by_placement() {
        let mut wb = book();
        edit(&mut wb, Axis::Rows, 0, 3);
        assert_eq!(rows(&wb, 0), (5, 9));
        assert_eq!(rows(&wb, 1), (5, 9));
        assert_eq!(rows(&wb, 2), (2, 6));
        assert_eq!(rows(&wb, 3).0, 5);
        assert_eq!(rows(&wb, 4), (2, 6));
        assert_eq!(wb.sheets[0].objects[0].anchor.cells.from_off, (4.0, 5.0));
    }

    #[test]
    fn insert_inside_sizes_only_move_and_size() {
        let mut wb = book();
        edit(&mut wb, Axis::Rows, 4, 2);
        assert_eq!(rows(&wb, 0), (2, 8));
        assert_eq!(rows(&wb, 1), (2, 6));
        assert_eq!(rows(&wb, 2), (2, 6));
    }

    #[test]
    fn delete_cols_collapses_without_panic() {
        let mut wb = book();
        edit(&mut wb, Axis::Cols, 1, -5);
        let a = wb.sheets[0].objects[0].anchor.cells;
        assert_eq!((a.from.col, a.to.col), (1, 1));
        assert_eq!(a.from_off.0, 0.0);
        let b = wb.sheets[0].objects[1].anchor.cells;
        assert_eq!((b.from.col, b.to.col), (1, 4));
        // Pushing everything off the end is fine too.
        edit(&mut wb, Axis::Rows, 0, i64::from(sp_core::MAX_ROWS) - 1);
        let c = wb.sheets[0].objects[1].anchor.cells;
        assert!(c.to.row < sp_core::MAX_ROWS);
    }
}
