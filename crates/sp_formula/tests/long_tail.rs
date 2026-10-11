//! End-to-end checks for the criteria engine and the function long tail (known values).

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

#[test]
fn criteria_engine() {
    let wb = wb_with(&[
        ("A1", "23"),
        ("A2", "=\"23\""),
        ("A3", "apple"),
        ("A5", "0"),
        ("B1", "=COUNTIF(A:A,\"<>23\")"),
        ("B2", "=COUNTIF(A1:A5,\"=23\")"),
        ("B3", "=COUNTIF(A1:A5,C9)"),
        ("B4", "=COUNTIF(A:A,\"\")"),
        ("B5", "=SUM(COUNTIF(A1:A5,{\"apple\",23}))"),
        ("B6", "=SUMIF(A1:A5,\">0\")"),
        ("B7", "=AVERAGEIFS(A1:A5,A1:A5,\">=0\")"),
    ]);
    // Whole-column "<>" counts every unstored blank as well.
    assert_eq!(num(&wb, "B1"), 1_048_576.0 - 1.0);
    assert_eq!(num(&wb, "B2"), 2.0); // numeric text matches "="
    assert_eq!(num(&wb, "B3"), 1.0); // blank criterion = 0
    assert_eq!(num(&wb, "B4"), 1_048_576.0 - 4.0);
    assert_eq!(num(&wb, "B5"), 3.0);
    assert_eq!(num(&wb, "B6"), 23.0);
    assert_eq!(num(&wb, "B7"), 11.5);
}

#[test]
fn database_functions() {
    let wb = wb_with(&[
        ("A1", "Tree"),
        ("B1", "Height"),
        ("C1", "Yield"),
        ("A2", "Apple"),
        ("B2", "18"),
        ("C2", "14"),
        ("A3", "Pear"),
        ("B3", "12"),
        ("C3", "10"),
        ("A4", "Apple"),
        ("B4", "8"),
        ("C4", "6"),
        ("E1", "Tree"),
        ("F1", "Height"),
        ("E2", "Apple"),
        ("F2", ">10"),
        ("G1", "=DSUM(A1:C4,\"Yield\",E1:F2)"),
        ("G2", "=DCOUNT(A1:C4,3,E1:E2)"),
        ("G3", "=DGET(A1:C4,\"Yield\",E1:E2)"),
        ("G4", "=DAVERAGE(A1:C4,\"Nope\",E1:E2)"),
        ("G5", "=DMAX(A1:C4,2,E1:E2)"),
    ]);
    assert_eq!(num(&wb, "G1"), 14.0);
    assert_eq!(num(&wb, "G2"), 2.0);
    assert_eq!(v(&wb, "G3"), Value::Error(ErrorKind::Num));
    assert_eq!(v(&wb, "G4"), Value::Error(ErrorKind::Value));
    assert_eq!(num(&wb, "G5"), 18.0);
}

#[test]
fn financial_known_values() {
    let wb = wb_with(&[
        (
            "A1",
            "=PRICE(DATE(2008,2,15),DATE(2017,11,15),0.0575,0.065,100,2,0)",
        ),
        (
            "A2",
            "=YIELD(DATE(2008,2,15),DATE(2016,11,15),0.0575,95.04287,100,2,0)",
        ),
        ("A3", "=ACCRINT(39508,39691,39569,0.1,1000,2,0)"),
        ("A4", "=ACCRINTM(39539,39614,0.1,1000,3)"),
        ("A5", "=COUPNUM(DATE(2011,1,25),DATE(2011,11,15),2,1)"),
        ("A6", "=DISC(DATE(2018,1,25),DATE(2018,6,15),97.975,100,1)"),
        ("A7", "=TBILLPRICE(DATE(2008,3,31),DATE(2008,6,1),0.09)"),
        ("A8", "=CUMIPMT(0.09/12,30*12,125000,13,24,0)"),
        (
            "A9",
            "=XNPV(0.09,{-10000,2750,4250,3250,2750},{39448,39508,39751,39859,39904})",
        ),
        (
            "A10",
            "=XIRR({-10000,2750,4250,3250,2750},{39448,39508,39751,39859,39904})",
        ),
        ("A11", "=VDB(2400,300,10,0,1)"),
        ("A12", "=DOLLARDE(1.02,16)"),
        (
            "A13",
            "=MDURATION(DATE(2008,1,1),DATE(2016,1,1),0.08,0.09,2,1)",
        ),
        ("A14", "=AMORLINC(2400,39679,39813,300,1,0.15,1)"),
    ]);
    assert!(close(num(&wb, "A1"), 94.634_361_31));
    assert!((num(&wb, "A2") - 0.065).abs() < 1e-6);
    assert!(close(num(&wb, "A3"), 16.666_666_67));
    assert!(close(num(&wb, "A4"), 20.547_945_21));
    assert_eq!(num(&wb, "A5"), 2.0);
    assert!(close(num(&wb, "A6"), 0.052_420_213));
    assert!(close(num(&wb, "A7"), 98.45));
    assert!(close(num(&wb, "A8"), -11_135.232_130_46));
    assert!(close(num(&wb, "A9"), 2_086.647_602));
    assert!((num(&wb, "A10") - 0.373_362_535).abs() < 1e-7);
    assert!(close(num(&wb, "A11"), 480.0));
    assert!(close(num(&wb, "A12"), 1.125));
    assert!((num(&wb, "A13") - 5.735_669_1).abs() < 1e-6);
    assert!(close(num(&wb, "A14"), 360.0));
}

