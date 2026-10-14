//! Pivot and what-if workflows driven through the JSON control protocol.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use serde_json::{Value as Json, json};
use sp_core::{CellAddr, Value};
use sp_engine::Session;
use sp_engine::control::handle_line;

fn call(s: &mut Session, req: &Json) -> Json {
    let (reply, _) = handle_line(s, &req.to_string());
    serde_json::from_str(&reply).expect("reply is JSON")
}

fn exec(s: &mut Session, id: &str, params: Json) -> Json {
    let r = call(s, &json!({"op": "exec", "id": id, "params": params}));
    assert_eq!(r["ok"], true, "{id} {params}: {r}");
    r["result"].clone()
}

fn set(s: &mut Session, cell: &str, input: &str) {
    let r = call(s, &json!({"op": "set", "cell": cell, "input": input}));
    assert_eq!(r["ok"], true, "set {cell}={input}: {r}");
}

fn num(s: &Session, sheet: &str, a: &str) -> f64 {
    let i = s.wb.sheet_index(sheet).unwrap();
    match s.wb.sheets[i].value(CellAddr::parse(a).unwrap()) {
        Value::Number(n) => *n,
        other => panic!("{sheet}!{a} = {other:?}"),
    }
}

#[test]
fn pivot_refresh_getpivotdata_and_slicer() {
    let mut s = Session::new();
    let data = [
        ("Region", "Rep", "Amount"),
        ("North", "Ann", "100"),
        ("South", "Bob", "250"),
        ("North", "Cid", "50"),
        ("East", "Ann", "75"),
        ("South", "Ann", "25"),
    ];
    for (r, (a, b, c)) in data.iter().enumerate() {
        set(&mut s, &format!("A{}", r + 1), a);
        set(&mut s, &format!("B{}", r + 1), b);
        set(&mut s, &format!("C{}", r + 1), c);
    }
    exec(&mut s, "insert.table", json!({"range": "A1:C6", "name": "Sales"}));
    // Pivot from the table onto a new sheet.
    let r = exec(
        &mut s,
        "insert.pivot_table",
        json!({"source": "Sales", "rows": ["Region"], "cols": ["Rep"], "values": [{"field": "Amount", "func": "sum"}]}),
    );
    let psheet = r["sheet"].as_str().unwrap().to_string();
    // A formula on the data sheet asks the pivot.
    let q = format!("=GETPIVOTDATA(\"Amount\",'{psheet}'!$A$3,\"Region\",\"North\")");
    set(&mut s, "Sheet1!E1", &q);
    assert_eq!(num(&s, "Sheet1", "E1"), 150.0);
    set(&mut s, "Sheet1!E2", &format!("=GETPIVOTDATA(\"Amount\",'{psheet}'!$A$3)"));
    assert_eq!(num(&s, "Sheet1", "E2"), 500.0);

    // Edit the source; values only change after a refresh.
    set(&mut s, "Sheet1!C2", "400");
    assert_eq!(num(&s, "Sheet1", "E1"), 150.0);
    let r = exec(&mut s, "data.refresh_all", json!({}));
    assert_eq!(r["refreshed"], 1);
    assert_eq!(num(&s, "Sheet1", "E1"), 450.0);
    assert_eq!(num(&s, "Sheet1", "E2"), 800.0);

    // Rows appended to the table are picked up too.
    set(&mut s, "Sheet1!A7", "East");
    set(&mut s, "Sheet1!B7", "Bob");
    set(&mut s, "Sheet1!C7", "1000");
    exec(&mut s, "data.refresh_all", json!({}));
    assert_eq!(num(&s, "Sheet1", "E2"), 1800.0);

    // A slicer on Rep filters the pivot (and GETPIVOTDATA follows it).
    exec(
        &mut s,
        "insert.slicer",
        json!({"pivot": "PivotTable1", "field": "Rep", "select": ["Ann"]}),
    );
    assert_eq!(num(&s, "Sheet1", "E1"), 400.0);
    assert_eq!(num(&s, "Sheet1", "E2"), 500.0);

    // Undo the slicer, then the refreshes: back to the original snapshot.
    exec(&mut s, "edit.undo", json!({}));
    assert_eq!(num(&s, "Sheet1", "E2"), 1800.0);
}

#[test]
fn goal_seek_loan_payment_then_data_table_and_scenarios() {
    let mut s = Session::new();
    set(&mut s, "A1", "Principal");
    set(&mut s, "B1", "100000");
    set(&mut s, "A2", "Rate");
    set(&mut s, "B2", "0.06");
    set(&mut s, "A3", "Months");
    set(&mut s, "B3", "360");
    set(&mut s, "A4", "Payment");
    set(&mut s, "B4", "=PMT(B2/12,B3,-B1)");
    assert!((num(&s, "Sheet1", "B4") - 599.55).abs() < 0.01);

    // How much can be borrowed for a 1,000 monthly payment?
    let r = exec(
        &mut s,
        "data.what_if.goal_seek",
        json!({"set_cell": "B4", "to_value": 1000, "by_changing": "B1"}),
    );
    assert_eq!(r["found"], true);
    let principal = num(&s, "Sheet1", "B1");
    assert!((principal - 166_791.61).abs() < 1.0, "{principal}");
    assert!((num(&s, "Sheet1", "B4") - 1000.0).abs() < 0.001);

    // Payment at several rates (one-variable data table, column input B2).
    set(&mut s, "E1", "=B4");
    for (i, rate) in ["0.04", "0.05", "0.06", "0.07"].iter().enumerate() {
        set(&mut s, &format!("D{}", i + 2), rate);
    }
    exec(
        &mut s,
        "data.what_if.data_table",
        json!({"range": "D1:E5", "col_input": "B2"}),
    );
    assert!((num(&s, "Sheet1", "E4") - 1000.0).abs() < 0.001);
    assert!(num(&s, "Sheet1", "E2") < num(&s, "Sheet1", "E5"));
    // The inputs are untouched by the table.
    assert_eq!(num(&s, "Sheet1", "B2"), 0.06);

    // Scenarios on rate and term; the summary recomputes the payment under each.
    exec(
        &mut s,
        "data.what_if.scenario",
        json!({"action": "add", "name": "Short", "changing": "B2:B3", "values": [0.05, 180]}),
    );
    exec(
        &mut s,
        "data.what_if.scenario",
        json!({"action": "add", "name": "Long", "changing": "B2:B3", "values": [0.07, 360]}),
    );
    let r = exec(
        &mut s,
        "data.what_if.scenario",
        json!({"action": "summary", "result_cells": "B4"}),
    );
    let sum_sheet = r["sheet"].as_str().unwrap().to_string();
    let short = num(&s, &sum_sheet, "C7");
    exec(&mut s, "sheet.previous", json!({}));
    exec(&mut s, "data.what_if.scenario", json!({"action": "show", "name": "Short"}));
    assert!((num(&s, "Sheet1", "B4") - short).abs() < 1e-9);
}
