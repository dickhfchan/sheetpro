//! Oracle-corpus behaviours fixed in wave 5, one test per root cause.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sp_core::{CellAddr, ErrorKind, Value};
use sp_formula::{AstCache, RecalcOptions, recalc};
use sp_model::{Cell, Workbook};

fn wb_with(cells: &[(&str, &str)]) -> Workbook {
    let mut wb = Workbook::new();
    for (a, input) in cells {
        let addr = CellAddr::parse(a).unwrap();
        let cell = match input.strip_prefix('=') {
            Some(f) => Cell::formula(f),
            None => match input.parse::<f64>() {
                Ok(n) => Cell::constant(Value::Number(n)),
                Err(_) => Cell::constant(Value::text(*input)),
            },
        };
        wb.sheets[0].set(addr, cell);
    }
    let mut cache = AstCache::default();
    recalc(
        &mut wb,
        &mut cache,
        RecalcOptions {
            now: Some(45306.5),
            seed: 7,
        },
    );
    wb
}

fn v(wb: &Workbook, a: &str) -> Value {
    wb.sheets[0].value(CellAddr::parse(a).unwrap()).clone()
}

fn num(wb: &Workbook, a: &str) -> f64 {
    match v(wb, a) {
        Value::Number(n) => n,
        other => panic!("{a}: {other:?}"),
    }
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-8 * b.abs().max(1.0)
}

fn err(wb: &Workbook, a: &str) -> ErrorKind {
    match v(wb, a) {
        Value::Error(e) => e,
        other => panic!("{a}: expected error, got {other:?}"),
    }
}

fn text(wb: &Workbook, a: &str) -> String {
    match v(wb, a) {
        Value::Text(s) => s.to_string(),
        other => panic!("{a}: expected text, got {other:?}"),
    }
}

#[test]
fn text_coercion_rejects_double_sign_and_padded_dates() {
    let wb = wb_with(&[
        ("A1", "=\"--8\""),
        ("A2", "=\" 2024-01-10 \""),
        ("A3", "=\" 10 \""),
        ("A4", "=\"2024-01-10\""),
        ("B1", "=SIN(A1)"),
        ("B2", "=FLOOR(A2,7)"),
        ("B3", "=EXP(A3)"),
        ("B4", "=GCD(A4,7)"),
        ("B5", "=A2+0"),
    ]);
    assert_eq!(err(&wb, "B1"), ErrorKind::Value);
    assert_eq!(err(&wb, "B2"), ErrorKind::Value);
    assert!(close(num(&wb, "B3"), 10f64.exp()));
    assert_eq!(num(&wb, "B4"), 1.0);
    assert_eq!(err(&wb, "B5"), ErrorKind::Value);
}

#[test]
fn roman_forms_and_form_argument() {
    let wb = wb_with(&[
        ("A1", "499"),
        ("B1", "=ROMAN(A1,1)"),
        ("B2", "=ROMAN(A1,4)"),
        ("B3", "=ROMAN(A1,TRUE)"),
        ("B4", "=ROMAN(A1,FALSE)"),
        ("B5", "=ROMAN(A1,5)"),
        ("B6", "=ROMAN(A1,\"2\")"),
        ("B7", "=ROMAN(A1,\"hello\")"),
    ]);
    assert_eq!(text(&wb, "B1"), "LDVLIV");
    assert_eq!(text(&wb, "B2"), "ID");
    assert_eq!(text(&wb, "B3"), "CDXCIX");
    assert_eq!(text(&wb, "B4"), "ID");
    assert_eq!(err(&wb, "B5"), ErrorKind::Value);
    assert_eq!(text(&wb, "B6"), "XDIX");
    assert_eq!(err(&wb, "B7"), ErrorKind::Value);
}

#[test]
fn floor_ceiling_keep_tiny_quotients_and_tiny_significance() {
    let wb = wb_with(&[
        ("B1", "=FLOOR(-1E-12,1)"),
        ("B2", "=CEILING(1E-12,1)"),
        ("B3", "=CEILING(10,1E-307)"),
        ("B4", "=CEILING(9999999999999,1E-307)"),
        ("B5", "=FLOOR(7.1,0.1)"),
        ("B6", "=FLOOR.MATH(-1E-12)"),
        ("B7", "=CEILING.PRECISE(1E-12)"),
    ]);
    assert_eq!(num(&wb, "B1"), -1.0);
    assert_eq!(num(&wb, "B2"), 1.0);
    assert!(close(num(&wb, "B3"), 10.0));
    assert_eq!(err(&wb, "B4"), ErrorKind::Num);
    assert!(close(num(&wb, "B5"), 7.1));
    assert_eq!(num(&wb, "B6"), -1.0);
    assert_eq!(num(&wb, "B7"), 1.0);
}

