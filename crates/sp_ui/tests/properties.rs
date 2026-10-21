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
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::{Value as Json, json};
use sp_core::{CellAddr, CellRange};
use sp_engine::props::{Change, PropMap, diff, schema_path, snapshot};
use sp_engine::{Session, settings};
use sp_ui::App;
use std::collections::BTreeSet;

mod common;
use common::*;

/// How the user does it.
enum Act {
    /// A ribbon button: tab, group, label.
    Ribbon(&'static str, &'static str, &'static str),
    /// A ribbon menu (tab, group, label), then the option chosen in it.
    Pick(&'static str, &'static str, &'static str, &'static str),
    /// A keyboard shortcut, e.g. `Ctrl+B`.
    Key(&'static str),
    /// Click a cell, type, press Enter.
    Type(&'static str, &'static str),
    /// Open a dialog from the ribbon, fill it in (`fill` edits the open dialog the way the
    /// user's typing would), then click `ok`.
    Dialog {
        ribbon: (&'static str, &'static str, &'static str),
        fill: fn(&mut App),
        ok: &'static str,
    },
}

struct Entry {
    name: &'static str,
    /// Commands run on the base workbook first.
    setup: Vec<(&'static str, Json)>,
    /// The selection the action starts from (active cell = its top-left).
    select: &'static str,
    act: Act,
    /// Exact new value per changed path (`None` = the property is gone).
    expect: Vec<(String, Option<String>)>,
    /// Further changes allowed without pinning their values (glob, `*` = any run).
    allow: Vec<&'static str>,
    /// Undoable (view changes such as zoom are not).
    undo: bool,
    /// Checked after save and reopen (a few properties are not stored in .xlsx).
    persist: bool,
    /// Also export a PDF (page and visual properties).
    print: bool,
}

impl Entry {
    fn new(name: &'static str, select: &'static str, act: Act) -> Self {
        Entry {
            name,
            setup: Vec::new(),
            select,
            act,
            expect: Vec::new(),
            allow: Vec::new(),
            undo: true,
            persist: true,
            print: false,
        }
    }
    fn setup(mut self, id: &'static str, p: Json) -> Self {
        self.setup.push((id, p));
        self
    }
    /// `path` becomes `value` (as the snapshot prints it: strings quoted, numbers as JSON).
    fn set(mut self, path: &str, value: &str) -> Self {
        self.expect
            .push((path.to_string(), Some(value.to_string())));
        self
    }
    fn gone(mut self, path: &str) -> Self {
        self.expect.push((path.to_string(), None));
        self
    }
    /// The same property on every cell of `range` (`Sheet1!A2:B3`).
    fn cells(mut self, range: &str, prop: &str, value: &str) -> Self {
        let (sheet, r) = range.split_once('!').unwrap();
        let r = CellRange::parse(r).unwrap();
        for a in r.iter() {
            self.expect.push((
                format!("{sheet}!{}.{prop}", a.to_a1()),
                Some(value.to_string()),
            ));
        }
        self
    }
    fn allow(mut self, glob: &'static str) -> Self {
        self.allow.push(glob);
        self
    }
    fn view_only(mut self) -> Self {
        self.undo = false;
        self.persist = false;
        self
    }
    fn print(mut self) -> Self {
        self.print = true;
        self
    }
}

fn glob(pat: &str, s: &str) -> bool {
    match pat.split_once('*') {
        None => pat == s,
        Some((a, rest)) => {
            let Some(tail) = s.strip_prefix(a) else {
                return false;
            };
            (0..=tail.len()).any(|i| tail.is_char_boundary(i) && glob(rest, &tail[i..]))
        }
    }
}

fn press(h: &mut Harness<'_, App>, sc: &str) -> Result<(), String> {
    let (m, k) = key_for(sc).ok_or_else(|| format!("no key for {sc}"))?;
    h.key_press_modifiers(m, k);
    h.run_steps(3);
    Ok(())
}

fn click_cell(h: &mut Harness<'_, App>, a1: &str) {
    let p = cell_center(h, a1);
    h.hover_at(p);
    h.run_steps(1);
    h.drag_at(p);
    h.run_steps(1);
    h.drop_at(p);
    h.run_steps(2);
}

/// Perform the action; the snapshot is taken after navigating (tab switch) and before the
/// click that changes the workbook.
fn perform(h: &mut Harness<'_, App>, act: &Act) -> Result<PropMap, String> {
    let snap = |h: &Harness<'_, App>| snapshot(&h.state().session);
    match act {
        Act::Ribbon(tab, group, label) => {
            click_tab(h, tab)?;
            let before = snap(h);
            click_in_ribbon(h, group, label)?;
            h.run_steps(3);
            Ok(before)
        }
        Act::Pick(tab, group, label, option) => {
            click_tab(h, tab)?;
            let before = snap(h);
            click_in_ribbon(h, group, label)?;
            h.run_steps(2);
            click_new(h, option, &[])?;
            Ok(before)
        }
        Act::Key(sc) => {
            let before = snap(h);
            press(h, sc)?;
            Ok(before)
        }
        Act::Type(cell, text) => {
            click_cell(h, cell);
            let before = snap(h);
            h.event(egui::Event::Text((*text).to_string()));
            h.run_steps(1);
            press(h, "Enter")?;
            Ok(before)
        }
        Act::Dialog { ribbon, fill, ok } => {
            click_tab(h, ribbon.0)?;
            let before = snap(h);
            click_in_ribbon(h, ribbon.1, ribbon.2)?;
            h.run_steps(3);
            if h.state().dialog_kind().is_none() {
                return Err("the dialog did not open".into());
            }
            fill(h.state_mut());
            h.run_steps(2);
            h.get_by_label(ok).click();
            h.run_steps(3);
            if let Some(k) = h.state().dialog_kind() {
                return Err(format!(
                    "dialog '{k}' stayed open: {:?}",
                    h.state().last_error()
                ));
            }
            Ok(before)
        }
    }
}

fn show(changes: &[Change]) -> String {
    changes
        .iter()
        .map(|c| format!("\n      {c}"))
        .collect::<String>()
}

/// What one entry proved, for the coverage report.
#[derive(Default)]
struct Proved {
    commands: BTreeSet<String>,
    changed: BTreeSet<String>,
    persisted: BTreeSet<String>,
}

fn run(e: &Entry, proved: &mut Proved) -> Result<(), String> {
    let mut s = base_session();
    for (id, p) in &e.setup {
        s.execute(id, p).map_err(|x| format!("setup {id}: {x}"))?;
    }
    let r = CellRange::parse(e.select).map_err(|_| "bad selection")?;
    s.select_range(r, r.start);
    // Undo in the checks may only reach the action under test.
    s.history.clear();
    let mut h = Harness::builder()
        .with_size(egui::Vec2::new(WIDTH, 900.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), App::new(s));
    h.run_steps(3);
    let log_before = settings::command_log();
    let before = perform(&mut h, &e.act)?;
    let after = snapshot(&h.state().session);
    proved.commands.extend(
        new_log(&log_before)
            .into_iter()
            .filter(|l| !l.ends_with("(failed)")),
    );

    // 1. Exactly the expected changes. A path with `*` covers every changed path it matches
    // (at least one), all of which must end at the given value.
    let changes = diff(&before, &after);
    let mut problems = Vec::new();
    for (path, want) in &e.expect {
        if path.contains('*') {
            let hit: Vec<&Change> = changes.iter().filter(|c| glob(path, &c.path)).collect();
            if hit.is_empty() {
                problems.push(format!("{path}: nothing matching changed"));
            }
            for c in hit {
                if c.after.as_ref() != want.as_ref() {
                    problems.push(format!("{}: expected {want:?}, got {:?}", c.path, c.after));
                }
            }
            continue;
        }
        let got = after.get(path);
        if got != want.as_ref() {
            problems.push(format!("{path}: expected {want:?}, got {got:?}"));
        }
    }
    let unexpected: Vec<Change> = changes
        .iter()
        .filter(|c| {
            !e.expect
                .iter()
                .any(|(p, _)| *p == c.path || glob(p, &c.path))
        })
        .filter(|c| !e.allow.iter().any(|g| glob(g, &c.path)))
        .cloned()
        .collect();
    if !unexpected.is_empty() {
        problems.push(format!("unexpected changes:{}", show(&unexpected)));
    }
    if !problems.is_empty() {
        return Err(format!(
            "{}\n    all changes:{}",
            problems.join("\n    "),
            show(&changes)
        ));
    }
    let sheets: Vec<String> = h
        .state()
        .session
        .wb
        .sheets
        .iter()
        .map(|s| s.name.clone())
        .collect();
    proved
        .changed
        .extend(changes.iter().map(|c| schema_path(&c.path, &sheets)));

    // 2. Undo restores everything, redo reapplies it (through Ctrl+Z / Ctrl+Y).
    if e.undo {
        press(&mut h, "Ctrl+Z")?;
        let undone = diff(&before, &snapshot(&h.state().session));
        if !undone.is_empty() {
            return Err(format!("undo left changes:{}", show(&undone)));
        }
        press(&mut h, "Ctrl+Y")?;
        let redone = diff(&after, &snapshot(&h.state().session));
        if !redone.is_empty() {
            return Err(format!("redo differs:{}", show(&redone)));
        }
    }

    // 3. Saved and reopened, the changed properties are still there.
    if e.persist {
        let mut buf = std::io::Cursor::new(Vec::new());
        sp_io::xlsx_write::write_xlsx(&h.state().session.wb, &mut buf)
            .map_err(|x| format!("save: {x}"))?;
        let bytes = buf.into_inner();
        let invalid = sp_io::xlsx_passthrough::validate_package(&bytes);
        if !invalid.is_empty() {
            return Err(format!("saved package invalid: {invalid:?}"));
        }
        let (wb, _) = sp_io::xlsx_read::read_xlsx(std::io::Cursor::new(bytes))
            .map_err(|x| format!("reopen: {x}"))?;
        let mut s2 = Session::new();
        s2.wb = wb;
        let back = snapshot(&s2);
        let lost: Vec<String> = changes
            .iter()
            .filter(|c| !c.path.starts_with("view."))
            .filter(|c| back.get(&c.path) != c.after.as_ref())
            .map(|c| format!("{}: {:?} after reopening", c, back.get(&c.path)))
            .collect();
        if !lost.is_empty() {
            return Err(format!(
                "lost on save/reopen:\n      {}",
                lost.join("\n      ")
            ));
        }
        proved.persisted.extend(
            changes
                .iter()
                .filter(|c| !c.path.starts_with("view."))
                .map(|c| schema_path(&c.path, &sheets)),
        );
    }

    // 4. The shell still renders the result.
    h.run_steps(3);

    // 5. Page and visual properties print.
    if e.print {
        let pdf = std::env::temp_dir().join(format!(
            "sp-props-{}-{}.pdf",
            std::process::id(),
            e.name.replace(' ', "_")
        ));
        h.state_mut()
            .try_exec("file.export.pdf", json!({ "path": pdf }))
            .map_err(|x| format!("pdf: {x}"))?;
        let data = std::fs::read(&pdf).unwrap_or_default();
        if !data.starts_with(b"%PDF-") {
            return Err("pdf not written".into());
        }
    }
    Ok(())
}

/// Values, formulas and a little structure to act on (Sheet1 A1:E6, a second sheet).
fn base_session() -> Session {
    let mut s = Session::new();
    let rows = [
        ["Region", "Product", "Qty", "Price"],
        ["East", "Apples", "10", "1.5"],
        ["West", "Pears", "4", "2.25"],
        ["East", "Plums", "7", "3"],
        ["North", "Apples", "12", "1.5"],
        ["West", "Apples", "3", "1.5"],
    ];
    for (r, row) in rows.iter().enumerate() {
        for (c, v) in row.iter().enumerate() {
            let a = CellAddr::new(r as u32, c as u32).to_a1();
            s.execute("cell.set", &json!({"cell": a, "input": v}))
                .unwrap();
        }
    }
    s.execute("cell.set", &json!({"cell": "E1", "input": "Total"}))
        .unwrap();
    s.execute("cell.set", &json!({"cell": "E2", "input": "=C2*D2"}))
        .unwrap();
    s.execute("home.editing.fill_down", &json!({"range": "E2:E6"}))
        .unwrap();
    s.execute("cell.set", &json!({"cell": "G1", "input": "=SUM(E2:E6)"}))
        .unwrap();
    s
}

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
    let mut proved = Proved::default();
    let mut failed = Vec::new();
    let all = entries();
    for e in &all {
        match run(e, &mut proved) {
            Ok(()) => println!("ok    {}", e.name),
            Err(why) => {
                println!("FAIL  {}\n    {why}", e.name);
                failed.push(e.name);
            }
        }
    }
    println!(
        "\nproperties: {} of {} entries pass; {} commands proved; {} property kinds changed, {} round-tripped",
        all.len() - failed.len(),
        all.len(),
        proved.commands.len(),
        proved.changed.len(),
        proved.persisted.len()
    );
    for p in &proved.changed {
        let saved = if proved.persisted.contains(p) {
            ""
        } else {
            "   (not round-tripped)"
        };
        println!("  {p}{saved}");
    }
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
