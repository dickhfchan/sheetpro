//! M5 editing-parity audit: clipboard, fill, insert/delete, find/replace and undo, each
//! checked against standard spreadsheet semantics through the JSON control protocol (the
//! way an agent or the shell drives the app). Every test also checks that the edit is a
//! single undo step that restores the whole workbook.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use serde_json::{Value as Json, json};
use sp_core::{CellAddr, CellRange};
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

fn refused(s: &mut Session, id: &str, params: Json) -> String {
    let r = call(s, &json!({"op": "exec", "id": id, "params": params}));
    assert_eq!(r["ok"], false, "{id} {params} should fail: {r}");
    r["error"].as_str().unwrap_or_default().to_string()
}

fn set(s: &mut Session, cell: &str, input: &str) {
    let r = call(s, &json!({"op": "set", "cell": cell, "input": input}));
    assert_eq!(r["ok"], true, "set {cell}={input}: {r}");
}

fn sets(s: &mut Session, cells: &[(&str, &str)]) {
    for (c, v) in cells {
        set(s, c, v);
    }
}

fn get(s: &mut Session, a: &str) -> Json {
    let r = call(s, &json!({"op": "get", "range": a}));
    assert_eq!(r["ok"], true, "get {a}: {r}");
    r["result"][0][0].clone()
}

fn input(s: &mut Session, a: &str) -> String {
    get(s, a)["input"].as_str().unwrap_or_default().to_string()
}

