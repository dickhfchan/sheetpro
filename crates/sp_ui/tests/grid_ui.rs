//! In-grid UI driven through the real shell with egui_kittest: AutoFilter drop-down,
//! comment card, validation list, chart drag, conditional-formatting and table dialogs, and
//! the ribbon layout at common window widths.

#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
use egui::Vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;
use sp_core::{CellAddr, CellRange};
use sp_engine::Session;
use sp_ui::App;
use sp_ui::dialogs::Dialog;

fn exec(s: &mut Session, id: &str, p: serde_json::Value) {
    s.execute(id, &p).unwrap_or_else(|e| panic!("{id}: {e}"));
}

/// Region / Units data in A1:B6.
fn session() -> Session {
    let mut s = Session::new();
    for (i, (r, u)) in [
        ("Region", "Units"),
        ("North", "120"),
        ("South", "45"),
        ("East", "300"),
        ("North", "88"),
        ("West", "64"),
    ]
    .iter()
    .enumerate()
    {
        exec(
            &mut s,
            "cell.set",
            json!({"cell": format!("A{}", i + 1), "input": r}),
        );
        exec(
            &mut s,
            "cell.set",
            json!({"cell": format!("B{}", i + 1), "input": u}),
        );
    }
    s.select(CellAddr::new(0, 0));
    s
}

fn harness_sized(app: App, w: f32, h: f32) -> Harness<'static, App> {
    let mut h = Harness::builder()
        .with_size(Vec2::new(w, h))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run_steps(3);
    h
}

fn harness(app: App) -> Harness<'static, App> {
    harness_sized(app, 1280.0, 800.0)
}

fn text(app: &App, a1: &str) -> String {
    let a = CellRange::parse(a1).unwrap().start;
    app.session
        .wb
        .sheet(0)
        .and_then(|sh| sh.get(a))
        .map(|c| c.input_text())
        .unwrap_or_default()
}

#[test]
fn filter_dropdown_filters_rows() {
    let mut s = session();
    exec(&mut s, "data.filter", json!({"range": "A1:B6", "on": true}));
    s.select(CellAddr::new(0, 0));
    let mut app = App::new(s);
    app.open_dialog("filter").unwrap();
    let mut h = harness(app);
    assert_eq!(h.state().dialog_kind().as_deref(), Some("filter"));
    // The dialog state round-trips through JSON (its `kind` tag must not collide).
    let ui_json = serde_json::to_string(&h.state().ui).unwrap();
    let back: sp_ui::dialogs::UiState = serde_json::from_str(&ui_json).unwrap();
    assert!(matches!(back.dialog, Some(Dialog::Filter(_))));
    // Untick "North" in the value list, then OK.
    h.get_by_label("North").click();
    h.run_steps(2);
    h.get_by_label("OK").click();
    h.run_steps(2);
    assert!(h.state().ui.dialog.is_none());
    let hidden = &h.state().session.wb.sheet(0).unwrap().hidden_rows;
    // Rows 2 and 5 (0-based 1 and 4) hold North.
    assert!(hidden.contains(&1) && hidden.contains(&4), "{hidden:?}");
    assert!(!hidden.contains(&2) && !hidden.contains(&3));
}

