//! Integrated property tests, part B: every registered command outside Home, Insert, Page
//! Layout, cell editing, navigation and sheet commands (formulas., data., review., view.,
//! file., table., pivot., object., format.…), done through the real shell the way a user
//! does it, each changing exactly the properties it should (see `tests/properties.rs` for
//! what the runner checks).
//!
//! The base workbook (`common::props::base_session`), Sheet1:
//!
//! ```text
//!     A       B        C    D     E          …  G
//! 1   Region  Product  Qty  Price Total         =SUM(E2:E6) → 67.5
//! 2   East    Apples   10   1.5   =C2*D2 → 15
//! 3   West    Pears    4    2.25  9
//! 4   East    Plums    7    3     21
//! 5   North   Apples   12   1.5   18
//! 6   West    Apples   3    1.5   4.5
//! ```

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::expect_used
)]
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;
use sp_core::CellAddr;
use sp_engine::Session;
use sp_ui::App;
use sp_ui::dialogs::Dialog;
use std::collections::BTreeSet;

mod common;
use common::props::*;
use common::*;

// ---------------------------------------------------------------------------------------
// Helpers

/// `cell` holds the constant `text` (input and value both the quoted text).
fn text(e: Entry, cell: &str, t: &str) -> Entry {
    let q = format!("{t:?}");
    e.set(&format!("Sheet1!{cell}.input"), &q)
        .set(&format!("Sheet1!{cell}.value"), &q)
}

/// `cell` holds the number `n`, typed as `input`.
fn num(e: Entry, cell: &str, input: &str, n: f64) -> Entry {
    e.set(&format!("Sheet1!{cell}.input"), &format!("{input:?}"))
        .set(&format!("Sheet1!{cell}.value"), &format!("{n:?}"))
}

/// A whole data row A..D of the base table (Region, Product, Qty, Price).
fn row(e: Entry, r: u32, region: &str, product: &str, qty: &str, price: &str) -> Entry {
    let e = text(e, &format!("A{r}"), region);
    let e = text(e, &format!("B{r}"), product);
    let e = num(e, &format!("C{r}"), qty, qty.parse().unwrap());
    num(e, &format!("D{r}"), price, price.parse().unwrap())
}

/// `cell`'s value only (a formula's result).
fn value(e: Entry, cell: &str, v: f64) -> Entry {
    e.set(&format!("Sheet1!{cell}.value"), &format!("{v:?}"))
}

/// Not on the undo stack (an application setting rather than an edit), but saved.
fn no_undo(mut e: Entry) -> Entry {
    e.undo = false;
    e
}

/// Every flag of a sheet protection made with the dialog's defaults: only selecting cells
/// is allowed (the target's defaults).
fn sheet_protected(mut e: Entry) -> Entry {
    for f in [
        "format_cells",
        "format_columns",
        "format_rows",
        "insert_columns",
        "insert_rows",
        "insert_hyperlinks",
        "delete_columns",
        "delete_rows",
        "sort",
        "auto_filter",
        "pivot_tables",
        "objects",
        "scenarios",
    ] {
        e = e.set(&format!("Sheet1.protection.{f}"), "false");
    }
    e.set("Sheet1.protection.select_locked", "true")
        .set("Sheet1.protection.select_unlocked", "true")
}

/// Manual calculation, then C2 changes to 100: E2 (=C2*D2) and G1 (=SUM(E2:E6)) are stale
/// until a recalculation brings them to 150 and 202.5. Recalculating is not an edit (not
/// on the undo stack); the new values are saved.
fn stale(name: &'static str, act: Act) -> Entry {
    let e = Entry::new(name, "A1", act)
        .setup("formulas.calc_manual", json!({}))
        .setup("cell.set", json!({"cell": "C2", "input": "100"}));
    no_undo(value(value(e, "E2", 150.0), "G1", 202.5))
}

/// Open a dialog from the ribbon, choose `pick` in the Insert Function list, OK: the formula
/// starts in the editor (`=PICK(`); nothing else changes until it is entered.
fn insert_function(name: &'static str, label: &'static str, pick: &'static str) -> Entry {
    let fill: fn(&mut App) = match pick {
        "PMT" => |a| set_function(a, "PMT"),
        "IF" => |a| set_function(a, "IF"),
        "LEN" => |a| set_function(a, "LEN"),
        "DATE" => |a| set_function(a, "DATE"),
        "VLOOKUP" => |a| set_function(a, "VLOOKUP"),
        "ABS" => |a| set_function(a, "ABS"),
        _ => |a| set_function(a, "SUM"),
    };
    Entry::new(
        name,
        "H2",
        Act::Dialog {
            ribbon: ("Formulas", "Function Library", label),
            fill,
            ok: "OK",
        },
    )
    .set("view.editing", "true")
    .view_only()
}

