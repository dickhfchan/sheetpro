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
