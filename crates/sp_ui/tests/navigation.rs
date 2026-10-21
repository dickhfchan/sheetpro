//! Navigation test: every ribbon tab and every button on it, clicked in the real shell the
//! way a user does, each in a fresh app over a populated workbook. A button must do what its
//! catalog command says: open that command's dialog (which Escape closes without changing
//! the workbook), ask for its parameters, run it, or for pickers (font, size, colours) open a
//! menu whose choice runs it. Doing nothing, failing, or running another command fails.
//!
//! Each button that does not behave is collected; the test prints a table and fails once at
//! the end, so one broken button doesn't hide the rest.

#![allow(
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
use sp_engine::{Session, registry, settings};
use sp_ui::App;
use sp_ui::dialogs::{dialog_for_command, required_params};
use sp_ui::ribbon::tab_groups;

/// Wide enough that every ribbon group shows all its buttons (none collapsed into menus).
const WIDTH: f32 = 3400.0;

/// A workbook where most commands have something to act on: data with formulas, a note,
/// a chart, a second sheet, and a selection over the data.
fn session() -> Session {
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
    s.select_range(CellRange::parse("A2:E6").unwrap(), CellAddr::new(2, 1));
    s
}

fn harness() -> Harness<'static, App> {
    let mut h = Harness::builder()
        .with_size(egui::Vec2::new(WIDTH, 900.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), App::new(session()));
    h.run_steps(3);
    h
}

/// The desktop app's native commands, registered as the app does.
fn register_desktop() {
    sp_ui::shell_hook::set_intercepted(&sp_ui::shell_hook::DESKTOP_INTERCEPTED);
    let _ = sp_ui::shell_hook::take_pending();
}

/// Enabled buttons whose command, in this workbook, answers with a message the target also
/// shows (nothing to act on here), rather than greying out.
const CONTEXT_MESSAGES: [(&str, &str); 5] = [
    (
        "home.editing.find_next",
        "there is no previous search to repeat",
    ),
    (
        "formulas.trace_precedents",
        "the active cell has no formula",
    ),
    (
        "formulas.trace_dependents",
        "no formula refers to the active cell",
    ),
    (
        "data.text_to_columns",
        "the selection spans several columns",
    ),
    (
        "layout.page_break.remove",
        "no manual break at the active cell",
    ),
];

