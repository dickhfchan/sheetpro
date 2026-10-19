//! Oracle-corpus behaviours fixed in wave 6, one test per root cause.

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
fn textjoin_keeps_blank_cells_when_not_ignoring_empty() {
    let wb = wb_with(&[
        ("A1", ","),
        ("C1", "Monday"),
        ("E1", "Wednesday"),
        ("G1", "=TEXTJOIN(A1,FALSE,C1:F1)"),
        ("H1", "=TEXTJOIN(A1,FALSE,C1,D1,E1,F1)"),
        ("I1", "=TEXTJOIN(A1,TRUE,C1:F1)"),
    ]);
    assert_eq!(text(&wb, "G1"), "Monday,,Wednesday,");
    assert_eq!(text(&wb, "H1"), "Monday,,Wednesday,");
    assert_eq!(text(&wb, "I1"), "Monday,Wednesday");
}

#[test]
fn search_tilde_escapes_any_char_and_trailing_tilde_matches_nothing() {
    let wb = wb_with(&[
        ("A1", "=SEARCH(\"~\",\"Hi ~Bang!\",1)"),
        ("A2", "=SEARCH(\"~\",\"Hello\")"),
        ("A3", "=SEARCH(\"~d\",\"My dear\")"),
        ("A4", "=SEARCH(\"~*\",\"a*b\")"),
        ("A5", "=SEARCH(\"b?d\",\"abcd\")"),
    ]);
    assert_eq!(num(&wb, "A1"), 1.0);
    assert_eq!(num(&wb, "A2"), 1.0);
    assert_eq!(num(&wb, "A3"), 4.0);
    assert_eq!(num(&wb, "A4"), 2.0);
    assert_eq!(num(&wb, "A5"), 2.0);
}

#[test]
fn argument_errors_win_over_bad_counts_and_shapes() {
    let wb = wb_with(&[
        ("A1", "=1/0"),
        ("B1", "=LEFT(A1,-1)"),
        ("B2", "=RIGHT(A1,-2)"),
        ("B3", "=LEFT(\"abc\",-1)"),
        ("B4", "=LOOKUP(A1,{1,2},{3,4})"),
        ("B5", "=HLOOKUP(\"x\",\"Nothing\",1)"),
        ("B6", "=VLOOKUP(\"x\",\"Nothing\",1)"),
        ("B7", "=INDEX({1,2,3},1,0,2)"),
        ("B8", "=SEQUENCE(-1)"),
        ("B9", "=SEQUENCE(0)"),
        ("B10", "=PV(-1,20,300)"),
        ("B11", "=HLOOKUP(\"x\",3,1)"),
        ("B12", "=VLOOKUP(\"x\",TRUE,1)"),
    ]);
    assert_eq!(err(&wb, "B1"), ErrorKind::Div0);
    assert_eq!(err(&wb, "B2"), ErrorKind::Div0);
    assert_eq!(err(&wb, "B3"), ErrorKind::Value);
    assert_eq!(err(&wb, "B4"), ErrorKind::Div0);
    assert_eq!(err(&wb, "B5"), ErrorKind::Value);
    assert_eq!(err(&wb, "B6"), ErrorKind::Value);
    assert_eq!(err(&wb, "B7"), ErrorKind::Ref);
    assert_eq!(err(&wb, "B8"), ErrorKind::Value);
    assert_eq!(err(&wb, "B9"), ErrorKind::Calc);
    assert_eq!(err(&wb, "B10"), ErrorKind::Div0);
    assert_eq!(err(&wb, "B11"), ErrorKind::NA);
    assert_eq!(err(&wb, "B12"), ErrorKind::NA);
}

