//! Row/column insert and delete for conditional formats and tables: move or shrink their
//! ranges, rename/insert table columns, drop tables whose header row (or every column) is
//! deleted, and turn structured references to removed tables/columns into `#REF!`.

use sp_core::{CellAddr, ErrorKind, Value};
use sp_formula::Expr;
use sp_formula::shift::shift_for_copy;
use sp_model::features::LineShift;
use sp_model::{Cell, CfKind, CfvoKind, Sheet, Table, TableColumn, Workbook};

/// What a structural edit removed: a whole table (`columns = None`) or some of its columns.
#[derive(Debug, Clone, PartialEq)]
pub struct TableLoss {
    /// Sheet name the table lived on (for unqualified references).
    pub sheet: String,
    pub table: String,
    pub columns: Option<Vec<String>>,
}

/// Formula texts of a sheet's conditional formats and tables.
fn formula_texts(s: &mut Sheet) -> Vec<&mut String> {
    let mut out: Vec<&mut String> = Vec::new();
    for cf in &mut s.cond_formats {
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
    }
    for t in &mut s.tables {
        for c in &mut t.columns {
            for f in [&mut c.calc_formula, &mut c.totals_formula]
                .into_iter()
                .flatten()
            {
                out.push(f);
            }
        }
    }
    out
}

/// Apply `f` (returns the rewritten text, or `None` when unchanged) to every formula of the
/// sheet's conditional formats and tables.
pub fn rewrite_formulas(s: &mut Sheet, f: &dyn Fn(&str) -> Option<String>) {
    for src in formula_texts(s) {
        if src.trim().is_empty() {
            continue;
        }
        if let Some(new) = f(src) {
            *src = new;
        }
    }
}

fn shift_table(t: &Table, sh: &LineShift, out: &mut Vec<TableLoss>, sheet: &str) -> Option<Table> {
    let lose = |cols: Option<Vec<String>>, out: &mut Vec<TableLoss>| {
        out.push(TableLoss {
            sheet: sheet.to_string(),
            table: t.name.clone(),
            columns: cols,
        });
    };
    let mut n = t.clone();
    if sh.rows {
        if t.header_row && sh.index(t.range.start.row).is_none() {
            lose(None, out);
            return None;
        }
        let Some(r) = sh.range(t.range) else {
            lose(None, out);
            return None;
        };
        if t.totals_row && sh.index(t.range.end.row).is_none() {
            n.totals_row = false;
        }
        n.range = r;
        return Some(n);
    }
    // Columns: keep surviving ones, add inserted ones inside the table.
    let mut cols: Vec<(u32, TableColumn)> = Vec::new();
    let mut gone = Vec::new();
    for (i, c) in t.columns.iter().enumerate() {
        let abs = t.range.start.col.checked_add(u32::try_from(i).ok()?)?;
        match sh.index(abs) {
            Some(new) => cols.push((new, c.clone())),
            None => gone.push(c.name.clone()),
        }
    }
    let Some(r) = sh.range(t.range) else {
        lose(None, out);
        return None;
    };
    if cols.is_empty() {
        lose(None, out);
        return None;
    }
    if !gone.is_empty() {
        lose(Some(gone), out);
    }
    let mut columns = Vec::new();
    let mut k = 1;
    for c in r.start.col..=r.end.col {
        match cols.iter().find(|(x, _)| *x == c) {
            Some((_, tc)) => columns.push(tc.clone()),
            None => {
                let name = loop {
                    let cand = format!("Column{k}");
                    k += 1;
                    if !t.columns.iter().any(|x| x.name.eq_ignore_ascii_case(&cand))
                        && !columns
                            .iter()
                            .any(|x: &TableColumn| x.name.eq_ignore_ascii_case(&cand))
                    {
                        break cand;
                    }
                };
                columns.push(TableColumn {
                    name,
                    ..TableColumn::default()
                });
            }
        }
    }
    n.range = r;
    n.columns = columns;
    Some(n)
}