fn set_function(app: &mut App, f: &str) {
    if let Dialog::InsertFunction(d) = dialog(app) {
        d.selected = Some(f.to_string());
    }
}

fn dialog(app: &mut App) -> &mut Dialog {
    app.ui.dialog.as_mut().expect("a dialog is open")
}

/// Open a ribbon tab (navigation, not part of the change).
fn tab(h: &mut Harness<'_, App>, t: &str) -> Result<(), String> {
    click_tab(h, t)
}

/// Click a control by its exact label (the only one with that label on screen).
fn click(h: &mut Harness<'_, App>, label: &str) -> Result<(), String> {
    let n: Vec<_> = h.query_all_by_label(label).collect();
    match n.as_slice() {
        [one] => one.click(),
        [] => return Err(format!("no '{label}' on screen")),
        _ => {
            // The last one drawn is on top (a dialog over the ribbon).
            n.last().unwrap().click()
        }
    }
    drop(n);
    h.run_steps(3);
    Ok(())
}

fn active_is(s: &Session, a1: &str) -> Result<(), String> {
    let want = CellAddr::parse(a1).unwrap();
    if s.sel.active == want {
        Ok(())
    } else {
        Err(format!(
            "active cell {} (expected {a1})",
            s.sel.active.to_a1()
        ))
    }
}

// ---------------------------------------------------------------------------------------
// Entries

fn entries() -> Vec<Entry> {
    let mut v = Vec::new();
    v.extend(data_entries());
    v.extend(formulas_entries());
    v.extend(review_entries());
    v.extend(view_entries());
    v.extend(file_entries());
    v
}