fn text(s: &mut Session, a: &str) -> String {
    get(s, a)["display"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn num(s: &mut Session, a: &str) -> f64 {
    let c = get(s, a);
    c["value"]
        .as_f64()
        .unwrap_or_else(|| panic!("{a} not a number: {c}"))
}

fn inputs(s: &mut Session, cells: &[&str]) -> Vec<String> {
    cells.iter().map(|c| input(s, c)).collect()
}

fn r(a: &str) -> CellRange {
    CellRange::parse(a).unwrap()
}

fn a(x: &str) -> CellAddr {
    CellAddr::parse(x).unwrap()
}

/// Undo must be one step back to `before`, and redo must return to `after`.
fn one_undo_step(s: &mut Session, before: &sp_model::Workbook) {
    let after = s.wb.clone();
    exec(s, "edit.undo", json!({}));
    assert_eq!(&s.wb, before, "one undo restores the whole workbook");
    exec(s, "edit.redo", json!({}));
    assert_eq!(s.wb, after, "redo re-applies the edit");
}

// ---------------------------------------------------------------- clipboard

#[test]
fn copy_paste_shifts_relative_refs_only() {
    let mut s = Session::new();
    set(&mut s, "B1", "=A1+$A$1+A$1+$A1+SUM(A1:A2)+Sheet1!A1");
    exec(&mut s, "home.clipboard.copy", json!({"range": "B1"}));
    let before = s.wb.clone();
    exec(&mut s, "home.clipboard.paste", json!({"cell": "D3"}));
    assert_eq!(input(&mut s, "D3"), "=C3+$A$1+C$1+$A3+SUM(C3:C4)+Sheet1!C3");
    // The copy is unchanged and the clipboard survives (copy can paste many times).
    assert_eq!(input(&mut s, "B1"), "=A1+$A$1+A$1+$A1+SUM(A1:A2)+Sheet1!A1");
    one_undo_step(&mut s, &before);
    exec(&mut s, "home.clipboard.paste", json!({"cell": "E4"}));
    assert_eq!(input(&mut s, "E4"), "=D4+$A$1+D$1+$A4+SUM(D4:D5)+Sheet1!D4");
}

#[test]
fn copy_paste_off_sheet_ref_becomes_ref_error() {
    let mut s = Session::new();
    set(&mut s, "B2", "=A1");
    exec(&mut s, "home.clipboard.copy", json!({"range": "B2"}));
    exec(&mut s, "home.clipboard.paste", json!({"cell": "A1"}));
    assert_eq!(input(&mut s, "A1"), "=#REF!");
}

#[test]
fn cut_paste_moves_cells_and_rewrites_refs_everywhere() {
    let mut s = Session::new();
    sets(
        &mut s,
        &[
            ("A1", "5"),
            ("A2", "7"),
            ("C1", "=A1+A2"),
            ("C2", "=SUM(A1:A2)"),
            ("C3", "=$A$1*2"),
        ],
    );
    exec(&mut s, "home.cells.insert_sheet", json!({"name": "Other"}));
    set(&mut s, "Other!A1", "=Sheet1!A1*10");
    exec(
        &mut s,
        "formulas.define_name",
        json!({"name": "Base", "refers_to": "=Sheet1!$A$1"}),
    );
    exec(&mut s, "nav.goto_cell", json!({"reference": "Sheet1!A1"}));
    exec(
        &mut s,
        "home.clipboard.cut",
        json!({"range": "Sheet1!A1:A2"}),
    );
    let before = s.wb.clone();
    exec(&mut s, "home.clipboard.paste", json!({"cell": "Sheet1!E5"}));
    assert_eq!(num(&mut s, "Sheet1!E5"), 5.0);
    assert_eq!(num(&mut s, "Sheet1!E6"), 7.0);
    assert_eq!(input(&mut s, "Sheet1!A1"), "");
    assert_eq!(input(&mut s, "Sheet1!A2"), "");
    assert_eq!(input(&mut s, "Sheet1!C1"), "=E5+E6");
    assert_eq!(input(&mut s, "Sheet1!C2"), "=SUM(E5:E6)");
    assert_eq!(input(&mut s, "Sheet1!C3"), "=$E$5*2");
    assert_eq!(input(&mut s, "Other!A1"), "=Sheet1!E5*10");
    assert_eq!(num(&mut s, "Other!A1"), 50.0);
    let base = s.wb.names.iter().find(|n| n.name == "Base").unwrap();
    assert_eq!(base.formula, "Sheet1!$E$5");
    // Cut is a one-shot move: the clipboard is spent.
    assert!(s.clipboard.is_none());
    one_undo_step(&mut s, &before);
}

#[test]
fn cut_formula_keeps_its_refs_unless_they_move_with_it() {
    let mut s = Session::new();
    sets(&mut s, &[("A1", "3"), ("B1", "=A1*2")]);
    exec(&mut s, "home.clipboard.cut", json!({"range": "B1"}));
    exec(&mut s, "home.clipboard.paste", json!({"cell": "D4"}));
    assert_eq!(input(&mut s, "D4"), "=A1*2");
    assert_eq!(num(&mut s, "D4"), 6.0);
    // Moving the formula together with its precedent: the ref follows.
    exec(&mut s, "home.clipboard.cut", json!({"range": "A1"}));
    exec(&mut s, "home.clipboard.paste", json!({"cell": "C4"}));
    assert_eq!(input(&mut s, "D4"), "=C4*2");
    exec(&mut s, "home.clipboard.cut", json!({"range": "C4:D4"}));
    exec(&mut s, "home.clipboard.paste", json!({"cell": "A10"}));
    assert_eq!(input(&mut s, "B10"), "=A10*2");
    assert_eq!(num(&mut s, "B10"), 6.0);
}

#[test]
fn cut_part_of_a_range_leaves_the_range_ref_alone() {
    let mut s = Session::new();
    sets(
        &mut s,
        &[
            ("A1", "1"),
            ("A2", "2"),
            ("A3", "3"),
            ("C1", "=SUM(A1:A3)"),
            ("C2", "=A1"),
        ],
    );
    exec(&mut s, "home.clipboard.cut", json!({"range": "A1"}));
    exec(&mut s, "home.clipboard.paste", json!({"cell": "F1"}));
    assert_eq!(input(&mut s, "C1"), "=SUM(A1:A3)");
    assert_eq!(input(&mut s, "C2"), "=F1");
}

#[test]
fn cut_paste_over_referenced_cells_makes_ref_errors() {
    let mut s = Session::new();
    sets(
        &mut s,
        &[("A1", "1"), ("G1", "9"), ("D1", "=G1"), ("D2", "=A1")],
    );
    exec(&mut s, "home.clipboard.cut", json!({"range": "A1"}));
    exec(&mut s, "home.clipboard.paste", json!({"cell": "G1"}));
    assert_eq!(input(&mut s, "D1"), "=#REF!");
    assert_eq!(input(&mut s, "D2"), "=G1");
    assert_eq!(num(&mut s, "D2"), 1.0);
}

#[test]
fn cut_paste_overlapping_move_down() {
    let mut s = Session::new();
    sets(
        &mut s,
        &[("A1", "1"), ("A2", "2"), ("A3", "=A1+A2"), ("C1", "=A3")],
    );
    exec(&mut s, "home.clipboard.cut", json!({"range": "A1:A3"}));
    exec(&mut s, "home.clipboard.paste", json!({"cell": "A2"}));
    assert_eq!(
        inputs(&mut s, &["A1", "A2", "A3", "A4"]),
        ["", "1", "2", "=A2+A3"]
    );
    assert_eq!(input(&mut s, "C1"), "=A4");
    assert_eq!(num(&mut s, "C1"), 3.0);
}

#[test]
fn cut_paste_to_other_sheet_qualifies_refs() {
    let mut s = Session::new();
    sets(&mut s, &[("A1", "4"), ("B1", "=A1+1")]);
    exec(&mut s, "home.cells.insert_sheet", json!({"name": "Dest"}));
    exec(&mut s, "home.clipboard.cut", json!({"range": "Sheet1!A1"}));
    exec(&mut s, "home.clipboard.paste", json!({"cell": "Dest!C3"}));
    assert_eq!(input(&mut s, "Sheet1!B1"), "=Dest!C3+1");
    assert_eq!(num(&mut s, "Sheet1!B1"), 5.0);
    assert_eq!(input(&mut s, "Sheet1!A1"), "");
}

#[test]
fn cut_moves_formats_and_notes() {
    let mut s = Session::new();
    set(&mut s, "A1", "x");
    exec(&mut s, "home.font.bold", json!({"range": "A1"}));
    exec(&mut s, "insert.note", json!({"cell": "A1", "text": "hi"}));
    exec(&mut s, "home.clipboard.cut", json!({"range": "A1"}));
    exec(&mut s, "home.clipboard.paste", json!({"cell": "C3"}));
    let sh = &s.wb.sheets[0];
    assert!(sh.get(a("A1")).is_none_or(|c| c.style.0 == 0));
    assert!(sh.comment_at(a("A1")).is_none());
    assert!(sh.comment_at(a("C3")).is_some());
    let st = s.wb.styles.get(sh.get(a("C3")).unwrap().style);
    assert!(st.bold);
}

#[test]
fn copy_paste_copies_notes() {
    let mut s = Session::new();
    set(&mut s, "A1", "x");
    exec(&mut s, "insert.note", json!({"cell": "A1", "text": "hi"}));
    exec(&mut s, "home.clipboard.copy", json!({"range": "A1"}));
    exec(&mut s, "home.clipboard.paste", json!({"cell": "B2"}));
    let sh = &s.wb.sheets[0];
    assert_eq!(sh.comment_at(a("B2")).map(|c| c.text.as_str()), Some("hi"));
    assert!(sh.comment_at(a("A1")).is_some());
}

#[test]
fn paste_into_multiple_of_copy_size_tiles() {
    let mut s = Session::new();
    sets(&mut s, &[("A1", "1"), ("A2", "=A1*10")]);
    exec(&mut s, "home.clipboard.copy", json!({"range": "A1:A2"}));
    let before = s.wb.clone();
    exec(&mut s, "home.clipboard.paste", json!({"range": "C1:D4"}));
    assert_eq!(
        inputs(&mut s, &["C1", "C2", "C3", "C4", "D1", "D2", "D3", "D4"]),
        ["1", "=C1*10", "1", "=C3*10", "1", "=D1*10", "1", "=D3*10"]
    );
    one_undo_step(&mut s, &before);
    // A paste area that is not a multiple pastes the copy once, at its top-left.
    exec(&mut s, "home.clipboard.paste", json!({"range": "F1:F3"}));
    assert_eq!(inputs(&mut s, &["F1", "F2", "F3"]), ["1", "=F1*10", ""]);
    // A single copied cell fills the whole selection.
    exec(&mut s, "home.clipboard.copy", json!({"range": "A2"}));
    exec(&mut s, "select.range", json!({"range": "H2:I3"}));
    exec(&mut s, "home.clipboard.paste", json!({}));
    assert_eq!(
        inputs(&mut s, &["H2", "I2", "H3", "I3"]),
        ["=H1*10", "=I1*10", "=H2*10", "=I2*10"]
    );
}

#[test]
fn paste_special_operations() {
    let mut s = Session::new();
    sets(
        &mut s,
        &[
            ("A1", "1"),
            ("A2", "2"),
            ("C1", "10"),
            ("C2", "20"),
            ("C3", "30"),
            ("D1", "=5*2"),
            ("E1", "hello"),
        ],
    );
    exec(&mut s, "home.clipboard.copy", json!({"range": "A1:A3"}));
    let before = s.wb.clone();
    exec(
        &mut s,
        "home.clipboard.paste_special",
        json!({"cell": "C1", "mode": "values", "operation": "add"}),
    );
    assert_eq!(
        [num(&mut s, "C1"), num(&mut s, "C2"), num(&mut s, "C3")],
        [11.0, 22.0, 30.0]
    );
    one_undo_step(&mut s, &before);
    exec(&mut s, "edit.undo", json!({}));
    exec(
        &mut s,
        "home.clipboard.paste_special",
        json!({"cell": "C1", "mode": "values", "operation": "multiply"}),
    );
    // A blank source counts as 0.
    assert_eq!(
        [num(&mut s, "C1"), num(&mut s, "C2"), num(&mut s, "C3")],
        [10.0, 40.0, 0.0]
    );
    exec(
        &mut s,
        "home.clipboard.paste_special",
        json!({"cell": "C1", "mode": "values", "operation": "subtract", "skip_blanks": true}),
    );
    assert_eq!(
        [num(&mut s, "C1"), num(&mut s, "C2"), num(&mut s, "C3")],
        [9.0, 38.0, 0.0]
    );
    // An operation onto a formula wraps it.
    exec(&mut s, "home.clipboard.copy", json!({"range": "A2"}));
    exec(
        &mut s,
        "home.clipboard.paste_special",
        json!({"cell": "D1", "mode": "values", "operation": "divide"}),
    );
    assert_eq!(input(&mut s, "D1"), "=(5*2)/2");
    assert_eq!(num(&mut s, "D1"), 5.0);
    // Text in the destination is left alone.
    exec(
        &mut s,
        "home.clipboard.paste_special",
        json!({"cell": "E1", "mode": "values", "operation": "add"}),
    );
    assert_eq!(text(&mut s, "E1"), "hello");
    assert!(
        refused(
            &mut s,
            "home.clipboard.paste_special",
            json!({"mode": "values", "operation": "modulo"})
        )
        .contains("operation")
    );
}

#[test]
fn paste_special_skip_blanks_keeps_destination() {
    let mut s = Session::new();
    sets(
        &mut s,
        &[
            ("A1", "1"),
            ("A3", "3"),
            ("C1", "x"),
            ("C2", "y"),
            ("C3", "z"),
        ],
    );
    exec(&mut s, "home.clipboard.copy", json!({"range": "A1:A3"}));
    exec(
        &mut s,
        "home.clipboard.paste_special",
        json!({"cell": "C1", "mode": "all", "skip_blanks": true}),
    );
    assert_eq!(inputs(&mut s, &["C1", "C2", "C3"]), ["1", "y", "3"]);
    exec(&mut s, "home.clipboard.paste", json!({"cell": "E1"}));
    set(&mut s, "E2", "q");
    exec(&mut s, "home.clipboard.paste", json!({"cell": "E1"}));
    assert_eq!(input(&mut s, "E2"), "", "plain paste copies blanks over");
}

#[test]
fn paste_transpose_transposes_references() {
    let mut s = Session::new();
    sets(&mut s, &[("A1", "1"), ("B1", "=A1*2"), ("C1", "=$A$1+B1")]);
    exec(&mut s, "home.clipboard.copy", json!({"range": "A1:C1"}));
    let before = s.wb.clone();
    exec(
        &mut s,
        "home.clipboard.paste_transpose",
        json!({"cell": "E1"}),
    );
    assert_eq!(
        inputs(&mut s, &["E1", "E2", "E3"]),
        ["1", "=E1*2", "=$A$1+E2"]
    );
    assert_eq!(num(&mut s, "E3"), 3.0);
    assert_eq!(input(&mut s, "F1"), "");
    one_undo_step(&mut s, &before);
    // Transpose through Paste Special as a flag.
    exec(
        &mut s,
        "home.clipboard.paste_special",
        json!({"cell": "E1", "mode": "all", "transpose": true}),
    );
    assert_eq!(input(&mut s, "E2"), "=E1*2");
}

#[test]
fn paste_carries_merges_and_refuses_partial_merge_overlap() {
    let mut s = Session::new();
    set(&mut s, "A1", "title");
    exec(&mut s, "home.align.merge_cells", json!({"range": "A1:B1"}));
    exec(&mut s, "home.clipboard.copy", json!({"range": "A1:B2"}));
    exec(&mut s, "home.clipboard.paste", json!({"cell": "D1"}));
    assert!(s.wb.sheets[0].merges.contains(&r("D1:E1")));
    assert_eq!(text(&mut s, "D1"), "title");
    // Pasting a 1x1 into part of a merge is refused, nothing changes.
    set(&mut s, "H1", "z");
    exec(&mut s, "home.clipboard.copy", json!({"range": "H1"}));
    let before = s.wb.clone();
    let e = refused(&mut s, "home.clipboard.paste", json!({"cell": "E1"}));
    assert!(e.contains("merged"), "{e}");
    assert_eq!(s.wb, before);
    // Pasting a block that covers a merge completely replaces it.
    exec(&mut s, "home.clipboard.copy", json!({"range": "G1:H2"}));
    exec(&mut s, "home.clipboard.paste", json!({"cell": "D1"}));
    assert!(!s.wb.sheets[0].merges.contains(&r("D1:E1")));
    assert_eq!(text(&mut s, "E1"), "z");
}

#[test]
fn paste_carries_validation_and_paste_values_does_not() {
    let mut s = Session::new();
    exec(
        &mut s,
        "data.validation",
        json!({"range": "A1", "type": "list", "source": "a,b,c"}),
    );
    set(&mut s, "A1", "a");
    exec(
        &mut s,
        "data.validation",
        json!({"range": "C5", "type": "whole", "operator": "greaterThan", "min": 0}),
    );
    exec(&mut s, "home.clipboard.copy", json!({"range": "A1"}));
    exec(&mut s, "home.clipboard.paste", json!({"cell": "C1"}));
    assert!(s.wb.sheets[0].validation_at(a("C1")).is_some());
    exec(&mut s, "home.clipboard.paste_values", json!({"cell": "E1"}));
    assert!(s.wb.sheets[0].validation_at(a("E1")).is_none());
    assert_eq!(input(&mut s, "E1"), "a");
    // Pasting a plain cell over a validated one replaces the rule.
    set(&mut s, "G1", "5");
    exec(&mut s, "home.clipboard.copy", json!({"range": "G1"}));
    exec(&mut s, "home.clipboard.paste", json!({"cell": "C5"}));
    assert!(s.wb.sheets[0].validation_at(a("C5")).is_none());
    // Paste values keeps the destination's rule.
    exec(&mut s, "home.clipboard.paste_values", json!({"cell": "A1"}));
    assert!(s.wb.sheets[0].validation_at(a("A1")).is_some());
}

#[test]
fn paste_carries_conditional_formats() {
    let mut s = Session::new();
    sets(&mut s, &[("A1", "10"), ("A2", "1")]);
    exec(
        &mut s,
        "home.styles.cf.highlight_rules",
        json!({"range": "A1:A2", "rule": "greater_than", "value": 5}),
    );
    exec(&mut s, "home.clipboard.copy", json!({"range": "A1:A2"}));
    exec(&mut s, "home.clipboard.paste", json!({"cell": "C1"}));
    let covers = |s: &Session, x: &str| s.wb.sheets[0].cond_formats.iter().any(|c| c.covers(a(x)));
    assert!(covers(&s, "C1") && covers(&s, "C2"));
    exec(&mut s, "home.clipboard.paste_values", json!({"cell": "E1"}));
    assert!(!covers(&s, "E1"));
    exec(
        &mut s,
        "home.clipboard.paste_formats",
        json!({"cell": "G1"}),
    );
    assert!(covers(&s, "G1"));
    assert_eq!(input(&mut s, "G1"), "");
}

#[test]
fn paste_into_table_keeps_table() {
    let mut s = Session::new();
    sets(
        &mut s,
        &[
            ("A1", "Name"),
            ("B1", "Qty"),
            ("A2", "a"),
            ("B2", "1"),
            ("A3", "b"),
            ("B3", "2"),
            ("D1", "=SUM(Table1[Qty])"),
            ("F2", "10"),
            ("F3", "20"),
        ],
    );
    exec(
        &mut s,
        "insert.table",
        json!({"range": "A1:B3", "name": "Table1"}),
    );
    exec(&mut s, "home.clipboard.copy", json!({"range": "F2:F3"}));
    exec(&mut s, "home.clipboard.paste", json!({"cell": "B2"}));
    assert_eq!(s.wb.sheets[0].tables[0].range, r("A1:B3"));
    assert_eq!(num(&mut s, "D1"), 30.0);
}

#[test]
fn copy_of_filtered_range_copies_visible_rows_only() {
    let mut s = Session::new();
    sets(
        &mut s,
        &[
            ("A1", "Fruit"),
            ("A2", "apple"),
            ("A3", "pear"),
            ("A4", "plum"),
            ("A5", "apple"),
        ],
    );
    exec(
        &mut s,
        "data.filter",
        json!({"range": "A1:A5", "column": "A", "values": ["apple"]}),
    );
    assert!(s.wb.sheets[0].hidden_rows.contains(&2));
    exec(&mut s, "home.clipboard.copy", json!({"range": "A1:A5"}));
    exec(&mut s, "home.clipboard.paste", json!({"cell": "C1"}));
    assert_eq!(
        inputs(&mut s, &["C1", "C2", "C3", "C4"]),
        ["Fruit", "apple", "apple", ""]
    );
}

#[test]
fn copy_of_manually_hidden_rows_copies_everything() {
    let mut s = Session::new();
    sets(&mut s, &[("A1", "1"), ("A2", "2"), ("A3", "3")]);
    exec(&mut s, "home.cells.hide_rows", json!({"range": "A2"}));
    exec(&mut s, "home.clipboard.copy", json!({"range": "A1:A3"}));
    exec(&mut s, "home.clipboard.paste", json!({"cell": "C1"}));
    assert_eq!(inputs(&mut s, &["C1", "C2", "C3"]), ["1", "2", "3"]);
}

#[test]
fn paste_values_formulas_formats() {
    let mut s = Session::new();
    sets(&mut s, &[("A1", "2"), ("B1", "=A1*3"), ("D1", "old")]);
    exec(&mut s, "home.font.bold", json!({"range": "B1"}));
    exec(&mut s, "home.font.italic", json!({"range": "D1:F1"}));
    exec(&mut s, "home.clipboard.copy", json!({"range": "B1"}));
    exec(&mut s, "home.clipboard.paste_values", json!({"cell": "D1"}));
    assert_eq!(input(&mut s, "D1"), "6");
    let st = |s: &Session, x: &str| {
        s.wb.styles
            .get(
                s.wb.sheets[0]
                    .get(a(x))
                    .map(|c| c.style)
                    .unwrap_or_default(),
            )
            .clone()
    };
    assert!(st(&s, "D1").italic && !st(&s, "D1").bold);
    exec(
        &mut s,
        "home.clipboard.paste_formulas",
        json!({"cell": "E1"}),
    );
    assert_eq!(input(&mut s, "E1"), "=D1*3");
    assert!(st(&s, "E1").italic && !st(&s, "E1").bold);
    exec(
        &mut s,
        "home.clipboard.paste_formats",
        json!({"cell": "F1"}),
    );
    assert_eq!(input(&mut s, "F1"), "");
    assert!(st(&s, "F1").bold);
}

#[test]
fn paste_link_references_sources() {
    let mut s = Session::new();
    sets(&mut s, &[("A1", "1"), ("A2", "2")]);
    exec(&mut s, "home.clipboard.copy", json!({"range": "A1:A2"}));
    exec(&mut s, "home.clipboard.paste_link", json!({"cell": "C1"}));
    assert_eq!(inputs(&mut s, &["C1", "C2"]), ["=$A$1", "=$A$2"]);
}

// ---------------------------------------------------------------- fill

fn autofill(s: &mut Session, src: &str, dest: &str) {
    exec(s, "cell.autofill", json!({"source": src, "dest": dest}));
}

fn nums(s: &mut Session, cells: &[&str]) -> Vec<f64> {
    cells.iter().map(|c| num(s, c)).collect()
}

#[test]
fn fill_linear_series_and_single_number_copy() {
    let mut s = Session::new();
    sets(&mut s, &[("A1", "1"), ("A2", "2"), ("B1", "5")]);
    let before = s.wb.clone();
    autofill(&mut s, "A1:A2", "A1:A5");
    assert_eq!(nums(&mut s, &["A3", "A4", "A5"]), [3.0, 4.0, 5.0]);
    one_undo_step(&mut s, &before);
    autofill(&mut s, "B1", "B1:B3");
    assert_eq!(nums(&mut s, &["B2", "B3"]), [5.0, 5.0]);
}

#[test]
fn fill_ctrl_modifier_inverts_copy_and_series() {
    let mut s = Session::new();
    sets(&mut s, &[("A1", "5"), ("B1", "1"), ("B2", "3")]);
    exec(
        &mut s,
        "cell.autofill",
        json!({"source": "A1", "dest": "A1:A3", "ctrl": true}),
    );
    assert_eq!(nums(&mut s, &["A2", "A3"]), [6.0, 7.0]);
    exec(
        &mut s,
        "cell.autofill",
        json!({"source": "B1:B2", "dest": "B1:B4", "ctrl": true}),
    );
    assert_eq!(nums(&mut s, &["B3", "B4"]), [1.0, 3.0]);
}

#[test]
fn fill_dates_by_day_month_year() {
    let mut s = Session::new();
    // 2024-01-15 = 45306.
    sets(
        &mut s,
        &[
            ("A1", "2024-01-15"),
            ("B1", "2024-01-15"),
            ("B2", "2024-01-16"),
            ("C1", "2024-01-15"),
            ("C2", "2024-02-15"),
            ("D1", "2024-01-15"),
            ("D2", "2025-01-15"),
            ("E1", "2024-01-31"),
            ("E2", "2024-02-29"),
        ],
    );
    autofill(&mut s, "A1", "A1:A3");
    assert_eq!(
        nums(&mut s, &["A2", "A3"]),
        [45307.0, 45308.0],
        "single date counts days"
    );
    autofill(&mut s, "B1:B2", "B1:B4");
    assert_eq!(nums(&mut s, &["B3", "B4"]), [45308.0, 45309.0]);
    autofill(&mut s, "C1:C2", "C1:C4");
    // 2024-03-15 = 45366, 2024-04-15 = 45397.
    assert_eq!(nums(&mut s, &["C3", "C4"]), [45366.0, 45397.0]);
    autofill(&mut s, "D1:D2", "D1:D3");
    // 2026-01-15 = 46037.
    assert_eq!(num(&mut s, "D3"), 46037.0);
    // Month ends stay month ends: 2024-03-31 = 45382.
    autofill(&mut s, "E1:E2", "E1:E3");
    assert_eq!(num(&mut s, "E3"), 45382.0);
    // The fill keeps the date format.
    assert_eq!(text(&mut s, "C1"), "1/15/2024");
    assert_eq!(text(&mut s, "C3"), "3/15/2024");
}

#[test]
fn fill_weekday_and_month_names() {
    let mut s = Session::new();
    sets(
        &mut s,
        &[
            ("A1", "Mon"),
            ("B1", "Friday"),
            ("C1", "Jan"),
            ("D1", "November"),
            ("E1", "SUN"),
            ("F1", "Mon"),
            ("F2", "Wed"),
        ],
    );
    autofill(&mut s, "A1", "A1:A3");
    autofill(&mut s, "B1", "B1:B4");
    autofill(&mut s, "C1", "C1:C3");
    autofill(&mut s, "D1", "D1:D3");
    autofill(&mut s, "E1", "E1:E2");
    autofill(&mut s, "F1:F2", "F1:F4");
    assert_eq!(inputs(&mut s, &["A2", "A3"]), ["Tue", "Wed"]);
    assert_eq!(
        inputs(&mut s, &["B2", "B3", "B4"]),
        ["Saturday", "Sunday", "Monday"]
    );
    assert_eq!(inputs(&mut s, &["C2", "C3"]), ["Feb", "Mar"]);
    assert_eq!(inputs(&mut s, &["D2", "D3"]), ["December", "January"]);
    assert_eq!(input(&mut s, "E2"), "MON");
    assert_eq!(inputs(&mut s, &["F3", "F4"]), ["Fri", "Sun"]);
}

#[test]
fn fill_text_with_trailing_number() {
    let mut s = Session::new();
    sets(
        &mut s,
        &[
            ("A1", "Item 1"),
            ("B1", "Item 1"),
            ("B2", "Item 3"),
            ("C1", "abc"),
            ("D1", "Q1"),
        ],
    );
    autofill(&mut s, "A1", "A1:A3");
    assert_eq!(inputs(&mut s, &["A2", "A3"]), ["Item 2", "Item 3"]);
    autofill(&mut s, "B1:B2", "B1:B4");
    assert_eq!(inputs(&mut s, &["B3", "B4"]), ["Item 5", "Item 7"]);
    autofill(&mut s, "C1", "C1:C3");
    assert_eq!(inputs(&mut s, &["C2", "C3"]), ["abc", "abc"]);
    autofill(&mut s, "D1", "D1:D3");
    assert_eq!(inputs(&mut s, &["D2", "D3"]), ["Q2", "Q3"]);
}

#[test]
fn fill_formulas_shift_like_paste() {
    let mut s = Session::new();
    sets(
        &mut s,
        &[("A1", "1"), ("A2", "2"), ("A3", "3"), ("B1", "=A1*$A$1")],
    );
    autofill(&mut s, "B1", "B1:B3");
    assert_eq!(inputs(&mut s, &["B2", "B3"]), ["=A2*$A$1", "=A3*$A$1"]);
    assert_eq!(num(&mut s, "B3"), 3.0);
    exec(&mut s, "home.editing.fill_right", json!({"range": "B1:D1"}));
    assert_eq!(input(&mut s, "D1"), "=C1*$A$1");
    exec(&mut s, "home.editing.fill_down", json!({"range": "D1:D3"}));
    assert_eq!(input(&mut s, "D3"), "=C3*$A$1");
}

#[test]
fn fill_handle_double_click_fills_to_adjacent_data() {
    let mut s = Session::new();
    sets(
        &mut s,
        &[
            ("A1", "1"),
            ("A2", "2"),
            ("A3", "3"),
            ("A4", "4"),
            ("B1", "=A1*2"),
        ],
    );
    let before = s.wb.clone();
    exec(&mut s, "cell.autofill", json!({"source": "B1"}));
    assert_eq!(input(&mut s, "B4"), "=A4*2");
    assert_eq!(input(&mut s, "B5"), "");
    one_undo_step(&mut s, &before);
}

#[test]
fn fill_up_and_left_extend_backwards() {
    let mut s = Session::new();
    sets(&mut s, &[("A5", "5"), ("A6", "6"), ("D2", "Item 3")]);
    autofill(&mut s, "A5:A6", "A2:A6");
    assert_eq!(nums(&mut s, &["A2", "A3", "A4"]), [2.0, 3.0, 4.0]);
    autofill(&mut s, "D2", "B2:D2");
    assert_eq!(inputs(&mut s, &["B2", "C2"]), ["Item 1", "Item 2"]);
}

#[test]
fn fill_series_dialog_months() {
    let mut s = Session::new();
    set(&mut s, "A1", "2024-01-31");
    exec(
        &mut s,
        "home.editing.fill_series",
        json!({"range": "A1:A3", "type": "date", "date_unit": "month"}),
    );
    // 2024-02-29 = 45351, 2024-03-31 = 45382.
    assert_eq!(nums(&mut s, &["A2", "A3"]), [45351.0, 45382.0]);
}

// ---------------------------------------------------------------- insert / delete

#[test]
fn insert_rows_fixes_refs_across_sheets_names_and_edges() {
    let mut s = Session::new();
    sets(
        &mut s,
        &[
            ("A1", "1"),
            ("A5", "5"),
            ("C1", "=SUM(A2:A5)"),
            ("C2", "=SUM(A6:A8)"),
            ("C3", "=A5"),
            ("C4", "=SUM(A1:A4)"),
            ("C5", "=SUM(3:4)"),
        ],
    );
    exec(&mut s, "home.cells.insert_sheet", json!({"name": "Other"}));
    set(&mut s, "Other!A1", "=Sheet1!A5+SUM(Sheet1!A1:A5)");
    exec(
        &mut s,
        "formulas.define_name",
        json!({"name": "Block", "refers_to": "=Sheet1!$A$2:$A$5"}),
    );
    exec(&mut s, "nav.goto_cell", json!({"reference": "Sheet1!A1"}));
    let before = s.wb.clone();
    exec(
        &mut s,
        "home.cells.insert_rows",
        json!({"range": "Sheet1!A3:A4"}),
    );
    assert_eq!(
        input(&mut s, "Sheet1!C1"),
        "=SUM(A2:A7)",
        "insert inside grows"
    );
    assert_eq!(input(&mut s, "Sheet1!C2"), "=SUM(A8:A10)", "below moves");
    assert_eq!(input(&mut s, "Sheet1!C5"), "=A7");
    assert_eq!(input(&mut s, "Sheet1!C6"), "=SUM(A1:A6)");
    assert_eq!(
        input(&mut s, "Sheet1!C7"),
        "=SUM(5:6)",
        "insert at a range's first row moves it"
    );
    assert_eq!(input(&mut s, "Other!A1"), "=Sheet1!A7+SUM(Sheet1!A1:A7)");
    let n = s.wb.names.iter().find(|n| n.name == "Block").unwrap();
    assert_eq!(n.formula, "Sheet1!$A$2:$A$7");
    one_undo_step(&mut s, &before);
}

#[test]
fn insert_row_just_below_range_does_not_grow_it() {
    let mut s = Session::new();
    sets(&mut s, &[("A1", "1"), ("A2", "2"), ("C1", "=SUM(A1:A2)")]);
    exec(&mut s, "home.cells.insert_rows", json!({"range": "A3"}));
    assert_eq!(input(&mut s, "C1"), "=SUM(A1:A2)");
    exec(&mut s, "home.cells.insert_columns", json!({"range": "A1"}));
    assert_eq!(input(&mut s, "D1"), "=SUM(B1:B2)");
}

#[test]
fn delete_rows_ref_errors_and_shrinking_ranges() {
    let mut s = Session::new();
    sets(
        &mut s,
        &[
            ("A3", "3"),
            ("C1", "=A3"),
            ("C2", "=SUM(A1:A10)"),
            ("C3", "=SUM(A3:A4)"),
            ("D1", "=A12"),
        ],
    );
    exec(
        &mut s,
        "formulas.define_name",
        json!({"name": "Gone", "refers_to": "=Sheet1!$A$3"}),
    );
    let before = s.wb.clone();
    exec(&mut s, "home.cells.delete_rows", json!({"range": "A3:A4"}));
    assert_eq!(input(&mut s, "C1"), "=#REF!");
    assert_eq!(text(&mut s, "C1"), "#REF!");
    assert_eq!(input(&mut s, "C2"), "=SUM(A1:A8)");
    assert_eq!(input(&mut s, "D1"), "=A10");
    // C3 itself was deleted; the old C5 moved up.
    let n = s.wb.names.iter().find(|n| n.name == "Gone").unwrap();
    assert!(n.formula.contains("#REF!"), "{}", n.formula);
    one_undo_step(&mut s, &before);
    assert_eq!(input(&mut s, "C1"), "=#REF!");
}

#[test]
fn delete_columns_shrinks_range_at_its_end() {
    let mut s = Session::new();
    sets(
        &mut s,
        &[("A1", "1"), ("B1", "2"), ("C1", "3"), ("F3", "=SUM(A1:C1)")],
    );
    exec(&mut s, "home.cells.delete_columns", json!({"range": "C1"}));
    assert_eq!(input(&mut s, "E3"), "=SUM(A1:B1)");
    assert_eq!(num(&mut s, "E3"), 3.0);
    exec(&mut s, "home.cells.delete_columns", json!({"range": "A1"}));
    assert_eq!(input(&mut s, "D3"), "=SUM(A1:A1)");
    assert_eq!(num(&mut s, "D3"), 2.0);
}

#[test]
fn insert_delete_moves_cf_validation_tables_and_print_area() {
    let mut s = Session::new();
    sets(
        &mut s,
        &[
            ("A1", "Name"),
            ("B1", "Qty"),
            ("A2", "a"),
            ("B2", "1"),
            ("A3", "b"),
            ("B3", "2"),
            ("A4", "c"),
            ("B4", "3"),
        ],
    );
    exec(
        &mut s,
        "insert.table",
        json!({"range": "A1:B4", "name": "T"}),
    );
    exec(
        &mut s,
        "home.styles.cf.highlight_rules",
        json!({"range": "D2:D6", "rule": "greater_than", "value": 1}),
    );
    exec(
        &mut s,
        "data.validation",
        json!({"range": "E2:E6", "type": "whole", "operator": "greaterThan", "min": 0}),
    );
    exec(&mut s, "layout.print_area.set", json!({"range": "A1:E6"}));
    set(&mut s, "G1", "=SUM(T[Qty])");
    let before = s.wb.clone();
    exec(&mut s, "home.cells.insert_rows", json!({"range": "A3"}));
    let sh = &s.wb.sheets[0];
    assert_eq!(sh.tables[0].range, r("A1:B5"));
    assert_eq!(sh.cond_formats[0].ranges, vec![r("D2:D7")]);
    assert_eq!(sh.validations[0].ranges, vec![r("E2:E7")]);
    let pa =
        s.wb.names
            .iter()
            .find(|n| n.name.eq_ignore_ascii_case("_xlnm.Print_Area"))
            .unwrap();
    assert_eq!(pa.formula, "Sheet1!$A$1:$E$7");
    one_undo_step(&mut s, &before);
    exec(&mut s, "edit.undo", json!({}));
    exec(&mut s, "home.cells.delete_rows", json!({"range": "A2:A3"}));
    let sh = &s.wb.sheets[0];
    assert_eq!(sh.tables[0].range, r("A1:B2"));
    assert_eq!(sh.cond_formats[0].ranges, vec![r("D2:D4")]);
    assert_eq!(sh.validations[0].ranges, vec![r("E2:E4")]);
    assert_eq!(num(&mut s, "G1"), 3.0);
}

#[test]
fn insert_delete_moves_charts_pivots_and_objects() {
    let mut s = Session::new();
    sets(
        &mut s,
        &[
            ("A1", "Region"),
            ("B1", "Sales"),
            ("A2", "N"),
            ("B2", "10"),
            ("A3", "S"),
            ("B3", "20"),
        ],
    );
    exec(&mut s, "insert.chart.column", json!({"range": "A1:B3"}));
    exec(
        &mut s,
        "insert.pivot_table",
        json!({"source": "A1:B3", "dest": "Sheet1!H1", "rows": ["Region"], "values": [{"field": "Sales"}]}),
    );
    exec(&mut s, "insert.shapes", json!({"cell": "D10"}));
    let chart_before = s.wb.sheets[0].drawings.charts[0].series[0].values.clone();
    let obj_row = s.wb.sheets[0].objects[0].anchor.cells.from.row;
    let before = s.wb.clone();
    exec(&mut s, "home.cells.insert_rows", json!({"range": "A1"}));
    let sh = &s.wb.sheets[0];
    let vals = &sh.drawings.charts[0].series[0].values;
    assert_ne!(vals, &chart_before);
    assert!(vals.contains("$B$3:$B$4"), "{vals}");
    assert!(
        sh.analysis.pivots[0].source.contains("$A$2:$B$4")
            || sh.analysis.pivots[0].source.contains("A2:B4"),
        "{}",
        sh.analysis.pivots[0].source
    );
    assert_eq!(sh.objects[0].anchor.cells.from.row, obj_row + 1);
    one_undo_step(&mut s, &before);
}

#[test]
fn insert_rows_inside_merge_grows_it_delete_shrinks_it() {
    let mut s = Session::new();
    set(&mut s, "A1", "m");
    exec(&mut s, "home.align.merge_cells", json!({"range": "A1:A3"}));
    exec(&mut s, "home.cells.insert_rows", json!({"range": "A2"}));
    assert_eq!(s.wb.sheets[0].merges, vec![r("A1:A4")]);
    exec(&mut s, "home.cells.delete_rows", json!({"range": "A2:A3"}));
    assert_eq!(s.wb.sheets[0].merges, vec![r("A1:A2")]);
}

// ---------------------------------------------------------------- find / replace

fn find_all(s: &mut Session, p: Json) -> Vec<String> {
    exec(s, "edit.find_all", p)
        .as_array()
        .unwrap()
        .iter()
        .map(|h| {
            format!(
                "{}!{}",
                h["sheet"].as_str().unwrap(),
                h["cell"].as_str().unwrap()
            )
        })
        .collect()
}

fn find_session() -> Session {
    let mut s = Session::new();
    sets(
        &mut s,
        &[
            ("A1", "apple"),
            ("A2", "Apple pie"),
            ("B1", "pineapple"),
            ("B2", "a*b"),
            ("C1", "=1+1"),
            ("C2", "1234.5"),
        ],
    );
    exec(&mut s, "home.number.currency", json!({"range": "C2"}));
    exec(&mut s, "home.cells.insert_sheet", json!({"name": "S2"}));
    set(&mut s, "S2!A1", "apple");
    exec(&mut s, "nav.goto_cell", json!({"reference": "Sheet1!A1"}));
    s
}

#[test]
fn find_match_case_and_whole_cell() {
    let mut s = find_session();
    assert_eq!(
        find_all(&mut s, json!({"what": "apple"})),
        ["Sheet1!A1", "Sheet1!B1", "Sheet1!A2"]
    );
    assert_eq!(
        find_all(&mut s, json!({"what": "Apple", "match_case": true})),
        ["Sheet1!A2"]
    );
    assert_eq!(
        find_all(&mut s, json!({"what": "APPLE", "whole": true})),
        ["Sheet1!A1"]
    );
}

#[test]
fn find_wildcards() {
    let mut s = find_session();
    assert_eq!(
        find_all(&mut s, json!({"what": "p*e", "whole": true})),
        ["Sheet1!B1"]
    );
    assert_eq!(
        find_all(&mut s, json!({"what": "?pple", "whole": true})),
        ["Sheet1!A1"]
    );
    assert_eq!(find_all(&mut s, json!({"what": "a~*b"})), ["Sheet1!B2"]);
    assert_eq!(
        find_all(&mut s, json!({"what": "~?"})),
        Vec::<String>::new()
    );
    assert_eq!(
        find_all(&mut s, json!({"what": "a*e"})),
        ["Sheet1!A1", "Sheet1!B1", "Sheet1!A2"]
    );
}

#[test]
fn find_look_in_formulas_vs_values() {
    let mut s = find_session();
    assert_eq!(
        find_all(&mut s, json!({"what": "1+1", "look_in": "formulas"})),
        ["Sheet1!C1"]
    );
    assert!(find_all(&mut s, json!({"what": "1+1", "look_in": "values"})).is_empty());
    assert_eq!(
        find_all(
            &mut s,
            json!({"what": "2", "look_in": "values", "whole": true})
        ),
        ["Sheet1!C1"]
    );
    // Values are searched as displayed.
    assert_eq!(
        find_all(&mut s, json!({"what": "1,234", "look_in": "values"})),
        ["Sheet1!C2"]
    );
    assert!(find_all(&mut s, json!({"what": "1,234", "look_in": "formulas"})).is_empty());
}

#[test]
fn find_within_sheet_or_workbook() {
    let mut s = find_session();
    assert_eq!(
        find_all(
            &mut s,
            json!({"what": "apple", "whole": true, "within": "workbook"})
        ),
        ["S2!A1", "Sheet1!A1"],
        "workbook search lists hits in sheet order (S2 was inserted before Sheet1)"
    );
    assert_eq!(
        find_all(&mut s, json!({"what": "apple", "whole": true})),
        ["Sheet1!A1"],
        "default searches the active sheet"
    );
    // Find next stays on the sheet by default and cycles.
    exec(&mut s, "nav.goto_cell", json!({"reference": "A1"}));
    let r = exec(
        &mut s,
        "home.editing.find",
        json!({"what": "apple", "whole": true}),
    );
    assert_eq!(r["cell"], "A1");
    assert_eq!(s.wb.active, 0);
    let r = exec(
        &mut s,
        "home.editing.find",
        json!({"what": "apple", "whole": true, "within": "workbook"}),
    );
    assert_eq!(
        (r["sheet"].as_str(), r["cell"].as_str()),
        (Some("S2"), Some("A1"))
    );
}

#[test]
fn find_by_rows_or_columns() {
    let mut s = find_session();
    assert_eq!(
        find_all(&mut s, json!({"what": "apple", "search": "columns"})),
        ["Sheet1!A1", "Sheet1!A2", "Sheet1!B1"]
    );
    exec(&mut s, "nav.goto_cell", json!({"reference": "A1"}));
    let r = exec(
        &mut s,
        "home.editing.find",
        json!({"what": "apple", "search": "columns"}),
    );
    assert_eq!(r["cell"], "A2");
    let r = exec(&mut s, "home.editing.find", json!({"what": "apple"}));
    assert_eq!(
        r["cell"], "B1",
        "by rows from A2... wraps to next row-major hit"
    );
}

#[test]
fn replace_inside_formulas_keeps_them_valid() {
    let mut s = Session::new();
    sets(
        &mut s,
        &[
            ("A1", "1"),
            ("A2", "2"),
            ("A3", "4"),
            ("B1", "=SUM(A1:A2)"),
            ("B2", "=A1+A2"),
        ],
    );
    let before = s.wb.clone();
    let n = exec(
        &mut s,
        "home.editing.replace",
        json!({"what": "A2", "with": "A3"}),
    );
    assert_eq!(n, 2);
    assert_eq!(input(&mut s, "B1"), "=SUM(A1:A3)");
    assert_eq!(num(&mut s, "B1"), 7.0);
    assert_eq!(num(&mut s, "B2"), 5.0);
    one_undo_step(&mut s, &before);
    // A replacement that would break a formula is skipped, not applied.
    let r = exec(
        &mut s,
        "home.editing.replace",
        json!({"what": ")", "with": ""}),
    );
    assert_eq!(r, 0);
    assert_eq!(input(&mut s, "B1"), "=SUM(A1:A2)");
}

#[test]
fn replace_whole_cell_wildcards_and_case() {
    let mut s = Session::new();
    sets(
        &mut s,
        &[
            ("A1", "cat hat cut"),
            ("A2", "cat"),
            ("A3", "Cat"),
            ("A4", "STRASSE straße"),
        ],
    );
    exec(
        &mut s,
        "home.editing.replace",
        json!({"what": "cat", "with": "dog", "whole": true, "match_case": true}),
    );
    assert_eq!(
        inputs(&mut s, &["A1", "A2", "A3"]),
        ["cat hat cut", "dog", "Cat"]
    );
    exec(
        &mut s,
        "home.editing.replace",
        json!({"what": "c?t", "with": "X"}),
    );
    assert_eq!(inputs(&mut s, &["A1", "A3"]), ["X hat X", "X"]);
    // Case-insensitive replace with non-ASCII text does not corrupt it.
    exec(
        &mut s,
        "home.editing.replace",
        json!({"what": "strasse", "with": "road"}),
    );
    assert_eq!(input(&mut s, "A4"), "road straße");
}

#[test]
fn replace_within_sheet_by_default() {
    let mut s = find_session();
    exec(
        &mut s,
        "home.editing.replace",
        json!({"what": "apple", "with": "kiwi"}),
    );
    assert_eq!(input(&mut s, "S2!A1"), "apple");
    assert_eq!(input(&mut s, "Sheet1!B1"), "pinekiwi");
    exec(
        &mut s,
        "home.editing.replace",
        json!({"what": "apple", "with": "kiwi", "within": "workbook"}),
    );
    assert_eq!(input(&mut s, "S2!A1"), "kiwi");
}

#[test]
fn replace_current_honours_wildcards() {
    let mut s = Session::new();
    sets(&mut s, &[("A1", "cat"), ("A3", "cot")]);
    exec(&mut s, "nav.goto_cell", json!({"reference": "A1"}));
    let r = exec(
        &mut s,
        "edit.replace_current",
        json!({"what": "c?t", "with": "dog"}),
    );
    assert_eq!(r["replaced"], true);
    assert_eq!(input(&mut s, "A1"), "dog");
    assert_eq!(r["next"]["cell"], "A3");
}

// ---------------------------------------------------------------- undo granularity

#[test]
fn each_edit_is_one_undo_step() {
    let mut s = Session::new();
    sets(&mut s, &[("A1", "1"), ("A2", "2"), ("B1", "=A1")]);
    for (id, p) in [
        ("home.editing.fill_down", json!({"range": "B1:B4"})),
        ("cell.autofill", json!({"source": "A1:A2", "dest": "A1:A6"})),
        ("home.cells.insert_rows", json!({"range": "A2"})),
        ("home.cells.delete_columns", json!({"range": "C1"})),
        ("home.editing.replace", json!({"what": "A", "with": "A"})),
        ("home.editing.fill_series", json!({"range": "A1:A8"})),
    ] {
        let before = s.wb.clone();
        let depth = s.history.summary().len();
        exec(&mut s, id, p);
        let _ = depth;
        one_undo_step(&mut s, &before);
    }
}