/// What clicking a button is expected to do.
#[derive(Debug, Clone, PartialEq)]
enum Expect {
    Dialog(String),
    Runs,
    /// Handed to the desktop app (native file dialogs, closing, options).
    Desktop,
    /// A picker: a menu opens; choosing `option` runs the command.
    Picker(&'static str),
}

fn expected(id: &str) -> Expect {
    if sp_ui::shell_hook::DESKTOP_INTERCEPTED.contains(&id) {
        return Expect::Desktop;
    }
    match id {
        "home.font.family" => return Expect::Picker("Arial"),
        "home.font.size" => return Expect::Picker("14"),
        "home.font.fill_color" => return Expect::Picker("No Fill"),
        "home.font.font_color" => return Expect::Picker("Automatic"),
        _ => {}
    }
    if let Some(d) = dialog_for_command(id) {
        return Expect::Dialog(
            dialog_type(d).unwrap_or_else(|| format!("(spec {d} fails to open)")),
        );
    }
    let doc = registry().get(id).map_or("", |c| c.params);
    if !required_params(doc).is_empty() {
        let spec = format!("params:{id}");
        return Expect::Dialog(
            dialog_type(&spec).unwrap_or_else(|| format!("(spec {spec} fails to open)")),
        );
    }
    Expect::Runs
}

/// Log lines added since `before` was taken. The log is per thread (tests don't mix) and
/// capped: old lines drop off the front, so align on the overlap rather than the length.
fn new_log(before: &[String]) -> Vec<String> {
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

/// The dialog type a dialog spec opens (`font` opens Format Cells on its Font tab).
fn dialog_type(spec: &str) -> Option<String> {
    let mut a = App::new(session());
    a.open_dialog(spec).ok()?;
    a.dialog_kind()
}

/// Click the ribbon control for `item`, looking only inside its group; when the group is
/// collapsed or the control sits in the group's overflow (⏷) menu, open that first.
fn click_in_ribbon(h: &mut Harness<'_, App>, group: &str, label: &str) -> Result<(), String> {
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

fn click_tab(h: &mut Harness<'_, App>, tab: &str) -> Result<(), String> {
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

/// Click one button and check it against what its command should do.
fn check_button(tab: &str, group: &str, id: &str, label: &str) -> Result<String, String> {
    // Work out the expectation first: it may build a scratch workbook (logged commands).
    let expect = expected(id);
    register_desktop();
    let mut h = harness();
    click_tab(&mut h, tab)?;
    let on = registry()
        .get(id)
        .is_some_and(|c| (c.spec.enabled)(&h.state().session));
    let before = settings::command_log();
    click_in_ribbon(&mut h, group, label)?;
    judge(&mut h, id, expect, on, &before)
}

/// Check what a click, key press or menu choice did against what command `id` should do.
/// `on`: the command was enabled; `before`: the command log just before the action.
fn judge(
    h: &mut Harness<'_, App>,
    id: &str,
    expect: Expect,
    on: bool,
    before: &[String],
) -> Result<String, String> {
    h.run_steps(3);
    let log = new_log(before);
    let dialog = h.state().dialog_kind();
    if !on {
        // A disabled button must do nothing at all.
        return if log.is_empty() && dialog.is_none() {
            Ok("disabled (did nothing)".into())
        } else {
            Err(format!("disabled but acted: {log:?} {dialog:?}"))
        };
    }
    match expect {
        Expect::Desktop => {
            let pending = sp_ui::shell_hook::take_pending();
            if pending == vec![id.to_string()] && log.is_empty() {
                Ok("handed to the desktop".into())
            } else {
                Err(format!(
                    "expected a desktop hand-off, got {pending:?} log {log:?}"
                ))
            }
        }
        Expect::Dialog(want) => {
            if dialog.as_deref() != Some(want.as_str()) {
                return Err(format!(
                    "expected dialog '{want}', got {dialog:?}, log {log:?}"
                ));
            }
            // Escape closes it and leaves the workbook as it was.
            h.key_press(Key::Escape);
            h.run_steps(3);
            if let Some(d) = h.state().dialog_kind() {
                return Err(format!("dialog '{d}' did not close on Escape"));
            }
            let after = new_log(before);
            if after
                .iter()
                .any(|l| !l.starts_with("view.") && !l.starts_with("ui."))
            {
                return Err(format!("cancelling the dialog ran {after:?}"));
            }
            Ok(if want == "params" {
                "form dialog".into()
            } else {
                format!("dialog {want}")
            })
        }
        Expect::Runs => match log.as_slice() {
            [l] if l == id => Ok("ran".into()),
            [l] if *l == format!("{id} (failed)") => {
                let msg = h.state().last_error().unwrap_or_default();
                match CONTEXT_MESSAGES.iter().find(|(c, _)| *c == id) {
                    Some((_, why)) => Ok(format!("message ({why}): {msg}")),
                    None => Err(format!("ran and failed: {msg}")),
                }
            }
            [] if id.starts_with("nav.") || id.starts_with("select.") => {
                // Navigation isn't logged; it must at least not report an error.
                match h.state().last_error() {
                    None => Ok("ran (navigation)".into()),
                    Some(e) => Err(format!("navigation failed: {e}")),
                }
            }
            [] => Err(format!("click did nothing (dialog {dialog:?})")),
            other => Err(format!("ran other commands: {other:?}")),
        },
        Expect::Picker(option) => {
            // The click opened a menu; choose an option from it.
            let picked = h
                .query_all_by_label(option)
                .rfind(|n| n.rect().top() > 0.0)
                .map(|n| n.click())
                .is_some();
            if !picked {
                return Err(format!("menu did not open (no '{option}')"));
            }
            h.run_steps(3);
            let log = new_log(before);
            if log.iter().any(|l| l == id) {
                Ok(format!("picker ({option})"))
            } else {
                Err(format!("choosing '{option}' logged {log:?}"))
            }
        }
    }
}

#[test]
fn every_ribbon_tab_and_button() {
    let mut rows: Vec<(String, String, String, Result<String, String>)> = Vec::new();
    for tab in sp_ui::RIBBON_TABS {
        let mut h = harness();
        let r = click_tab(&mut h, tab).map(|_| "tab shown".to_string());
        rows.push((tab.into(), "(tab)".into(), String::new(), r));
        for (group, items) in tab_groups(tab) {
            for item in items {
                let r =
                    std::panic::catch_unwind(|| check_button(tab, &group, &item.id, &item.label))
                        .unwrap_or_else(|p| {
                            let msg = p
                                .downcast_ref::<String>()
                                .cloned()
                                .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                                .unwrap_or_default();
                            Err(format!("PANIC: {msg}"))
                        });
                rows.push((
                    tab.into(),
                    format!("{group} › {}", item.label),
                    item.id.clone(),
                    r,
                ));
            }
        }
    }
    let total = rows.len();
    let failed: Vec<_> = rows.iter().filter(|r| r.3.is_err()).collect();
    let disabled = rows
        .iter()
        .filter(|r| r.3.as_ref().is_ok_and(|s| s.starts_with("disabled")))
        .count();
    for (tab, what, id, r) in &rows {
        match r {
            Ok(s) => println!("ok    {tab:<12} {what:<48} {id:<40} {s}"),
            Err(e) => println!("FAIL  {tab:<12} {what:<48} {id:<40} {e}"),
        }
    }
    println!(
        "navigation: {} of {total} tabs/buttons behave ({disabled} disabled in this context)",
        total - failed.len()
    );
    assert!(
        failed.is_empty(),
        "{} buttons misbehave (see the table above)",
        failed.len()
    );
}

/// (tab, group, button label, setup commands)
type ContextCase = (
    &'static str,
    &'static str,
    &'static str,
    Vec<(&'static str, serde_json::Value)>,
);

/// Buttons greyed out when there is nothing to act on come back, and work, once there is.
#[test]
fn context_buttons_enable_when_there_is_something_to_act_on() {
    let cases: Vec<ContextCase> = vec![
        (
            "Home",
            "Clipboard",
            "Paste Link",
            vec![("home.clipboard.copy", json!({"range": "C2"}))],
        ),
        (
            "Page Layout",
            "Page Setup",
            "Remove Page Break",
            vec![("layout.page_break.insert", json!({"cell": "B3"}))],
        ),
        (
            "Data",
            "Sort & Filter",
            "Clear Filter",
            vec![(
                "data.filter",
                json!({"range": "A1:E6", "on": true, "column": "A", "values": ["East"]}),
            )],
        ),
        (
            "Data",
            "Sort & Filter",
            "Reapply",
            vec![("data.filter", json!({"range": "A1:E6", "on": true}))],
        ),
        (
            "Data",
            "Outline",
            "Ungroup",
            vec![("data.group", json!({"range": "A2:A6", "by": "rows"}))],
        ),
        (
            "Data",
            "Outline",
            "Hide Detail",
            vec![("data.group", json!({"range": "A2:A6", "by": "rows"}))],
        ),
        (
            "Data",
            "Outline",
            "Show Detail",
            vec![
                ("data.group", json!({"range": "A2:A6", "by": "rows"})),
                ("data.hide_detail", json!({"range": "A2:A6", "by": "rows"})),
            ],
        ),
        (
            "Review",
            "Comments",
            "Delete Comment",
            vec![("insert.note", json!({"cell": "B3", "text": "x"}))],
        ),
    ];
    let mut bad = Vec::new();
    for (tab, group, label, setup) in cases {
        let mut h = harness();
        for (id, p) in &setup {
            h.state_mut().session.execute(id, p).unwrap();
        }
        h.state_mut()
            .session
            .select_range(CellRange::parse("A2:E6").unwrap(), CellAddr::new(2, 1));
        h.run_steps(2);
        click_tab(&mut h, tab).unwrap();
        let before = settings::command_log();
        click_in_ribbon(&mut h, group, label).unwrap();
        h.run_steps(3);
        let log = new_log(&before);
        if log.len() != 1 || log[0].ends_with("(failed)") {
            bad.push(format!("{label}: {log:?} {:?}", h.state().last_error()));
        }
    }
    assert!(bad.is_empty(), "{bad:#?}");
}

// ---------------------------------------------------------------------------------------
// Keyboard shortcuts

/// Shortcuts that only mean something while editing a cell (the grid ignores them).
const EDITOR_ONLY: [&str; 5] = [
    "Alt+Enter",
    "Ctrl+Shift+Enter",
    "Ctrl+Enter",
    "Escape",
    "F4",
];

/// A key press the shell names `sc` (the inverse of `sp_ui::shortcut_names`).
fn key_for(sc: &str) -> Option<(egui::Modifiers, Key)> {
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

#[test]
fn every_keyboard_shortcut() {
    let mut rows: Vec<(String, String, Result<String, String>)> = Vec::new();
    for cmd in registry().all() {
        let sc = cmd.shortcut.as_str();
        if sc.is_empty() {
            continue;
        }
        let id = cmd.id;
        let r = (|| -> Result<String, String> {
            if EDITOR_ONLY.iter().any(|e| e.eq_ignore_ascii_case(sc)) {
                return Ok("editor-only (not a grid shortcut)".into());
            }
            // Enter on the grid moves down (nav.enter_next); cell.set is the editor's Enter.
            if id == "cell.set" {
                return Ok("editor's Enter (grid Enter is nav.enter_next)".into());
            }
            let expect = expected(id);
            register_desktop();
            let mut h = harness();
            let on = (cmd.spec.enabled)(&h.state().session);
            let before = settings::command_log();
            match sc.to_ascii_lowercase().as_str() {
                // The windowing layer turns these into clipboard events, not key presses.
                "ctrl+c" => h.event(egui::Event::Copy),
                "ctrl+x" => h.event(egui::Event::Cut),
                "ctrl+v" => h.event(egui::Event::Paste(String::new())),
                _ => {
                    let (m, k) = key_for(sc).ok_or("no key press produces this shortcut")?;
                    h.key_press_modifiers(m, k);
                }
            }
            h.run_steps(1);
            if id == "cell.edit" {
                // F2 starts editing the active cell.
                return if h.state().session.view.editing {
                    Ok("starts editing".into())
                } else {
                    Err("F2 did not start editing".into())
                };
            }
            judge(&mut h, id, expect, on, &before)
        })();
        rows.push((sc.to_string(), id.to_string(), r));
    }
    let failed: Vec<_> = rows.iter().filter(|r| r.2.is_err()).collect();
    for (sc, id, r) in &rows {
        match r {
            Ok(s) => println!("ok    {sc:<22} {id:<40} {s}"),
            Err(e) => println!("FAIL  {sc:<22} {id:<40} {e}"),
        }
    }
    println!(
        "shortcuts: {} of {} behave",
        rows.len() - failed.len(),
        rows.len()
    );
    assert!(
        failed.is_empty(),
        "{} shortcuts misbehave (see the table above)",
        failed.len()
    );
}

// ---------------------------------------------------------------------------------------
// Right-click menus

fn cell_center(h: &Harness<'_, App>, a1: &str) -> egui::Pos2 {
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

fn right_click(h: &mut Harness<'_, App>, p: egui::Pos2) {
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
fn click_new(h: &mut Harness<'_, App>, label: &str, before: &[egui::Rect]) -> Result<(), String> {
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

fn rects(h: &Harness<'_, App>, label: &str) -> Vec<egui::Rect> {
    h.query_all_by_label(label).map(|n| n.rect()).collect()
}

/// (menu path, command it runs, how to judge it)
type MenuEntry = (&'static [&'static str], &'static str, Expect);

fn check_menu(open: &dyn Fn(&mut Harness<'_, App>), entries: Vec<MenuEntry>) -> Vec<String> {
    let mut bad = Vec::new();
    for (path, id, expect) in entries {
        register_desktop();
        let mut h = harness();
        let on = registry()
            .get(id)
            .is_some_and(|c| (c.spec.enabled)(&h.state().session));
        // Labels already on screen (ribbon, sheet tabs) aren't menu entries.
        let before_rects: Vec<Vec<egui::Rect>> = path.iter().map(|l| rects(&h, l)).collect();
        open(&mut h);
        let before = settings::command_log();
        let mut r = Ok(());
        for (label, prior) in path.iter().zip(&before_rects) {
            r = click_new(&mut h, label, prior);
            if r.is_err() {
                break;
            }
        }
        let r = r.and_then(|()| judge(&mut h, id, expect, on, &before));
        match r {
            Ok(s) => println!("ok    {:<34} {id:<34} {s}", path.join(" › ")),
            Err(e) => {
                println!("FAIL  {:<34} {id:<34} {e}", path.join(" › "));
                bad.push(format!("{}: {e}", path.join(" › ")));
            }
        }
    }
    bad
}

#[test]
fn grid_right_click_menu() {
    let runs = |id| expected(id);
    let entries: Vec<MenuEntry> = vec![
        (&["Cut"], "home.clipboard.cut", runs("home.clipboard.cut")),
        (
            &["Copy"],
            "home.clipboard.copy",
            runs("home.clipboard.copy"),
        ),
        (
            &["Paste"],
            "home.clipboard.paste",
            runs("home.clipboard.paste"),
        ),
        (
            &["Paste Special…"],
            "home.clipboard.paste_special",
            runs("home.clipboard.paste_special"),
        ),
        (
            &["Insert Rows"],
            "home.cells.insert_rows",
            runs("home.cells.insert_rows"),
        ),
        (
            &["Insert Columns"],
            "home.cells.insert_columns",
            runs("home.cells.insert_columns"),
        ),
        (
            &["Delete Rows"],
            "home.cells.delete_rows",
            runs("home.cells.delete_rows"),
        ),
        (
            &["Delete Columns"],
            "home.cells.delete_columns",
            runs("home.cells.delete_columns"),
        ),
        (
            &["Clear Contents"],
            "home.editing.clear_contents",
            runs("home.editing.clear_contents"),
        ),
        (
            &["Sort", "Sort A to Z"],
            "home.editing.sort_az",
            runs("home.editing.sort_az"),
        ),
        (
            &["Sort", "Sort Z to A"],
            "home.editing.sort_za",
            runs("home.editing.sort_za"),
        ),
        (
            &["Sort", "Custom Sort…"],
            "home.editing.sort_custom",
            runs("home.editing.sort_custom"),
        ),
        (
            &["Insert Function…"],
            "formulas.insert_function",
            runs("formulas.insert_function"),
        ),
        (
            &["Define Name…"],
            "formulas.define_name",
            runs("formulas.define_name"),
        ),
        (
            &["Format Cells…"],
            "home.number.dialog",
            runs("home.number.dialog"),
        ),
        (
            &["Column Width…"],
            "home.cells.column_width",
            runs("home.cells.column_width"),
        ),
        (
            &["Row Height…"],
            "home.cells.row_height",
            runs("home.cells.row_height"),
        ),
    ];
    let open = |h: &mut Harness<'_, App>| {
        let p = cell_center(h, "B3");
        right_click(h, p);
    };
    let bad = check_menu(&open, entries);
    assert!(bad.is_empty(), "{bad:#?}");
}

#[test]
fn sheet_tab_right_click_menu() {
    let prompt = || Expect::Dialog(dialog_type("rename_sheet").unwrap());
    let entries: Vec<MenuEntry> = vec![
        (&["Insert…"], "home.cells.insert_sheet", prompt()),
        (&["Delete"], "home.cells.delete_sheet", Expect::Runs),
        (&["Rename"], "home.cells.rename_sheet", prompt()),
        (
            &["Move or Copy…"],
            "home.cells.move_copy_sheet",
            Expect::Dialog("params".into()),
        ),
        (&["Duplicate"], "sheet.duplicate", Expect::Runs),
        (
            &["Tab Color", "No Color"],
            "home.cells.tab_color",
            Expect::Runs,
        ),
        (&["Hide"], "home.cells.hide_sheet", Expect::Runs),
    ];
    let open = |h: &mut Harness<'_, App>| {
        // The sheet tab is the lowest control labelled with the sheet's name.
        let tab = h
            .query_all_by_label("Sheet1")
            .max_by(|a, b| a.rect().top().total_cmp(&b.rect().top()))
            .expect("sheet tab");
        let p = tab.rect().center();
        right_click(h, p);
    };
    let bad = check_menu(&open, entries);
    assert!(bad.is_empty(), "{bad:#?}");
    // Unhide… is greyed out until a sheet is hidden.
    let mut h = harness();
    let tab_rect = h
        .query_all_by_label("Sheet1")
        .max_by(|a, b| a.rect().top().total_cmp(&b.rect().top()))
        .unwrap()
        .rect();
    right_click(&mut h, tab_rect.center());
    let before = settings::command_log();
    let _ = click_new(&mut h, "Unhide…", &[]);
    assert!(new_log(&before).is_empty() && h.state().dialog_kind().is_none());
}

#[test]
fn key_presses_are_named_like_the_catalog() {
    use egui::Modifiers as M;
    let names = |m, k| sp_ui::shortcut_names(k, m);
    assert_eq!(names(M::COMMAND, Key::Semicolon), vec!["Ctrl+;"]);
    assert_eq!(names(M::COMMAND, Key::Minus), vec!["Ctrl+-"]);
    assert!(names(M::COMMAND | M::SHIFT, Key::Num4).contains(&"Ctrl+Shift+$".to_string()));
    assert!(names(M::COMMAND | M::SHIFT, Key::Colon).contains(&"Ctrl+Shift+;".to_string()));
    assert!(names(M::COMMAND | M::SHIFT, Key::Quote).contains(&"Ctrl+Shift+\"".to_string()));
    assert!(names(M::COMMAND | M::SHIFT, Key::Backtick).contains(&"Ctrl+Shift+~".to_string()));
    assert_eq!(names(M::NONE, Key::F2), vec!["F2"]);
    assert_eq!(names(M::COMMAND, Key::A), vec!["Ctrl+A"]);
    assert!(names(M::NONE, Key::ArrowDown) == vec!["Down"]);
}

/// A form dialog is filled in and submitted the way a user does: New Note from the ribbon,
/// type the text, OK.
#[test]
fn form_dialog_fills_in_and_runs() {
    let mut h = harness();
    h.state_mut()
        .session
        .select_range(CellRange::parse("C3").unwrap(), CellAddr::new(2, 2));
    h.run_steps(2);
    click_tab(&mut h, "Review").unwrap();
    click_in_ribbon(&mut h, "Notes", "New Note").unwrap();
    h.run_steps(3);
    assert_eq!(h.state().dialog_kind().as_deref(), Some("params"));
    // OK with the required text empty explains instead of running.
    h.get_by_label("OK").click();
    h.run_steps(3);
    assert!(
        h.state().dialog_kind().is_some(),
        "an empty required field must keep the form open"
    );
    // Type into the Text box (the one without a value yet) and confirm.
    let text_box = h
        .query_all_by_role(egui::accesskit::Role::TextInput)
        .find(|n| n.value().is_none_or(|v| v.is_empty()))
        .expect("text box");
    text_box.click();
    h.run_steps(1);
    h.event(egui::Event::Text("Check this".into()));
    h.run_steps(2);
    h.get_by_label("OK").click();
    h.run_steps(3);
    assert!(
        h.state().dialog_kind().is_none(),
        "form should close: {:?}",
        h.state().last_error()
    );
    let s = &h.state().session;
    let note = s.wb.sheet(s.sheet_index()).and_then(|sh| {
        sh.comments
            .iter()
            .find(|c| c.addr == CellAddr::new(2, 2))
            .cloned()
    });
    assert!(note.is_some_and(|n| format!("{n:?}").contains("Check this")));
}

/// Formulas › Function Library menus open Insert Function on their category, and picking a
/// function starts the formula in the active cell.
#[test]
fn function_library_buttons_open_their_category() {
    for (label, category) in [
        ("Financial", "Financial"),
        ("Date & Time", "Date & Time"),
        ("Math & Trig", "Math & Trig"),
    ] {
        let mut h = harness();
        click_tab(&mut h, "Formulas").unwrap();
        click_in_ribbon(&mut h, "Function Library", label).unwrap();
        h.run_steps(3);
        assert_eq!(
            h.state().dialog_kind().as_deref(),
            Some("insert_function"),
            "{label}"
        );
        let shown = h.query_all_by_label(category).count();
        assert!(shown > 0, "{label}: category {category} not shown");
    }
}

/// Forms whose command answers with text or a result: Use in Formula puts the name into the
/// formula being typed; Evaluate Formula stays open and shows its steps.
#[test]
fn form_inserts_text_or_shows_the_result() {
    // Use in Formula.
    let mut h = harness();
    h.state_mut()
        .session
        .execute(
            "formulas.define_name",
            &json!({"name": "Rate", "refers_to": "=Sheet1!$C$2"}),
        )
        .unwrap();
    h.state_mut()
        .open_dialog(r#"params:formulas.use_in_formula"#)
        .unwrap();
    h.run_steps(2);
    let name_box = h
        .query_all_by_role(egui::accesskit::Role::TextInput)
        .find(|n| n.value().is_none_or(|v| v.is_empty()))
        .expect("name box");
    name_box.click();
    h.run_steps(1);
    h.event(egui::Event::Text("Rate".into()));
    h.run_steps(2);
    h.get_by_label("OK").click();
    h.run_steps(3);
    assert!(h.state().dialog_kind().is_none());
    assert_eq!(h.state().edit_buf, "=Rate");

    // Evaluate Formula on E2 (=C2*D2).
    let mut h = harness();
    h.state_mut()
        .session
        .select_range(CellRange::parse("E2").unwrap(), CellAddr::new(1, 4));
    h.run_steps(1);
    h.state_mut()
        .open_dialog("params:formulas.evaluate_formula")
        .unwrap();
    h.run_steps(2);
    h.get_by_label("OK").click();
    h.run_steps(3);
    assert_eq!(
        h.state().dialog_kind().as_deref(),
        Some("params"),
        "stays open"
    );
    assert!(
        h.query_all_by_label_contains("=C2*D2").next().is_some(),
        "the steps are shown"
    );
}