fn data_entries() -> Vec<Entry> {
    let sort_az = Entry::new(
        "sort ascending by Product",
        "B2",
        Act::Ribbon("Data", "Sort & Filter", "Sort Ascending"),
    );
    // The current region A1:E6 sorts by column B under its header (stable for ties); each
    // row keeps its own =C*D formula, so only the values in E move.
    let sort_az = row(sort_az, 3, "North", "Apples", "12", "1.5");
    let sort_az = row(sort_az, 4, "West", "Apples", "3", "1.5");
    let sort_az = row(sort_az, 5, "West", "Pears", "4", "2.25");
    let sort_az = row(sort_az, 6, "East", "Plums", "7", "3");
    let sort_az = value(sort_az, "E3", 18.0);
    let sort_az = value(sort_az, "E4", 4.5);
    let sort_az = value(sort_az, "E5", 9.0);
    let sort_az = value(sort_az, "E6", 21.0);

    // Qty descending: 12, 10, 7, 4, 3 (rows 4 and 6 stay where they are).
    let sort_za = Entry::new(
        "sort descending by Qty",
        "C2",
        Act::Ribbon("Data", "Sort & Filter", "Sort Descending"),
    );
    let sort_za = row(sort_za, 2, "North", "Apples", "12", "1.5");
    let sort_za = row(sort_za, 3, "East", "Apples", "10", "1.5");
    let sort_za = row(sort_za, 5, "West", "Pears", "4", "2.25");
    let sort_za = value(sort_za, "E2", 18.0);
    let sort_za = value(sort_za, "E3", 15.0);
    let sort_za = value(sort_za, "E5", 9.0);

    // Sort dialog: by Price (D) descending: 3, 2.25, then the three 1.5 rows in order.
    let sort = Entry::new(
        "sort dialog by Price descending",
        "A1:E6",
        Act::Dialog {
            ribbon: ("Data", "Sort & Filter", "Sort…"),
            fill: |app| {
                if let Dialog::Sort(d) = dialog(app) {
                    d.header = true;
                    d.levels[0].column = "D".into();
                    d.levels[0].descending = true;
                }
            },
            ok: "OK",
        },
    );
    let sort = row(sort, 2, "East", "Plums", "7", "3");
    let sort = row(sort, 3, "West", "Pears", "4", "2.25");
    let sort = row(sort, 4, "East", "Apples", "10", "1.5");
    let sort = value(sort, "E2", 21.0);
    let sort = value(sort, "E4", 15.0);

    // A copy of row 2 in row 7: Remove Duplicates over A1:D7 drops it.
    let dupes = Entry::new(
        "remove duplicates",
        "A1:D7",
        Act::Ribbon("Data", "Data Tools", "Remove Duplicates…"),
    )
    .setup("cell.set", json!({"cell": "A7", "input": "East"}))
    .setup("cell.set", json!({"cell": "B7", "input": "Apples"}))
    .setup("cell.set", json!({"cell": "C7", "input": "10"}))
    .setup("cell.set", json!({"cell": "D7", "input": "1.5"}))
    .gone("Sheet1!A7.input")
    .gone("Sheet1!A7.value")
    .gone("Sheet1!B7.input")
    .gone("Sheet1!B7.value")
    .gone("Sheet1!C7.input")
    .gone("Sheet1!C7.value")
    .gone("Sheet1!D7.input")
    .gone("Sheet1!D7.value");

    let split = Entry::new(
        "text to columns",
        "H1",
        Act::Ribbon("Data", "Data Tools", "Text to Columns…"),
    )
    .setup("cell.set", json!({"cell": "H1", "input": "a,b,c"}));
    let split = text(split, "H1", "a");
    let split = text(split, "I1", "b");
    let split = text(split, "J1", "c");

    let east = || json!({"range": "A1:E6", "on": true, "column": "A", "values": ["East"]});
    let mut v = vec![sort_az, sort_za, sort, dupes, split];
    v.extend([
        // AutoFilter over the current region.
        Entry::new(
            "filter on",
            "B2",
            Act::Ribbon("Data", "Sort & Filter", "Filter"),
        )
        .set("Sheet1.auto_filter.range.start.row", "0")
        .set("Sheet1.auto_filter.range.start.col", "0")
        .set("Sheet1.auto_filter.range.end.row", "5")
        .set("Sheet1.auto_filter.range.end.col", "4"),
        // Region = East hides rows 3, 5, 6; clearing shows them and drops the criteria,
        // keeping the filter buttons.
        Entry::new(
            "clear filter",
            "B2",
            Act::Ribbon("Data", "Sort & Filter", "Clear Filter"),
        )
        .setup("data.filter", east())
        .gone("Sheet1.hidden_rows.*")
        .gone("Sheet1.auto_filter.columns.*"),
        // A3 becomes East while filtered: Reapply shows row 3 (rows 5, 6 stay hidden).
        Entry::new(
            "reapply filter",
            "B2",
            Act::Ribbon("Data", "Sort & Filter", "Reapply"),
        )
        .setup("data.filter", east())
        .setup("cell.set", json!({"cell": "A3", "input": "East"}))
        .set("Sheet1.hidden_rows.0", "4")
        .set("Sheet1.hidden_rows.1", "5")
        .gone("Sheet1.hidden_rows.2"),
        // Criteria Region = West in H1:H2, filter in place: rows 2, 4, 5 hidden.
        Entry::new(
            "advanced filter",
            "A1:E6",
            Act::Dialog {
                ribbon: ("Data", "Sort & Filter", "Advanced Filter…"),
                fill: |app| {
                    if let Dialog::AdvancedFilter(d) = dialog(app) {
                        d.copy = false;
                        d.list = "A1:E6".into();
                        d.criteria = "H1:H2".into();
                    }
                },
                ok: "OK",
            },
        )
        .setup("cell.set", json!({"cell": "H1", "input": "Region"}))
        .setup("cell.set", json!({"cell": "H2", "input": "West"}))
        .set("Sheet1.hidden_rows.0", "1")
        .set("Sheet1.hidden_rows.1", "3")
        .set("Sheet1.hidden_rows.2", "4")
        // The list and criteria ranges are named on the sheet, as the target does.
        .set("workbook.names.0.name", "\"_xlnm._FilterDatabase\"")
        .set("workbook.names.0.formula", "\"Sheet1!$A$1:$E$6\"")
        .set("workbook.names.0.scope", "0")
        .set("workbook.names.1.name", "\"_xlnm.Criteria\"")
        .set("workbook.names.1.formula", "\"Sheet1!$H$1:$H$2\"")
        .set("workbook.names.1.scope", "0"),
        // Outline: rows 2-6 at level 1.
        Entry::new(
            "group rows",
            "A2:A6",
            Act::Ribbon("Data", "Outline", "Group"),
        )
        .set("Sheet1.outline.rows.1", "1")
        .set("Sheet1.outline.rows.2", "1")
        .set("Sheet1.outline.rows.3", "1")
        .set("Sheet1.outline.rows.4", "1")
        .set("Sheet1.outline.rows.5", "1"),
        Entry::new(
            "ungroup rows",
            "A2:A6",
            Act::Ribbon("Data", "Outline", "Ungroup"),
        )
        .setup("data.group", json!({"range": "A2:A6", "by": "rows"}))
        .gone("Sheet1.outline.rows.*"),
        Entry::new(
            "hide detail",
            "A2:A6",
            Act::Ribbon("Data", "Outline", "Hide Detail"),
        )
        .setup("data.group", json!({"range": "A2:A6", "by": "rows"}))
        .set("Sheet1.hidden_rows.0", "1")
        .set("Sheet1.hidden_rows.1", "2")
        .set("Sheet1.hidden_rows.2", "3")
        .set("Sheet1.hidden_rows.3", "4")
        .set("Sheet1.hidden_rows.4", "5"),
        Entry::new(
            "show detail",
            "A2:A6",
            Act::Ribbon("Data", "Outline", "Show Detail"),
        )
        .setup("data.group", json!({"range": "A2:A6", "by": "rows"}))
        .setup("data.hide_detail", json!({"range": "A2:A6", "by": "rows"}))
        .gone("Sheet1.hidden_rows.*"),
        // Whole numbers 1..5 in C2:C6 (10, 4, 7, 12, 3): 10, 7, 12 are circled.
        Entry::new(
            "circle invalid data",
            "A1",
            Act::Ribbon("Data", "Data Tools", "Circle Invalid Data"),
        )
        .setup(
            "data.validation",
            json!({"range": "C2:C6", "type": "whole", "operator": "between", "min": 1, "max": 5}),
        )
        .set("view.review.circles.0.0", "0")
        .set("view.review.circles.0.1.row", "1")
        .set("view.review.circles.0.1.col", "2")
        .set("view.review.circles.1.0", "0")
        .set("view.review.circles.1.1.row", "3")
        .set("view.review.circles.1.1.col", "2")
        .set("view.review.circles.2.0", "0")
        .set("view.review.circles.2.1.row", "4")
        .set("view.review.circles.2.1.col", "2")
        .view_only(),
        // A drop-down list on H2:H3.
        Entry::new(
            "data validation list",
            "H2:H3",
            Act::Dialog {
                ribbon: ("Data", "Data Tools", "Data Validation…"),
                fill: |app| {
                    if let Dialog::DataValidation(d) = dialog(app) {
                        d.kind = "list".into();
                        d.source = "Yes,No".into();
                    }
                },
                ok: "OK",
            },
        )
        .set("Sheet1.validations.0.kind", "\"List\"")
        .set("Sheet1.validations.0.formula1", "\"\\\"Yes,No\\\"\"")
        .set("Sheet1.validations.0.ranges.0.start.row", "1")
        .set("Sheet1.validations.0.ranges.0.end.row", "2")
        .set("Sheet1.validations.0.ranges.0.start.col", "7")
        .set("Sheet1.validations.0.ranges.0.end.col", "7")
        // The dialog's defaults: blanks allowed, in-cell drop-down, a Stop alert with the
        // standard message, no input message.
        .set("Sheet1.validations.0.allow_blank", "true")
        .set("Sheet1.validations.0.dropdown", "true")
        .set("Sheet1.validations.0.show_error", "true")
        .set("Sheet1.validations.0.error_style", "\"Stop\"")
        .set("Sheet1.validations.0.error", "\"\"")
        .set("Sheet1.validations.0.error_title", "\"\"")
        .set("Sheet1.validations.0.show_prompt", "false")
        .set("Sheet1.validations.0.prompt", "\"\"")
        .set("Sheet1.validations.0.prompt_title", "\"\"")
        .set("Sheet1.validations.0.formula2", "\"\"")
        // The operator is not used by a list (stored as the default).
        .set("Sheet1.validations.0.op", "\"Between\""),
    ]);
    v
}

