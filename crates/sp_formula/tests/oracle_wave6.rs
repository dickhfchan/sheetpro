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

#[test]
fn bessel_j_y_reproduce_target_approximations() {
    let wb = wb_with(&[
        ("A1", "=BESSELJ(0,0)"),
        ("A2", "=BESSELJ(10,1)"),
        ("A3", "=BESSELJ(10,7)"),
        ("A4", "=BESSELJ(-10,1)"),
        ("A5", "=BESSELJ(1.9,2)"),
        ("A6", "=BESSELY(10,1)"),
    ]);
    // Values the target computes (its approximations are accurate to ~1e-8, not exact).
    assert!(close(num(&wb, "A1"), 1.000_000_002_831_41));
    assert!(close(num(&wb, "A2"), 4.347_224_965_225_415_1e-2));
    assert!(close(num(&wb, "A3"), 0.216_710_438_003_148_9));
    assert!(close(num(&wb, "A4"), -4.347_224_965_225_415_1e-2));
    assert!(close(num(&wb, "A5"), 0.329_925_728));
    assert!(close(num(&wb, "A6"), 0.249_015_424_1));
}

#[test]
fn xnpv_xirr_argument_rules() {
    let wb = wb_with(&[
        ("A1", "-20"),
        ("B1", "5"),
        ("D1", "5"),
        ("A2", "-20"),
        ("B2", "5"),
        ("C2", "hello"),
        ("D2", "5"),
        ("A3", "44958"),
        ("B3", "44986"),
        ("C3", "45017"),
        ("D3", "45078"),
        ("A4", "-20"),
        ("B4", "5"),
        ("C4", "0"),
        ("D4", "5"),
        ("F1", "=XNPV(0.1,A1:D1,A3:D3)"),
        ("F2", "=XNPV(0.1,A2:D2,A3:D3)"),
        ("F3", "=XNPV(-0.3,A4:D4,A3:D3)"),
        ("F4", "=XIRR(A2:D2,A3:D3)"),
        ("F5", "=XIRR(A1:D1,A3:D3)"),
        ("F6", "=XIRR(A4:D4,A3:D3)"),
    ]);
    assert_eq!(err(&wb, "F1"), ErrorKind::Num);
    assert_eq!(err(&wb, "F2"), ErrorKind::Num);
    assert_eq!(err(&wb, "F3"), ErrorKind::Num);
    assert_eq!(err(&wb, "F4"), ErrorKind::Value);
    // A blank cash flow counts as zero in XIRR.
    assert!(close(num(&wb, "F5"), num(&wb, "F6")));
}

#[test]
fn db_truncates_month_and_ddb_takes_fractional_periods() {
    let wb = wb_with(&[
        ("A1", "=DB(100,10,4,3,9.2222)"),
        ("A2", "=DB(45,10,4,2,12.7)"),
        ("A3", "=DDB(100,10.233,4.2312,3.2,1.5)"),
        ("A4", "=DDB(100,10,4,3,2.7)"),
        ("A5", "=DDB(100,10,4,3,40.777)"),
        ("A6", "=DDB(1,0,1E300,1E300)"),
    ]);
    assert!(close(num(&wb, "A1"), 16.529_375_4));
    assert!(close(num(&wb, "A2"), 9.676_395));
    assert!(close(num(&wb, "A3"), 13.532_735_530_775_124));
    assert!(close(num(&wb, "A4"), 0.5625));
    assert_eq!(num(&wb, "A5"), 0.0);
    // Huge lives no longer loop once per period.
    assert!(num(&wb, "A6").is_finite());
}

#[test]
fn tbilleq_counts_a_366_day_term_as_one_year() {
    let wb = wb_with(&[
        ("A1", "14640"),
        ("B1", "=A1+366"),
        ("C1", "=TBILLEQ(A1,B1,0.5)"),
        ("B2", "=A1+365"),
        ("C2", "=TBILLEQ(A1,B2,0.5)"),
    ]);
    assert!(close(num(&wb, "C1"), 0.852_296_131_249_170_1));
    assert!(close(num(&wb, "C2"), 0.848_275_979_665_248_2));
}
