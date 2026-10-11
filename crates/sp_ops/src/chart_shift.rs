//! Row/column insert and delete for charts and sparklines: anchors move and resize with
//! their cells (a corner whose line is deleted snaps to the edit position), sparklines move
//! with their cells or go with them, and series / sparkline references are rewritten like
//! any other formula.

use sp_core::CellAddr;
use sp_model::Sheet;
use sp_model::chart::Anchor;
use sp_model::features::LineShift;

/// A corner on line `v` (with in-cell offset `off`) after the edit.
fn corner(sh: &LineShift, v: u32, off: f64) -> (u32, f64) {
    match sh.index(v) {
        Some(n) => (n, off),
        // Deleted line: the corner lands where the deleted block was.
        None => (sh.at.min(sh.max.saturating_sub(1)), 0.0),
    }
}

fn shift_anchor(a: &mut Anchor, sh: &LineShift) {
    let (from_v, from_off, to_v, to_off) = if sh.rows {
        (a.from.row, a.from_off.1, a.to.row, a.to_off.1)
    } else {
        (a.from.col, a.from_off.0, a.to.col, a.to_off.0)
    };
    let (f, fo) = corner(sh, from_v, from_off);
    let (t, to) = corner(sh, to_v, to_off);
    if sh.rows {
        a.from = CellAddr::new(f, a.from.col);
        a.to = CellAddr::new(t, a.to.col);
        a.from_off.1 = fo;
        a.to_off.1 = to;
    } else {
        a.from = CellAddr::new(a.from.row, f);
        a.to = CellAddr::new(a.to.row, t);
        a.from_off.0 = fo;
        a.to_off.0 = to;
    }
}

/// Move chart anchors and sparkline cells of the edited sheet.
pub fn shift_drawings(s: &mut Sheet, sh: &LineShift) {
    for c in &mut s.drawings.charts {
        shift_anchor(&mut c.anchor, sh);
    }
    for g in &mut s.drawings.sparklines {
        g.items = std::mem::take(&mut g.items)
            .into_iter()
            .filter_map(|mut sp| {
                sp.location = sh.addr(sp.location)?;
                Some(sp)
            })
            .collect();
    }
    s.drawings.sparklines.retain(|g| !g.items.is_empty());
}