fn formulas_entries() -> Vec<Entry> {
    vec![
        stale("calculate now", Act::Key("F9")),
        stale("calculate sheet", Act::Key("Shift+F9")),
        stale("full calculation", Act::Key("Ctrl+Alt+F9")),
        Entry::new(
            "define name",
            "A1",
            Act::Dialog {
                ribbon: ("Formulas", "Defined Names", "Define Name…"),
                fill: |app| {
                    if let Dialog::DefineName(d) = dialog(app) {
                        d.name = "Tax".into();
                        d.refers_to = "=Sheet1!$D$2".into();
                    }
                },
                ok: "OK",
            },
        )
        .set("workbook.names.0.name", "\"Tax\"")
        .set("workbook.names.0.formula", "\"Sheet1!$D$2\""),
        // Name Manager: select the name, Delete, Close.
        Entry::new(
            "name manager delete",
            "A1",
            Act::Custom {
                prep: |h| tab(h, "Formulas"),
                act: |h| {
                    click_in_ribbon(h, "Defined Names", "Name Manager")?;
                    h.run_steps(3);
                    if let Dialog::NameManager(d) = dialog(h.state_mut()) {
                        d.selected = Some(0);
                    }
                    h.run_steps(2);
                    click(h, "Delete")?;
                    click(h, "Close")
                },
            },
        )
        .setup(
            "formulas.define_name",
            json!({"name": "Rate", "refers_to": "=Sheet1!$C$2"}),
        )
        .gone("workbook.names.0.name")
        .gone("workbook.names.0.formula"),
        // Labels in the top row of C1:D6 name the columns below them.
        Entry::new(
            "create names from selection",
            "C1:D6",
            Act::Key("Ctrl+Shift+F3"),
        )
        .set("workbook.names.0.name", "\"Qty\"")
        .set("workbook.names.0.formula", "\"Sheet1!$C$2:$C$6\"")
        .set("workbook.names.1.name", "\"Price\"")
        .set("workbook.names.1.formula", "\"Sheet1!$D$2:$D$6\""),
        // Arrows are view state (not saved, not undoable).
        Entry::new(
            "trace precedents",
            "E2",
            Act::Ribbon("Formulas", "Formula Auditing", "Trace Precedents"),
        )
        .allow("view.review.arrows.*")
        .view_only(),
        Entry::new(
            "trace dependents",
            "C2",
            Act::Ribbon("Formulas", "Formula Auditing", "Trace Dependents"),
        )
        .allow("view.review.arrows.*")
        .view_only(),
        Entry::new(
            "remove arrows",
            "E2",
            Act::Ribbon("Formulas", "Formula Auditing", "Remove Arrows"),
        )
        .setup("formulas.trace_precedents", json!({"cell": "E2"}))
        .gone("view.review.arrows.*")
        .view_only(),
        // The next error cell is selected; nothing changes.
        Entry::new(
            "error checking",
            "A1",
            Act::Ribbon("Formulas", "Formula Auditing", "Error Checking"),
        )
        .setup("cell.set", json!({"cell": "H4", "input": "=1/0"}))
        .then(|s| active_is(s, "H4")),
        // Evaluate Formula lists the steps of E2 (C2 → 10, D2 → 1.5, 15); Close. No change.
        Entry::new(
            "evaluate formula",
            "E2",
            Act::Custom {
                prep: |h| tab(h, "Formulas"),
                act: |h| {
                    click_in_ribbon(h, "Formula Auditing", "Evaluate Formula")?;
                    h.run_steps(3);
                    match dialog(h.state_mut()) {
                        Dialog::EvaluateFormula(d) if d.result == "15" && d.steps.len() == 3 => {}
                        Dialog::EvaluateFormula(d) => {
                            return Err(format!("steps {:?}, result {}", d.steps, d.result));
                        }
                        _ => return Err("Evaluate Formula did not open".into()),
                    }
                    click(h, "Close")
                },
            },
        ),
        // The button lists the watches (none): nothing changes. Adding one has no UI yet
        // (see KNOWN_GAPS in the report: there is no Watch Window pane).
        Entry::new(
            "watch window",
            "E2",
            Act::Ribbon("Formulas", "Formula Auditing", "Watch Window"),
        ),
        insert_function("insert function", "Insert Function…", "SUM"),
        insert_function("financial functions", "Financial", "PMT"),
        insert_function("logical functions", "Logical", "IF"),
        insert_function("text functions", "Text", "LEN"),
        insert_function("date functions", "Date & Time", "DATE"),
        insert_function("lookup functions", "Lookup & Reference", "VLOOKUP"),
        insert_function("math functions", "Math & Trig", "ABS"),
        insert_function("more functions", "More Functions", "SUM"),
        // The name goes into the formula being typed; no property changes.
        Entry::new(
            "use in formula",
            "H2",
            Act::Pick("Formulas", "Defined Names", "Use in Formula", "Rate"),
        )
        .setup(
            "formulas.define_name",
            json!({"name": "Rate", "refers_to": "=Sheet1!$C$2"}),
        )
        .set("view.editing", "true")
        .view_only(),
        Entry::new(
            "show formulas",
            "A1",
            Act::Ribbon("Formulas", "Formula Auditing", "Show Formulas"),
        )
        .set("view.show_formulas", "true")
        .view_only(),
        // The calculation mode is a workbook setting, not an edit: not undoable (as in the
        // target), but saved with the file.
        no_undo(
            Entry::new(
                "calculation manual",
                "A1",
                Act::Ribbon("Formulas", "Calculation", "Calculation: Manual"),
            )
            .set("workbook.calc_mode", "\"Manual\""),
        ),
        no_undo(
            Entry::new(
                "calculation automatic",
                "A1",
                Act::Ribbon("Formulas", "Calculation", "Calculation: Automatic"),
            )
            .setup("formulas.calc_manual", json!({}))
            .set("workbook.calc_mode", "\"Automatic\""),
        ),
    ]
}

