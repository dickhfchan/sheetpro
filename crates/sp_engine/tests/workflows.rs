//! Realistic multi-command tasks driven only through the JSON control protocol
//! (`control::handle_line`), the way an agent drives the app. Each asserts the final
//! workbook state.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use serde_json::{Value as Json, json};
use sp_engine::Session;
use sp_engine::control::handle_line;
use std::path::PathBuf;

fn call(s: &mut Session, req: &Json) -> Json {
    let (reply, _) = handle_line(s, &req.to_string());
    serde_json::from_str(&reply).expect("reply is JSON")
}

/// Run a command that must succeed; returns its result.
fn exec(s: &mut Session, id: &str, params: Json) -> Json {
    let r = call(s, &json!({"op": "exec", "id": id, "params": params}));
    assert_eq!(r["ok"], true, "{id} {params}: {r}");
    r["result"].clone()
}

/// Run a command that must fail; returns the error message.
fn refused(s: &mut Session, id: &str, params: Json) -> String {
    let r = call(s, &json!({"op": "exec", "id": id, "params": params}));
    assert_eq!(r["ok"], false, "{id} {params} should fail: {r}");
    r["error"].as_str().unwrap_or_default().to_string()
}

fn set(s: &mut Session, cell: &str, input: &str) {
    let r = call(s, &json!({"op": "set", "cell": cell, "input": input}));
    assert_eq!(r["ok"], true, "set {cell}={input}: {r}");
}

/// Fill a block starting at `top_left` from rows of inputs.
fn fill(s: &mut Session, top_left: &str, rows: &[&[&str]]) {
    let (c0, r0) = split(top_left);
    for (i, row) in rows.iter().enumerate() {
        for (j, v) in row.iter().enumerate() {
            if v.is_empty() {
                continue;
            }
            let col = char::from(c0 + j as u8);
            set(s, &format!("{col}{}", r0 + i), v);
        }
    }
}

fn split(a: &str) -> (u8, usize) {
    let col = a.as_bytes()[0];
    (col, a[1..].parse().unwrap())
}

fn cell(s: &mut Session, a: &str) -> Json {
    let r = call(s, &json!({"op": "get", "range": a}));
    assert_eq!(r["ok"], true, "get {a}: {r}");
    r["result"][0][0].clone()
}

fn num(s: &mut Session, a: &str) -> f64 {
    let c = cell(s, a);
    c["value"]
        .as_f64()
        .unwrap_or_else(|| panic!("{a} not a number: {c}"))
}