#[test]
fn text_math_info() {
    let wb = wb_with(&[
        ("A1", "JamesBond"),
        (
            "B1",
            "=REGEXREPLACE(A1,\"([A-Z][a-z]+)([A-Z][a-z]+)\",\"$2, $1 $2\")",
        ),
        ("B2", "=REGEXTEST(\"abc123\",\"\\d{3}$\")"),
        ("B3", "=REGEXEXTRACT(\"Lonely Planet\",\"[A-Z][a-z]+$\")"),
        ("B4", "=TEXTSPLIT(\"a,b,c\",\",\")"),
        ("B5", "=MDETERM({1,3,8,5;1,3,6,1;1,1,1,0;7,3,10,2})"),
        ("B6", "=SERIESSUM(2,0,1,{1,2,3})"),
        ("B7", "=AGGREGATE(9,6,C1:C3)"),
        ("C1", "1"),
        ("C2", "=1/0"),
        ("C3", "2"),
        ("B8", "=CELL(\"address\",C3)"),
        ("B9", "=LENB(\"abc\")"),
        (
            "B10",
            "=REGEXTEST(\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa!\",\"(a+)+b\")",
        ),
    ]);
    assert_eq!(v(&wb, "B1"), Value::text("Bond, James Bond"));
    assert_eq!(v(&wb, "B2"), Value::Bool(true));
    assert_eq!(v(&wb, "B3"), Value::text("Planet"));
    assert_eq!(v(&wb, "B4"), Value::text("a"));
    assert!(close(num(&wb, "B5"), 88.0));
    assert_eq!(num(&wb, "B6"), 17.0);
    assert_eq!(num(&wb, "B7"), 3.0);
    assert_eq!(v(&wb, "B8"), Value::text("$C$3"));
    assert_eq!(num(&wb, "B9"), 3.0);
    // Catastrophic backtracking is cut off rather than hanging.
    assert!(matches!(
        v(&wb, "B10"),
        Value::Bool(false) | Value::Error(_)
    ));
}

#[test]
fn hostile_inputs_do_not_panic() {
    let wb = wb_with(&[
        ("A1", "=PRICE(1,2958465,1E300,-1E300,1E-300,4,4)"),
        ("A2", "=XIRR({1,-1},{0,1E9})"),
        ("A3", "=VDB(1,0,1E300,0,1E300)"),
        ("A4", "=MUNIT(1E9)"),
        ("A5", "=COUPNUM(0,2958465,4,1)"),
        ("A6", "=REGEXEXTRACT(REPT(\"a\",30000),\"(a|b)*c\")"),
        ("A7", "=AMORDEGRC(1E300,1,2,0,1E300,1E-300)"),
        ("A8", "=TEXTSPLIT(REPT(\",\",30000),\",\",\",\")"),
        ("A9", "=DSUM(A:A,1,A:A)"),
        ("A10", "=ODDFYIELD(1,1000,0,500,0.1,-5,100,2)"),
    ]);
    for r in 1..=10 {
        let _ = v(&wb, &format!("A{r}"));
    }
}