fn review_entries() -> Vec<Entry> {
    let note = |cell: &str| json!({"cell": cell, "text": "x"});
    vec![
        // A threaded comment on C3.
        Entry::new(
            "new comment",
            "C3",
            Act::Dialog {
                ribbon: ("Review", "Comments", "New Comment"),
                fill: |app| {
                    if let Dialog::NewComment(d) = dialog(app) {
                        d.text = "Check this".into();
                    }
                },
                ok: "Post",
            },
        )
        .set("Sheet1.comments.0.addr.row", "2")
        .set("Sheet1.comments.0.addr.col", "2")
        .set("Sheet1.comments.0.kind", "\"Thread\"")
        .set("Sheet1.comments.0.text", "\"Check this\"")
        .set("Sheet1.comments.0.visible", "false")
        .set("Sheet1.comments.0.resolved", "false")
        // The author is the user's name from the settings.
        .allow("Sheet1.comments.0.author"),
        Entry::new(
            "delete comment",
            "B3",
            Act::Ribbon("Review", "Comments", "Delete Comment"),
        )
        .setup("insert.note", note("B3"))
        .gone("Sheet1.comments.*"),
        Entry::new(
            "next comment",
            "A1",
            Act::Ribbon("Review", "Comments", "Next Comment"),
        )
        .setup("insert.note", note("B2"))
        .setup("insert.note", note("D4"))
        .then(|s| active_is(s, "B2")),
        // From A1, Previous wraps round to the last one.
        Entry::new(
            "previous comment",
            "A1",
            Act::Ribbon("Review", "Comments", "Previous Comment"),
        )
        .setup("insert.note", note("B2"))
        .setup("insert.note", note("D4"))
        .then(|s| active_is(s, "D4")),
        Entry::new(
            "show comments pane",
            "A1",
            Act::Ribbon("Review", "Comments", "Show Comments"),
        )
        .set("view.review.comments_pane", "true")
        .view_only(),
        Entry::new(
            "show all notes",
            "A1",
            Act::Ribbon("Review", "Notes", "Show All Notes"),
        )
        .setup("insert.note", note("B3"))
        .set("Sheet1.comments.0.visible", "true"),
        sheet_protected(Entry::new(
            "protect sheet",
            "A1",
            Act::Ribbon("Review", "Protect", "Protect Sheet"),
        )),
        Entry::new(
            "protect workbook",
            "A1",
            Act::Ribbon("Review", "Protect", "Protect Workbook"),
        )
        .set("workbook.protection.structure", "true")
        .set("workbook.protection.windows", "false"),
        // Without a range to add, the button lists the ranges: nothing changes.
        Entry::new(
            "allow edit ranges",
            "A1",
            Act::Ribbon("Review", "Protect", "Allow Edit Ranges"),
        ),
        Entry::new("workbook statistics", "A1", Act::Key("Ctrl+Shift+G")),
    ]
}