#[test]
fn comment_card_shows_text() {
    let mut s = session();
    exec(
        &mut s,
        "insert.comment",
        json!({"cell": "B3", "text": "Is this a bulk order?", "author": "Lee"}),
    );
    exec(
        &mut s,
        "insert.note",
        json!({"cell": "A4", "text": "Restock due", "author": "Dana"}),
    );
    s.select(CellAddr::new(2, 1));
    let mut app = App::new(s);
    app.open_dialog("comment").unwrap();
    let mut h = harness(app);
    assert_eq!(h.state().dialog_kind().as_deref(), Some("comment"));
    assert!(
        h.query_by_label_contains("Is this a bulk order?").is_some(),
        "thread text is shown"
    );
    assert!(
        h.query_by_label_contains("Lee").is_some(),
        "author is shown"
    );
    // A note opens in an editable card with its text.
    h.state_mut().session.select(CellAddr::new(3, 0));
    h.state_mut().open_dialog("comment").unwrap();
    h.run_steps(2);
    match &h.state().ui.dialog {
        Some(Dialog::Comment(c)) => assert_eq!(c.draft, "Restock due"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn validation_list_picks_a_value() {
    let mut s = session();
    exec(
        &mut s,
        "data.validation",
        json!({"range": "D2:D6", "type": "list", "source": "Open,Closed,Pending"}),
    );
    s.select(CellAddr::new(2, 3));
    let mut app = App::new(s);
    app.open_dialog("validation_list").unwrap();
    let mut h = harness(app);
    h.get_by_label("Closed").click();
    h.run_steps(2);
    assert!(h.state().ui.dialog.is_none());
    assert_eq!(text(h.state(), "D3"), "Closed");
}

#[test]
fn chart_drag_moves_the_anchor() {
    let mut s = session();
    exec(
        &mut s,
        "insert.chart.column",
        json!({"range": "A1:B6", "anchor": "D2:H12"}),
    );
    let mut h = harness(App::new(s));
    let origin = sp_ui::overlays::cells_origin(&h.ctx).expect("grid painted");
    let rect = sp_ui::chart::chart_rects(&h.state().session, origin)[0].1;
    let before = h.state().session.wb.sheet(0).unwrap().drawings.charts[0].anchor;
    let start = rect.center();
    // Two columns right, three rows down (default column ≈ 64 px, row ≈ 20 px).
    let end = start + Vec2::new(130.0, 61.0);
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
    let after = h.state().session.wb.sheet(0).unwrap().drawings.charts[0].anchor;
    assert_ne!(format!("{before:?}"), format!("{after:?}"), "anchor moved");
    assert_eq!(h.state().ui.grid.chart, Some(0), "chart stays selected");
    // One undo step puts it back.
    h.state_mut().exec("edit.undo", json!({}));
    let undone = h.state().session.wb.sheet(0).unwrap().drawings.charts[0].anchor;
    assert_eq!(format!("{before:?}"), format!("{undone:?}"));
}

#[test]
fn cf_dialog_creates_a_rule() {
    let mut s = session();
    s.select_range(CellRange::parse("B2:B6").unwrap(), CellAddr::new(1, 1));
    let mut app = App::new(s);
    // The ribbon command opens the dialog (it would fail with empty params otherwise).
    app.activate_command("home.styles.cf.new_rule");
    let mut h = harness(app);
    assert_eq!(h.state().dialog_kind().as_deref(), Some("cond_format"));
    if let Some(Dialog::CondFormat(d)) = &mut h.state_mut().ui.dialog {
        assert_eq!(d.range, "B2:B6");
        d.value = "100".into();
        d.format = "green".into();
    }
    h.run_steps(1);
    h.get_by_label("OK").click();
    h.run_steps(2);
    assert!(h.state().ui.dialog.is_none(), "{:?}", h.state().ui.dialog);
    let sh = h.state().session.wb.sheet(0).unwrap();
    assert_eq!(sh.cond_formats.len(), 1);
    let rule = &sh.cond_formats[0].rules[0];
    assert!(matches!(
        rule.kind,
        sp_model::CfKind::CellIs {
            op: sp_model::CfOp::Greater,
            ..
        }
    ));
    assert_eq!(rule.style, sp_model::DiffStyle::preset("green"));
}

#[test]
fn cf_manager_lists_reorders_and_deletes() {
    let mut s = session();
    exec(
        &mut s,
        "home.styles.cf.highlight_rules",
        json!({"range": "B2:B6", "rule": "greater_than", "value": 100}),
    );
    exec(
        &mut s,
        "home.styles.cf.data_bars",
        json!({"range": "B2:B6"}),
    );
    let mut app = App::new(s);
    app.activate_command("home.styles.cf.manage");
    let mut h = harness(app);
    let rules = |h: &Harness<'_, App>| match &h.state().ui.dialog {
        Some(Dialog::CondFormat(d)) => d.rules.clone(),
        _ => Vec::new(),
    };
    assert_eq!(rules(&h).len(), 2);
    assert_eq!(rules(&h)[0].text, "Data Bar");
    // Selected: the first (data bar). Move it down, then delete the (now top) cell rule.
    h.get_by_label("Move Down").click();
    h.run_steps(2);
    assert_eq!(rules(&h)[1].text, "Data Bar");
    h.get_by_label("Cell Value > 100").click();
    h.run_steps(1);
    h.get_by_label("Delete Rule").click();
    h.run_steps(2);
    assert_eq!(rules(&h).len(), 1);
    // New Rule… from the manager comes back to the manager after OK.
    h.get_by_label("New Rule…").click();
    h.run_steps(2);
    if let Some(Dialog::CondFormat(d)) = &mut h.state_mut().ui.dialog {
        d.category = sp_ui::dialogs::cf_dialog::CAT_UNIQUE;
        d.range = "A2:A6".into();
    }
    h.run_steps(1);
    h.get_by_label("OK").click();
    h.run_steps(2);
    assert_eq!(rules(&h).len(), 2);
    // The modal changed size (rule editor → manager); let it settle before clicking.
    h.run_steps(4);
    h.get_by_label("Close").click();
    h.run_steps(2);
    assert!(h.state().ui.dialog.is_none(), "{:?}", h.state().ui.dialog);
}

#[test]
fn ctrl_t_creates_table_and_design_dialog_edits_it() {
    let mut app = App::new(session());
    app.activate_command("insert.table");
    let mut h = harness(app);
    assert_eq!(h.state().dialog_kind().as_deref(), Some("create_table"));
    h.get_by_label("OK").click();
    h.run_steps(2);
    let t = h.state().session.wb.sheet(0).unwrap().tables[0].clone();
    assert_eq!(t.range.to_a1(), "A1:B6");
    assert!(t.header_row);
    h.state_mut().activate_command("table.design");
    h.run_steps(2);
    assert_eq!(h.state().dialog_kind().as_deref(), Some("table_design"));
    // A freshly (re)opened modal fades in for a few frames; let it settle before clicking.
    h.run_steps(4);
    h.get_by_label("Total Row").click();
    h.run_steps(1);
    match &h.state().ui.dialog {
        Some(Dialog::TableDesign(d)) => assert!(d.opts.total_row, "{:?}", d.opts),
        other => panic!("{other:?}"),
    }
    if let Some(Dialog::TableDesign(d)) = &mut h.state_mut().ui.dialog {
        d.name = "Sales".into();
    }
    h.run_steps(1);
    h.get_by_label("OK").click();
    h.run_steps(2);
    assert!(h.state().ui.dialog.is_none(), "{:?}", h.state().ui.dialog);
    let t = &h.state().session.wb.sheet(0).unwrap().tables[0];
    assert_eq!(t.name, "Sales");
    assert!(t.totals_row);
}

#[test]
fn ribbon_groups_never_overlap() {
    for width in [1024.0, 1280.0, 1600.0, 1920.0] {
        for tab in sp_ui::RIBBON_TABS {
            let mut s = session();
            s.view.ribbon_tab = tab.to_string();
            let h = harness_sized(App::new(s), width, 700.0);
            let rects = sp_ui::ribbon::group_rects(&h.ctx);
            assert!(!rects.is_empty(), "{tab} @ {width}");
            for (i, a) in rects.iter().enumerate() {
                assert!(
                    a.rect.max.x <= width + 0.5,
                    "{tab} @ {width}: group {} ends at {}",
                    a.name,
                    a.rect.max.x
                );
                for b in rects.iter().skip(i + 1) {
                    let overlap = a.rect.shrink(0.5).intersects(b.rect.shrink(0.5));
                    assert!(!overlap, "{tab} @ {width}: {} overlaps {}", a.name, b.name);
                }
            }
        }
    }
}

#[test]
fn wider_windows_show_more_of_the_home_tab() {
    let expanded = |w: f32| {
        let h = harness_sized(App::new(session()), w, 700.0);
        sp_ui::ribbon::group_rects(&h.ctx)
            .iter()
            .filter(|g| !g.collapsed)
            .map(|g| g.rect.width())
            .sum::<f32>()
    };
    let (a, b, c) = (expanded(1280.0), expanded(1600.0), expanded(1920.0));
    assert!(a < b && b < c, "{a} {b} {c}");
}

/// Re-renders `docs/img/grid-ui.png` (filter drop-down + note card + validation arrow).
/// Run with `cargo test -p sp_ui --test grid_ui -- --ignored render_grid_ui_png`.
#[test]
#[ignore = "writes docs/img/grid-ui.png"]
fn render_grid_ui_png() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let script = std::fs::read_to_string(root.join("docs/img/grid-ui.jsonl")).unwrap();
    let mut s = Session::new();
    for line in script.lines().filter(|l| !l.trim().is_empty()) {
        let (reply, _) = sp_engine::control::handle_line(&mut s, line);
        assert!(!reply.contains("\"ok\":false"), "{line} → {reply}");
    }
    // The filter drop-down of the Region column, then the active cell moves to a Status
    // cell (validation list) so its arrow shows too.
    s.select(CellAddr::new(0, 0));
    let mut app = App::new(s);
    app.open_dialog("filter").unwrap();
    app.exec("select.range", json!({"range": "E5"}));
    let mut h = Harness::builder()
        .with_size(Vec2::new(1280.0, 560.0))
        .wgpu()
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run_steps(4);
    let img = h.render().unwrap();
    img.save(root.join("docs/img/grid-ui.png")).unwrap();
}

/// Every text shape painted last frame as (text, italics).
fn painted_texts(h: &Harness<'static, App>) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    for cs in &h.output().shapes {
        if let egui::Shape::Text(t) = &cs.shape {
            let italic = t.galley.job.sections.iter().any(|s| s.format.italics);
            out.push((t.galley.text().to_string(), italic));
        }
    }
    out
}

