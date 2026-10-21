//! Helpers shared by the UI integration tests: a populated workbook, the shell in a test
//! harness, and clicking ribbon tabs, buttons, menus and cells the way a user does.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::expect_used
)]
use egui::Key;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;
use sp_core::{CellAddr, CellRange};
use sp_engine::{Session, settings};
use sp_ui::App;

pub mod props;

/// Wide enough that every ribbon group shows all its buttons (none collapsed into menus).
pub const WIDTH: f32 = 3400.0;

/// A workbook where most commands have something to act on: data with formulas, a note,
/// a chart, a second sheet, and a selection over the data.
pub fn session() -> Session {
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
    s.execute("insert.note", &json!({"cell": "A1", "text": "note"}))
        .unwrap();
    s.execute(
        "insert.chart.column",
        &json!({"series": [{"values": "E2:E6", "categories": "B2:B6"}], "cell": "H2"}),
    )
    .unwrap();
    s.execute("home.cells.insert_sheet", &json!({"name": "Two"}))
        .unwrap();
    s.execute("sheet.activate", &json!({"sheet": "Sheet1"}))
        .unwrap();
    s.execute(
        "formulas.define_name",
        &json!({"name": "Rate", "refers_to": "=Sheet1!$C$2"}),
    )
    .unwrap();
    s.select_range(CellRange::parse("A2:E6").unwrap(), CellAddr::new(2, 1));
    s
}

pub fn harness() -> Harness<'static, App> {
    let mut h = Harness::builder()
        .with_size(egui::Vec2::new(WIDTH, 900.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), App::new(session()));
    h.run_steps(3);
    h
}

/// The desktop app's native commands, registered as the app does.
pub fn register_desktop() {
    sp_ui::shell_hook::set_intercepted(&sp_ui::shell_hook::DESKTOP_INTERCEPTED);
    let _ = sp_ui::shell_hook::take_pending();
}

/// Log lines added since `before` was taken. The log is per thread (tests don't mix) and
/// capped: old lines drop off the front, so align on the overlap rather than the length.
pub fn new_log(before: &[String]) -> Vec<String> {
    let after = settings::command_log();
    let skip = (0..=before.len())
        .find(|&n| after.starts_with(&before[n..]))
        .map_or(0, |n| before.len() - n);
    after
        .into_iter()
        .skip(skip)
        .map(|l| {
            l.split_once(' ')
                .map_or(l.clone(), |(_, rest)| rest.to_string())
        })
        .collect()
}

/// Click the ribbon control for `item`, looking only inside its group; when the group is
/// collapsed or the control sits in the group's overflow (⏷) menu, open that first.
pub fn click_in_ribbon(h: &mut Harness<'_, App>, group: &str, label: &str) -> Result<(), String> {
    let g = sp_ui::ribbon::group_rects(&h.ctx)
        .into_iter()
        .find(|g| g.name == group)
        .ok_or("group not shown")?;
    let inside = |n: &egui_kittest::Node<'_>| g.rect.expand(1.0).contains_rect(n.rect());
    let found: Vec<_> = h.query_all_by_label(label).filter(|n| inside(n)).collect();
    match found.as_slice() {
        [n] => {
            n.click();
            return Ok(());
        }
        [] => {}
        many => {
            return Err(format!(
                "{} controls share this label in the group",
                many.len()
            ));
        }
    }
    drop(found);
    // Open the collapsed group, or the group's overflow menu.
    let collapsed = sp_ui::ribbon::collapsed_label(group);
    let opener = if g.collapsed {
        h.query_all_by_label(&collapsed).find(|n| inside(n))
    } else {
        h.query_all_by_label("⏷").find(|n| inside(n))
    };
    let Some(opener) = opener else {
        return Err("button not found in its group".into());
    };
    let before: Vec<egui::Rect> = h.query_all_by_label(label).map(|n| n.rect()).collect();
    opener.click();
    h.run_steps(2);
    // The menu's entry is the control with this label that wasn't there before it opened.
    let n = h
        .query_all_by_label(label)
        .find(|n| !before.contains(&n.rect()))
        .ok_or("button not found in its group's menu")?;
    n.click();
    Ok(())
}