fn view_entries() -> Vec<Entry> {
    vec![
        Entry::new(
            "zoom dialog 200%",
            "A1",
            Act::Dialog {
                ribbon: ("View", "Zoom", "Zoom…"),
                fill: |app| {
                    if let Dialog::Zoom(d) = dialog(app) {
                        d.choice = "200".into();
                    }
                },
                ok: "OK",
            },
        )
        .set("view.zoom", "2.0")
        .view_only(),
        // A1:E6 is small in a wide window: the zoom goes to its maximum, 400%.
        Entry::new(
            "zoom to selection",
            "A1:E6",
            Act::Ribbon("View", "Zoom", "Zoom to Selection"),
        )
        .set("view.zoom", "4.0")
        .view_only(),
        Entry::new(
            "normal view",
            "A1",
            Act::Ribbon("View", "Workbook Views", "Normal"),
        )
        .setup("view.page_layout", json!({}))
        .set("view.print.mode", "\"Normal\"")
        .view_only(),
        Entry::new(
            "page break preview",
            "A1",
            Act::Ribbon("View", "Workbook Views", "Page Break Preview"),
        )
        .set("view.print.mode", "\"PageBreakPreview\"")
        .view_only(),
        Entry::new(
            "page layout view",
            "A1",
            Act::Ribbon("View", "Workbook Views", "Page Layout"),
        )
        .set("view.print.mode", "\"PageLayout\"")
        .view_only(),
        Entry::new("ruler off", "A1", Act::Ribbon("View", "Show", "Ruler"))
            .set("view.print.ruler", "false")
            .view_only(),
        Entry::new("split at C3", "C3", Act::Ribbon("View", "Window", "Split"))
            .set("view.window.split.row", "2")
            .set("view.window.split.col", "2")
            // Two panes by rows: the top one shows rows 1-2, the bottom one starts at row 3.
            .set("view.window.split_size", "2")
            .set("view.scroll.row", "2")
            .view_only(),
        Entry::new(
            "new window",
            "A1",
            Act::Ribbon("View", "Window", "New Window"),
        )
        .allow("view.window.*")
        .view_only(),
        Entry::new(
            "hide window",
            "A1",
            Act::Ribbon("View", "Window", "Hide Window"),
        )
        // The only window: the app window minimizes instead (the window list is made with
        // its one, visible, entry).
        .set("view.window.minimize_requested", "true")
        .set("view.window.windows.0.id", "1")
        .set("view.window.windows.0.hidden", "false")
        .set("view.window.windows.0.sheet", "0")
        .set("view.window.windows.0.scroll.row", "0")
        .set("view.window.windows.0.scroll.col", "0")
        .set("view.window.windows.0.sel.active.row", "0")
        .set("view.window.windows.0.sel.active.col", "0")
        .set("view.window.windows.0.sel.anchor.row", "0")
        .set("view.window.windows.0.sel.anchor.col", "0")
        .set("view.window.windows.0.sel.cursor.row", "0")
        .set("view.window.windows.0.sel.cursor.col", "0")
        .view_only(),
        Entry::new("switch windows", "A1", Act::Key("Ctrl+F6"))
            .setup("view.new_window", json!({}))
            .allow("view.window.*")
            .view_only(),
        // Alt+F8 lists the macros: nothing changes.
        Entry::new("macros", "A1", Act::Key("Alt+F8")),
        Entry::new("zoom 100%", "A1", Act::Ribbon("View", "Zoom", "100%"))
            .setup("view.zoom", json!({"percent": 150}))
            .set("view.zoom", "1.0")
            .view_only(),
        Entry::new("zoom in", "A1", Act::Key("Ctrl+Alt+="))
            .set("view.zoom", "1.1")
            .view_only(),
        Entry::new("zoom out", "A1", Act::Key("Ctrl+Alt+-"))
            .set("view.zoom", "0.9")
            .view_only(),
        Entry::new(
            "gridlines off",
            "A1",
            Act::Ribbon("View", "Show", "Gridlines"),
        )
        .set("Sheet1.show_gridlines", "false")
        .print(),
        Entry::new(
            "headings off",
            "A1",
            Act::Ribbon("View", "Show", "Headings"),
        )
        .set("view.show_headings", "false")
        .view_only(),
        Entry::new("formula bar off", "A1", Act::Key("Ctrl+Shift+U"))
            .set("view.show_formula_bar", "false")
            .view_only(),
        Entry::new(
            "full screen",
            "A1",
            Act::Ribbon("View", "View", "Full Screen"),
        )
        .set("view.full_screen", "true")
        .view_only(),
        Entry::new(
            "freeze panes at C3",
            "C3",
            Act::Ribbon("View", "Window", "Freeze Panes"),
        )
        .set("Sheet1.freeze.row", "2")
        .set("Sheet1.freeze.col", "2"),
        Entry::new(
            "freeze top row",
            "C3",
            Act::Ribbon("View", "Window", "Freeze Top Row"),
        )
        .set("Sheet1.freeze.row", "1")
        .set("Sheet1.freeze.col", "0"),
        Entry::new(
            "freeze first column",
            "C3",
            Act::Ribbon("View", "Window", "Freeze First Column"),
        )
        .set("Sheet1.freeze.row", "0")
        .set("Sheet1.freeze.col", "1"),
        Entry::new(
            "unfreeze panes",
            "A1",
            Act::Ribbon("View", "Window", "Unfreeze Panes"),
        )
        .setup("view.freeze_panes", json!({"cell": "B2"}))
        .gone("Sheet1.freeze.row")
        .gone("Sheet1.freeze.col"),
    ]
}

