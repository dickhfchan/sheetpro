//! Whole-sheet operations on sheets large enough to take the parallel formula paths
//! (sort, insert rows): every formula must come out exactly as the serial rule says.

use serde_json::json;
use sp_core::{CellAddr, Value};
use sp_engine::Session;
use sp_model::{Cell, Workbook};

const N: u32 = 5_000;

/// A: r, B: `=A{n}*2`, C: `=B{n}+$A$1`, D: running total `=D{n-1}+B{n}`.
fn session() -> Session {
    let mut wb = Workbook::new();
    let s = &mut wb.sheets[0];
    for r in 0..N {
        let n = r + 1;
        s.set(
            CellAddr::new(r, 0),
            Cell::constant(Value::Number(f64::from(r))),
        );
        s.set(CellAddr::new(r, 1), Cell::formula(format!("A{n}*2")));
        s.set(CellAddr::new(r, 2), Cell::formula(format!("B{n}+$A$1")));
        let d = if r == 0 {
            "B1".to_string()
        } else {
            format!("D{r}+B{n}")
        };
        s.set(CellAddr::new(r, 3), Cell::formula(d));
    }
    Session::with_workbook(wb)
}

fn formula(s: &Session, row: u32, col: u32) -> String {
    s.wb.sheets[0]
        .get(CellAddr::new(row, col))
        .and_then(|c| c.formula_src())
        .unwrap_or_default()
        .to_string()
}

fn matches_full_recalc(s: &Session) {
    let mut wb = s.wb.clone();
    sp_formula::recalc(&mut wb, &mut sp_formula::AstCache::default(), s.recalc_opts);
    for (a, c) in wb.sheets[0].iter() {
        assert_eq!(s.wb.sheets[0].value(a), &c.value, "{}", a.to_a1());
    }
}

#[test]
fn big_sort_shifts_every_formula() {
    let mut s = session();
    s.execute(
        "data.sort_za",
        &json!({"range": format!("A1:D{N}"), "header": false}),
    )
    .unwrap();
    for r in 0..N {
        let n = r + 1;
        // Row r now holds the row that had A = N-1-r; relative refs follow the row.
        assert_eq!(
            s.wb.sheets[0].value(CellAddr::new(r, 0)),
            &Value::Number(f64::from(N - 1 - r))
        );
        assert_eq!(formula(&s, r, 1), format!("A{n}*2"));
        assert_eq!(formula(&s, r, 2), format!("B{n}+$A$1"));
    }
    matches_full_recalc(&s);
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(formula(&s, 9, 3), "D9+B10");
    matches_full_recalc(&s);
}

#[test]
fn big_insert_rows_rewrites_every_formula() {
    let mut s = session();
    s.execute("home.cells.insert_rows", &json!({"range": "A3:A4"}))
        .unwrap();
    // Rows 1-2 stay; everything from row 3 moved down by 2.
    assert_eq!(formula(&s, 1, 1), "A2*2");
    assert_eq!(formula(&s, 1, 2), "B2+$A$1");
    assert!(s.wb.sheets[0].get(CellAddr::new(2, 1)).is_none());
    for r in 4..N + 2 {
        let n = r + 1;
        assert_eq!(formula(&s, r, 1), format!("A{n}*2"));
        assert_eq!(formula(&s, r, 2), format!("B{n}+$A$1"));
    }
    // The running total across the gap: D5 = D2 + B5 (D3, D4 were shifted references).
    assert_eq!(formula(&s, 4, 3), "D2+B5");
    matches_full_recalc(&s);
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(formula(&s, 2, 3), "D2+B3");
    matches_full_recalc(&s);
}