pub fn click_tab(h: &mut Harness<'_, App>, tab: &str) -> Result<(), String> {
    let nodes: Vec<_> = h.query_all_by_label(tab).collect();
    // The tab strip is the topmost control with this label.
    let n = nodes
        .iter()
        .min_by(|a, b| a.rect().top().total_cmp(&b.rect().top()))
        .ok_or("tab not found")?;
    n.click();
    drop(nodes);
    h.run_steps(2);
    let shown = h.state().session.view.ribbon_tab.clone();
    if shown == tab {
        Ok(())
    } else {
        Err(format!("clicking the tab showed '{shown}'"))
    }
}

/// A key press the shell names `sc` (the inverse of `sp_ui::shortcut_names`).
pub fn key_for(sc: &str) -> Option<(egui::Modifiers, Key)> {
    use egui::Modifiers as M;
    let mods = [
        M::NONE,
        M::COMMAND,
        M::SHIFT,
        M::ALT,
        M::COMMAND | M::SHIFT,
        M::COMMAND | M::ALT,
        M::ALT | M::SHIFT,
        M::COMMAND | M::ALT | M::SHIFT,
    ];
    Key::ALL.iter().find_map(|k| {
        mods.iter().find_map(|m| {
            sp_ui::shortcut_names(*k, *m)
                .iter()
                .any(|s| s.eq_ignore_ascii_case(sc))
                .then_some((*m, *k))
        })
    })
}

pub fn cell_center(h: &Harness<'_, App>, a1: &str) -> egui::Pos2 {
    let origin = sp_ui::overlays::cells_origin(&h.ctx).expect("grid painted");
    let a = CellAddr::parse(a1).unwrap();
    let s = &h.state().session;
    let sh = s.wb.sheet(s.sheet_index()).unwrap();
    let x = sp_render::chart::col_offset_px(sh, 0, a.col);
    let y = sp_render::chart::row_offset_px(sh, 0, a.row);
    let w = sp_render::col_width_px(sh.col_width(a.col));
    let ht = sp_render::row_height_px(sh.row_height(a.row));
    origin + egui::Vec2::new(x + w / 2.0, y + ht / 2.0)
}

pub fn right_click(h: &mut Harness<'_, App>, p: egui::Pos2) {
    h.hover_at(p);
    h.run_steps(1);
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
        h.run_steps(1);
    }
    // A popup measures itself, then settles into place (upwards near the window bottom).
    h.run_steps(2);
}

/// Click the entry labelled `label` in a menu that just opened (`before`: the rects of
/// controls with that label before it opened, e.g. a ribbon button with the same text).
pub fn click_new(
    h: &mut Harness<'_, App>,
    label: &str,
    before: &[egui::Rect],
) -> Result<(), String> {
    // A submenu's button carries an arrow: "Sort ⏵".
    let sub = format!("{label} ⏵");
    let r = [label, sub.as_str()]
        .iter()
        .find_map(|l| {
            h.query_all_by_label(l)
                .find(|n| !before.contains(&n.rect()))
                .map(|n| n.rect())
        })
        .ok_or_else(|| format!("'{label}' not in the menu"))?;
    // Press and release on separate frames, as a user's click arrives.
    let p = r.center();
    h.hover_at(p);
    h.run_steps(1);
    h.drag_at(p);
    h.run_steps(1);
    h.drop_at(p);
    h.run_steps(3);
    Ok(())
}

pub fn rects(h: &Harness<'_, App>, label: &str) -> Vec<egui::Rect> {
    h.query_all_by_label(label).map(|n| n.rect()).collect()
}
