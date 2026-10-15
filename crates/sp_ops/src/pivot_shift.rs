//! Row/column insert/delete and sheet renames for the analysis objects of a sheet (pivot
//! tables, scenarios, data tables): every reference they hold is formula text, rewritten
//! like any other formula, so pivot sources/locations/outputs, scenario changing cells and
//! data-table areas move with their cells (a deleted area becomes `#REF!`).

use sp_model::Sheet;

/// Rewrite every reference text of `s`'s analysis objects with `f` (returns the new text,
/// `None` when unchanged). Table-name pivot sources parse as names and stay as they are.
pub fn rewrite_formulas(s: &mut Sheet, f: &dyn Fn(&str) -> Option<String>) {
    let a = &mut s.analysis;
    let texts = a
        .pivots
        .iter_mut()
        .flat_map(|p| [&mut p.source, &mut p.location, &mut p.output])
        .chain(a.scenarios.iter_mut().map(|sc| &mut sc.changing))
        .chain(
            a.data_tables
                .iter_mut()
                .flat_map(|d| [&mut d.range, &mut d.row_input, &mut d.col_input]),
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

/// Apply an expression rewrite (e.g. a sheet rename) to every reference text of `s`.
pub fn rewrite_exprs(s: &mut Sheet, f: &dyn Fn(&mut sp_formula::Expr)) {
    rewrite_formulas(s, &|src| {
        let orig = sp_formula::parse(src).ok()?;
        let mut e = orig.clone();
        f(&mut e);
        (e != orig).then(|| sp_formula::to_formula(&e))
    });
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use crate::edit::insert_delete;
    use crate::history::Tx;
    use sp_formula::shift::Axis;
    use sp_model::Workbook;
    use sp_model::pivot::{DataTable, PivotTable, Scenario};

    fn book() -> Workbook {
        let mut wb = Workbook::new();
        wb.sheets.push(sp_model::Sheet::new("Data"));
        let a = &mut wb.sheets[0].analysis;
        a.pivots.push(PivotTable {
            name: "P1".into(),
            source: "Data!$A$1:$C$10".into(),
            location: "$E$3".into(),
            output: "$E$3:$G$8".into(),
            ..PivotTable::default()
        });
        a.scenarios.push(Scenario {
            name: "S".into(),
            changing: "$B$2,$B$5".into(),
            ..Scenario::default()
        });
        a.data_tables.push(DataTable {
            range: "$A$20:$C$25".into(),
            row_input: String::new(),
            col_input: "$B$2".into(),
        });
        wb
    }

    #[test]
    fn rows_inserted_above_move_everything() {
        let mut wb = book();
        let mut tx = Tx::new(&mut wb);
        insert_delete(&mut tx, 0, Axis::Rows, 1, 2).unwrap();
        drop(tx);
        let a = &wb.sheets[0].analysis;
        assert_eq!(a.pivots[0].location, "$E$5");
        assert_eq!(a.pivots[0].output, "$E$5:$G$10");
        // Source on another sheet is untouched by an edit here.
        assert_eq!(a.pivots[0].source, "Data!$A$1:$C$10");
        assert_eq!(a.scenarios[0].changing, "$B$4,$B$7");
        assert_eq!(a.data_tables[0].range, "$A$22:$C$27");
        assert_eq!(a.data_tables[0].col_input, "$B$4");
    }

    #[test]
    fn source_sheet_edit_and_delete() {
        let mut wb = book();
        let mut tx = Tx::new(&mut wb);
        insert_delete(&mut tx, 1, Axis::Cols, 0, 1).unwrap();
        insert_delete(&mut tx, 0, Axis::Cols, 4, -1).unwrap();
        drop(tx);
        let p = &wb.sheets[0].analysis.pivots[0];
        assert_eq!(p.source, "Data!$B$1:$D$10");
        assert_eq!(p.output, "$E$3:$F$8");
        let mut tx = Tx::new(&mut wb);
        insert_delete(&mut tx, 0, Axis::Rows, 2, -1).unwrap();
        drop(tx);
        assert!(wb.sheets[0].analysis.pivots[0].location.contains("#REF!"));
    }

    #[test]
    fn rename_rewrites_source() {
        let mut wb = book();
        super::rewrite_exprs(&mut wb.sheets[0], &|e| {
            sp_formula::shift::rename_sheet(e, "Data", "Raw Data")
        });
        assert_eq!(
            wb.sheets[0].analysis.pivots[0].source,
            "'Raw Data'!$A$1:$C$10"
        );
        assert_eq!(wb.sheets[0].analysis.pivots[0].location, "$E$3");
    }
}
