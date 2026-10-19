//! Which cells a committed transaction changed, so recalculation can stay incremental.
//!
//! Cell-level transactions list their cells. Whole-sheet transactions (AutoFilter, hide
//! rows, sort, structural edits) record before/after sheet versions; those are diffed cell
//! by cell (rows still shared between the versions are skipped, see
//! `sp_model::Sheet::changed_cells`). Row/column visibility changes also dirty every formula
//! that may skip hidden rows (SUBTOTAL, AGGREGATE). Anything else a formula can observe
//! (sheet name, tables, spill areas, pivot/what-if records) forces a full recalc, as does a
//! diff larger than [`MAX_INCREMENTAL_CELLS`].

use sp_core::CellAddr;
use sp_model::{Content, Sheet, Workbook};
use sp_ops::history::Txn;

/// Beyond this many changed cells a full recalc is cheaper than propagating changes.
pub const MAX_INCREMENTAL_CELLS: usize = 200_000;

/// Changed cells of `t` (applied in either direction), or `None` when the whole workbook
/// must be recalculated.
pub fn changed_keys(wb: &Workbook, t: &Txn) -> Option<Vec<(usize, CellAddr)>> {
    if let Some(cells) = t.changed_cells() {
        return Some(cells);
    }
    let mut keys = t.cell_keys();
    let mut visibility = false;
    for (index, before, after) in t.sheet_versions()? {
        if !same_formula_inputs(before, after) {
            return None;
        }
        visibility |= before.hidden_rows != after.hidden_rows
            || before.hidden_cols != after.hidden_cols
            || before.auto_filter != after.auto_filter
            || before.outline != after.outline;
        let room = MAX_INCREMENTAL_CELLS.checked_sub(keys.len())?;
        let diff = after.changed_cells(before, room)?;
        keys.extend(diff.into_iter().map(|a| (index, a)));
    }
    if visibility {
        for (si, sh) in wb.sheets.iter().enumerate() {
            for (a, c) in sh.iter() {
                if let Content::Formula(src) = &c.content
                    && reads_visibility(src)
                {
                    keys.push((si, a));
                    if keys.len() > MAX_INCREMENTAL_CELLS {
                        return None;
                    }
                }
            }
        }
    }
    keys.sort_unstable();
    keys.dedup();
    Some(keys)
}

/// Formula text that may call a function skipping hidden or filtered rows (a superset:
/// any mention of the names, in any case, counts).
fn reads_visibility(src: &str) -> bool {
    let has = |name: &str| {
        src.as_bytes()
            .windows(name.len())
            .any(|w| w.eq_ignore_ascii_case(name.as_bytes()))
    };
    has("SUBTOTAL") || has("AGGREGATE")
}

/// True if every sheet property a formula can observe, other than cells and row/column
/// visibility, is unchanged. Destructured so a new `Sheet` field must be classified here.
fn same_formula_inputs(a: &Sheet, b: &Sheet) -> bool {
    let Sheet {
        name,
        cells: _,
        col_widths: _,
        row_heights: _,
        hidden_rows: _,
        hidden_cols: _,
        merges: _,
        freeze: _,
        tab_color: _,
        visibility: _,
        show_gridlines: _,
        default_col_width: _,
        default_row_height: _,
        auto_filter: _,
        outline: _,
        comments: _,
        hyperlinks: _,
        protection: _,
        allow_edit_ranges: _,
        validations: _,
        page_setup: _,
        spills,
        drawings: _,
        cond_formats: _,
        tables,
        passthrough: _,
        analysis,
        objects: _,
        background: _,
    } = a;
    *name == b.name && *spills == b.spills && *tables == b.tables && *analysis == b.analysis
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Session;
    use serde_json::json;
    use sp_core::Value;

    /// Every cell value of `s` equals a full recalculation of a copy.
    fn assert_matches_full(s: &Session, step: &str) {
        let mut wb = s.wb.clone();
        let mut cache = sp_formula::AstCache::default();
        sp_formula::recalc(&mut wb, &mut cache, s.recalc_opts);
        for (si, sh) in wb.sheets.iter().enumerate() {
            for (a, c) in sh.iter() {
                let got = s.wb.sheets[si].value(a);
                assert_eq!(got, &c.value, "{step}: {}!{}", sh.name, a.to_a1());
            }
        }
    }

    fn session() -> Session {
        let mut s = Session::new();
        let mut run = |cell: &str, input: String| {
            s.execute("cell.set", &json!({"cell": cell, "input": input}))
                .unwrap();
        };
        run("A1", "Region".into());
        run("B1", "Amount".into());
        for r in 2..=40 {
            run(&format!("A{r}"), format!("R{}", r % 3));
            run(&format!("B{r}"), format!("{}", r * 10));
            run(&format!("C{r}"), format!("=B{r}*2"));
        }
        run("E1", "=SUBTOTAL(9,B2:B40)".into());
        run("E2", "=aggregate(9,5,B2:B40)".into());
        run("E3", "=SUM(C2:C40)".into());
        run("E4", "=E1+E2".into());
        s
    }

    #[test]
    fn whole_sheet_steps_recalc_incrementally_and_match_full() {
        let mut s = session();
        let total = s.wb.sheets[0].value(CellAddr::new(0, 4)).clone();
        let steps: Vec<(&str, serde_json::Value, bool)> = vec![
            (
                "data.filter",
                json!({"range": "A1:C40", "column": "A", "values": ["R1"]}),
                true,
            ),
            ("edit.undo", json!({}), true),
            ("edit.redo", json!({}), true),
            ("home.cells.hide_rows", json!({"range": "A5:A9"}), true),
            ("data.filter", json!({"on": false}), true),
            ("home.cells.unhide_rows", json!({"range": "A1:A40"}), true),
            ("cell.set", json!({"cell": "B7", "input": "1000"}), true),
            (
                "data.sort_za",
                json!({"range": "A1:C40", "header": true}),
                true,
            ),
            ("edit.undo", json!({}), true),
            ("home.cells.insert_rows", json!({"range": "A3"}), false),
            ("edit.undo", json!({}), false),
        ];
        for (id, p, incremental) in steps {
            s.execute(id, &p).unwrap();
            assert_matches_full(&s, id);
            if incremental {
                assert!(s.last_recalc.incremental, "{id}: {:?}", s.last_recalc);
            }
        }
        // Filtering changed the SUBTOTAL: it really was recomputed.
        let mut s = session();
        s.execute(
            "data.filter",
            &json!({"range": "A1:C40", "column": "A", "values": ["R1"]}),
        )
        .unwrap();
        assert_ne!(s.wb.sheets[0].value(CellAddr::new(0, 4)), &total);
        assert!(matches!(
            s.wb.sheets[0].value(CellAddr::new(2, 4)),
            Value::Number(_)
        ));
    }

    #[test]
    fn visibility_names_any_case() {
        assert!(reads_visibility("SUBTOTAL(9,A:A)"));
        assert!(reads_visibility("1+aggregate(4,5,B1:B9)"));
        assert!(!reads_visibility("SUM(A1:A9)"));
    }
}
