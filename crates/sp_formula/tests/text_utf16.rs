//! Text functions count UTF-16 code units, as the target does: a character outside the
//! Basic Multilingual Plane (an emoji) counts as 2. Half of a cut surrogate pair becomes
//! U+FFFD (one unit), so lengths of results still agree with the target's.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sp_core::{CellAddr, ErrorKind, Value};
use sp_formula::{AstCache, RecalcOptions, recalc};
use sp_model::{Cell, Workbook};

fn eval_all(cells: &[(&str, &str)]) -> Workbook {
    let mut wb = Workbook::new();
    for (a, f) in cells {
        let addr = CellAddr::parse(a).unwrap();
        wb.sheets[0].set(addr, Cell::formula(f.trim_start_matches('=')));
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

fn check(cases: &[(&str, Value)]) {
    let cells: Vec<(String, &str)> = cases
        .iter()
        .enumerate()
        .map(|(i, (f, _))| (format!("A{}", i + 1), *f))
        .collect();
    let refs: Vec<(&str, &str)> = cells.iter().map(|(a, f)| (a.as_str(), *f)).collect();
    let wb = eval_all(&refs);
    for (i, (f, want)) in cases.iter().enumerate() {
        let got = wb.sheets[0].value(CellAddr::new(i as u32, 0)).clone();
        assert_eq!(&got, want, "{f}");
    }
}

fn n(x: f64) -> Value {
    Value::Number(x)
}

fn t(s: &str) -> Value {
    Value::text(s)
}

#[test]
fn len_counts_utf16_units() {
    check(&[
        ("=LEN(\"héllo 😀\")", n(8.0)),
        ("=LEN(\"😀😀\")", n(4.0)),
        ("=LEN(\"\")", n(0.0)),
        ("=LENB(\"a😀\")", n(3.0)),
    ]);
}

#[test]
fn left_right_mid_use_utf16_positions() {
    check(&[
        ("=LEFT(\"😀ab\",2)", t("😀")),
        ("=LEFT(\"😀ab\",3)", t("😀a")),
        ("=RIGHT(\"ab😀\",2)", t("😀")),
        ("=RIGHT(\"ab😀\",3)", t("b😀")),
        ("=MID(\"a😀b\",2,2)", t("😀")),
        ("=MID(\"a😀b\",4,1)", t("b")),
        ("=MID(\"a😀b\",5,1)", t("")),
    ]);
}

#[test]
fn cut_surrogate_pair_becomes_one_replacement_char() {
    check(&[
        ("=LEFT(\"😀\",1)", t("\u{FFFD}")),
        ("=LEN(LEFT(\"😀\",1))", n(1.0)),
        ("=RIGHT(\"😀\",1)", t("\u{FFFD}")),
        ("=MID(\"a😀b\",3,2)", t("\u{FFFD}b")),
        ("=REPLACE(\"😀x\",2,1,\"-\")", t("\u{FFFD}-x")),
    ]);
}

#[test]
fn find_search_return_utf16_positions() {
    check(&[
        ("=FIND(\"b\",\"😀ab\")", n(4.0)),
        ("=SEARCH(\"B\",\"😀ab\")", n(4.0)),
        ("=SEARCH(\"?b\",\"😀ab\")", n(3.0)),
        ("=FIND(\"😀\",\"x😀\")", n(2.0)),
        ("=FIND(\"a\",\"😀a\",2)", n(3.0)),
        ("=FIND(\"a\",\"😀a\",4)", Value::Error(ErrorKind::Value)),
        ("=FIND(\"\",\"😀a\",4)", n(4.0)),
        ("=FIND(\"\",\"😀a\",5)", Value::Error(ErrorKind::Value)),
    ]);
}

#[test]
fn replace_and_substitute_with_emoji() {
    check(&[
        ("=REPLACE(\"a😀b\",2,2,\"X\")", t("aXb")),
        ("=REPLACE(\"a😀b\",4,1,\"X\")", t("a😀X")),
        ("=SUBSTITUTE(\"😀a😀\",\"😀\",\"b\",2)", t("😀ab")),
    ]);
}

#[test]
fn textbefore_after_instance_bounds_use_utf16_length() {
    // A one-character emoji text has length 2, so instance -2 of "" is still in range.
    check(&[
        ("=TEXTAFTER(\"a😀b\",\"😀\")", t("b")),
        ("=TEXTBEFORE(\"a😀b\",\"😀\")", t("a")),
        ("=TEXTBEFORE(\"😀\",\"\",-2)", t("")),
    ]);
}
