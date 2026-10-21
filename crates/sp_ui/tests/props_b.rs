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

    vec![sort_az, sort_za, sort, dupes, split]
}

fn formulas_entries() -> Vec<Entry> {
    vec![
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
    vec![]
}

fn view_entries() -> Vec<Entry> {
    vec![
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
    vec![]
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