#[test]
fn textbefore_textafter_instance_match_end_and_fallback() {
    let wb = wb_with(&[
        ("A1", "123 the 456 the 891"),
        ("B1", "=TEXTAFTER(A1,\"the\",-1.2)"),
        ("B2", "=TEXTBEFORE(A1,\"the\",-1.6)"),
        ("B3", "=TEXTAFTER(A1,\"Hello\",1,1,1,\"F\")"),
        ("B4", "=TEXTBEFORE(A1,\"Hello\",1,1,1,\"F\")"),
        ("B5", "=TEXTAFTER(A1,\"Hello\",-1,1,1,\"F\")"),
        ("B6", "=TEXTAFTER(A1,\"Hello\",2,1,1,\"F\")"),
        ("B7", "=TEXTAFTER(A1,\"Hello\",-1,1,2,\"F\")"),
        ("B8", "=TEXTAFTER(\"del\",\"deli\",1)"),
        ("B9", "=TEXTAFTER(\"del\",\"deli\")"),
        ("B10", "=TEXTAFTER(\"TRUE\",\"r\",1,0,0,Z99)"),
        ("B11", "=TEXTAFTER(\"you@example.com\",\"- \",-1,,1)"),
        ("B12", "=TEXTBEFORE(\"you@example.com\",\"- \",-1,,1)"),
        ("B13", "=TEXTAFTER(A1,\"the\",0)"),
    ]);
    assert_eq!(text(&wb, "B1"), " 456 the 891");
    assert_eq!(text(&wb, "B2"), "123 ");
    assert_eq!(text(&wb, "B3"), "");
    assert_eq!(text(&wb, "B4"), "123 the 456 the 891");
    assert_eq!(text(&wb, "B5"), "123 the 456 the 891");
    assert_eq!(text(&wb, "B6"), "F");
    assert_eq!(err(&wb, "B7"), ErrorKind::Value);
    assert_eq!(err(&wb, "B8"), ErrorKind::Value);
    assert_eq!(err(&wb, "B9"), ErrorKind::NA);
    assert_eq!(text(&wb, "B10"), "");
    assert_eq!(text(&wb, "B11"), "you@example.com");
    assert_eq!(text(&wb, "B12"), "");
    assert_eq!(err(&wb, "B13"), ErrorKind::Value);
}

#[test]
fn indirect_r1c1_absolute_relative_ranges_and_wrap() {
    let wb = wb_with(&[
        ("A1", "1"),
        ("B1", "2"),
        ("C1", "3"),
        ("A2", "4"),
        ("B2", "5"),
        ("C2", "6"),
        ("E1", "=INDIRECT(\"R2C2\",FALSE)"),
        ("E2", "=INDIRECT(\"r1c3\",0)"),
        ("E3", "=SUM(INDIRECT(\"R2C3:R1C2\",FALSE))"),
        ("E4", "=INDIRECT(\"R2C2\",)"),
        ("E5", "=INDIRECT(\"B2\",FALSE)"),
        ("D2", "=INDIRECT(\"RC[-1]\",FALSE)"),
        ("D3", "=INDIRECT(\"R[-1]C[-2]\",FALSE)"),
        ("F6", "=INDIRECT(\"R[-6]C\",FALSE)"),
        ("E6", "=SUM(INDIRECT(\"R2\",FALSE))"),
        ("E7", "=SUM(INDIRECT(\"C1:C2\",FALSE))"),
        ("E8", "=SUM(INDIRECT(\"C[-3]\",FALSE))"),
        ("E9", "=INDIRECT(\"R0C1\",FALSE)"),
    ]);
    assert_eq!(num(&wb, "E1"), 5.0);
    assert_eq!(num(&wb, "E2"), 3.0);
    assert_eq!(num(&wb, "E3"), 16.0);
    assert_eq!(num(&wb, "E4"), 5.0);
    assert_eq!(err(&wb, "E5"), ErrorKind::Ref);
    assert_eq!(num(&wb, "D2"), 6.0);
    assert_eq!(num(&wb, "D3"), 5.0);
    // R[-6] from row 6 wraps to the last row of the sheet (blank).
    assert_eq!(num(&wb, "F6"), 0.0);
    // Row 2: A2:C2 plus D2 (6) and E2 (3).
    assert_eq!(num(&wb, "E6"), 24.0);
    assert_eq!(num(&wb, "E7"), 12.0);
    assert_eq!(num(&wb, "E8"), 7.0);
    assert_eq!(err(&wb, "E9"), ErrorKind::Ref);
}