/// Shift the sheet's conditional formats and tables (cells must already be moved).
pub fn shift_cf_and_tables(s: &mut Sheet, sh: &LineShift) -> Vec<TableLoss> {
    s.cond_formats = std::mem::take(&mut s.cond_formats)
        .into_iter()
        .filter_map(|mut cf| {
            let old_anchor = cf.anchor();
            cf.ranges = cf.ranges.iter().filter_map(|r| sh.range(*r)).collect();
            if cf.ranges.is_empty() {
                return None;
            }
            // Formulas are relative to the anchor: re-base them when it moved differently
            // from the cells (the old anchor was deleted).
            if sh.addr(old_anchor).is_none() {
                let new_anchor = cf.anchor();
                let (dr, dc) = (
                    i64::from(new_anchor.row) - i64::from(old_anchor.row),
                    i64::from(new_anchor.col) - i64::from(old_anchor.col),
                );
                let mut tmp = Sheet::new("");
                tmp.cond_formats.push(cf);
                rewrite_formulas(&mut tmp, &|src| {
                    let mut e = sp_formula::parse(src).ok()?;
                    shift_for_copy(&mut e, dr, dc);
                    Some(sp_formula::to_formula(&e))
                });
                return tmp.cond_formats.pop();
            }
            Some(cf)
        })
        .collect();
    let mut losses = Vec::new();
    let name = s.name.clone();
    let tables = std::mem::take(&mut s.tables);
    for t in &tables {
        if let Some(n) = shift_table(t, sh, &mut losses, &name) {
            // Header cells of inserted columns get their generated names.
            if n.header_row {
                for (i, c) in n.columns.iter().enumerate() {
                    let Some(col) = u32::try_from(i)
                        .ok()
                        .and_then(|i| n.range.start.col.checked_add(i))
                    else {
                        continue;
                    };
                    let a = CellAddr::new(n.range.start.row, col);
                    if s.value(a).is_empty() {
                        s.set(a, Cell::constant(Value::text(c.name.as_str())));
                    }
                }
            }
            s.tables.push(n);
        }
    }
    losses
}

