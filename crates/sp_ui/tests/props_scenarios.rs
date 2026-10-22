//! Cross-feature scenarios: realistic sessions built only through the shell (typing,
//! selecting, ribbon, keys, dialogs) from a blank workbook. At the end the whole workbook
//! must differ from blank by exactly the expected properties; undo must walk every step
//! back to blank and redo forward to the end; the result must survive saving to .xlsx and
//! reopening, render, and print.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::expect_used
)]
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use sp_engine::props::{PropMap, diff, snapshot};
use sp_engine::{Session, settings};
use sp_ui::App;
use sp_ui::dialogs::Dialog;
use std::collections::BTreeSet;

mod common;
use common::props::*;
use common::*;

/// One thing the user does.
enum Step {
    /// Click the first cell and type the rows (each value then Enter), starting there.
    Grid(&'static str, &'static [&'static [&'static str]]),
    /// Click a cell, type, Enter.
    Type(&'static str, &'static str),
    /// Click a cell, or drag across a range.
    Select(&'static str),
    /// A ribbon button, menu pick, key, or dialog (see `Act`).
    Do(Act),
}

struct Scenario {
    name: &'static str,
    steps: Vec<Step>,
    expect: Vec<(String, Option<String>)>,
    allow: Vec<&'static str>,
    /// Checks that need more than exact values (iterative results, selection).
    then: Option<Check>,
}

impl Scenario {
    fn new(name: &'static str, steps: Vec<Step>) -> Self {
        Scenario {
            name,
            steps,
            expect: Vec::new(),
            allow: Vec::new(),
            then: None,
        }
    }
    fn set(mut self, path: &str, value: &str) -> Self {
        self.expect
            .push((path.to_string(), Some(value.to_string())));
        self
    }
    /// A typed constant: its input, and its value as the grid shows it (numbers parsed,
    /// anything else as text).
    fn typed(self, cell: &str, input: &str) -> Self {
        let value = match input.parse::<f64>() {
            Ok(n) => serde_json::Number::from_f64(n).unwrap().to_string(),
            Err(_) => format!("{input:?}"),
        };
        self.set(&format!("Sheet1!{cell}.input"), &format!("{input:?}"))
            .set(&format!("Sheet1!{cell}.value"), &value)
    }
    /// The constants of a typed grid (formulas, starting with `=`, are pinned separately).
    fn grid(mut self, start: &str, rows: &[&[&str]]) -> Self {
        let s = sp_core::CellAddr::parse(start).unwrap();
        for (r, row) in rows.iter().enumerate() {
            for (c, v) in row.iter().enumerate() {
                if !v.starts_with('=') {
                    let a = sp_core::CellAddr::new(s.row + r as u32, s.col + c as u32);
                    self = self.typed(&a.to_a1(), v);
                }
            }
        }
        self
    }
    fn formula(self, cell: &str, input: &str, value: &str) -> Self {
        self.set(&format!("{cell}.input"), &format!("{input:?}"))
            .set(&format!("{cell}.value"), value)
    }
    /// The same property on every cell of `range` (`Sheet1!A2:B3`).
    fn cells(mut self, range: &str, prop: &str, value: &str) -> Self {
        let (sheet, r) = range.split_once('!').unwrap();
        for a in sp_core::CellRange::parse(r).unwrap().iter() {
            self = self.set(&format!("{sheet}!{}.{prop}", a.to_a1()), value);
        }
        self
    }
    /// A sheet added during the scenario starts with a blank sheet's settings.
    fn new_sheet(mut self, name: &str) -> Self {
        for (k, v) in snapshot(&Session::new()) {
            if let Some(rest) = k.strip_prefix("Sheet1.") {
                self = self.set(&format!("{name}.{rest}"), &v);
            }
        }
        self
    }
    fn allow(mut self, glob: &'static str) -> Self {
        self.allow.push(glob);
        self
    }
    fn then(mut self, f: Check) -> Self {
        self.then = Some(f);
        self
    }
}

