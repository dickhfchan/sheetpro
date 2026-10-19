//! Floating objects through the real shell (egui_kittest): pictures decode once, checkbox
//! clicks toggle (undoably), slicer buttons filter, the PivotTable Fields pane edits the
//! pivot, and objects select / move / delete with the mouse and keyboard.

#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
use egui::{Pos2, Vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;
use sp_core::CellAddr;
use sp_engine::Session;
use sp_ui::App;

fn exec(s: &mut Session, id: &str, p: serde_json::Value) -> serde_json::Value {
    s.execute(id, &p).unwrap_or_else(|e| panic!("{id}: {e}"))
}

fn harness(app: App) -> Harness<'static, App> {
    let mut h = Harness::builder()
        .with_size(Vec2::new(1280.0, 800.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run_steps(3);
    h
}

fn click(h: &mut Harness<'static, App>, p: Pos2) {
    h.hover_at(p);
    h.run_steps(1);
    h.drag_at(p);
    h.run_steps(1);
    h.drop_at(p);
    h.run_steps(2);
}

fn png_path(name: &str) -> String {
    let dir = std::env::temp_dir().join(format!("sp-ui-objects-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    let px: Vec<u8> = (0..30 * 20)
        .flat_map(|i| [(i * 5) as u8, 120, 200, 255])
        .collect();
    std::fs::write(&path, sp_render::objects::encode_png(30, 20, px).unwrap()).unwrap();
    path.to_string_lossy().into_owned()
}

/// Screen centre of a cell at 100% zoom with no scroll.
fn cell_center(h: &Harness<'static, App>, a1: &str) -> Pos2 {
    let origin = sp_ui::overlays::cells_origin(&h.ctx).expect("grid painted");
    let a = CellAddr::parse(a1).unwrap();
    let sh = h.state().session.wb.sheet(0).unwrap();
    let x = sp_render::chart::col_offset_px(sh, 0, a.col);
    let y = sp_render::chart::row_offset_px(sh, 0, a.row);
    let w = sp_render::col_width_px(sh.col_width(a.col));
    let ht = sp_render::row_height_px(sh.row_height(a.row));
    origin + Vec2::new(x + w / 2.0, y + ht / 2.0)
}

#[test]
fn pictures_decode_once_across_frames() {
    let mut s = Session::new();
    let path = png_path("pic.png");
    exec(
        &mut s,
        "insert.picture",
        json!({"path": path, "cell": "C3"}),
    );
    exec(
        &mut s,
        "insert.picture",
        json!({"path": path, "cell": "F3"}),
    );
    exec(&mut s, "layout.background", json!({"path": path}));
    let mut h = harness(App::new(s));
    h.run_steps(5);
    // Three images (two pictures, one background): each decoded exactly once.
    assert_eq!(h.state().textures.decodes(), 3);
    assert_eq!(h.state().textures.len(), 3);
    let origin = sp_ui::overlays::cells_origin(&h.ctx).unwrap();
    let rects = sp_ui::objdraw::object_rects(&h.state().session, origin);
    assert_eq!(rects.len(), 2);
    assert!((rects[0].1.width() - 30.0).abs() < 0.5, "{:?}", rects[0]);
    // Deleting a picture eventually frees its texture; the others stay cached.
    h.state_mut().exec("object.delete", json!({"object": 1}));
    h.run_steps(700);
    assert_eq!(h.state().textures.decodes(), 3);
    assert_eq!(h.state().textures.len(), 2);
}

#[test]
fn clicking_a_cell_checkbox_toggles_it() {
    let mut s = Session::new();
    exec(&mut s, "cell.set", json!({"cell": "A2", "input": "Task"}));
    exec(&mut s, "insert.checkbox", json!({"range": "B2"}));
    s.select(CellAddr::new(0, 0));
    let mut h = harness(App::new(s));
    let value = |h: &Harness<'static, App>| {
        h.state()
            .session
            .wb
            .sheet(0)
            .unwrap()
            .value(CellAddr::parse("B2").unwrap())
            .clone()
    };
    assert_eq!(value(&h), sp_core::Value::Bool(false));
    let p = cell_center(&h, "B2");
    click(&mut h, p);
    assert_eq!(value(&h), sp_core::Value::Bool(true));
    assert_eq!(h.state().session.sel.active, CellAddr::parse("B2").unwrap());
    // Clicking the cell away from the box only selects it.
    let edge = p + Vec2::new(25.0, 0.0);
    click(&mut h, edge);
    assert_eq!(value(&h), sp_core::Value::Bool(true));
    // Space toggles the active checkbox; undo restores each step.
    h.key_press(egui::Key::Space);
    h.run_steps(2);
    assert_eq!(value(&h), sp_core::Value::Bool(false));
    h.state_mut().exec("edit.undo", json!({}));
    assert_eq!(value(&h), sp_core::Value::Bool(true));
    h.state_mut().exec("edit.undo", json!({}));
    assert_eq!(value(&h), sp_core::Value::Bool(false));
}

/// Region/Product/Sales data with a pivot at F1 and a Region slicer at J2:L12.
fn pivot_session() -> Session {
    let mut s = Session::new();
    let rows = [
        ["Region", "Product", "Sales"],
        ["East", "A", "100"],
        ["West", "B", "150"],
        ["East", "B", "200"],
        ["North", "A", "80"],
    ];
    for (r, row) in rows.iter().enumerate() {
        for (c, v) in row.iter().enumerate() {
            let a = CellAddr::new(r as u32, c as u32).to_a1();
            exec(&mut s, "cell.set", json!({"cell": a, "input": v}));
        }
    }
    exec(
        &mut s,
        "insert.pivot_table",
        json!({"source": "A1:C5", "dest": "F1", "rows": ["Region"], "values": ["Sales"]}),
    );
    exec(
        &mut s,
        "insert.slicer",
        json!({"pivot": "PivotTable1", "field": "Region", "anchor": "J2:L12"}),
    );
    s.select(CellAddr::new(0, 0));
    s
}

fn hidden_regions(app: &App) -> Vec<String> {
    let pv = &app.session.wb.sheet(0).unwrap().analysis.pivots[0];
    pv.hidden_items(0).iter().map(|v| v.to_display()).collect()
}

#[test]
fn slicer_buttons_filter_the_pivot() {
    let mut h = harness(App::new(pivot_session()));
    let origin = sp_ui::overlays::cells_origin(&h.ctx).unwrap();
    let r = sp_ui::objdraw::slicer_rects(&h.state().session, origin)[0].1;
    // Items are sorted: East, North, West.
    let lay =
        sp_render::objects::slicer_layout((r.min.x, r.min.y, r.width(), r.height()), 3, 1, 1.0);
    let center = |i: usize| {
        let (x, y, w, ht) = lay.buttons[i];
        Pos2::new(x + w / 2.0, y + ht / 2.0)
    };
    click(&mut h, center(1));
    assert_eq!(hidden_regions(h.state()), vec!["East", "West"]);
    // Clicking a slicer never selects the cell under it.
    assert_eq!(h.state().session.sel.active, CellAddr::new(0, 0));
    // The clear-filter button shows everything again.
    let (cx, cy, cw, ch) = lay.clear;
    click(&mut h, Pos2::new(cx + cw / 2.0, cy + ch / 2.0));
    assert!(hidden_regions(h.state()).is_empty());
    // Ctrl+click from "all shown" picks one item, a second Ctrl+click adds another.
    h.state_mut().slicer_click(0, "West", true);
    assert_eq!(hidden_regions(h.state()), vec!["East", "North"]);
    h.state_mut().slicer_click(0, "East", true);
    assert_eq!(hidden_regions(h.state()), vec!["North"]);
    // The last selected item can't be toggled off.
    h.state_mut().slicer_click(0, "North", false);
    h.state_mut().slicer_click(0, "North", true);
    assert_eq!(hidden_regions(h.state()), vec!["East", "West"]);
    h.state_mut().exec("edit.undo", json!({}));
    assert_eq!(hidden_regions(h.state()), vec!["North"]);
}

#[test]
fn field_list_pane_edits_the_pivot() {
    let mut s = pivot_session();
    s.select(CellAddr::parse("G2").unwrap());
    let mut h = harness(App::new(s));
    assert!(h.state().field_list_visible());
    h.get_by_label("PivotTable Fields");
    h.get_by_label("Product").click();
    h.run_steps(2);
    let rows = |h: &Harness<'static, App>| {
        h.state().session.wb.sheet(0).unwrap().analysis.pivots[0]
            .rows
            .clone()
    };
    assert_eq!(rows(&h), vec![0, 1], "Product checked → added to Rows");
    h.state_mut().exec("edit.undo", json!({}));
    assert_eq!(rows(&h), vec![0]);
    // Closing hides the pane; outside a pivot it is gone too.
    h.get_by_label("Close").click();
    h.run_steps(2);
    assert!(!h.state().field_list_visible());
    h.state_mut().ui.grid.hide_field_list = false;
    h.state_mut().session.select(CellAddr::new(20, 0));
    h.run_steps(2);
    assert!(!h.state().field_list_visible());
    assert!(h.query_by_label("PivotTable Fields").is_none());
}

#[test]
fn objects_select_move_and_delete() {
    let mut s = Session::new();
    exec(
        &mut s,
        "insert.shapes",
        json!({"shape": "rect", "range": "D4:F8"}),
    );
    let mut h = harness(App::new(s));
    let origin = sp_ui::overlays::cells_origin(&h.ctx).unwrap();
    let rect = sp_ui::objdraw::object_rects(&h.state().session, origin)[0].1;
    let before = h.state().session.wb.sheet(0).unwrap().objects[0].anchor;
    let start = rect.center();
    let end = start + Vec2::new(128.0, 40.0);
    h.hover_at(start);
    h.run_steps(1);
    h.drag_at(start);
    h.run_steps(1);
    for i in 1..=4 {
        h.hover_at(start + (end - start) * (i as f32 / 4.0));
        h.run_steps(1);
    }
    h.drop_at(end);
    h.run_steps(3);
    assert_eq!(h.state().ui.grid.object, Some(0), "object selected");
    let moved = sp_ui::objdraw::object_rects(&h.state().session, origin)[0].1;
    assert!(
        (moved.min.x - rect.min.x - 128.0).abs() < 1.0,
        "{rect:?} → {moved:?}"
    );
    assert!(
        (moved.min.y - rect.min.y - 40.0).abs() < 1.0,
        "{rect:?} → {moved:?}"
    );
    assert!((moved.width() - rect.width()).abs() < 1.0);
    // Undo puts it back.
    h.state_mut().exec("edit.undo", json!({}));
    let after = h.state().session.wb.sheet(0).unwrap().objects[0].anchor;
    assert_eq!(format!("{before:?}"), format!("{after:?}"));
    // Delete removes the selected object (not the cell contents).
    h.key_press(egui::Key::Delete);
    h.run_steps(2);
    assert!(h.state().session.wb.sheet(0).unwrap().objects.is_empty());
    assert_eq!(h.state().ui.grid.object, None);
}