#[test]
fn xlookup_binary_search_modes_and_validation() {
    let mut cells: Vec<(String, String)> = Vec::new();
    // Ascending with duplicates in A, descending in C, labels in B/D.
    let asc = [1, 2, 3, 4, 5, 5, 5, 6, 7, 7, 7, 8, 9];
    for (i, v) in asc.iter().enumerate() {
        cells.push((format!("A{}", i + 1), v.to_string()));
        cells.push((format!("B{}", i + 1), format!("r{}", i + 1)));
        cells.push((format!("C{}", i + 1), asc[asc.len() - 1 - i].to_string()));
    }
    cells.push(("E1".into(), "=XLOOKUP(5,A1:A13,B1:B13,,0,2)".into()));
    cells.push(("E2".into(), "=XLOOKUP(7.1,A1:A13,B1:B13,,-1,2)".into()));
    cells.push(("E3".into(), "=XLOOKUP(6.8,A1:A13,B1:B13,,1,2)".into()));
    cells.push(("E4".into(), "=XLOOKUP(5,C1:C13,B1:B13,,0,-2)".into()));
    cells.push(("E5".into(), "=XLOOKUP(7.1,C1:C13,B1:B13,,-1,-2)".into()));
    cells.push(("E6".into(), "=XLOOKUP(5,A1:A13,B1:B13,,12)".into()));
    cells.push(("E7".into(), "=XLOOKUP(\"a*\",B1:B13,A1:A13,,2,2)".into()));
    cells.push(("E8".into(), "=XLOOKUP(TRUE,A1:A13,B1:B13,,-1)".into()));
    cells.push(("E9".into(), "=XLOOKUP(1/0,A1:A13,B1:B13,\"nf\")".into()));
    cells.push(("E10".into(), "=XMATCH(\"r1[0-9]\",B1:B13,3)".into()));
    let refs: Vec<(&str, &str)> = cells.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let wb = wb_with(&refs);
    assert_eq!(text(&wb, "E1"), "r5"); // first of the duplicate run
    assert_eq!(text(&wb, "E2"), "r11"); // last 7
    assert_eq!(text(&wb, "E3"), "r9"); // first 7
    assert_eq!(text(&wb, "E4"), "r9"); // descending: last 5
    assert_eq!(text(&wb, "E5"), "r3"); // descending: first 7
    assert_eq!(err(&wb, "E6"), ErrorKind::Value);
    assert_eq!(err(&wb, "E7"), ErrorKind::Value);
    assert_eq!(text(&wb, "E8"), "r13"); // logicals sort after numbers
    assert_eq!(err(&wb, "E9"), ErrorKind::Div0);
    assert_eq!(num(&wb, "E10"), 10.0);
}

#[test]
fn xlookup_blank_lookup_value_finds_blank_cell() {
    let wb = wb_with(&[
        ("A1", "a"),
        ("A2", "b"),
        ("B1", "1"),
        ("B2", "2"),
        ("B3", "3"),
        ("C1", "=XLOOKUP(,A1:A3,B1:B3,,1,2)"),
    ]);
    assert_eq!(num(&wb, "C1"), 3.0);
}

#[test]
fn text_general_width_and_date_validation() {
    let wb = wb_with(&[
        ("A1", "=1/3"),
        ("A2", "123456789012"),
        ("A3", "-0.0000000025"),
        ("A4", "-0.00000001"),
        ("A5", "2958466"),
        ("B1", "=TEXT(A1,\"General\")"),
        ("B2", "=TEXT(A2,\"General\")"),
        ("B3", "=TEXT(A3,\"General\")"),
        ("B4", "=TEXT(A4,\"General\")"),
        ("B5", "=TEXT(A5,\"m/d/y\")"),
        ("B6", "=TEXT(41181,\"mm###\")"),
        ("B7", "=TEXT(-1,\"m/d/y\")"),
        ("B8", "=TEXT(12,\"00-00-00-00\")"),
        ("B9", "=TEXT(12,\"#.#\")"),
    ]);
    assert_eq!(text(&wb, "B1"), "0.333333333");
    assert_eq!(text(&wb, "B2"), "1.23457E+11");
    assert_eq!(text(&wb, "B3"), "-2.5E-09");
    assert_eq!(text(&wb, "B4"), "-0.00000001");
    assert_eq!(err(&wb, "B5"), ErrorKind::Value);
    assert_eq!(err(&wb, "B6"), ErrorKind::Value);
    assert_eq!(err(&wb, "B7"), ErrorKind::Value);
    assert_eq!(text(&wb, "B8"), "00-00-00-12");
    assert_eq!(text(&wb, "B9"), "12.");
}