fn select(h: &mut Harness<'_, App>, range: &str) {
    let (a, b) = range.split_once(':').unwrap_or((range, range));
    if a == b {
        return click_cell(h, a);
    }
    let (from, to) = (cell_center(h, a), cell_center(h, b));
    h.hover_at(from);
    h.run_steps(1);
    h.drag_at(from);
    h.run_steps(1);
    for k in 1..=4 {
        h.hover_at(from + (to - from) * (k as f32 / 4.0));
        h.run_steps(1);
    }
    h.drop_at(to);
    h.run_steps(2);
}

fn type_in(h: &mut Harness<'_, App>, cell: &str, text: &str) -> Result<(), String> {
    click_cell(h, cell);
    h.event(egui::Event::Text(text.to_string()));
    h.run_steps(1);
    press(h, "Enter")
}

fn step(h: &mut Harness<'_, App>, s: &Step) -> Result<(), String> {
    match s {
        Step::Grid(start, rows) => {
            let s0 = sp_core::CellAddr::parse(start).unwrap();
            for (r, row) in rows.iter().enumerate() {
                for (c, v) in row.iter().enumerate() {
                    let a = sp_core::CellAddr::new(s0.row + r as u32, s0.col + c as u32);
                    type_in(h, &a.to_a1(), v)?;
                }
            }
            Ok(())
        }
        Step::Type(cell, text) => type_in(h, cell, text),
        Step::Select(r) => {
            select(h, r);
            Ok(())
        }
        Step::Do(act) => perform(h, act).map(|_| ()),
    }
}

