//! Integrated property tests: every user action, done through the real shell, must change
//! exactly the spreadsheet properties it should (nothing else), undo and redo exactly,
//! survive saving to .xlsx and reopening, render, and (for page/visual properties) print.
//!
//! Each entry gives a setup, a selection, the user action (ribbon click, menu choice, key,
//! typing, or a dialog filled in and confirmed) and the expected property changes, in the
//! `sp_engine::props` path form (`Sheet1!B3.style.bold`, `Sheet1.freeze`, `view.zoom`). The
//! runner applies the same checks to all entries and reports coverage: which commands ran,
//! and which property schema paths were changed and round-tripped.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::expect_used
)]
use serde_json::json;

mod common;
use common::props::*;

fn entries() -> Vec<Entry> {
    vec![
        // Formatting from the ribbon and the keyboard.
        Entry::new("bold", "A1:B2", Act::Ribbon("Home", "Font", "Bold")).cells(
            "Sheet1!A1:B2",
            "style.bold",
            "true",
        ),
        Entry::new("italic by key", "C3", Act::Key("Ctrl+I")).cells(
            "Sheet1!C3:C3",
            "style.italic",
            "true",
        ),
        Entry::new(
            "percent style",
            "D2:D3",
            Act::Ribbon("Home", "Number", "Percent Style"),
        )
        .cells("Sheet1!D2:D3", "style.num_fmt", "\"0%\""),
        Entry::new(
            "remove fill colour",
            "A1",
            Act::Pick("Home", "Font", "Fill Color", "No Fill"),
        )
        .setup(
            "home.font.fill_color",
            json!({"range": "A1", "color": "FFFF00"}),
        )
        .gone("Sheet1!A1.style.fill.*"),
        Entry::new(
            "merge and centre",
            "A8:C8",
            Act::Ribbon("Home", "Alignment", "Merge & Center"),
        )
        .set("Sheet1.merges.0.start.row", "7")
        .set("Sheet1.merges.0.start.col", "0")
        .set("Sheet1.merges.0.end.row", "7")
        .set("Sheet1.merges.0.end.col", "2")
        .cells("Sheet1!A8:A8", "style.h_align", "\"Center\"")
        .print(),
        // Typing.
        Entry::new("type a formula", "H2", Act::Type("H2", "=C2+1"))
            .set("Sheet1!H2.input", "\"=C2+1\"")
            .set("Sheet1!H2.value", "11.0"),
        // A cell that turns into an error shows its code.
        Entry::new("type a division by zero", "H3", Act::Type("H3", "=1/0"))
            .set("Sheet1!H3.input", "\"=1/0\"")
            .set("Sheet1!H3.value", "#DIV/0!"),
        // Structure: inserting a row moves cells and rewrites references.
        Entry::new(
            "insert row",
            "A3",
            Act::Ribbon("Home", "Cells", "Insert Sheet Rows"),
        )
        // References that span the insertion grow; the inserted row is empty and the old
        // row 3 is now row 4 (the shifted cells are allowed below without pinning each).
        .set("Sheet1!G1.input", "\"=SUM(E2:E7)\"")
        .set("Sheet1!A4.input", "\"West\"")
        .set("Sheet1!E4.input", "\"=C4*D4\"")
        .gone("Sheet1!A3.input")
        .gone("Sheet1!E3.input")
        .allow("Sheet1!A*")
        .allow("Sheet1!B*")
        .allow("Sheet1!C*")
        .allow("Sheet1!D*")
        .allow("Sheet1!E*")
        .allow("Sheet1!G1.value"),
        // Sheet and view properties.
        Entry::new(
            "freeze panes",
            "B2",
            Act::Ribbon("View", "Window", "Freeze Panes"),
        )
        .set("Sheet1.freeze.row", "1")
        .set("Sheet1.freeze.col", "1")
        .print(),
        Entry::new("zoom in", "A1", Act::Key("Ctrl+Alt+="))
            .set("view.zoom", "1.1")
            .view_only(),
        Entry::new("zoom 100%", "A1", Act::Ribbon("View", "Zoom", "100%"))
            .setup("view.zoom", json!({"percent": 150}))
            .set("view.zoom", "1.0")
            .view_only(),
        // A dialog filled in and confirmed.
        Entry::new(
            "data validation",
            "C2:C6",
            Act::Dialog {
                ribbon: ("Data", "Data Tools", "Data Validation…"),
                fill: |app| {
                    if let Some(sp_ui::dialogs::Dialog::DataValidation(d)) = &mut app.ui.dialog {
                        d.kind = "whole".into();
                        d.op = "between".into();
                        d.value1 = "1".into();
                        d.value2 = "100".into();
                    }
                },
                ok: "OK",
            },
        )
        .set("Sheet1.validations.0.kind", "\"Whole\"")
        .set("Sheet1.validations.0.op", "\"Between\"")
        .set("Sheet1.validations.0.formula1", "\"1\"")
        .set("Sheet1.validations.0.formula2", "\"100\"")
        .set("Sheet1.validations.0.ranges.0.start.row", "1")
        .set("Sheet1.validations.0.ranges.0.end.row", "5")
        .set("Sheet1.validations.0.ranges.0.start.col", "2")
        .set("Sheet1.validations.0.ranges.0.end.col", "2")
        // The rule's remaining fields keep the dialog's defaults.
        .allow("Sheet1.validations.0.*"),
    ]
}

#[test]
fn every_entry_changes_exactly_its_properties() {
    let (_, failed) = check_all(&entries());
    assert!(
        failed.is_empty(),
        "{} entries failed: {failed:?}",
        failed.len()
    );
}

/// Coverage may only grow: raise these as entries are added.
const COMMANDS_FLOOR: usize = 11;
const PROPERTY_KINDS_FLOOR: usize = 31;

#[test]
fn coverage_floors() {
    let mut proved = Proved::default();
    for e in &entries() {
        let _ = run(e, &mut proved);
    }
    assert!(
        proved.commands.len() >= COMMANDS_FLOOR,
        "commands proved {} < floor {COMMANDS_FLOOR}",
        proved.commands.len()
    );
    assert!(
        proved.changed.len() >= PROPERTY_KINDS_FLOOR,
        "property kinds {} < floor {PROPERTY_KINDS_FLOOR}",
        proved.changed.len()
    );
}

/// The runner rejects wrong expectations: a change that didn't happen, and one that did
/// but wasn't expected (the strict side-effect check).
#[test]
fn the_runner_catches_missing_and_extra_changes() {
    let mut p = Proved::default();
    // Bold applies to A1:B2, but this expects A1:C2: C1/C2 never change.
    let missing = Entry::new(
        "bold, wrong range",
        "A1:B2",
        Act::Ribbon("Home", "Font", "Bold"),
    )
    .cells("Sheet1!A1:C2", "style.bold", "true");
    let err = run(&missing, &mut p).unwrap_err();
    assert!(err.contains("Sheet1!C1.style.bold"), "{err}");
    // Expecting only A1 leaves A2/B1/B2 as unexpected side effects.
    let extra = Entry::new(
        "bold, too narrow",
        "A1:B2",
        Act::Ribbon("Home", "Font", "Bold"),
    )
    .cells("Sheet1!A1:A1", "style.bold", "true");
    let err = run(&extra, &mut p).unwrap_err();
    assert!(
        err.contains("unexpected changes") && err.contains("Sheet1!B2.style.bold"),
        "{err}"
    );
}