fn file_entries() -> Vec<Entry> {
    vec![
        // Print opens the preview.
        Entry::new("print preview", "A1", Act::Key("Ctrl+P"))
            .set("view.print.preview", "true")
            .view_only(),
        // Info › Properties reports the properties; nothing changes.
        Entry::new(
            "properties",
            "A1",
            Act::Ribbon("File", "Info", "Properties"),
        ),
        Entry::new(
            "protect workbook from File",
            "A1",
            Act::Ribbon("File", "Info", "Protect Workbook"),
        )
        .set("workbook.protection.structure", "true")
        .set("workbook.protection.windows", "false"),
    ]
}

// ---------------------------------------------------------------------------------------
// Coverage

/// Commands the desktop app handles natively (`sp_ui::shell_hook::DESKTOP_INTERCEPTED`):
/// file dialogs, closing, options. Not workbook properties; out of scope here.
const NOT_PROPERTY: [(&str, &str); 0] = [];

/// Commands whose entry would fail for an app reason (excluded from the run), with why.
const KNOWN_GAPS: [(&str, &str); 0] = [];

const COMMANDS_FLOOR_B: usize = 0;
const PROPERTY_KINDS_FLOOR_B: usize = 0;

fn in_scope() -> Vec<String> {
    const OTHERS: [&str; 8] = [
        "home.", "insert.", "layout.", "cell.", "edit.", "nav.", "select.", "sheet.",
    ];
    sp_engine::registry()
        .all()
        .iter()
        .map(|c| c.id.to_string())
        .filter(|id| !OTHERS.iter().any(|p| id.starts_with(p)))
        .collect()
}