/// Paths compared through undo/redo and reopening: the workbook, not the view (zoom,
/// selection and scrolling are not undone or saved per change).
fn model(p: &PropMap) -> PropMap {
    p.iter()
        .filter(|(k, _)| !k.starts_with("view."))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

fn run_scenario(sc: &Scenario, commands: &mut BTreeSet<String>) -> Result<(), String> {
    let mut h = Harness::builder()
        .with_size(egui::Vec2::new(WIDTH, 900.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), App::new(Session::new()));
    h.run_steps(3);
    let start = snapshot(&h.state().session);
    let log_before = settings::command_log();
    for (i, s) in sc.steps.iter().enumerate() {
        step(&mut h, s).map_err(|e| format!("step {}: {e}", i + 1))?;
        if let Some(k) = h.state().dialog_kind() {
            return Err(format!("step {}: dialog '{k}' left open", i + 1));
        }
    }
    commands.extend(
        new_log(&log_before)
            .into_iter()
            .filter(|l| !l.ends_with("(failed)")),
    );
    let end = snapshot(&h.state().session);
    if let Some(f) = sc.then {
        f(&h.state().session)?;
    }

    // 1. The workbook differs from blank by exactly the expected properties.
    // Which ribbon tab is showing is the shell's, not the workbook's.
    let changes: Vec<_> = diff(&start, &end)
        .into_iter()
        .filter(|c| c.path != "view.ribbon_tab")
        .collect();
    check_changes(&changes, &end, &sc.expect, &sc.allow)?;

    // 2. Undo walks every step back to blank; redo walks forward to the end.
    let mut n = 0;
    while h.state().session.history.can_undo() && n < 500 {
        press(&mut h, "Ctrl+Z")?;
        n += 1;
    }
    let undone = diff(&model(&start), &model(&snapshot(&h.state().session)));
    if !undone.is_empty() {
        return Err(format!("after {n} undos, not blank:{}", show(&undone)));
    }
    while h.state().session.history.can_redo() && n > 0 {
        press(&mut h, "Ctrl+Y")?;
        n -= 1;
    }
    let redone = diff(&model(&end), &model(&snapshot(&h.state().session)));
    if !redone.is_empty() {
        return Err(format!("redo differs from the end:{}", show(&redone)));
    }

    // 3. Saved and reopened, the workbook is the same.
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

    // 4. It renders, and 5. prints.
    h.run_steps(3);
    let pdf = std::env::temp_dir().join(format!(
        "sp-scenario-{}-{}.pdf",
        std::process::id(),
        sc.name.replace(' ', "_")
    ));
    h.state_mut()
        .try_exec("file.export.pdf", serde_json::json!({ "path": pdf }))
        .map_err(|x| format!("pdf: {x}"))?;
    let data = std::fs::read(&pdf).unwrap_or_default();
    let _ = std::fs::remove_file(&pdf);
    if !data.starts_with(b"%PDF-") {
        return Err("pdf not written".into());
    }
    Ok(())
}

macro_rules! dialog {
    ($app:expr, $variant:ident) => {
        match &mut $app.ui.dialog {
            Some(Dialog::$variant(d)) => d,
            _ => panic!("expected the {} dialog", stringify!($variant)),
        }
    };
}

fn click_label(h: &mut Harness<'_, App>, label: &str) -> Result<(), String> {
    h.get_by_label(label).click();
    h.run_steps(3);
    Ok(())
}

fn rb(tab: &'static str, group: &'static str, label: &'static str) -> Act {
    Act::Ribbon(tab, group, label)
}

fn value(s: &Session, sheet: usize, a1: &str) -> f64 {
    let a = sp_core::CellAddr::parse(a1).unwrap();
    match s.wb.sheets[sheet].get(a).map(|c| &c.value) {
        Some(sp_core::Value::Number(n)) => *n,
        other => panic!("{a1}: {other:?}"),
    }
}

// ---------------------------------------------------------------------------------------

const BUDGET: &[&[&str]] = &[
    &["Item", "Jan", "Feb", "Total"],
    &["Rent", "1000", "1000"],
    &["Food", "300", "350"],
    &["Fun", "150", "90"],
];

/// A monthly budget: row totals filled down, column totals by AutoSum filled right, a
/// bold header, currency, and the header row and item column frozen.
fn budget() -> Scenario {
    use Step::*;
    let currency = format!("{:?}", "\"$\"#,##0.00_);(\"$\"#,##0.00)");
    let sc = Scenario::new(
        "monthly budget",
        vec![
            Grid("A1", BUDGET),
            Type("D2", "=SUM(B2:C2)"),
            Select("D2:D4"),
            Do(Act::Key("Ctrl+D")),
            Type("A5", "Total"),
            Select("B2:B4"),
            Do(rb("Home", "Editing", "AutoSum")),
            Select("B5:D5"),
            Do(Act::Key("Ctrl+R")),
            Select("A1:D1"),
            Do(Act::Key("Ctrl+B")),
            Select("B2:D5"),
            Do(Act::Key("Ctrl+Shift+$")),
            Select("B2"),
            Do(rb("View", "Window", "Freeze Panes")),
        ],
    )
    .grid("A1", BUDGET)
    .typed("A5", "Total")
    .formula("Sheet1!D2", "=SUM(B2:C2)", "2000.0")
    .formula("Sheet1!D3", "=SUM(B3:C3)", "650.0")
    .formula("Sheet1!D4", "=SUM(B4:C4)", "240.0")
    .formula("Sheet1!B5", "=SUM(B2:B4)", "1450.0")
    .formula("Sheet1!C5", "=SUM(C2:C4)", "1440.0")
    .formula("Sheet1!D5", "=SUM(D2:D4)", "2890.0")
    .cells("Sheet1!A1:D1", "style.bold", "true")
    .set("Sheet1.freeze.row", "1")
    .set("Sheet1.freeze.col", "1");
    sc.cells("Sheet1!B2:D5", "style.num_fmt", &currency)
}

const SALES: &[&[&str]] = &[
    &["Region", "Product", "Qty"],
    &["East", "Apples", "10"],
    &["West", "Pears", "4"],
    &["East", "Plums", "7"],
    &["North", "Apples", "12"],
];

/// A sales report: the data made a table, a pivot on a new sheet summing Qty by Region.
fn sales_report() -> Scenario {
    use Step::*;
    Scenario::new(
        "sales report",
        vec![
            Grid("A1", SALES),
            Select("B3"),
            Do(Act::Dialog {
                ribbon: ("Insert", "Tables", "Table"),
                fill: |app| assert!(dialog!(app, CreateTable).has_headers),
                ok: "OK",
            }),
            Do(rb("Insert", "Tables", "PivotTable")),
            Do(Act::Custom {
                prep: |_| Ok(()),
                act: |h| {
                    click_label(h, "Region")?;
                    click_label(h, "Qty")
                },
            }),
        ],
    )
    .grid("A1", SALES)
    .set("Sheet1.tables.0.name", "\"Table1\"")
    .set("Sheet1.tables.0.header_row", "true")
    .set("Sheet1.tables.0.range.start.row", "0")
    .set("Sheet1.tables.0.range.start.col", "0")
    .set("Sheet1.tables.0.range.end.row", "4")
    .set("Sheet1.tables.0.range.end.col", "2")
    .allow("Sheet1.tables.0.*")
    // The pivot sheet goes in front of the data and becomes active.
    .set("workbook.sheet_order.0", "\"Sheet2\"")
    .set("workbook.sheet_order.1", "\"Sheet1\"")
    .set("workbook.active", "0")
    .set("Sheet2.analysis.pivots.0.source", "\"Table1\"")
    .set("Sheet2.analysis.pivots.0.rows.0", "0")
    .set("Sheet2.analysis.pivots.0.values.0.field", "2")
    .set("Sheet2.analysis.pivots.0.values.0.func", "\"Sum\"")
    // Row labels with the sum of Qty per region, sorted, and a grand total.
    .set("Sheet2!A3.value", "\"Row Labels\"")
    .set("Sheet2!B3.value", "\"Sum of Qty\"")
    .set("Sheet2!A4.value", "\"East\"")
    .set("Sheet2!B4.value", "17.0")
    .set("Sheet2!A5.value", "\"North\"")
    .set("Sheet2!B5.value", "12.0")
    .set("Sheet2!A6.value", "\"West\"")
    .set("Sheet2!B6.value", "4.0")
    .set("Sheet2!A7.value", "\"Grand Total\"")
    .set("Sheet2!B7.value", "33.0")
    // The pivot style shows the header and the grand total bold.
    .set("Sheet2!A3.style.bold", "true")
    .set("Sheet2!B3.style.bold", "true")
    .set("Sheet2!A7.style.bold", "true")
    .set("Sheet2!B7.style.bold", "true")
    .new_sheet("Sheet2")
    // The pivot's remaining layout fields (style, totals, cache), the inputs behind its
    // values, and its columns autofitted to the labels.
    .allow("Sheet2.analysis.pivots.0.*")
    .allow("Sheet2!A*.input")
    .allow("Sheet2!B*.input")
    .allow("Sheet2.col_widths.*")
}

const INVOICE: &[&[&str]] = &[
    &["Item", "Qty", "Price", "Amount"],
    &["Widget", "2", "9.5"],
    &["Gadget", "1", "24"],
];

/// A printable invoice: a merged title, line amounts and a total, borders, landscape, and
/// a print area.
fn invoice() -> Scenario {
    use Step::*;
    let sc = Scenario::new(
        "printable invoice",
        vec![
            Type("A1", "Invoice"),
            Grid("A3", INVOICE),
            Type("D4", "=B4*C4"),
            Select("D4:D5"),
            Do(Act::Key("Ctrl+D")),
            Select("D4:D5"),
            Do(rb("Home", "Editing", "AutoSum")),
            Type("C6", "Total"),
            Select("A1:D1"),
            Do(rb("Home", "Alignment", "Merge & Center")),
            Select("A3:D6"),
            Do(rb("Home", "Font", "Borders")),
            Do(rb("Page Layout", "Page Setup", "Orientation")),
            Select("A1:D6"),
            Do(rb("Page Layout", "Page Setup", "Set Print Area")),
        ],
    )
    .typed("A1", "Invoice")
    .grid("A3", INVOICE)
    .typed("C6", "Total")
    .formula("Sheet1!D4", "=B4*C4", "19.0")
    .formula("Sheet1!D5", "=B5*C5", "24.0")
    .formula("Sheet1!D6", "=SUM(D4:D5)", "43.0")
    .set("Sheet1.merges.0.start.row", "0")
    .set("Sheet1.merges.0.start.col", "0")
    .set("Sheet1.merges.0.end.row", "0")
    .set("Sheet1.merges.0.end.col", "3")
    .set("Sheet1!A1.style.h_align", "\"Center\"")
    .set("Sheet1.page_setup.orientation", "\"Landscape\"")
    .set("workbook.names.0.name", "\"_xlnm.Print_Area\"")
    .set("workbook.names.0.formula", "\"Sheet1!$A$1:$D$6\"")
    .allow("workbook.names.0.*");
    // All Borders: every edge of every cell in A3:D6, thin.
    let mut sc = sc;
    for edge in ["left", "right", "top", "bottom"] {
        sc = sc.cells(
            "Sheet1!A3:D6",
            &format!("style.border_{edge}.style"),
            "\"Thin\"",
        );
    }
    sc
}

const PEOPLE: &[&[&str]] = &[
    &["Name", "City"],
    &["Cy", "Oslo"],
    &["ann", "Paris"],
    &["Bob", "Rome"],
    &["ann", "Paris"],
];

/// Cleaning a list: duplicates removed, sorted by name (case-insensitively), one city
/// replaced everywhere, and a filter on.
fn clean_list() -> Scenario {
    use Step::*;
    Scenario::new(
        "clean a list",
        vec![
            Grid("A1", PEOPLE),
            // A bold header row, so sorting knows it is not data.
            Select("A1:B1"),
            Do(Act::Key("Ctrl+B")),
            Select("A1:B5"),
            Do(rb("Data", "Data Tools", "Remove Duplicates…")),
            Select("A2"),
            Do(rb("Data", "Sort & Filter", "Sort Ascending")),
            Select("A1"),
            Do(Act::Custom {
                prep: |h| click_tab(h, "Home"),
                act: |h| {
                    click_in_ribbon(h, "Editing", "Replace…")?;
                    h.run_steps(3);
                    let d = dialog!(h.state_mut(), Find);
                    d.what = "Paris".into();
                    d.with = "Lyon".into();
                    h.run_steps(1);
                    click_label(h, "Replace All")?;
                    click_label(h, "Close")
                },
            }),
            Do(rb("Data", "Sort & Filter", "Filter")),
        ],
    )
    .typed("A1", "Name")
    .typed("B1", "City")
    .cells("Sheet1!A1:B1", "style.bold", "true")
    .typed("A2", "ann")
    .typed("B2", "Lyon")
    .typed("A3", "Bob")
    .typed("B3", "Rome")
    .typed("A4", "Cy")
    .typed("B4", "Oslo")
    .set("Sheet1.auto_filter.range.start.row", "0")
    .set("Sheet1.auto_filter.range.start.col", "0")
    .set("Sheet1.auto_filter.range.end.row", "3")
    .set("Sheet1.auto_filter.range.end.col", "1")
}

const LOAN: &[&[&str]] = &[
    &["Rate", "0.05"],
    &["Years", "10"],
    &["Amount", "10000"],
    &["Payment"],
];

/// A loan what-if: the monthly payment, Goal Seek for the amount that makes it 100, and
/// that saved as a scenario.
fn loan() -> Scenario {
    use Step::*;
    Scenario::new(
        "loan what-if",
        vec![
            Grid("A1", LOAN),
            Type("B4", "=PMT(B1/12,B2*12,-B3)"),
            Do(Act::Custom {
                prep: |h| click_tab(h, "Data"),
                act: |h| {
                    click_in_ribbon(h, "Forecast", "Goal Seek…")?;
                    h.run_steps(3);
                    let d = dialog!(h.state_mut(), GoalSeek);
                    d.set_cell = "B4".into();
                    d.to_value = "100".into();
                    d.by_changing = "B3".into();
                    h.run_steps(2);
                    click_label(h, "OK")?;
                    click_label(h, "OK")
                },
            }),
            Do(Act::Custom {
                prep: |h| click_tab(h, "Data"),
                act: |h| {
                    click_in_ribbon(h, "Forecast", "Scenario Manager…")?;
                    h.run_steps(3);
                    let d = dialog!(h.state_mut(), ScenarioManager);
                    d.adding = true;
                    d.name = "Target".into();
                    d.changing = "B3".into();
                    h.run_steps(2);
                    click_label(h, "Save scenario")?;
                    click_label(h, "Close")
                },
            }),
        ],
    )
    .typed("A1", "Rate")
    .typed("B1", "0.05")
    .typed("A2", "Years")
    .typed("B2", "10")
    .typed("A3", "Amount")
    .typed("A4", "Payment")
    .set("Sheet1!B4.input", "\"=PMT(B1/12,B2*12,-B3)\"")
    // Goal Seek's amount and the payment it gives are checked numerically below.
    .allow("Sheet1!B3.input")
    .allow("Sheet1!B3.value")
    .allow("Sheet1!B4.value")
    .set("Sheet1.analysis.scenarios.0.name", "\"Target\"")
    .set("Sheet1.analysis.scenarios.0.changing", "\"$B$3\"")
    .set("Sheet1.analysis.scenarios.0.comment", "\"\"")
    .allow("Sheet1.analysis.scenarios.0.values.0.Number")
    .then(|s| {
        // Payment 100 a month for 120 months at 5%/12: amount = 100 · (1 − v¹²⁰) / i.
        let i: f64 = 0.05 / 12.0;
        let want = 100.0 * (1.0 - (1.0 + i).powi(-120)) / i;
        let (amount, payment) = (value(s, 0, "B3"), value(s, 0, "B4"));
        if (payment - 100.0).abs() > 1e-6 || (amount - want).abs() > 1e-4 {
            return Err(format!(
                "goal seek: amount {amount} (want {want}), payment {payment}"
            ));
        }
        // The cell's input (what F2 edits) keeps the amount to 15 digits.
        let a = sp_core::CellAddr::parse("B3").unwrap();
        let typed: f64 = s.wb.sheets[0].get(a).unwrap().input_text().parse().unwrap();
        if ((typed - amount) / amount).abs() > 1e-14 {
            return Err(format!("B3's input {typed} is not its value {amount}"));
        }
        let saved = &s.wb.sheets[0].analysis.scenarios[0].values[0];
        match saved {
            sp_core::Value::Number(n) if (n - amount).abs() < 1e-9 => Ok(()),
            other => Err(format!("scenario saved {other:?}, not the amount {amount}")),
        }
    })
}

fn scenarios() -> Vec<Scenario> {
    vec![budget(), sales_report(), invoice(), clean_list(), loan()]
}

/// Coverage may only grow: raise this as scenarios are added.
const COMMANDS_FLOOR: usize = 20;

#[test]
fn every_scenario_ends_exactly_as_expected() {
    let mut commands = BTreeSet::new();
    let mut failed = Vec::new();
    for sc in &scenarios() {
        match run_scenario(sc, &mut commands) {
            Ok(()) => println!("ok    {}", sc.name),
            Err(e) => {
                println!("FAIL  {}\n    {e}", sc.name);
                failed.push(sc.name);
            }
        }
    }
    println!("scenarios: {} commands used", commands.len());
    assert!(
        commands.len() >= COMMANDS_FLOOR,
        "scenarios used {} commands < floor {COMMANDS_FLOOR}",
        commands.len()
    );
    assert!(
        failed.is_empty(),
        "{} scenarios failed: {failed:?}",
        failed.len()
    );
}