fn text(s: &mut Session, a: &str) -> String {
    cell(s, a)["display"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn input(s: &mut Session, a: &str) -> String {
    cell(s, a)["input"].as_str().unwrap_or_default().to_string()
}

fn column(s: &mut Session, range: &str) -> Vec<String> {
    let r = call(s, &json!({"op": "get", "range": range}));
    r["result"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row[0]["display"].as_str().unwrap_or_default().to_string())
        .collect()
}

fn tmp(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("workflows");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

#[test]
fn budget_with_totals_percent_and_sort() {
    let mut s = Session::new();
    fill(
        &mut s,
        "A1",
        &[
            &["Item", "Amount", "Share"],
            &["Rent", "1200"],
            &["Food", "450"],
            &["Travel", "150"],
            &["Utilities", "200"],
            &["Total", "=SUM(B2:B5)"],
        ],
    );
    for r in 2..=5 {
        set(&mut s, &format!("C{r}"), &format!("=B{r}/$B$6"));
    }
    exec(&mut s, "home.number.percent", json!({"range": "C2:C5"}));
    exec(&mut s, "home.font.bold", json!({"range": "A1:C1"}));
    exec(
        &mut s,
        "data.sort",
        json!({"range": "A1:C5", "keys": [{"column": "B", "order": "desc"}], "header": true}),
    );
    assert_eq!(
        column(&mut s, "A2:A5"),
        ["Rent", "Food", "Utilities", "Travel"]
    );
    assert_eq!(num(&mut s, "B6"), 2000.0);
    assert_eq!(input(&mut s, "C3"), "=B3/$B$6");
    assert_eq!(text(&mut s, "C2"), "60%");
    assert_eq!(text(&mut s, "C5"), "8%");
    // Change an amount: total and shares follow.
    set(&mut s, "B5", "350");
    assert_eq!(num(&mut s, "B6"), 2200.0);
    assert!(close(num(&mut s, "C5"), 350.0 / 2200.0));
}

#[test]
fn csv_import_split_dedupe_filter() {
    let path = tmp("people.csv");
    std::fs::write(
        &path,
        "City,Name\nParis,Curie|Marie\nLondon,Turing|Alan\nParis,Curie|Marie\nBerlin,Planck|Max\nLondon,Lovelace|Ada\n",
    )
    .unwrap();
    let mut s = Session::new();
    exec(&mut s, "file.open", json!({"path": path.to_str().unwrap()}));
    assert_eq!(text(&mut s, "B2"), "Curie|Marie");
    exec(
        &mut s,
        "data.text_to_columns",
        json!({"range": "B1:B6", "delimiter": "|"}),
    );
    assert_eq!(
        (text(&mut s, "B3"), text(&mut s, "C3")),
        ("Turing".into(), "Alan".into())
    );
    exec(
        &mut s,
        "data.remove_duplicates",
        json!({"range": "A1:C6", "header": true}),
    );
    assert_eq!(
        column(&mut s, "A2:A6"),
        ["Paris", "London", "Berlin", "London", ""]
    );
    exec(
        &mut s,
        "data.filter",
        json!({"range": "A1:C5", "on": true, "column": "A", "values": ["London"]}),
    );
    let hidden: Vec<u32> = s.wb.sheets[0].hidden_rows.iter().copied().collect();
    assert_eq!(hidden, vec![1, 3]); // rows 2 (Paris) and 4 (Berlin)
    set(&mut s, "E1", "=SUBTOTAL(3,A2:A5)");
    assert_eq!(num(&mut s, "E1"), 2.0);
    // Inserting a row inside the filtered list grows the filter and keeps hidden rows.
    exec(&mut s, "home.cells.insert_rows", json!({"range": "A3"}));
    let af = s.wb.sheets[0].auto_filter.clone().unwrap();
    assert_eq!(af.range.to_a1(), "A1:C6");
    let hidden: Vec<u32> = s.wb.sheets[0].hidden_rows.iter().copied().collect();
    assert_eq!(hidden, vec![1, 4]);
    assert_eq!(input(&mut s, "E1"), "=SUBTOTAL(3,A2:A6)");
    set(&mut s, "A3", "London");
    assert_eq!(num(&mut s, "E1"), 3.0);
    exec(&mut s, "data.filter_reapply", json!({}));
    assert_eq!(num(&mut s, "E1"), 3.0);
    exec(&mut s, "edit.undo", json!({}));
    exec(&mut s, "edit.undo", json!({}));
    exec(&mut s, "edit.undo", json!({}));
    assert_eq!(
        s.wb.sheets[0].auto_filter.clone().unwrap().range.to_a1(),
        "A1:C5"
    );
    exec(&mut s, "data.filter_clear", json!({}));
    assert!(s.wb.sheets[0].hidden_rows.is_empty());
    assert_eq!(num(&mut s, "E1"), 4.0);
}

#[test]
fn loan_amortisation_fill_down() {
    let mut s = Session::new();
    fill(
        &mut s,
        "A1",
        &[
            &["Principal", "10000"],
            &["Rate", "=0.06/12"],
            &["Periods", "12"],
            &["Payment", "=PMT(B2,B3,-B1)"],
            &[],
            &["Period", "Interest", "Principal", "Balance"],
            &["0", "", "", "=B1"],
            &[
                "=A7+1",
                "=IPMT($B$2,A8,$B$3,-$B$1)",
                "=PPMT($B$2,A8,$B$3,-$B$1)",
                "=D7-C8",
            ],
        ],
    );
    exec(&mut s, "home.editing.fill_down", json!({"range": "A8:D19"}));
    assert_eq!(input(&mut s, "D19"), "=D18-C19");
    assert_eq!(num(&mut s, "A19"), 12.0);
    assert!(close(num(&mut s, "D19"), 0.0), "final balance");
    let payment = num(&mut s, "B4");
    assert!(close(payment, 860.664_297_7), "{payment}");
    // Interest + principal = payment in every period; total interest = 12·pmt − principal.
    set(&mut s, "F1", "=SUM(B8:B19)");
    assert!(close(num(&mut s, "F1"), 12.0 * payment - 10_000.0));
    assert!(close(num(&mut s, "B10") + num(&mut s, "C10"), payment));
}

#[test]
fn insert_rows_inside_sum_range() {
    let mut s = Session::new();
    fill(
        &mut s,
        "A1",
        &[
            &["10"],
            &["20"],
            &["30"],
            &["40"],
            &["=SUM(A1:A4)"],
            &["=A5*2"],
        ],
    );
    exec(
        &mut s,
        "insert.note",
        json!({"cell": "A3", "text": "check"}),
    );
    exec(&mut s, "home.cells.insert_rows", json!({"range": "A3:A4"}));
    assert_eq!(input(&mut s, "A7"), "=SUM(A1:A6)");
    assert_eq!(input(&mut s, "A8"), "=A7*2");
    set(&mut s, "A3", "5");
    set(&mut s, "A4", "5");
    assert_eq!(num(&mut s, "A7"), 110.0);
    assert_eq!(num(&mut s, "A8"), 220.0);
    // The note moved with its cell.
    assert_eq!(s.wb.sheets[0].comments[0].addr.to_a1(), "A5");
    exec(&mut s, "home.cells.delete_rows", json!({"range": "A1:A2"}));
    assert_eq!(input(&mut s, "A5"), "=SUM(A1:A4)");
    assert_eq!(num(&mut s, "A5"), 80.0);
    assert_eq!(s.wb.sheets[0].comments[0].addr.to_a1(), "A3");
    for _ in 0..4 {
        exec(&mut s, "edit.undo", json!({}));
    }
    assert_eq!(input(&mut s, "A5"), "=SUM(A1:A4)");
    assert_eq!(num(&mut s, "A5"), 100.0);
    assert_eq!(s.wb.sheets[0].comments[0].addr.to_a1(), "A3");
}

#[test]
fn dynamic_array_filter_sort_report() {
    let mut s = Session::new();
    fill(
        &mut s,
        "A1",
        &[
            &["Rep", "Sales"],
            &["Ann", "120"],
            &["Bob", "40"],
            &["Cy", "300"],
            &["Di", "75"],
            &["Ed", "55"],
        ],
    );
    set(&mut s, "D2", "=SORT(FILTER(A2:B6,B2:B6>50),2,-1)");
    assert_eq!(column(&mut s, "D2:D6"), ["Cy", "Ann", "Di", "Ed", ""]);
    assert_eq!(num(&mut s, "E3"), 120.0);
    set(&mut s, "G1", "=SUM(INDEX(D2#,0,2))");
    assert_eq!(num(&mut s, "G1"), 550.0);
    // Source changes re-spill the report.
    set(&mut s, "B3", "500");
    assert_eq!(
        column(&mut s, "D2:D7"),
        ["Bob", "Cy", "Ann", "Di", "Ed", ""]
    );
    assert_eq!(num(&mut s, "G1"), 1050.0);
    // A blocker makes it #SPILL!; clearing the blocker restores it.
    set(&mut s, "D5", "x");
    assert_eq!(text(&mut s, "D2"), "#SPILL!");
    exec(
        &mut s,
        "home.editing.clear_contents",
        json!({"range": "D5"}),
    );
    assert_eq!(text(&mut s, "D2"), "Bob");
}

#[test]
fn protected_sheet_refuses_edits() {
    let mut s = Session::new();
    fill(
        &mut s,
        "A1",
        &[&["Qty", "5"], &["Price", "2"], &["Total", "=B1*B2"]],
    );
    exec(&mut s, "home.cells.lock_cell", json!({"range": "B1:B2"})); // unlock inputs
    exec(
        &mut s,
        "review.protect_sheet",
        json!({"protect": true, "password": "pw"}),
    );
    let e = refused(&mut s, "cell.set", json!({"cell": "B3", "input": "1"}));
    assert!(e.contains("protected"), "{e}");
    refused(&mut s, "home.cells.insert_rows", json!({"range": "A2"}));
    refused(&mut s, "home.font.bold", json!({"range": "A1"}));
    set(&mut s, "B1", "7");
    assert_eq!(num(&mut s, "B3"), 14.0);
    refused(
        &mut s,
        "review.protect_sheet",
        json!({"protect": false, "password": "nope"}),
    );
    exec(
        &mut s,
        "review.protect_sheet",
        json!({"protect": false, "password": "pw"}),
    );
    set(&mut s, "B3", "1");
    assert_eq!(num(&mut s, "B3"), 1.0);
}

#[test]
fn validation_list_rejects_bad_entries() {
    let mut s = Session::new();
    fill(&mut s, "A1", &[&["Colour"]]);
    exec(
        &mut s,
        "data.validation",
        json!({"range": "A2:A10", "type": "list", "source": "Red,Green,Blue", "error": "Pick a colour"}),
    );
    set(&mut s, "A2", "Green");
    let e = refused(&mut s, "cell.set", json!({"cell": "A3", "input": "Purple"}));
    assert!(e.contains("Pick a colour"), "{e}");
    assert_eq!(text(&mut s, "A3"), "");
    // Rows inserted above move the rule with the cells.
    exec(&mut s, "home.cells.insert_rows", json!({"range": "A1"}));
    refused(&mut s, "cell.set", json!({"cell": "A4", "input": "Purple"}));
    set(&mut s, "A2", "Purple"); // the header row is outside the rule
    set(&mut s, "A4", "blue");
    assert_eq!(text(&mut s, "A3"), "Green");
}

#[test]
fn find_replace_across_sheets_then_undo() {
    let mut s = Session::new();
    fill(
        &mut s,
        "A1",
        &[&["apple pie"], &["Apple"], &["=\"apple\"&\"s\""]],
    );
    exec(&mut s, "home.cells.insert_sheet", json!({"name": "Two"}));
    set(&mut s, "Two!B2", "green apple");
    // Find & Replace searches the active sheet unless asked for the whole workbook.
    let found = exec(&mut s, "edit.find_all", json!({"what": "apple"}));
    assert_eq!(found.as_array().map(Vec::len), Some(1));
    let found = exec(
        &mut s,
        "edit.find_all",
        json!({"what": "apple", "within": "workbook"}),
    );
    assert_eq!(found.as_array().map(Vec::len), Some(4));
    let n = exec(
        &mut s,
        "home.editing.replace",
        json!({"what": "apple", "with": "pear", "within": "workbook"}),
    );
    assert_eq!(n, 4);
    assert_eq!(text(&mut s, "Sheet1!A1"), "pear pie");
    assert_eq!(text(&mut s, "Sheet1!A2"), "pear");
    assert_eq!(text(&mut s, "Sheet1!A3"), "pears");
    assert_eq!(text(&mut s, "Two!B2"), "green pear");
    exec(&mut s, "edit.undo", json!({}));
    assert_eq!(text(&mut s, "Sheet1!A1"), "apple pie");
    assert_eq!(text(&mut s, "Sheet1!A3"), "apples");
    assert_eq!(text(&mut s, "Two!B2"), "green apple");
}

#[test]
fn save_xlsx_and_reopen() {
    let path = tmp("roundtrip.xlsx");
    let mut s = Session::new();
    fill(
        &mut s,
        "A1",
        &[
            &["Name", "Score"],
            &["a", "0.5"],
            &["b", "0.25"],
            &["Mean", "=AVERAGE(B2:B3)"],
        ],
    );
    exec(&mut s, "home.font.bold", json!({"range": "A1:B1"}));
    exec(&mut s, "home.number.percent", json!({"range": "B2:B4"}));
    exec(
        &mut s,
        "formulas.define_name",
        json!({"name": "Scores", "refers_to": "=Sheet1!$B$2:$B$3"}),
    );
    set(&mut s, "C1", "=SUM(Scores)");
    exec(
        &mut s,
        "insert.note",
        json!({"cell": "A2", "text": "first"}),
    );
    exec(
        &mut s,
        "data.validation",
        json!({"range": "B2:B3", "type": "decimal", "operator": "between", "min": 0, "max": 1}),
    );
    exec(&mut s, "data.filter", json!({"range": "A1:B3", "on": true}));
    set(&mut s, "E1", "=SORT(A2:A3,,-1)");
    exec(
        &mut s,
        "file.save_as",
        json!({"path": path.to_str().unwrap()}),
    );

    let mut t = Session::new();
    exec(&mut t, "file.open", json!({"path": path.to_str().unwrap()}));
    assert_eq!(input(&mut t, "B4"), "=AVERAGE(B2:B3)");
    assert!(close(num(&mut t, "B4"), 0.375));
    assert_eq!(text(&mut t, "B4"), "38%");
    assert_eq!(num(&mut t, "C1"), 0.75);
    let sh = &t.wb.sheets[0];
    let a1 = sh.get(sp_core::CellAddr::new(0, 0)).unwrap();
    assert!(t.wb.styles.get(a1.style).bold);
    assert_eq!(sh.comments.len(), 1);
    assert_eq!(sh.validations.len(), 1);
    assert_eq!(
        sh.auto_filter.as_ref().map(|f| f.range.to_a1()),
        Some("A1:B3".into())
    );
    assert_eq!(column(&mut t, "E1:E2"), ["b", "a"]);
    refused(&mut t, "cell.set", json!({"cell": "B2", "input": "2"}));
    // Recalculates after an edit in the reopened book.
    set(&mut t, "B3", "1");
    assert_eq!(text(&mut t, "B4"), "75%");
}

#[test]
fn rename_sheet_referenced_elsewhere() {
    let mut s = Session::new();
    fill(&mut s, "A1", &[&["10"], &["20"]]);
    exec(&mut s, "home.cells.insert_sheet", json!({"name": "Report"}));
    set(&mut s, "Report!A1", "=Sheet1!A1+Sheet1!A2");
    set(&mut s, "Report!A2", "=SUM(Sheet1!A1:A2)*2");
    exec(
        &mut s,
        "data.validation",
        json!({"range": "Report!B1", "type": "list", "source": "=Sheet1!$A$1:$A$2"}),
    );
    exec(&mut s, "sheet.activate", json!({"sheet": "Sheet1"}));
    exec(&mut s, "layout.print_area.set", json!({"range": "A1:A2"}));
    exec(
        &mut s,
        "home.cells.rename_sheet",
        json!({"sheet": "Sheet1", "name": "Raw Data"}),
    );
    assert_eq!(input(&mut s, "Report!A1"), "='Raw Data'!A1+'Raw Data'!A2");
    assert_eq!(num(&mut s, "Report!A1"), 30.0);
    set(&mut s, "'Raw Data'!A2", "25");
    assert_eq!(num(&mut s, "Report!A2"), 70.0);
    // Structural edits on the renamed sheet still reach the other sheet's formulas.
    exec(
        &mut s,
        "home.cells.insert_rows",
        json!({"range": "'Raw Data'!A1"}),
    );
    assert_eq!(input(&mut s, "Report!A2"), "=SUM('Raw Data'!A2:A3)*2");
    assert_eq!(num(&mut s, "Report!A2"), 70.0);
    // Validation sources and the print area follow both the rename and the insert.
    assert_eq!(
        s.wb.sheets[s.wb.sheet_index("Report").unwrap()].validations[0].formula1,
        "'Raw Data'!$A$2:$A$3"
    );
    set(&mut s, "Report!B1", "25");
    refused(
        &mut s,
        "cell.set",
        json!({"cell": "Report!B1", "input": "11"}),
    );
    let area =
        s.wb.names
            .iter()
            .find(|n| n.name.ends_with("Print_Area"))
            .unwrap();
    assert_eq!(area.formula, "'Raw Data'!$A$2:$A$3");
    exec(&mut s, "edit.undo", json!({}));
    exec(&mut s, "edit.undo", json!({}));
    exec(&mut s, "edit.undo", json!({}));
    exec(&mut s, "edit.undo", json!({}));
    assert_eq!(input(&mut s, "Report!A1"), "=Sheet1!A1+Sheet1!A2");
}

#[test]
fn subtotal_report_collapse_and_grand_total() {
    let mut s = Session::new();
    fill(
        &mut s,
        "A1",
        &[
            &["Region", "Sales"],
            &["East", "10"],
            &["East", "20"],
            &["West", "5"],
            &["West", "15"],
        ],
    );
    exec(
        &mut s,
        "data.subtotal",
        json!({"range": "A1:B5", "by": "A", "columns": ["B"]}),
    );
    assert_eq!(text(&mut s, "A8"), "Grand Total");
    assert_eq!(num(&mut s, "B8"), 50.0);
    set(&mut s, "D1", "=SUBTOTAL(109,B2:B7)");
    set(&mut s, "D2", "=SUBTOTAL(9,B2:B7)");
    // Collapse the East group (rows 2-3): 109 skips hidden rows, 9 does not.
    exec(&mut s, "data.hide_detail", json!({"range": "A4"}));
    assert_eq!(num(&mut s, "D1"), 20.0);
    assert_eq!(num(&mut s, "D2"), 50.0);
}