#[test]
fn italic_cells_and_text_boxes_draw_oblique() {
    let mut s = session();
    exec(
        &mut s,
        "cell.set",
        json!({"cell": "D1", "input": "slanted"}),
    );
    exec(&mut s, "select.range", json!({"range": "D1"}));
    exec(&mut s, "home.font.italic", json!({}));
    exec(
        &mut s,
        "insert.text_box",
        json!({"text": "boxed", "italic": true, "cell": "F3", "width": 120, "height": 40}),
    );
    let h = harness(App::new(s));
    let t = painted_texts(&h);
    // (The formula bar also shows "slanted", upright.)
    let has = |w: &str, it: bool| t.iter().any(|x| x.0 == w && x.1 == it);
    assert!(has("slanted", true), "{t:?}");
    assert!(has("boxed", true), "{t:?}");
    assert!(
        has("North", false) && !has("North", true),
        "upright cells stay upright"
    );
}

#[test]
fn charts_and_objects_cover_filter_buttons() {
    let mut s = session();
    exec(&mut s, "data.filter", json!({"range": "A1:B6", "on": true}));
    // A chart over A1 and a text box over B1 hide those cells' filter buttons.
    exec(
        &mut s,
        "insert.chart.column",
        json!({"range": "A1:B6", "anchor": "A1:A8"}),
    );
    exec(
        &mut s,
        "insert.text_box",
        json!({"text": "note", "cell": "B1", "width": 64, "height": 60}),
    );
    let mut h = harness(App::new(s));
    let origin = sp_ui::overlays::cells_origin(&h.ctx).expect("grid painted");
    // The filter button sits at the bottom-right corner of the header cell.
    let sh = h.state().session.wb.sheet(0).unwrap();
    let a_w = sp_render::col_width_px(sh.col_width(0));
    let b_w = sp_render::col_width_px(sh.col_width(1));
    let r1 = sp_render::row_height_px(sh.row_height(0));
    let a_btn = origin + Vec2::new(a_w - 8.0, r1 - 8.0);
    let b_btn = origin + Vec2::new(a_w + b_w - 8.0, r1 - 8.0);
    // Paint order: the dropdown arrows are drawn before the chart and the text box.
    let shapes = &h.output().shapes;
    let last_text_box = shapes
        .iter()
        .rposition(|c| matches!(&c.shape, egui::Shape::Text(t) if t.galley.text() == "note"))
        .expect("text box painted");
    let first_arrow = shapes
        .iter()
        .position(|c| match &c.shape {
            egui::Shape::Path(p) => {
                p.points.len() == 3
                    && p.points
                        .iter()
                        .all(|q| (q.x - b_btn.x).abs() < 10.0 && (q.y - b_btn.y).abs() < 10.0)
            }
            _ => false,
        })
        .expect("filter arrow painted");
    assert!(first_arrow < last_text_box, "arrow under the text box");
    for (at, what) in [(a_btn, "chart"), (b_btn, "object")] {
        h.hover_at(at);
        h.run_steps(1);
        h.drag_at(at);
        h.run_steps(1);
        h.drop_at(at);
        h.run_steps(2);
        assert!(h.state().ui.dialog.is_none(), "{what}: no filter dropdown");
        match what {
            "chart" => assert_eq!(h.state().ui.grid.chart, Some(0)),
            _ => assert_eq!(h.state().ui.grid.object, Some(0)),
        }
    }
}