/// Rewrite every chart series reference and sparkline data range on `s` with `f` (returns
/// the new text, `None` when unchanged).
pub fn rewrite_formulas(s: &mut Sheet, f: &dyn Fn(&str) -> Option<String>) {
    let texts = s
        .drawings
        .charts
        .iter_mut()
        .flat_map(|c| c.series.iter_mut())
        .flat_map(|ser| [&mut ser.name, &mut ser.categories, &mut ser.values])
        .chain(
            s.drawings
                .sparklines
                .iter_mut()
                .flat_map(|g| g.items.iter_mut())
                .map(|sp| &mut sp.data),
        );
    for t in texts {
        if t.trim().is_empty() {
            continue;
        }
        if let Some(new) = f(t) {
            *t = new;
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::edit::insert_delete;
    use crate::history::Tx;
    use sp_core::CellAddr;
    use sp_formula::shift::Axis;
    use sp_model::chart::{Anchor, Chart, Series, Sparkline, SparklineGroup};
    use sp_model::{Sheet, Workbook};

    fn book() -> Workbook {
        let mut wb = Workbook::new();
        wb.sheets.push(Sheet::new("Other"));
        let s = &mut wb.sheets[0];
        s.drawings.charts.push(Chart {
            anchor: Anchor {
                from: CellAddr::new(2, 4),
                from_off: (5.0, 6.0),
                to: CellAddr::new(12, 9),
                to_off: (7.0, 8.0),
            },
            series: vec![Series {
                name: "Sheet1!$B$1".into(),
                categories: "Sheet1!$A$2:$A$6".into(),
                values: "Sheet1!$B$2:$B$6".into(),
                ..Series::default()
            }],
            ..Chart::default()
        });
        s.drawings.sparklines.push(SparklineGroup {
            items: vec![
                Sparkline {
                    location: CellAddr::new(1, 3),
                    data: "Sheet1!A2:C2".into(),
                },
                Sparkline {
                    location: CellAddr::new(4, 3),
                    data: "Sheet1!A5:C5".into(),
                },
            ],
            ..SparklineGroup::default()
        });
        wb
    }

    fn edit(wb: &mut Workbook, sheet: usize, axis: Axis, at: u32, count: i64) {
        let mut tx = Tx::new(wb);
        insert_delete(&mut tx, sheet, axis, at, count).unwrap();
        tx.commit("edit");
    }

    #[test]
    fn insert_rows_moves_anchor_refs_and_sparklines() {
        let mut wb = book();
        edit(&mut wb, 0, Axis::Rows, 0, 2);
        let s = &wb.sheets[0];
        let c = &s.drawings.charts[0];
        assert_eq!(c.anchor.from, CellAddr::new(4, 4));
        assert_eq!(c.anchor.to, CellAddr::new(14, 9));
        assert_eq!(c.anchor.from_off, (5.0, 6.0));
        assert_eq!(c.series[0].name, "Sheet1!$B$3");
        assert_eq!(c.series[0].values, "Sheet1!$B$4:$B$8");
        let g = &s.drawings.sparklines[0];
        assert_eq!(g.items[0].location, CellAddr::new(3, 3));
        assert_eq!(g.items[0].data, "Sheet1!A4:C4");
    }

    #[test]
    fn insert_inside_grows_anchor_and_range() {
        let mut wb = book();
        edit(&mut wb, 0, Axis::Rows, 3, 1);
        let c = &wb.sheets[0].drawings.charts[0];
        assert_eq!(c.anchor.from.row, 2);
        assert_eq!(c.anchor.to.row, 13);
        assert_eq!(c.series[0].values, "Sheet1!$B$2:$B$7");
    }

    #[test]
    fn delete_cols_shrinks_and_drops() {
        let mut wb = book();
        // Delete C:D (cols 2..=3): sparkline cells in D go, the chart's left edge (E) moves
        // to C, data ranges A:C lose C.
        edit(&mut wb, 0, Axis::Cols, 2, -2);
        let s = &wb.sheets[0];
        assert!(s.drawings.sparklines.is_empty());
        let c = &s.drawings.charts[0];
        assert_eq!(c.anchor.from.col, 2);
        assert_eq!(c.anchor.to.col, 7);
        assert_eq!(c.anchor.from_off.0, 5.0);
        // Deleting the categories' column turns the reference into #REF!.
        edit(&mut wb, 0, Axis::Cols, 0, -1);
        let c = &wb.sheets[0].drawings.charts[0];
        assert!(
            c.series[0].categories.contains("#REF!"),
            "{}",
            c.series[0].categories
        );
        assert_eq!(c.series[0].values, "Sheet1!$A$2:$A$6");
    }

    #[test]
    fn deleting_the_anchor_rows_collapses_without_panic() {
        let mut wb = book();
        edit(&mut wb, 0, Axis::Rows, 2, -11);
        let c = &wb.sheets[0].drawings.charts[0];
        assert_eq!(c.anchor.from.row, 2);
        assert_eq!(c.anchor.to.row, 2);
        assert_eq!(c.anchor.from_off.1, 0.0);
        // Pushing everything off the sheet's end is fine too.
        edit(&mut wb, 0, Axis::Rows, 0, i64::from(sp_core::MAX_ROWS) - 1);
    }

    #[test]
    fn edits_on_other_sheets_only_rewrite_refs() {
        let mut wb = book();
        wb.sheets[0].drawings.charts[0].series[0].values = "Other!$B$2:$B$6".into();
        edit(&mut wb, 1, Axis::Rows, 0, 1);
        let c = &wb.sheets[0].drawings.charts[0];
        assert_eq!(c.anchor.from.row, 2);
        assert_eq!(c.series[0].values, "Other!$B$3:$B$7");
        assert_eq!(c.series[0].categories, "Sheet1!$A$2:$A$6");
    }
}