/// Structured references to removed tables or columns become `#REF!`.
pub fn invalidate_refs(wb: &mut Workbook, losses: &[TableLoss]) {
    if losses.is_empty() {
        return;
    }
    let hit = |t: &sp_formula::structref::StructRef, home: &str| {
        losses.iter().any(|l| {
            let same = match &t.table {
                Some(n) => n.eq_ignore_ascii_case(&l.table),
                None => l.sheet == home,
            };
            same && match &l.columns {
                None => t.table.is_some(),
                Some(cols) => [&t.col0, &t.col1]
                    .into_iter()
                    .flatten()
                    .any(|c| cols.iter().any(|x| x.eq_ignore_ascii_case(c))),
            }
        })
    };
    let fix = |src: &str, home: &str| -> Option<String> {
        if !src.contains('[') {
            return None;
        }
        let mut e = sp_formula::parse(src).ok()?;
        let mut changed = false;
        e.walk_mut(&mut |x| {
            if let Expr::Table(t) = x
                && hit(t, home)
            {
                *x = Expr::Error(ErrorKind::Ref);
                changed = true;
            }
        });
        changed.then(|| sp_formula::to_formula(&e))
    };
    for s in &mut wb.sheets {
        let home = s.name.clone();
        for row in s.cells.values_mut() {
            let updates: Vec<(u32, String)> = row
                .iter()
                .filter_map(|(c, cell)| match &cell.content {
                    sp_model::Content::Formula(f) => fix(f, &home).map(|n| (*c, n)),
                    sp_model::Content::Value(_) => None,
                })
                .collect();
            if !updates.is_empty() {
                let row = std::sync::Arc::make_mut(row);
                for (c, n) in updates {
                    if let Some(cell) = row.get_mut(&c) {
                        cell.content = sp_model::Content::Formula(n);
                    }
                }
            }
        }
        rewrite_formulas(s, &|src| fix(src, &home));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::Tx;
    use sp_core::CellRange;
    use sp_formula::shift::Axis;
    use sp_model::{CfRule, CondFormat, DiffStyle};

    fn r(s: &str) -> CellRange {
        CellRange::parse(s).unwrap()
    }

    fn book() -> Workbook {
        let mut wb = Workbook::new();
        let s = &mut wb.sheets[0];
        for (i, h) in ["A", "B", "C"].iter().enumerate() {
            s.set(CellAddr::new(1, i as u32), Cell::constant(Value::text(*h)));
        }
        for row in 2..5u32 {
            for c in 0..3u32 {
                s.set(
                    CellAddr::new(row, c),
                    Cell::constant(Value::Number(f64::from(row + c))),
                );
            }
        }
        s.tables.push(Table::new(
            1,
            "T",
            r("A2:C5"),
            vec!["A".into(), "B".into(), "C".into()],
        ));
        s.cond_formats.push(CondFormat {
            ranges: vec![r("B3:B5")],
            rules: vec![CfRule {
                kind: CfKind::Expression("B3>$A$1".into()),
                style: DiffStyle::preset("red"),
                priority: 1,
                stop_if_true: false,
            }],
        });
        s.set(CellAddr::parse("E1").unwrap(), Cell::formula("SUM(T[B])"));
        s.set(
            CellAddr::parse("E2").unwrap(),
            Cell::formula("SUM(T[C])+ROWS(T[#All])"),
        );
        wb
    }

    fn run(wb: &mut Workbook, axis: Axis, at: u32, count: i64) {
        let mut tx = Tx::new(wb);
        crate::edit::insert_delete(&mut tx, 0, axis, at, count).unwrap();
        let _ = tx.commit("t");
    }

    fn f(wb: &Workbook, a: &str) -> String {
        wb.sheets[0]
            .get(CellAddr::parse(a).unwrap())
            .and_then(|c| c.formula_src())
            .unwrap_or("")
            .to_string()
    }

    #[test]
    fn insert_rows_moves_table_and_cf() {
        let mut wb = book();
        run(&mut wb, Axis::Rows, 0, 2);
        let s = &wb.sheets[0];
        assert_eq!(s.tables[0].range.to_a1(), "A4:C7");
        assert_eq!(s.cond_formats[0].ranges[0].to_a1(), "B5:B7");
        assert_eq!(
            s.cond_formats[0].rules[0].kind,
            CfKind::Expression("B5>$A$3".into())
        );
        // Inside the body: the table grows.
        run(&mut wb, Axis::Rows, 5, 1);
        assert_eq!(wb.sheets[0].tables[0].range.to_a1(), "A4:C8");
        assert_eq!(f(&wb, "E3"), "SUM(T[B])");
    }

    #[test]
    fn delete_rows_shrinks_or_drops() {
        let mut wb = book();
        run(&mut wb, Axis::Rows, 2, -2);
        assert_eq!(wb.sheets[0].tables[0].range.to_a1(), "A2:C3");
        // Anchor deleted: formula re-based onto the new top-left.
        assert_eq!(wb.sheets[0].cond_formats[0].ranges[0].to_a1(), "B3");
        // Deleting the header row drops the table; references become #REF!.
        run(&mut wb, Axis::Rows, 1, -1);
        assert!(wb.sheets[0].tables.is_empty());
        assert_eq!(f(&wb, "E1"), "SUM(#REF!)");
    }

    #[test]
    fn columns_insert_and_delete() {
        let mut wb = book();
        run(&mut wb, Axis::Cols, 1, 1);
        let t = &wb.sheets[0].tables[0];
        assert_eq!(t.range.to_a1(), "A2:D5");
        let names: Vec<&str> = t.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["A", "Column1", "B", "C"]);
        assert_eq!(
            wb.sheets[0].value(CellAddr::parse("B2").unwrap()),
            &Value::text("Column1")
        );
        assert_eq!(wb.sheets[0].cond_formats[0].ranges[0].to_a1(), "C3:C5");
        // Delete column C (was B): references to it break, others survive.
        run(&mut wb, Axis::Cols, 2, -1);
        let t = &wb.sheets[0].tables[0];
        assert_eq!(t.range.to_a1(), "A2:C5");
        assert!(wb.sheets[0].cond_formats.is_empty());
        assert_eq!(f(&wb, "E1"), "SUM(#REF!)");
        assert_eq!(f(&wb, "E2"), "SUM(T[C])+ROWS(T[#All])");
    }
}
