//! Drive the real shell with egui_kittest: open dialogs the way a user does (shortcut,
//! ribbon command, context entry point), press OK, and check the command ran.

#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
use egui::{Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;
use sp_core::CellAddr;
use sp_engine::Session;
use sp_ui::App;
use sp_ui::dialogs::Dialog;

fn session() -> Session {
    let mut s = Session::new();
    for (c, v) in [
        ("A1", "Name"),
        ("B1", "Qty"),
        ("A2", "apple"),
        ("B2", "3"),
        ("A3", "cherry"),
        ("B3", "1"),
        ("A4", "banana"),
        ("B4", "2"),
    ] {
        s.execute("cell.set", &json!({"cell": c, "input": v}))
            .unwrap();
    }
    s.select(CellAddr::new(0, 0));
    s
}

fn harness(app: App) -> Harness<'static, App> {
    let mut h = Harness::builder()
        .with_size(egui::Vec2::new(1200.0, 800.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run_steps(2);
    h
}

fn text(app: &App, a1: &str) -> String {
    let a = sp_core::CellRange::parse(a1).unwrap().start;
    app.session
        .wb
        .sheet(0)
        .and_then(|sh| sh.get(a))
        .map(|c| c.input_text())
        .unwrap_or_default()
}

fn style(app: &App, a1: &str) -> sp_model::Style {
    let a = sp_core::CellRange::parse(a1).unwrap().start;
    let id = app
        .session
        .wb
        .sheet(0)
        .and_then(|sh| sh.get(a))
        .map(|c| c.style)
        .unwrap_or_default();
    app.session.wb.styles.get(id).clone()
}

#[test]
fn ctrl_1_opens_format_cells_and_ok_applies() {
    let mut h = harness(App::new(session()));
    h.key_press_modifiers(Modifiers::COMMAND, Key::Num1);
    h.run_steps(2);
    assert_eq!(h.state().dialog_kind().as_deref(), Some("format_cells"));
    if let Some(Dialog::FormatCells(fc)) = &mut h.state_mut().ui.dialog {
        fc.v.bold = true;
        fc.v.fill = Some("FFFF00".into());
        fc.v.locked = false;
    }
    h.get_by_label("OK").click();
    h.run_steps(2);
    assert!(h.state().ui.dialog.is_none());
    let st = style(h.state(), "A1");
    assert!(st.bold && !st.locked);
    assert_eq!(st.fill, Some(sp_model::Color(255, 255, 0)));
    // One undo step reverts the whole dialog.
    h.state_mut().exec("edit.undo", json!({}));
    assert!(!style(h.state(), "A1").bold);
}

#[test]
fn every_tab_renders() {
    for name in [
        "number",
        "alignment",
        "font",
        "border",
        "fill",
        "protection",
    ] {
        let mut app = App::new(session());
        app.open_dialog(name).unwrap();
        let mut h = harness(app);
        h.run_steps(2);
        let _ = h.get_by_label("OK");
    }
}

#[test]
fn find_all_lists_hits_and_clicking_selects() {
    let mut app = App::new(session());
    app.open_dialog(r#"find {"what": "an"}"#).unwrap();
    let mut h = harness(app);
    h.get_by_label("Find All").click();
    h.run_steps(2);
    let Some(Dialog::Find(f)) = &h.state().ui.dialog else {
        panic!("find closed")
    };
    assert_eq!(f.hits.len(), 1, "{:?}", f.hits);
    h.get_by_label_contains("banana").click_accesskit();
    h.run_steps(2);

    assert_eq!(h.state().session.sel.active, CellAddr::new(3, 0));
    h.get_by_label("Close").click();
    h.run_steps(2);
    assert!(h.state().ui.dialog.is_none());
}

#[test]
fn replace_all_runs_replace() {
    let mut app = App::new(session());
    app.open_dialog(r#"replace {"what": "apple", "with": "pear"}"#)
        .unwrap();
    let mut h = harness(app);
    h.get_by_label("Replace All").click();
    h.run_steps(2);
    assert_eq!(text(h.state(), "A2"), "pear");
}

#[test]
fn goto_jumps() {
    let mut app = App::new(session());
    app.open_dialog(r#"goto {"reference": "B4"}"#).unwrap();
    let mut h = harness(app);
    h.get_by_label("OK").click();
    h.run_steps(2);
    assert_eq!(h.state().session.sel.active, CellAddr::new(3, 1));
    assert!(h.state().ui.dialog.is_none());
}

#[test]
fn sort_dialog_sorts_with_header() {
    let mut app = App::new(session());
    app.activate_command("data.sort");
    let Some(Dialog::Sort(s)) = &mut app.ui.dialog else {
        panic!("no sort dialog")
    };
    assert_eq!(s.range, "A1:B4");
    assert!(s.header);
    s.levels[0].column = "B".into();
    s.levels[0].descending = true;
    let mut h = harness(app);
    h.get_by_label("OK").click();
    h.run_steps(2);
    assert_eq!(text(h.state(), "A1"), "Name");
    assert_eq!(text(h.state(), "A2"), "apple");
    assert_eq!(text(h.state(), "A3"), "banana");
    assert_eq!(text(h.state(), "A4"), "cherry");
}

#[test]
fn paste_special_values() {
    let mut s = session();
    s.execute("cell.set", &json!({"cell": "C1", "input": "=B2*2"}))
        .unwrap();
    s.execute("home.clipboard.copy", &json!({"range": "C1"}))
        .unwrap();
    s.select(CellAddr::new(5, 0));
    let mut app = App::new(s);
    app.on_shortcut("Ctrl+Alt+V");
    let Some(Dialog::PasteSpecial(p)) = &mut app.ui.dialog else {
        panic!("no dialog")
    };
    p.mode = "values".into();
    let mut h = harness(app);
    h.get_by_label("OK").click();
    h.run_steps(2);
    assert_eq!(text(h.state(), "A6"), "6");
}

#[test]
fn insert_function_fills_edit_buffer() {
    let mut app = App::new(session());
    app.on_shortcut("Shift+F3");
    let Some(Dialog::InsertFunction(f)) = &mut app.ui.dialog else {
        panic!("no dialog")
    };
    f.selected = Some("SUM".into());
    let mut h = harness(app);
    h.get_by_label("OK").click();
    h.run_steps(2);
    assert_eq!(h.state().edit_buf, "=SUM(");
}

#[test]
fn name_manager_define_and_delete() {
    let mut app = App::new(session());
    app.open_dialog(r#"define_name {"name": "Qty", "refers_to": "=Sheet1!$B$2:$B$4"}"#)
        .unwrap();
    let mut h = harness(app);
    h.get_by_label("OK").click();
    h.run_steps(2);
    assert_eq!(h.state().session.wb.names.len(), 1);
    h.state_mut().on_shortcut("Ctrl+F3");
    h.run_steps(2);
    if let Some(Dialog::NameManager(m)) = &mut h.state_mut().ui.dialog {
        m.selected = Some(0);
    }
    h.run_steps(1);
    h.get_by_label("Delete").click();
    h.run_steps(2);
    assert!(h.state().session.wb.names.is_empty());
}

#[test]
fn prompts_run_their_commands() {
    let mut app = App::new(session());
    app.open_dialog(r#"column_width {"value": "20"}"#).unwrap();
    let mut h = harness(app);
    h.get_by_label("OK").click();
    h.run_steps(2);
    let w = h
        .state()
        .session
        .wb
        .sheet(0)
        .map(|s| s.col_width(0))
        .unwrap();
    assert!((w - 20.0).abs() < 1e-9);
    h.state_mut()
        .open_dialog(r#"rename_sheet {"value": "Data"}"#)
        .unwrap();
    h.run_steps(2);
    h.get_by_label("OK").click();
    h.run_steps(2);
    assert_eq!(h.state().session.wb.sheets[0].name, "Data");
    h.state_mut()
        .open_dialog(r#"zoom {"choice": "200"}"#)
        .unwrap();
    h.run_steps(2);
    h.get_by_label("OK").click();
    h.run_steps(2);
    assert!((h.state().session.view.zoom - 2.0).abs() < 1e-6);
}

#[test]
fn ribbon_commands_with_required_params_open_dialogs() {
    let mut app = App::new(session());
    app.activate_command("home.cells.move_copy_sheet");
    assert_eq!(app.dialog_kind().as_deref(), Some("params"));
    app.ui.dialog = None;
    app.activate_command("view.zoom");
    assert_eq!(app.dialog_kind().as_deref(), Some("zoom"));
    app.ui.dialog = None;
    app.activate_command("home.font.bold");
    assert!(app.ui.dialog.is_none());
    assert!(style(&app, "A1").bold);
    for sc in [
        "Ctrl+F",
        "Ctrl+H",
        "Ctrl+G",
        "Ctrl+F3",
        "Shift+F3",
        "Ctrl+Shift+F",
    ] {
        app.ui.dialog = None;
        app.on_shortcut(sc);
        assert!(app.ui.dialog.is_some(), "{sc}");
    }
}

#[test]
fn ui_state_round_trips_through_json() {
    let mut app = App::new(session());
    app.open_dialog("number").unwrap();
    let j = serde_json::to_string(&app.ui).unwrap();
    let back: sp_ui::dialogs::UiState = serde_json::from_str(&j).unwrap();
    assert!(matches!(back.dialog, Some(Dialog::FormatCells(_))));
    assert!(app.open_dialog("nope").is_err());
    assert!(app.open_dialog("find {bad json").is_err());
}

#[test]
fn find_all_searches_sheet_or_workbook() {
    use sp_ui::dialogs::find::FindReplace;
    let mut s = session();
    s.execute("home.cells.insert_sheet", &json!({"name": "Two"}))
        .unwrap();
    s.execute("cell.set", &json!({"cell": "C5", "input": "apple pie"}))
        .unwrap();
    let mut app = App::new(s);
    // Make sure the first sheet is active whatever insert_sheet did.
    app.try_exec("home.editing.goto", json!({"reference": "Sheet1!A1"}))
        .unwrap();
    let mut d = FindReplace::new(false);
    d.what = "apple".into();
    d.find_all(&mut app);
    assert_eq!(d.hits.len(), 1, "sheet only: {:?}", d.hits);
    d.in_workbook = true;
    d.find_all(&mut app);
    assert_eq!(d.hits.len(), 2, "workbook: {:?}", d.hits);
}

#[test]
fn formula_bar_shows_dates_and_percents_as_typed() {
    // Regression: a typed date showed its serial (46053) in the formula bar.
    let mut s = Session::new();
    for (c, v) in [
        ("A1", "2026-01-31"),
        ("A2", "1/31/2026 13:30"),
        ("A3", "50%"),
        ("A4", "1234.5"),
    ] {
        s.execute("cell.set", &json!({"cell": c, "input": v}))
            .unwrap();
    }
    let mut app = App::new(s);
    for (cell, want) in [
        ("A1", "1/31/2026"),
        ("A2", "1/31/2026 1:30:00 PM"),
        ("A3", "50%"),
        ("A4", "1234.5"),
    ] {
        app.exec("select.range", json!({"range": cell}));
        assert_eq!(app.edit_buf, want, "formula bar for {cell}");
    }
}