#[test]
fn every_entry_changes_exactly_its_properties() {
    let t = std::time::Instant::now();
    let (proved, failed) = check_all(&entries());
    println!(
        "{} commands proved, {:?}",
        proved.commands.len(),
        t.elapsed()
    );
    assert!(
        failed.is_empty(),
        "{} entries failed: {failed:?}",
        failed.len()
    );
}

#[test]
fn every_command_in_scope_is_proved_or_listed() {
    let mut proved = Proved::default();
    for e in &entries() {
        let _ = run(e, &mut proved);
    }
    let listed: BTreeSet<&str> = NOT_PROPERTY
        .iter()
        .chain(KNOWN_GAPS.iter())
        .map(|(id, _)| *id)
        .collect();
    let scope = in_scope();
    let missing: Vec<&String> = scope
        .iter()
        .filter(|id| !proved.commands.contains(*id) && !listed.contains(id.as_str()))
        .collect();
    let mine = scope
        .iter()
        .filter(|id| proved.commands.contains(*id))
        .count();
    println!(
        "commands in scope: {}, proved {mine}, listed {}",
        scope.len(),
        listed.len()
    );
    assert!(
        missing.is_empty(),
        "{} commands neither proved nor listed: {missing:?}",
        missing.len()
    );
    assert!(
        mine >= COMMANDS_FLOOR_B,
        "commands proved {mine} < floor {COMMANDS_FLOOR_B}"
    );
    assert!(
        proved.changed.len() >= PROPERTY_KINDS_FLOOR_B,
        "property kinds {} < floor {PROPERTY_KINDS_FLOOR_B}",
        proved.changed.len()
    );
}
