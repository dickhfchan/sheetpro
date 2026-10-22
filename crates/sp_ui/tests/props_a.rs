//! Integrated property tests, part A: every command under `home.`, `insert.`, `layout.`,
//! `cell.`, `edit.`, `nav.`, `select.` and `sheet.`, plus the grid and sheet-tab right-click
//! menus, each done the way a user does it (ribbon, menu, dialog, key, typing, right-click).
//!
//! Expectations are what the spreadsheet should do (the target app's behaviour), written
//! before running the action; the runner (`common/props.rs`) checks that exactly those
//! properties change, that undo/redo restore them, that they survive saving to .xlsx and
//! reopening, and that the shell still renders. Commands the app gets wrong in a way too big
//! to fix here are listed in `KNOWN_GAPS`; commands that change no spreadsheet property
//! (queries, editor-only helpers) in `NOT_PROPERTY`. The coverage gate at the end checks
//! that every command in these prefixes is accounted for.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::expect_used
)]
use egui_kittest::Harness;
use serde_json::json;
use sp_core::CellAddr;
use sp_engine::Session;
use sp_ui::App;
use sp_ui::dialogs::Dialog;
use sp_ui::dialogs::format_cells::{FcTab, FormatCells, NumCat};
use std::sync::OnceLock;

mod common;
use common::props::*;
use common::*;

fn rb(tab: &'static str, group: &'static str, label: &'static str) -> Act {
    Act::Ribbon(tab, group, label)
}

fn fc(app: &mut App) -> &mut FormatCells {
    match &mut app.ui.dialog {
        Some(Dialog::FormatCells(d)) => d,
        other => panic!(
            "expected Format Cells, got {:?}",
            other.as_ref().map(|_| ())
        ),
    }
}

/// The selection is `range` with the active cell at `active`.
fn sel_is(s: &Session, range: &str, active: &str) -> Result<(), String> {
    let r = s.sel.range().to_a1();
    let a = s.sel.active.to_a1();
    if r == range && a == active {
        Ok(())
    } else {
        Err(format!(
            "selection {r} (active {a}), expected {range} (active {active})"
        ))
    }
}

/// Press a key with the given modifiers on the grid.
fn key(h: &mut Harness<'_, App>, sc: &str) -> Result<(), String> {
    press(h, sc)
}

fn no_prep(_: &mut Harness<'_, App>) -> Result<(), String> {
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Home › Font

fn home_font() -> Vec<Entry> {
    vec![
        Entry::new("bold", "A1:B2", rb("Home", "Font", "Bold")).cells(
            "Sheet1!A1:B2",
            "style.bold",
            "true",
        ),
        Entry::new("bold by key", "C3", Act::Key("Ctrl+B")).cells(
            "Sheet1!C3:C3",
            "style.bold",
            "true",
        ),
        // Bold on a selection that is already bold turns it off.
        Entry::new("bold toggles off", "A1", Act::Key("Ctrl+B"))
            .setup("home.font.bold", json!({"range": "A1"}))
            .gone("Sheet1!A1.style.bold"),
        Entry::new("italic", "B2:B3", rb("Home", "Font", "Italic")).cells(
            "Sheet1!B2:B3",
            "style.italic",
            "true",
        ),
        Entry::new("italic by key", "C3", Act::Key("Ctrl+I")).cells(
            "Sheet1!C3:C3",
            "style.italic",
            "true",
        ),
        Entry::new("underline", "A2", rb("Home", "Font", "Underline")).cells(
            "Sheet1!A2:A2",
            "style.underline",
            "1",
        ),
        Entry::new("underline by key", "A3", Act::Key("Ctrl+U")).cells(
            "Sheet1!A3:A3",
            "style.underline",
            "1",
        ),
        Entry::new(
            "double underline",
            "A2",
            rb("Home", "Font", "Double Underline"),
        )
        .cells("Sheet1!A2:A2", "style.underline", "2"),
        Entry::new("strikethrough by key", "B2:B3", Act::Key("Ctrl+5")).cells(
            "Sheet1!B2:B3",
            "style.strike",
            "true",
        ),
        Entry::new(
            "font family",
            "A1:B1",
            Act::Pick("Home", "Font", "Font", "Arial"),
        )
        .cells("Sheet1!A1:B1", "style.font_name", "\"Arial\""),
        Entry::new(
            "font size",
            "A1",
            Act::Pick("Home", "Font", "Font Size", "14"),
        )
        .cells("Sheet1!A1:A1", "style.font_size_tenths", "140"),
        // 11 → 12 and 11 → 10: the next size in the size list.
        Entry::new("grow font", "A1", rb("Home", "Font", "Increase Font Size")).cells(
            "Sheet1!A1:A1",
            "style.font_size_tenths",
            "120",
        ),
        Entry::new(
            "shrink font",
            "A1",
            rb("Home", "Font", "Decrease Font Size"),
        )
        .cells("Sheet1!A1:A1", "style.font_size_tenths", "100"),
        Entry::new(
            "fill colour",
            "A1:A2",
            Act::Pick("Home", "Font", "Fill Color", "#FFFF00"),
        )
        .cells("Sheet1!A1:A2", "style.fill.0", "255")
        .cells("Sheet1!A1:A2", "style.fill.1", "255")
        .cells("Sheet1!A1:A2", "style.fill.2", "0"),
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
            "font colour",
            "B2",
            Act::Pick("Home", "Font", "Font Color", "#FF0000"),
        )
        .set("Sheet1!B2.style.font_color.0", "255")
        .set("Sheet1!B2.style.font_color.1", "0")
        .set("Sheet1!B2.style.font_color.2", "0"),
        Entry::new(
            "font colour automatic",
            "B2",
            Act::Pick("Home", "Font", "Font Color", "Automatic"),
        )
        .setup(
            "home.font.font_color",
            json!({"range": "B2", "color": "FF0000"}),
        )
        .gone("Sheet1!B2.style.font_color.*"),
        Entry::new(
            "outline border",
            "A2:B3",
            rb("Home", "Font", "Outline Border"),
        )
        .cells("Sheet1!A2:B2", "style.border_top.style", "\"Thin\"")
        .cells("Sheet1!A3:B3", "style.border_bottom.style", "\"Thin\"")
        .cells("Sheet1!A2:A3", "style.border_left.style", "\"Thin\"")
        .cells("Sheet1!B2:B3", "style.border_right.style", "\"Thin\""),
        Entry::new("outline border by key", "C4", Act::Key("Ctrl+Shift+7"))
            .set("Sheet1!C4.style.border_top.style", "\"Thin\"")
            .set("Sheet1!C4.style.border_bottom.style", "\"Thin\"")
            .set("Sheet1!C4.style.border_left.style", "\"Thin\"")
            .set("Sheet1!C4.style.border_right.style", "\"Thin\""),
        Entry::new("no border", "C4", rb("Home", "Font", "No Border"))
            .setup("home.font.border_outline", json!({"range": "C4"}))
            .gone("Sheet1!C4.style.border_top.style")
            .gone("Sheet1!C4.style.border_bottom.style")
            .gone("Sheet1!C4.style.border_left.style")
            .gone("Sheet1!C4.style.border_right.style"),
        Entry::new("no border by key", "C4", Act::Key("Ctrl+Shift+-"))
            .setup("home.font.border_outline", json!({"range": "C4"}))
            .gone("Sheet1!C4.style.border_top.style")
            .gone("Sheet1!C4.style.border_bottom.style")
            .gone("Sheet1!C4.style.border_left.style")
            .gone("Sheet1!C4.style.border_right.style"),
        Entry::new(
            "format cells font tab",
            "A1:B1",
            Act::Dialog {
                ribbon: ("Home", "Font", "Format Cells: Font…"),
                fill: |app| {
                    let d = fc(app);
                    assert_eq!(d.tab, FcTab::Font);
                    d.v.bold = true;
                    d.v.strike = true;
                    d.v.size = 16.0;
                    d.v.font = "Georgia".into();
                },
                ok: "OK",
            },
        )
        .cells("Sheet1!A1:B1", "style.bold", "true")
        .cells("Sheet1!A1:B1", "style.strike", "true")
        .cells("Sheet1!A1:B1", "style.font_size_tenths", "160")
        .cells("Sheet1!A1:B1", "style.font_name", "\"Georgia\""),
        // Ctrl+Shift+F opens the same tab.
        Entry::new(
            "format cells font by key",
            "C2",
            Act::Custom {
                prep: no_prep,
                act: |h| {
                    key(h, "Ctrl+Shift+F")?;
                    let d = fc(h.state_mut());
                    if d.tab != FcTab::Font {
                        return Err("Ctrl+Shift+F did not open the Font tab".into());
                    }
                    d.v.italic = true;
                    h.run_steps(2);
                    click_label(h, "OK")
                },
            },
        )
        .set("Sheet1!C2.style.italic", "true"),
    ]
}

/// Click the (single) control with this label.
fn click_label(h: &mut Harness<'_, App>, label: &str) -> Result<(), String> {
    use egui_kittest::kittest::Queryable;
    let n = h
        .query_all_by_label(label)
        .last()
        .ok_or_else(|| format!("no '{label}'"))?;
    n.click();
    h.run_steps(3);
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Home › Alignment

fn home_alignment() -> Vec<Entry> {
    vec![
        Entry::new("align left", "C2:C3", rb("Home", "Alignment", "Align Left")).cells(
            "Sheet1!C2:C3",
            "style.h_align",
            "\"Left\"",
        ),
        Entry::new("align center", "A1", rb("Home", "Alignment", "Center")).cells(
            "Sheet1!A1:A1",
            "style.h_align",
            "\"Center\"",
        ),
        Entry::new(
            "align right",
            "A1:B1",
            rb("Home", "Alignment", "Align Right"),
        )
        .cells("Sheet1!A1:B1", "style.h_align", "\"Right\""),
        Entry::new("align top", "A1", rb("Home", "Alignment", "Top Align")).cells(
            "Sheet1!A1:A1",
            "style.v_align",
            "\"Top\"",
        ),
        Entry::new(
            "align middle",
            "A1",
            rb("Home", "Alignment", "Middle Align"),
        )
        .cells("Sheet1!A1:A1", "style.v_align", "\"Center\""),
        // Bottom is the default: from Top back to Bottom the property is gone.
        Entry::new(
            "align bottom",
            "A1",
            rb("Home", "Alignment", "Bottom Align"),
        )
        .setup("home.align.top", json!({"range": "A1"}))
        .gone("Sheet1!A1.style.v_align"),
        Entry::new("wrap text", "B2:B3", rb("Home", "Alignment", "Wrap Text")).cells(
            "Sheet1!B2:B3",
            "style.wrap",
            "true",
        ),
        // Indenting a General-aligned cell left-aligns it.
        Entry::new(
            "increase indent",
            "A2",
            rb("Home", "Alignment", "Increase Indent"),
        )
        .set("Sheet1!A2.style.indent", "1")
        .set("Sheet1!A2.style.h_align", "\"Left\""),
        Entry::new(
            "decrease indent",
            "A2",
            rb("Home", "Alignment", "Decrease Indent"),
        )
        .setup("home.align.indent_increase", json!({"range": "A2"}))
        .setup("home.align.indent_increase", json!({"range": "A2"}))
        .set("Sheet1!A2.style.indent", "1"),
        Entry::new(
            "orientation",
            "A1:B1",
            Act::Dialog {
                ribbon: ("Home", "Alignment", "Orientation"),
                fill: |app| {
                    let d = fc(app);
                    assert_eq!(d.tab, FcTab::Alignment);
                    d.v.rotation = 45;
                },
                ok: "OK",
            },
        )
        .cells("Sheet1!A1:B1", "style.rotation", "45"),
        Entry::new(
            "merge and centre",
            "A8:C8",
            rb("Home", "Alignment", "Merge & Center"),
        )
        .set("Sheet1.merges.0.start.row", "7")
        .set("Sheet1.merges.0.start.col", "0")
        .set("Sheet1.merges.0.end.row", "7")
        .set("Sheet1.merges.0.end.col", "2")
        .cells("Sheet1!A8:A8", "style.h_align", "\"Center\"")
        .print(),
        // Merge Cells keeps the alignment; only the top-left value survives.
        Entry::new(
            "merge cells",
            "A8:B9",
            rb("Home", "Alignment", "Merge Cells"),
        )
        .setup("cell.set", json!({"cell": "A8", "input": "keep"}))
        .set("Sheet1.merges.0.start.row", "7")
        .set("Sheet1.merges.0.start.col", "0")
        .set("Sheet1.merges.0.end.row", "8")
        .set("Sheet1.merges.0.end.col", "1"),
        // Merge Across: one merge per row.
        Entry::new(
            "merge across",
            "A8:C9",
            rb("Home", "Alignment", "Merge Across"),
        )
        .set("Sheet1.merges.0.start.row", "7")
        .set("Sheet1.merges.0.start.col", "0")
        .set("Sheet1.merges.0.end.row", "7")
        .set("Sheet1.merges.0.end.col", "2")
        .set("Sheet1.merges.1.start.row", "8")
        .set("Sheet1.merges.1.start.col", "0")
        .set("Sheet1.merges.1.end.row", "8")
        .set("Sheet1.merges.1.end.col", "2"),
        Entry::new("unmerge", "A8", rb("Home", "Alignment", "Unmerge Cells"))
            .setup("home.align.merge_cells", json!({"range": "A8:C8"}))
            .gone("Sheet1.merges.0.start.row")
            .gone("Sheet1.merges.0.start.col")
            .gone("Sheet1.merges.0.end.row")
            .gone("Sheet1.merges.0.end.col"),
    ]
}

// ---------------------------------------------------------------------------------------
// Home › Number

fn home_number() -> Vec<Entry> {
    let fmt = |name: &'static str, sel: &'static str, act: Act, range: &str, code: &str| {
        Entry::new(name, sel, act).cells(range, "style.num_fmt", code)
    };
    vec![
        Entry::new("general", "D2", rb("Home", "Number", "General"))
            .setup("home.number.percent", json!({"range": "D2"}))
            .gone("Sheet1!D2.style.num_fmt"),
        Entry::new("general by key", "D2", Act::Key("Ctrl+Shift+~"))
            .setup("home.number.percent", json!({"range": "D2"}))
            .gone("Sheet1!D2.style.num_fmt"),
        // Number, Currency, Short Date and Time are one command each, shared by the ribbon
        // and the shortcut, and apply the shortcut's format (Ctrl+Shift+! #,##0.00,
        // Ctrl+Shift+$ built-in currency, Ctrl+Shift+# d-mmm-yy, Ctrl+Shift+@ h:mm AM/PM).
        // The target's ribbon drop-down gives 0.00, "$"#,##0.00, m/d/yyyy and
        // h:mm:ss AM/PM instead: reported, and proved here through the keys.
        fmt(
            "number by key",
            "D2",
            Act::Key("Ctrl+Shift+!"),
            "Sheet1!D2:D2",
            "\"#,##0.00\"",
        ),
        fmt(
            "currency by key",
            "D2:D3",
            Act::Key("Ctrl+Shift+$"),
            "Sheet1!D2:D3",
            "\"\\\"$\\\"#,##0.00_);(\\\"$\\\"#,##0.00)\"",
        ),
        fmt(
            "accounting",
            "D2",
            rb("Home", "Number", "Accounting Number Format"),
            "Sheet1!D2:D2",
            "\"_(\\\"$\\\"* #,##0.00_);_(\\\"$\\\"* \\\\(#,##0.00\\\\);_(\\\"$\\\"* \\\"-\\\"??_);_(@_)\"",
        ),
        fmt(
            "percent",
            "D2:D3",
            rb("Home", "Number", "Percent Style"),
            "Sheet1!D2:D3",
            "\"0%\"",
        ),
        fmt(
            "percent by key",
            "D2",
            Act::Key("Ctrl+Shift+%"),
            "Sheet1!D2:D2",
            "\"0%\"",
        ),
        fmt(
            "comma",
            "C2",
            rb("Home", "Number", "Comma Style"),
            "Sheet1!C2:C2",
            "\"_(* #,##0.00_);_(* \\\\(#,##0.00\\\\);_(* \\\"-\\\"??_);_(@_)\"",
        ),
        fmt(
            "date by key",
            "C2",
            Act::Key("Ctrl+Shift+#"),
            "Sheet1!C2:C2",
            "\"d-mmm-yy\"",
        ),
        fmt(
            "time by key",
            "C2",
            Act::Key("Ctrl+Shift+@"),
            "Sheet1!C2:C2",
            "\"h:mm AM/PM\"",
        ),
        fmt(
            "scientific",
            "C2",
            rb("Home", "Number", "Scientific"),
            "Sheet1!C2:C2",
            "\"0.00E+00\"",
        ),
        fmt(
            "scientific by key",
            "C2",
            Act::Key("Ctrl+Shift+^"),
            "Sheet1!C2:C2",
            "\"0.00E+00\"",
        ),
        fmt(
            "text format",
            "C2",
            rb("Home", "Number", "Text"),
            "Sheet1!C2:C2",
            "\"@\"",
        ),
        // Increase Decimal starts from what the cell shows: 10 → 10.0, 2.25 → 2.250.
        fmt(
            "increase decimal",
            "C2",
            rb("Home", "Number", "Increase Decimal"),
            "Sheet1!C2:C2",
            "\"0.0\"",
        ),
        fmt(
            "increase decimal from shown decimals",
            "D3",
            rb("Home", "Number", "Increase Decimal"),
            "Sheet1!D3:D3",
            "\"0.000\"",
        ),
        fmt(
            "decrease decimal",
            "D3",
            rb("Home", "Number", "Decrease Decimal"),
            "Sheet1!D3:D3",
            "\"0.0\"",
        ),
        Entry::new(
            "number format dialog",
            "D2:D3",
            Act::Dialog {
                ribbon: ("Home", "Number", "Number Format"),
                fill: |app| {
                    let d = fc(app);
                    assert_eq!(d.tab, FcTab::Number);
                    d.v.category = NumCat::Percentage;
                    d.v.decimals = 1;
                },
                ok: "OK",
            },
        )
        .cells("Sheet1!D2:D3", "style.num_fmt", "\"0.0%\""),
        Entry::new(
            "format cells dialog",
            "C2",
            Act::Dialog {
                ribbon: ("Home", "Number", "Format Cells…"),
                fill: |app| {
                    let d = fc(app);
                    d.v.category = NumCat::Number;
                    d.v.decimals = 3;
                    d.v.thousands = true;
                },
                ok: "OK",
            },
        )
        .set("Sheet1!C2.style.num_fmt", "\"#,##0.000\""),
        // Ctrl+1 opens Format Cells; its Border tab draws edges.
        Entry::new(
            "format cells by key, border",
            "B2:C3",
            Act::Custom {
                prep: no_prep,
                act: |h| {
                    key(h, "Ctrl+1")?;
                    let d = fc(h.state_mut());
                    d.tab = FcTab::Border;
                    d.border.style = "double".into();
                    d.border.edges[1] = Some("double".into());
                    h.run_steps(2);
                    click_label(h, "OK")
                },
            },
        )
        .cells("Sheet1!B3:C3", "style.border_bottom.style", "\"Double\""),
    ]
}

/// Pins for typed cell contents: `put` a constant (its input and the value it shows),
/// `formula` a formula and its value, `rows` a block of constants from `top` (A1).
trait Pin: Sized {
    fn put(self, cell: &str, input: &str) -> Self;
    fn formula(self, cell: &str, f: &str, value: &str) -> Self;
    fn rows(self, sheet: &str, top: &str, rows: &[&[&str]]) -> Self;
}

fn num(n: f64) -> String {
    serde_json::Number::from_f64(n).map_or_else(|| n.to_string(), |x| x.to_string())
}

impl Pin for Entry {
    fn put(self, cell: &str, input: &str) -> Self {
        let e = self.set(&format!("{cell}.input"), &format!("{input:?}"));
        match input.parse::<f64>() {
            Ok(n) => e.set(&format!("{cell}.value"), &num(n)),
            Err(_) => e.set(&format!("{cell}.value"), &format!("{input:?}")),
        }
    }
    fn formula(self, cell: &str, f: &str, value: &str) -> Self {
        self.set(&format!("{cell}.input"), &format!("{f:?}"))
            .set(&format!("{cell}.value"), value)
    }
    fn rows(mut self, sheet: &str, top: &str, rows: &[&[&str]]) -> Self {
        let t = CellAddr::parse(top).unwrap();
        for (r, row) in rows.iter().enumerate() {
            for (c, v) in row.iter().enumerate() {
                let a = CellAddr::new(t.row + r as u32, t.col + c as u32).to_a1();
                self = self.put(&format!("{sheet}!{a}"), v);
            }
        }
        self
    }
}

/// Right-click `cell` and choose `path` (a submenu, then its entry).
fn grid_menu(h: &mut Harness<'_, App>, cell: &str, path: &[&str]) -> Result<(), String> {
    let before: Vec<Vec<egui::Rect>> = path.iter().map(|l| rects(h, l)).collect();
    let p = cell_center(h, cell);
    right_click(h, p);
    for (label, prior) in path.iter().zip(&before) {
        click_new(h, label, prior)?;
    }
    Ok(())
}

/// Right-click the tab of `sheet` and choose `path`.
fn tab_menu(h: &mut Harness<'_, App>, sheet: &str, path: &[&str]) -> Result<(), String> {
    use egui_kittest::kittest::Queryable;
    let before: Vec<Vec<egui::Rect>> = path.iter().map(|l| rects(h, l)).collect();
    let p = h
        .query_all_by_label(sheet)
        .max_by(|a, b| a.rect().top().total_cmp(&b.rect().top()))
        .ok_or("no sheet tab")?
        .rect()
        .center();
    right_click(h, p);
    for (label, prior) in path.iter().zip(&before) {
        click_new(h, label, prior)?;
    }
    Ok(())
}

/// The open dialog, as the variant the caller expects.
/// A new threaded comment "Check this" on C3 (the author is the user's name in settings).
fn thread_on_c3(e: Entry) -> Entry {
    e.set("Sheet1.comments.0.addr.row", "2")
        .set("Sheet1.comments.0.addr.col", "2")
        .set("Sheet1.comments.0.kind", "\"Thread\"")
        .set("Sheet1.comments.0.text", "\"Check this\"")
        .set("Sheet1.comments.0.visible", "false")
        .set("Sheet1.comments.0.resolved", "false")
        .allow("Sheet1.comments.0.author")
}

macro_rules! dialog {
    ($app:expr, $variant:ident) => {
        match &mut $app.ui.dialog {
            Some(Dialog::$variant(d)) => d,
            other => panic!(
                "expected the {} dialog, got {:?}",
                stringify!($variant),
                other
                    .as_ref()
                    .map(|d| format!("{d:?}").chars().take(40).collect::<String>())
            ),
        }
    };
}

/// The base data sorted (rows 2..6 of A:D), for the sort entries.
const BY_PRODUCT: [&[&str]; 5] = [
    &["East", "Apples", "10", "1.5"],
    &["North", "Apples", "12", "1.5"],
    &["West", "Apples", "3", "1.5"],
    &["West", "Pears", "4", "2.25"],
    &["East", "Plums", "7", "3"],
];

// ---------------------------------------------------------------------------------------
// Home › Clipboard

fn home_clipboard() -> Vec<Entry> {
    let copy_c2 = ("home.clipboard.copy", json!({"range": "Sheet1!C2:D2"}));
    vec![
        // Copy and Cut change nothing until the paste; they fill the clipboard.
        Entry::new("copy", "A2:B3", rb("Home", "Clipboard", "Copy")).then(|s| {
            s.clipboard
                .as_ref()
                .map(|_| ())
                .ok_or("clipboard empty".into())
        }),
        Entry::new("copy by key", "A2", Act::Key("Ctrl+C")).then(|s| {
            s.clipboard
                .as_ref()
                .map(|_| ())
                .ok_or("clipboard empty".into())
        }),
        Entry::new("cut", "A2", rb("Home", "Clipboard", "Cut")).then(|s| {
            s.clipboard
                .as_ref()
                .map(|_| ())
                .ok_or("clipboard empty".into())
        }),
        Entry::new("paste", "H2", rb("Home", "Clipboard", "Paste"))
            .setup(copy_c2.0, copy_c2.1.clone())
            .put("Sheet1!H2", "10")
            .put("Sheet1!I2", "1.5"),
        // A formula pasted elsewhere keeps its relative shape.
        Entry::new("paste by key", "H3", Act::Key("Ctrl+V"))
            .setup("home.clipboard.copy", json!({"range": "E2"}))
            .formula("Sheet1!H3", "=F3*G3", "0.0"),
        // Cut and paste moves the cells; formulas that pointed at them follow.
        Entry::new("cut and paste", "H2", Act::Key("Ctrl+V"))
            .setup("home.clipboard.cut", json!({"range": "C2"}))
            .put("Sheet1!H2", "10")
            .gone("Sheet1!C2.input")
            .gone("Sheet1!C2.value")
            .set("Sheet1!E2.input", "\"=H2*D2\""),
        Entry::new(
            "paste values",
            "H2",
            rb("Home", "Clipboard", "Paste Values"),
        )
        .setup("home.clipboard.copy", json!({"range": "E2:E3"}))
        .put("Sheet1!H2", "15")
        .put("Sheet1!H3", "9"),
        Entry::new(
            "paste formulas",
            "H2",
            rb("Home", "Clipboard", "Paste Formulas"),
        )
        .setup("home.font.bold", json!({"range": "E2"}))
        .setup("home.clipboard.copy", json!({"range": "E2"}))
        .formula("Sheet1!H2", "=F2*G2", "0.0"),
        Entry::new(
            "paste formatting",
            "H2",
            rb("Home", "Clipboard", "Paste Formatting"),
        )
        .setup("home.font.bold", json!({"range": "C2"}))
        .setup("home.clipboard.copy", json!({"range": "C2"}))
        .set("Sheet1!H2.style.bold", "true"),
        Entry::new(
            "paste transpose",
            "H1",
            rb("Home", "Clipboard", "Paste Transpose"),
        )
        .setup("home.clipboard.copy", json!({"range": "A1:C1"}))
        .put("Sheet1!H1", "Region")
        .put("Sheet1!H2", "Product")
        .put("Sheet1!H3", "Qty"),
        // Paste Link of one cell refers to it absolutely.
        Entry::new("paste link", "H2", rb("Home", "Clipboard", "Paste Link"))
            .setup("home.clipboard.copy", json!({"range": "C2"}))
            .formula("Sheet1!H2", "=$C$2", "10.0"),
        Entry::new(
            "paste special values",
            "H2",
            Act::Dialog {
                ribbon: ("Home", "Clipboard", "Paste Special…"),
                fill: |app| dialog!(app, PasteSpecial).mode = "values".into(),
                ok: "OK",
            },
        )
        .setup("home.clipboard.copy", json!({"range": "E2"}))
        .put("Sheet1!H2", "15"),
        Entry::new(
            "paste special transpose by key",
            "H1",
            Act::Custom {
                prep: no_prep,
                act: |h| {
                    key(h, "Ctrl+Alt+V")?;
                    dialog!(h.state_mut(), PasteSpecial).transpose = true;
                    h.run_steps(2);
                    click_label(h, "OK")
                },
            },
        )
        .setup("home.clipboard.copy", json!({"range": "A2:B2"}))
        .put("Sheet1!H1", "East")
        .put("Sheet1!H2", "Apples"),
        // Format Painter: arm it on A1, then click the cell to format.
        Entry::new(
            "format painter",
            "A1",
            Act::Custom {
                prep: no_prep,
                act: |h| {
                    click_in_ribbon(h, "Clipboard", "Format Painter")?;
                    h.run_steps(2);
                    click_cell(h, "H2");
                    Ok(())
                },
            },
        )
        .setup("home.font.bold", json!({"range": "A1"}))
        .setup("home.align.center", json!({"range": "A1"}))
        .set("Sheet1!H2.style.bold", "true")
        .set("Sheet1!H2.style.h_align", "\"Center\""),
    ]
}

// ---------------------------------------------------------------------------------------
// Home › Cells

fn home_cells() -> Vec<Entry> {
    let two = ("home.cells.insert_sheet", json!({"name": "Two"}));
    let back = ("sheet.activate", json!({"sheet": "Sheet1"}));
    vec![
        // Inserting a row moves cells down and rewrites references that span it.
        Entry::new("insert row", "A3", rb("Home", "Cells", "Insert Sheet Rows"))
            .set("Sheet1!G1.input", "\"=SUM(E2:E7)\"")
            .put("Sheet1!A4", "West")
            .put("Sheet1!B4", "Pears")
            .set("Sheet1!E4.input", "\"=C4*D4\"")
            .set("Sheet1!E7.input", "\"=C7*D7\"")
            .gone("Sheet1!A3.input")
            .gone("Sheet1!E3.input")
            // The other shifted cells of A:E.
            .allow("Sheet1!A*")
            .allow("Sheet1!B*")
            .allow("Sheet1!C*")
            .allow("Sheet1!D*")
            .allow("Sheet1!E*"),
        Entry::new(
            "insert column",
            "B2",
            rb("Home", "Cells", "Insert Sheet Columns"),
        )
        .gone("Sheet1!B1.input")
        .put("Sheet1!C1", "Product")
        .put("Sheet1!F1", "Total")
        .set("Sheet1!F2.input", "\"=D2*E2\"")
        .gone("Sheet1!G1.input")
        .formula("Sheet1!H1", "=SUM(F2:F6)", "67.5")
        // The other shifted cells of B:F.
        .allow("Sheet1!B*")
        .allow("Sheet1!C*")
        .allow("Sheet1!D*")
        .allow("Sheet1!E*")
        .allow("Sheet1!F*")
        .allow("Sheet1!G1.value"),
        Entry::new("delete row", "A3", rb("Home", "Cells", "Delete Sheet Rows"))
            .put("Sheet1!A3", "East")
            .put("Sheet1!B3", "Plums")
            .set("Sheet1!E3.input", "\"=C3*D3\"")
            .formula("Sheet1!G1", "=SUM(E2:E5)", "58.5")
            .gone("Sheet1!A6.input")
            .gone("Sheet1!E6.input")
            // The other cells of rows 3..6 move up.
            .allow("Sheet1!A*")
            .allow("Sheet1!B*")
            .allow("Sheet1!C*")
            .allow("Sheet1!D*")
            .allow("Sheet1!E*"),
        Entry::new(
            "delete column",
            "A1",
            rb("Home", "Cells", "Delete Sheet Columns"),
        )
        .put("Sheet1!A1", "Product")
        .set("Sheet1!D2.input", "\"=B2*C2\"")
        .formula("Sheet1!F1", "=SUM(D2:D6)", "67.5")
        .gone("Sheet1!G1.input")
        .gone("Sheet1!E2.input")
        // The other cells of B:E move left.
        .allow("Sheet1!A*")
        .allow("Sheet1!B*")
        .allow("Sheet1!C*")
        .allow("Sheet1!D*")
        .allow("Sheet1!E*")
        .allow("Sheet1!G1.value"),
        // Insert / Delete Cells on whole rows insert / delete them (partial ranges: see
        // KNOWN_GAPS).
        Entry::new(
            "insert cells",
            "A3:XFD3",
            rb("Home", "Cells", "Insert Cells…"),
        )
        .set("Sheet1!G1.input", "\"=SUM(E2:E7)\"")
        .put("Sheet1!A4", "West")
        .gone("Sheet1!A3.input")
        .allow("Sheet1!A*")
        .allow("Sheet1!B*")
        .allow("Sheet1!C*")
        .allow("Sheet1!D*")
        .allow("Sheet1!E*"),
        Entry::new("insert cells by key", "A3:XFD3", Act::Key("Ctrl+Shift+="))
            .set("Sheet1!G1.input", "\"=SUM(E2:E7)\"")
            .put("Sheet1!A4", "West")
            .gone("Sheet1!A3.input")
            .allow("Sheet1!A*")
            .allow("Sheet1!B*")
            .allow("Sheet1!C*")
            .allow("Sheet1!D*")
            .allow("Sheet1!E*"),
        Entry::new(
            "delete cells",
            "A3:XFD3",
            rb("Home", "Cells", "Delete Cells…"),
        )
        .put("Sheet1!A3", "East")
        .formula("Sheet1!G1", "=SUM(E2:E5)", "58.5")
        .gone("Sheet1!A6.input")
        .allow("Sheet1!A*")
        .allow("Sheet1!B*")
        .allow("Sheet1!C*")
        .allow("Sheet1!D*")
        .allow("Sheet1!E*"),
        Entry::new("delete cells by key", "A3:XFD3", Act::Key("Ctrl+-"))
            .put("Sheet1!A3", "East")
            .formula("Sheet1!G1", "=SUM(E2:E5)", "58.5")
            .gone("Sheet1!A6.input")
            .allow("Sheet1!A*")
            .allow("Sheet1!B*")
            .allow("Sheet1!C*")
            .allow("Sheet1!D*")
            .allow("Sheet1!E*"),
        Entry::new(
            "row height",
            "B2",
            Act::Dialog {
                ribbon: ("Home", "Cells", "Row Height…"),
                fill: |app| dialog!(app, Prompt).value = "24".into(),
                ok: "OK",
            },
        )
        .set("Sheet1.row_heights.1", "24.0")
        .print(),
        Entry::new(
            "column width",
            "B2:C2",
            Act::Dialog {
                ribbon: ("Home", "Cells", "Column Width…"),
                fill: |app| dialog!(app, Prompt).value = "15".into(),
                ok: "OK",
            },
        )
        .set("Sheet1.col_widths.1", "15.0")
        .set("Sheet1.col_widths.2", "15.0")
        .print(),
        Entry::new(
            "default width",
            "A1",
            Act::Dialog {
                ribbon: ("Home", "Cells", "Default Width…"),
                fill: |app| dialog!(app, Prompt).value = "12".into(),
                ok: "OK",
            },
        )
        .set("Sheet1.default_col_width", "12.0"),
        // AutoFit row: back to the height the font needs (the default).
        Entry::new(
            "autofit row",
            "A2",
            rb("Home", "Cells", "AutoFit Row Height"),
        )
        .setup(
            "home.cells.row_height",
            json!({"range": "A2", "height": 40}),
        )
        .gone("Sheet1.row_heights.1"),
        // AutoFit column: as wide as "Product" / "Apples" need (a measured width).
        Entry::new(
            "autofit column",
            "B1",
            rb("Home", "Cells", "AutoFit Column Width"),
        )
        .setup(
            "home.cells.column_width",
            json!({"range": "B1", "width": 30}),
        )
        .allow("Sheet1.col_widths.1")
        .then(|s| {
            let w = s.wb.sheets[0].col_width(1);
            if (5.0..10.0).contains(&w) {
                Ok(())
            } else {
                Err(format!("autofit width {w}"))
            }
        }),
        Entry::new("hide rows", "A3:B4", rb("Home", "Cells", "Hide Rows"))
            .set("Sheet1.hidden_rows.0", "2")
            .set("Sheet1.hidden_rows.1", "3")
            .print(),
        Entry::new("hide rows by key", "A3", Act::Key("Ctrl+9")).set("Sheet1.hidden_rows.0", "2"),
        Entry::new("hide columns", "B2", rb("Home", "Cells", "Hide Columns"))
            .set("Sheet1.hidden_cols.0", "1")
            .print(),
        Entry::new("hide columns by key", "C2", Act::Key("Ctrl+0"))
            .set("Sheet1.hidden_cols.0", "2"),
        Entry::new("unhide rows", "A2:A5", rb("Home", "Cells", "Unhide Rows"))
            .setup("home.cells.hide_rows", json!({"range": "A3:A4"}))
            .gone("Sheet1.hidden_rows.0")
            .gone("Sheet1.hidden_rows.1"),
        Entry::new("unhide rows by key", "A2:A4", Act::Key("Ctrl+Shift+9"))
            .setup("home.cells.hide_rows", json!({"range": "A3"}))
            .gone("Sheet1.hidden_rows.0"),
        Entry::new(
            "unhide columns",
            "A1:D1",
            rb("Home", "Cells", "Unhide Columns"),
        )
        .setup("home.cells.hide_columns", json!({"range": "B1:C1"}))
        .gone("Sheet1.hidden_cols.0")
        .gone("Sheet1.hidden_cols.1"),
        // A new sheet goes before the active one, gets the next free name and is shown.
        Entry::new("insert sheet", "A1", rb("Home", "Cells", "Insert Sheet"))
            .set("workbook.sheet_order.0", "\"Sheet2\"")
            .set("workbook.sheet_order.1", "\"Sheet1\"")
            .set("workbook.active", "0")
            // The new sheet's own (default) properties.
            .allow("Sheet2.*"),
        Entry::new("insert sheet by key", "A1", Act::Key("Shift+F11"))
            .set("workbook.sheet_order.0", "\"Sheet2\"")
            .set("workbook.sheet_order.1", "\"Sheet1\"")
            .set("workbook.active", "0")
            .allow("Sheet2.*"),
        Entry::new("delete sheet", "A1", rb("Home", "Cells", "Delete Sheet"))
            .setup(two.0, two.1.clone())
            // Two was inserted before Sheet1 and is the active sheet.
            .set("workbook.sheet_order.0", "\"Sheet1\"")
            .gone("workbook.sheet_order.1")
            .set("workbook.active", "0")
            // The deleted sheet's properties.
            .allow("Two.*"),
        Entry::new(
            "rename sheet",
            "A1",
            Act::Dialog {
                ribbon: ("Home", "Cells", "Rename Sheet"),
                fill: |app| dialog!(app, Prompt).value = "Data".into(),
                ok: "OK",
            },
        )
        .setup(two.0, two.1.clone())
        .set("workbook.sheet_order.0", "\"Data\"")
        // The sheet's properties move from `Two.` to `Data.`.
        .allow("Two.*")
        .allow("Data.*"),
        Entry::new(
            "move or copy sheet",
            "A1",
            Act::Dialog {
                ribbon: ("Home", "Cells", "Move or Copy Sheet…"),
                fill: |app| {
                    let d = dialog!(app, MoveCopy);
                    d.copy = true;
                    d.before = d.sheets.len();
                },
                ok: "OK",
            },
        )
        .setup(two.0, two.1.clone())
        .setup(back.0, back.1.clone())
        .set("workbook.sheet_order.2", "\"Sheet1 (2)\"")
        .set("workbook.active", "2")
        .put("Sheet1 (2)!A1", "Region")
        .formula("Sheet1 (2)!G1", "=SUM(E2:E6)", "67.5")
        // The rest of the copy: every cell and property of Sheet1.
        .allow("Sheet1 (2)*"),
        Entry::new("hide sheet", "A1", rb("Home", "Cells", "Hide Sheet"))
            .setup(two.0, two.1.clone())
            .set("Two.visibility", "\"Hidden\"")
            // Two (first, active) is hidden: Sheet1 is shown.
            .set("workbook.active", "1"),
        Entry::new(
            "unhide sheet",
            "A1",
            Act::Dialog {
                ribbon: ("Home", "Cells", "Unhide Sheet…"),
                fill: |app| dialog!(app, UnhideSheet).selected = 0,
                ok: "OK",
            },
        )
        .setup(two.0, two.1.clone())
        .setup("home.cells.hide_sheet", json!({"sheet": "Two"}))
        .set("Two.visibility", "\"Visible\"")
        // The unhidden sheet is shown.
        .set("workbook.active", "0"),
        Entry::new(
            "tab colour",
            "A1",
            Act::Custom {
                prep: no_prep,
                act: |h| {
                    click_in_ribbon(h, "Cells", "Tab Color")?;
                    h.run_steps(3);
                    click_new(h, "#FF0000", &[])
                },
            },
        )
        .set("Sheet1.tab_color.0", "255")
        .set("Sheet1.tab_color.1", "0")
        .set("Sheet1.tab_color.2", "0"),
        // Protect Sheet: the sheet is protected with the default permissions (select
        // locked and unlocked cells only).
        Entry::new("protect sheet", "A1", rb("Home", "Cells", "Protect Sheet…"))
            .set("Sheet1.protection.select_locked", "true")
            .set("Sheet1.protection.select_unlocked", "true")
            .set("Sheet1.protection.format_cells", "false")
            .set("Sheet1.protection.insert_rows", "false")
            .set("Sheet1.protection.delete_rows", "false")
            .set("Sheet1.protection.sort", "false")
            // The remaining permission flags, all off.
            .allow("Sheet1.protection.*"),
        Entry::new("lock cell", "A2:A3", rb("Home", "Cells", "Lock Cell")).cells(
            "Sheet1!A2:A3",
            "style.locked",
            "false",
        ),
    ]
}

// ---------------------------------------------------------------------------------------
// Home › Editing

fn home_editing() -> Vec<Entry> {
    vec![
        // AutoSum over a column selection writes the total below it.
        Entry::new("autosum", "C2:C6", rb("Home", "Editing", "AutoSum")).formula(
            "Sheet1!C7",
            "=SUM(C2:C6)",
            "36.0",
        ),
        // With one cell selected under numbers, Alt+= sums the numbers above.
        Entry::new("autosum by key", "D7", Act::Key("Alt+=")).formula(
            "Sheet1!D7",
            "=SUM(D2:D6)",
            "9.75",
        ),
        Entry::new("average", "D2:D6", rb("Home", "Editing", "Average")).formula(
            "Sheet1!D7",
            "=AVERAGE(D2:D6)",
            "1.95",
        ),
        Entry::new(
            "count numbers",
            "C2:C6",
            rb("Home", "Editing", "Count Numbers"),
        )
        .formula("Sheet1!C7", "=COUNT(C2:C6)", "5.0"),
        Entry::new("max", "C2:C6", rb("Home", "Editing", "Max")).formula(
            "Sheet1!C7",
            "=MAX(C2:C6)",
            "12.0",
        ),
        Entry::new("min", "C2:C6", rb("Home", "Editing", "Min")).formula(
            "Sheet1!C7",
            "=MIN(C2:C6)",
            "3.0",
        ),
        Entry::new("fill down", "H1:H3", rb("Home", "Editing", "Fill Down"))
            .setup("cell.set", json!({"cell": "H1", "input": "7"}))
            .put("Sheet1!H2", "7")
            .put("Sheet1!H3", "7"),
        Entry::new("fill down by key", "E6:E7", Act::Key("Ctrl+D")).formula(
            "Sheet1!E7",
            "=C7*D7",
            "0.0",
        ),
        Entry::new("fill right", "H1:J1", rb("Home", "Editing", "Fill Right"))
            .setup("cell.set", json!({"cell": "H1", "input": "x"}))
            .put("Sheet1!I1", "x")
            .put("Sheet1!J1", "x"),
        Entry::new("fill right by key", "E2:F2", Act::Key("Ctrl+R")).formula(
            "Sheet1!F2",
            "=D2*E2",
            "22.5",
        ),
        Entry::new("fill up", "H1:H3", rb("Home", "Editing", "Fill Up"))
            .setup("cell.set", json!({"cell": "H3", "input": "5"}))
            .put("Sheet1!H1", "5")
            .put("Sheet1!H2", "5"),
        Entry::new("fill left", "H1:J1", rb("Home", "Editing", "Fill Left"))
            .setup("cell.set", json!({"cell": "J1", "input": "y"}))
            .put("Sheet1!H1", "y")
            .put("Sheet1!I1", "y"),
        // Series with the defaults: a linear step of 1 down the selection.
        Entry::new("fill series", "H1:H4", rb("Home", "Editing", "Series…"))
            .setup("cell.set", json!({"cell": "H1", "input": "1"}))
            .put("Sheet1!H2", "2")
            .put("Sheet1!H3", "3")
            .put("Sheet1!H4", "4"),
        // Flash Fill learns "lower-case the region" from one example, in the column next
        // to the data.
        Entry::new("flash fill", "F3", Act::Key("Ctrl+E"))
            .setup("cell.set", json!({"cell": "F1", "input": "Lower"}))
            .setup("cell.set", json!({"cell": "F2", "input": "east"}))
            .put("Sheet1!F3", "west")
            .put("Sheet1!F4", "east")
            .put("Sheet1!F5", "north")
            .put("Sheet1!F6", "west"),
        Entry::new(
            "flash fill from the ribbon",
            "F3",
            rb("Home", "Editing", "Flash Fill"),
        )
        .setup("cell.set", json!({"cell": "F1", "input": "Lower"}))
        .setup("cell.set", json!({"cell": "F2", "input": "east"}))
        .put("Sheet1!F3", "west")
        .put("Sheet1!F4", "east")
        .put("Sheet1!F5", "north")
        .put("Sheet1!F6", "west"),
        Entry::new("clear all", "A2", rb("Home", "Editing", "Clear All"))
            .setup("home.font.bold", json!({"range": "A2"}))
            .gone("Sheet1!A2.input")
            .gone("Sheet1!A2.value")
            .gone("Sheet1!A2.style.bold"),
        Entry::new(
            "clear formats",
            "A2",
            rb("Home", "Editing", "Clear Formats"),
        )
        .setup("home.font.bold", json!({"range": "A2"}))
        .setup(
            "home.font.fill_color",
            json!({"range": "A2", "color": "FFFF00"}),
        )
        .gone("Sheet1!A2.style.bold")
        .gone("Sheet1!A2.style.fill.*"),
        // Clearing a number recalculates what depends on it.
        Entry::new(
            "clear contents",
            "C2",
            rb("Home", "Editing", "Clear Contents"),
        )
        .gone("Sheet1!C2.input")
        .gone("Sheet1!C2.value")
        .set("Sheet1!E2.value", "0.0")
        .set("Sheet1!G1.value", "52.5"),
        Entry::new("clear contents by key", "A2:B2", Act::Key("Delete"))
            .gone("Sheet1!A2.input")
            .gone("Sheet1!A2.value")
            .gone("Sheet1!B2.input")
            .gone("Sheet1!B2.value"),
        Entry::new(
            "clear comments and notes",
            "A1:B2",
            rb("Home", "Editing", "Clear Comments and Notes"),
        )
        .setup("insert.note", json!({"cell": "A1", "text": "n"}))
        .gone("Sheet1.comments.*"),
        // Clear Hyperlinks removes the link and keeps the cell's formatting.
        Entry::new(
            "clear hyperlinks",
            "A1",
            rb("Home", "Editing", "Clear Hyperlinks"),
        )
        .setup(
            "insert.hyperlink",
            json!({"cell": "A1", "address": "https://example.com/"}),
        )
        .gone("Sheet1.hyperlinks.*"),
        // Sort the region around the active cell by its column, header kept on top.
        Entry::new("sort a to z", "B3", rb("Home", "Editing", "Sort A to Z"))
            .rows("Sheet1", "A2", &BY_PRODUCT)
            .set("Sheet1!E3.value", "18.0")
            .set("Sheet1!E4.value", "4.5")
            .set("Sheet1!E5.value", "9.0")
            .set("Sheet1!E6.value", "21.0"),
        Entry::new("sort z to a", "B3", rb("Home", "Editing", "Sort Z to A"))
            .rows(
                "Sheet1",
                "A2",
                &[
                    &["East", "Plums", "7", "3"],
                    &["West", "Pears", "4", "2.25"],
                    &["East", "Apples", "10", "1.5"],
                    &["North", "Apples", "12", "1.5"],
                    &["West", "Apples", "3", "1.5"],
                ],
            )
            .set("Sheet1!E2.value", "21.0")
            .set("Sheet1!E3.value", "9.0")
            .set("Sheet1!E4.value", "15.0")
            .set("Sheet1!E5.value", "18.0")
            .set("Sheet1!E6.value", "4.5"),
        Entry::new(
            "custom sort",
            "B3",
            Act::Dialog {
                ribbon: ("Home", "Editing", "Custom Sort…"),
                fill: |app| {
                    let d = dialog!(app, Sort);
                    assert!(d.header, "the header row is detected");
                    d.levels = vec![sp_ui::dialogs::sort::SortLevel {
                        column: "C".into(),
                        descending: true,
                    }];
                },
                ok: "OK",
            },
        )
        .rows(
            "Sheet1",
            "A2",
            &[
                &["North", "Apples", "12", "1.5"],
                &["East", "Apples", "10", "1.5"],
                &["East", "Plums", "7", "3"],
                &["West", "Pears", "4", "2.25"],
                &["West", "Apples", "3", "1.5"],
            ],
        )
        .set("Sheet1!E2.value", "18.0")
        .set("Sheet1!E3.value", "15.0")
        .set("Sheet1!E4.value", "21.0")
        .set("Sheet1!E5.value", "9.0")
        .set("Sheet1!E6.value", "4.5"),
        // Filter turns on drop-downs over the region around the active cell.
        Entry::new("filter", "B3", rb("Home", "Editing", "Filter"))
            .set("Sheet1.auto_filter.range.start.row", "0")
            .set("Sheet1.auto_filter.range.start.col", "0")
            .set("Sheet1.auto_filter.range.end.row", "5")
            .set("Sheet1.auto_filter.range.end.col", "4"),
        Entry::new("filter off by key", "B3", Act::Key("Ctrl+Shift+L"))
            .setup("home.editing.filter", json!({"range": "A1:E6", "on": true}))
            .gone("Sheet1.auto_filter.range.start.row")
            .gone("Sheet1.auto_filter.range.start.col")
            .gone("Sheet1.auto_filter.range.end.row")
            .gone("Sheet1.auto_filter.range.end.col"),
        // Find, Replace and Go To select cells; only replacing changes the sheet.
        Entry::new(
            "find",
            "A1",
            Act::Custom {
                prep: no_prep,
                act: |h| {
                    click_in_ribbon(h, "Editing", "Find…")?;
                    h.run_steps(3);
                    dialog!(h.state_mut(), Find).what = "Plums".into();
                    h.run_steps(1);
                    click_label(h, "Find Next")?;
                    click_label(h, "Close")
                },
            },
        )
        .then(|s| sel_is(s, "B4", "B4")),
        Entry::new(
            "find all",
            "A1",
            Act::Custom {
                prep: no_prep,
                act: |h| {
                    key(h, "Ctrl+F")?;
                    dialog!(h.state_mut(), Find).what = "Apples".into();
                    h.run_steps(1);
                    click_label(h, "Find All")?;
                    let hits = dialog!(h.state_mut(), Find).hits.len();
                    if hits != 3 {
                        return Err(format!("Find All listed {hits} cells, expected 3"));
                    }
                    click_label(h, "Close")
                },
            },
        ),
        Entry::new("find next by key", "A1", Act::Key("Shift+F4"))
            .setup("home.editing.find", json!({"what": "Apples"}))
            .then(|s| sel_is(s, "B2", "B2")),
        Entry::new(
            "replace all",
            "A1",
            Act::Custom {
                prep: no_prep,
                act: |h| {
                    click_in_ribbon(h, "Editing", "Replace…")?;
                    h.run_steps(3);
                    let d = dialog!(h.state_mut(), Find);
                    d.what = "Apples".into();
                    d.with = "Pomme".into();
                    h.run_steps(1);
                    click_label(h, "Replace All")?;
                    click_label(h, "Close")
                },
            },
        )
        .put("Sheet1!B2", "Pomme")
        .put("Sheet1!B5", "Pomme")
        .put("Sheet1!B6", "Pomme"),
        // Replace (one): the active cell matches, so it is replaced and the next is found.
        Entry::new(
            "replace one by key",
            "B2",
            Act::Custom {
                prep: no_prep,
                act: |h| {
                    key(h, "Ctrl+H")?;
                    let d = dialog!(h.state_mut(), Find);
                    d.what = "Apples".into();
                    d.with = "Pomme".into();
                    h.run_steps(1);
                    click_label(h, "Replace")?;
                    click_label(h, "Close")
                },
            },
        )
        .put("Sheet1!B2", "Pomme")
        .then(|s| sel_is(s, "B5", "B5")),
        Entry::new(
            "go to",
            "A1",
            Act::Dialog {
                ribbon: ("Home", "Editing", "Go To…"),
                fill: |app| dialog!(app, GoTo).reference = "D5".into(),
                ok: "OK",
            },
        )
        .then(|s| sel_is(s, "D5", "D5")),
        Entry::new(
            "go to by key",
            "A1",
            Act::Custom {
                prep: no_prep,
                act: |h| {
                    key(h, "Ctrl+G")?;
                    dialog!(h.state_mut(), GoTo).reference = "C3:D4".into();
                    h.run_steps(1);
                    click_label(h, "OK")
                },
            },
        )
        .then(|s| sel_is(s, "C3:D4", "C3")),
        Entry::new(
            "go to special blanks",
            "A1:F6",
            Act::Dialog {
                ribbon: ("Home", "Editing", "Go To Special…"),
                fill: |app| dialog!(app, GoToSpecial).kind = "blanks".into(),
                ok: "OK",
            },
        )
        .then(|s| sel_is(s, "F1:F6", "F1")),
        Entry::new(
            "select formulas",
            "E1:E6",
            rb("Home", "Editing", "Select Formulas"),
        )
        .then(|s| sel_is(s, "E2:E6", "E2")),
        Entry::new(
            "select constants",
            "C1:D6",
            rb("Home", "Editing", "Select Constants"),
        )
        .then(|s| sel_is(s, "C1:D6", "C1")),
    ]
}

// ---------------------------------------------------------------------------------------
// Home › Styles

fn home_styles() -> Vec<Entry> {
    vec![
        // Highlight Cells Rules: Greater Than 5 with the default light-red format.
        Entry::new(
            "highlight cells rule",
            "C2:C6",
            Act::Dialog {
                ribbon: ("Home", "Styles", "Highlight Cells Rules"),
                fill: |app| {
                    let d = dialog!(app, CondFormat);
                    d.contains = "cell_value".into();
                    d.op = "greater_than".into();
                    d.value = "5".into();
                },
                ok: "OK",
            },
        )
        .set("Sheet1.cond_formats.0.ranges.0.start.row", "1")
        .set("Sheet1.cond_formats.0.ranges.0.end.row", "5")
        .set("Sheet1.cond_formats.0.ranges.0.start.col", "2")
        .set("Sheet1.cond_formats.0.ranges.0.end.col", "2")
        // The rule's kind, operator, value and format.
        .allow("Sheet1.cond_formats.0.*")
        .print(),
        Entry::new(
            "top bottom rule",
            "C2:C6",
            Act::Dialog {
                ribbon: ("Home", "Styles", "Top/Bottom Rules"),
                fill: |app| {
                    let d = dialog!(app, CondFormat);
                    d.bottom = false;
                    d.rank = 2;
                },
                ok: "OK",
            },
        )
        .set("Sheet1.cond_formats.0.ranges.0.start.row", "1")
        .set("Sheet1.cond_formats.0.ranges.0.end.row", "5")
        .allow("Sheet1.cond_formats.0.*"),
        Entry::new("data bars", "C2:C6", rb("Home", "Styles", "Data Bars"))
            .set("Sheet1.cond_formats.0.ranges.0.start.row", "1")
            .set("Sheet1.cond_formats.0.ranges.0.end.row", "5")
            .allow("Sheet1.cond_formats.0.*")
            .print(),
        Entry::new(
            "color scales",
            "C2:C6",
            rb("Home", "Styles", "Color Scales"),
        )
        .set("Sheet1.cond_formats.0.ranges.0.start.row", "1")
        .set("Sheet1.cond_formats.0.ranges.0.end.row", "5")
        .allow("Sheet1.cond_formats.0.*"),
        Entry::new("icon sets", "C2:C6", rb("Home", "Styles", "Icon Sets"))
            .set("Sheet1.cond_formats.0.ranges.0.start.row", "1")
            .set("Sheet1.cond_formats.0.ranges.0.end.row", "5")
            .allow("Sheet1.cond_formats.0.*"),
        Entry::new(
            "new rule with a formula",
            "A2:E6",
            Act::Dialog {
                ribbon: ("Home", "Styles", "New Rule…"),
                fill: |app| {
                    let d = dialog!(app, CondFormat);
                    d.category = sp_ui::dialogs::cf_dialog::CAT_FORMULA;
                    d.formula = "=$C2>5".into();
                    d.format = "custom".into();
                    d.bold = true;
                },
                ok: "OK",
            },
        )
        .set("Sheet1.cond_formats.0.ranges.0.start.row", "1")
        .set("Sheet1.cond_formats.0.ranges.0.end.col", "4")
        .allow("Sheet1.cond_formats.0.*"),
        Entry::new("clear rules", "C2:C6", rb("Home", "Styles", "Clear Rules"))
            .setup("home.styles.cf.data_bars", json!({"range": "C2:C6"}))
            .gone("Sheet1.cond_formats.*"),
        // Rules Manager: select the rule, Delete Rule, Close.
        Entry::new(
            "manage rules delete",
            "C2",
            Act::Custom {
                prep: no_prep,
                act: |h| {
                    click_in_ribbon(h, "Styles", "Manage Rules…")?;
                    h.run_steps(3);
                    dialog!(h.state_mut(), CondFormat).selected = Some(1);
                    h.run_steps(1);
                    click_label(h, "Delete Rule")?;
                    click_label(h, "Close")
                },
            },
        )
        .setup("home.styles.cf.data_bars", json!({"range": "C2:C6"}))
        .gone("Sheet1.cond_formats.*"),
        Entry::new(
            "format as table",
            "B3",
            Act::Dialog {
                ribbon: ("Home", "Styles", "Format as Table"),
                fill: |app| {
                    let d = dialog!(app, CreateTable);
                    assert_eq!(d.range.replace('$', ""), "A1:E6");
                    d.style = "TableStyleMedium9".into();
                },
                ok: "OK",
            },
        )
        .set("Sheet1.tables.0.name", "\"Table1\"")
        .set("Sheet1.tables.0.style", "\"TableStyleMedium9\"")
        .set("Sheet1.tables.0.range.start.row", "0")
        .set("Sheet1.tables.0.range.end.row", "5")
        .set("Sheet1.tables.0.range.end.col", "4")
        // Column names, flags and the like.
        .allow("Sheet1.tables.0.*")
        .print(),
        // Good: dark green text on light green (the shades are the app's own look).
        Entry::new(
            "cell style",
            "A1:B1",
            Act::Pick("Home", "Styles", "Cell Styles", "Good"),
        )
        .allow("Sheet1!A1.style.fill.*")
        .allow("Sheet1!B1.style.fill.*")
        .allow("Sheet1!A1.style.font_color.*")
        .allow("Sheet1!B1.style.font_color.*")
        .then(|s| {
            let sh = &s.wb.sheets[0];
            for a in ["A1", "B1"] {
                let st =
                    s.wb.styles
                        .get(sh.get(CellAddr::parse(a).unwrap()).unwrap().style);
                let (Some(f), Some(c)) = (st.fill, st.font_color) else {
                    return Err(format!("{a}: no fill or font colour"));
                };
                let green = |x: sp_model::Color| x.1 > x.0 && x.1 > x.2;
                if !(green(f) && green(c) && f.1 > 200 && c.1 < 128) {
                    return Err(format!("{a}: fill {f:?} text {c:?} is not Good"));
                }
            }
            Ok(())
        }),
    ]
}

// ---------------------------------------------------------------------------------------
// Right-click menus: the grid and the sheet tabs

fn grid_menus() -> Vec<Entry> {
    vec![
        Entry::new(
            "menu cut then paste",
            "C2",
            Act::Custom {
                prep: no_prep,
                act: |h| {
                    grid_menu(h, "C2", &["Cut"])?;
                    click_cell(h, "H2");
                    grid_menu(h, "H2", &["Paste"])
                },
            },
        )
        .put("Sheet1!H2", "10")
        .gone("Sheet1!C2.input")
        .gone("Sheet1!C2.value")
        .set("Sheet1!E2.input", "\"=H2*D2\""),
        Entry::new(
            "menu copy then paste",
            "A2:B2",
            Act::Custom {
                prep: no_prep,
                act: |h| {
                    grid_menu(h, "A2", &["Copy"])?;
                    click_cell(h, "H2");
                    grid_menu(h, "H2", &["Paste"])
                },
            },
        )
        .put("Sheet1!H2", "East")
        .put("Sheet1!I2", "Apples"),
        Entry::new(
            "menu paste special",
            "H2",
            Act::Custom {
                prep: no_prep,
                act: |h| {
                    grid_menu(h, "H2", &["Paste Special…"])?;
                    dialog!(h.state_mut(), PasteSpecial).mode = "formulas".into();
                    h.run_steps(1);
                    click_label(h, "OK")
                },
            },
        )
        .setup("home.font.bold", json!({"range": "E2"}))
        .setup("home.clipboard.copy", json!({"range": "E2"}))
        .formula("Sheet1!H2", "=F2*G2", "0.0"),
        Entry::new(
            "menu insert rows",
            "A3",
            Act::Custom {
                prep: no_prep,
                act: |h| grid_menu(h, "A3", &["Insert Rows"]),
            },
        )
        .set("Sheet1!G1.input", "\"=SUM(E2:E7)\"")
        .put("Sheet1!A4", "West")
        .gone("Sheet1!A3.input")
        .allow("Sheet1!A*")
        .allow("Sheet1!B*")
        .allow("Sheet1!C*")
        .allow("Sheet1!D*")
        .allow("Sheet1!E*"),
        Entry::new(
            "menu insert columns",
            "B2",
            Act::Custom {
                prep: no_prep,
                act: |h| grid_menu(h, "B2", &["Insert Columns"]),
            },
        )
        .put("Sheet1!C1", "Product")
        .gone("Sheet1!B1.input")
        .formula("Sheet1!H1", "=SUM(F2:F6)", "67.5")
        .gone("Sheet1!G1.input")
        .allow("Sheet1!B*")
        .allow("Sheet1!C*")
        .allow("Sheet1!D*")
        .allow("Sheet1!E*")
        .allow("Sheet1!F*")
        .allow("Sheet1!G1.value"),
        Entry::new(
            "menu delete rows",
            "A3",
            Act::Custom {
                prep: no_prep,
                act: |h| grid_menu(h, "A3", &["Delete Rows"]),
            },
        )
        .put("Sheet1!A3", "East")
        .formula("Sheet1!G1", "=SUM(E2:E5)", "58.5")
        .gone("Sheet1!A6.input")
        .allow("Sheet1!A*")
        .allow("Sheet1!B*")
        .allow("Sheet1!C*")
        .allow("Sheet1!D*")
        .allow("Sheet1!E*"),
        Entry::new(
            "menu delete columns",
            "A1",
            Act::Custom {
                prep: no_prep,
                act: |h| grid_menu(h, "A1", &["Delete Columns"]),
            },
        )
        .put("Sheet1!A1", "Product")
        .formula("Sheet1!F1", "=SUM(D2:D6)", "67.5")
        .gone("Sheet1!G1.input")
        .gone("Sheet1!E2.input")
        .allow("Sheet1!A*")
        .allow("Sheet1!B*")
        .allow("Sheet1!C*")
        .allow("Sheet1!D*")
        .allow("Sheet1!E*")
        .allow("Sheet1!G1.value"),
        Entry::new(
            "menu clear contents",
            "A2:B2",
            Act::Custom {
                prep: no_prep,
                act: |h| grid_menu(h, "A2", &["Clear Contents"]),
            },
        )
        .gone("Sheet1!A2.input")
        .gone("Sheet1!A2.value")
        .gone("Sheet1!B2.input")
        .gone("Sheet1!B2.value"),
        Entry::new(
            "menu sort a to z",
            "B3",
            Act::Custom {
                prep: no_prep,
                act: |h| grid_menu(h, "B3", &["Sort", "Sort A to Z"]),
            },
        )
        .rows("Sheet1", "A2", &BY_PRODUCT)
        .set("Sheet1!E3.value", "18.0")
        .set("Sheet1!E4.value", "4.5")
        .set("Sheet1!E5.value", "9.0")
        .set("Sheet1!E6.value", "21.0"),
        // Sort Z to A on Qty: 12, 10, 7, 4, 3.
        Entry::new(
            "menu sort z to a",
            "C3",
            Act::Custom {
                prep: no_prep,
                act: |h| grid_menu(h, "C3", &["Sort", "Sort Z to A"]),
            },
        )
        .rows(
            "Sheet1",
            "A2",
            &[
                &["North", "Apples", "12", "1.5"],
                &["East", "Apples", "10", "1.5"],
                &["East", "Plums", "7", "3"],
                &["West", "Pears", "4", "2.25"],
                &["West", "Apples", "3", "1.5"],
            ],
        )
        .set("Sheet1!E2.value", "18.0")
        .set("Sheet1!E3.value", "15.0")
        .set("Sheet1!E4.value", "21.0")
        .set("Sheet1!E5.value", "9.0")
        .set("Sheet1!E6.value", "4.5"),
        Entry::new(
            "menu custom sort",
            "B3",
            Act::Custom {
                prep: no_prep,
                act: |h| {
                    grid_menu(h, "B3", &["Sort", "Custom Sort…"])?;
                    dialog!(h.state_mut(), Sort).levels = vec![sp_ui::dialogs::sort::SortLevel {
                        column: "B".into(),
                        descending: false,
                    }];
                    h.run_steps(1);
                    click_label(h, "OK")
                },
            },
        )
        .rows("Sheet1", "A2", &BY_PRODUCT)
        .set("Sheet1!E3.value", "18.0")
        .set("Sheet1!E4.value", "4.5")
        .set("Sheet1!E5.value", "9.0")
        .set("Sheet1!E6.value", "21.0"),
        // Insert Function puts =SUM( in the cell being edited; the user finishes it.
        Entry::new(
            "menu insert function",
            "H2",
            Act::Custom {
                prep: no_prep,
                act: |h| {
                    grid_menu(h, "H2", &["Insert Function…"])?;
                    dialog!(h.state_mut(), InsertFunction).selected = Some("SUM".into());
                    h.run_steps(1);
                    click_label(h, "OK")?;
                    h.event(egui::Event::Text("C2:C3)".into()));
                    h.run_steps(1);
                    key(h, "Enter")
                },
            },
        )
        .formula("Sheet1!H2", "=SUM(C2:C3)", "14.0"),
        Entry::new(
            "menu define name",
            "C2:C6",
            Act::Custom {
                prep: no_prep,
                act: |h| {
                    grid_menu(h, "C3", &["Define Name…"])?;
                    let d = dialog!(h.state_mut(), DefineName);
                    d.name = "Qty".into();
                    h.run_steps(1);
                    click_label(h, "OK")
                },
            },
        )
        .set("workbook.names.0.name", "\"Qty\"")
        .set("workbook.names.0.formula", "\"Sheet1!$C$2:$C$6\"")
        // The name's other fields (scope, comment) keep their defaults.
        .allow("workbook.names.0.*"),
        Entry::new(
            "menu format cells",
            "A1:E1",
            Act::Custom {
                prep: no_prep,
                act: |h| {
                    grid_menu(h, "B1", &["Format Cells…"])?;
                    let d = fc(h.state_mut());
                    d.tab = FcTab::Fill;
                    d.v.fill = Some("DDEBF7".into());
                    h.run_steps(1);
                    click_label(h, "OK")
                },
            },
        )
        .cells("Sheet1!A1:E1", "style.fill.0", "221")
        .cells("Sheet1!A1:E1", "style.fill.1", "235")
        .cells("Sheet1!A1:E1", "style.fill.2", "247"),
        Entry::new(
            "menu column width",
            "B2",
            Act::Custom {
                prep: no_prep,
                act: |h| {
                    grid_menu(h, "B2", &["Column Width…"])?;
                    dialog!(h.state_mut(), Prompt).value = "20".into();
                    h.run_steps(1);
                    click_label(h, "OK")
                },
            },
        )
        .set("Sheet1.col_widths.1", "20.0"),
        Entry::new(
            "menu row height",
            "B2",
            Act::Custom {
                prep: no_prep,
                act: |h| {
                    grid_menu(h, "B2", &["Row Height…"])?;
                    dialog!(h.state_mut(), Prompt).value = "30".into();
                    h.run_steps(1);
                    click_label(h, "OK")
                },
            },
        )
        .set("Sheet1.row_heights.1", "30.0"),
    ]
}

fn tab_menus() -> Vec<Entry> {
    let two = ("home.cells.insert_sheet", json!({"name": "Two"}));
    vec![
        Entry::new(
            "tab menu insert",
            "A1",
            Act::Custom {
                prep: no_prep,
                act: |h| {
                    tab_menu(h, "Sheet1", &["Insert…"])?;
                    dialog!(h.state_mut(), Prompt).value = "Notes".into();
                    h.run_steps(1);
                    click_label(h, "OK")
                },
            },
        )
        .set("workbook.sheet_order.0", "\"Notes\"")
        .set("workbook.sheet_order.1", "\"Sheet1\"")
        .set("workbook.active", "0")
        .allow("Notes.*"),
        Entry::new(
            "tab menu delete",
            "A1",
            Act::Custom {
                prep: no_prep,
                act: |h| tab_menu(h, "Two", &["Delete"]),
            },
        )
        .setup(two.0, two.1.clone())
        .set("workbook.sheet_order.0", "\"Sheet1\"")
        .gone("workbook.sheet_order.1")
        .set("workbook.active", "0")
        .allow("Two.*"),
        Entry::new(
            "tab menu rename",
            "A1",
            Act::Custom {
                prep: no_prep,
                act: |h| {
                    tab_menu(h, "Sheet1", &["Rename"])?;
                    dialog!(h.state_mut(), Prompt).value = "Sales".into();
                    h.run_steps(1);
                    click_label(h, "OK")
                },
            },
        )
        .set("workbook.sheet_order.0", "\"Sales\"")
        // Every property of the sheet moves from `Sheet1` to `Sales`.
        .allow("Sheet1*")
        .allow("Sales*")
        .then(|s| {
            let sh = s.wb.sheet_index("Sales").ok_or("no sheet Sales")?;
            let n = s.wb.sheets[sh].iter().count();
            if n == 0 {
                return Err("the renamed sheet lost its cells".into());
            }
            Ok(())
        }),
        Entry::new(
            "tab menu move",
            "A1",
            Act::Custom {
                prep: no_prep,
                act: |h| {
                    tab_menu(h, "Two", &["Move or Copy…"])?;
                    let d = dialog!(h.state_mut(), MoveCopy);
                    d.before = d.sheets.len();
                    h.run_steps(1);
                    click_label(h, "OK")
                },
            },
        )
        .setup(two.0, two.1.clone())
        .set("workbook.sheet_order.0", "\"Sheet1\"")
        .set("workbook.sheet_order.1", "\"Two\"")
        .set("workbook.active", "1"),
        Entry::new(
            "tab menu duplicate",
            "A1",
            Act::Custom {
                prep: no_prep,
                act: |h| tab_menu(h, "Sheet1", &["Duplicate"]),
            },
        )
        .set("workbook.sheet_order.1", "\"Sheet1 (2)\"")
        .set("workbook.active", "1")
        .put("Sheet1 (2)!B2", "Apples")
        .formula("Sheet1 (2)!E2", "=C2*D2", "15.0")
        .allow("Sheet1 (2)*"),
        Entry::new(
            "tab menu colour",
            "A1",
            Act::Custom {
                prep: no_prep,
                act: |h| tab_menu(h, "Sheet1", &["Tab Color", "#FFC000"]),
            },
        )
        .set("Sheet1.tab_color.0", "255")
        .set("Sheet1.tab_color.1", "192")
        .set("Sheet1.tab_color.2", "0"),
        Entry::new(
            "tab menu no colour",
            "A1",
            Act::Custom {
                prep: no_prep,
                act: |h| tab_menu(h, "Sheet1", &["Tab Color", "No Color"]),
            },
        )
        .setup(
            "home.cells.tab_color",
            json!({"sheet": "Sheet1", "color": "FF0000"}),
        )
        .gone("Sheet1.tab_color.*"),
        Entry::new(
            "tab menu hide",
            "A1",
            Act::Custom {
                prep: no_prep,
                act: |h| tab_menu(h, "Two", &["Hide"]),
            },
        )
        // Two (first) is the active sheet; hiding it shows Sheet1.
        .setup(two.0, two.1.clone())
        .set("Two.visibility", "\"Hidden\"")
        .set("workbook.active", "1"),
        Entry::new(
            "tab menu unhide",
            "A1",
            Act::Custom {
                prep: no_prep,
                act: |h| {
                    tab_menu(h, "Sheet1", &["Unhide…"])?;
                    dialog!(h.state_mut(), UnhideSheet).selected = 0;
                    h.run_steps(1);
                    click_label(h, "OK")
                },
            },
        )
        .setup(two.0, two.1.clone())
        .setup("home.cells.hide_sheet", json!({"sheet": "Two"}))
        .set("Two.visibility", "\"Visible\"")
        .set("workbook.active", "0"),
    ]
}

// ---------------------------------------------------------------------------------------
// Insert

/// A chart over Product (categories) and Qty (values) of the given kind.
fn chart(name: &'static str, label: &'static str, kind: &str) -> Entry {
    Entry::new(name, "B1:C6", rb("Insert", "Charts", label))
        .set("Sheet1.drawings.charts.0.kind", &format!("{kind:?}"))
        .set(
            "Sheet1.drawings.charts.0.series.0.values",
            "\"Sheet1!$C$2:$C$6\"",
        )
        .set(
            "Sheet1.drawings.charts.0.series.0.categories",
            "\"Sheet1!$B$2:$B$6\"",
        )
        .gone("Sheet1.drawings.charts.0.series.1.values")
        // Placement, name, axes and the other chart defaults.
        .allow("Sheet1.drawings.charts.0.*")
        .print()
}

fn insert_tab() -> Vec<Entry> {
    vec![
        chart("column chart", "Column Chart", "Column"),
        chart("bar chart", "Bar Chart", "Bar"),
        chart("line chart", "Line Chart", "Line"),
        chart("pie chart", "Pie Chart", "Pie"),
        chart("area chart", "Area Chart", "Area"),
        chart("scatter chart", "Scatter Chart", "Scatter"),
        chart("recommended chart", "Recommended Charts", "Column"),
        Entry::new("recommended chart by key", "B1:C6", Act::Key("Alt+F1"))
            .set("Sheet1.drawings.charts.0.kind", "\"Column\"")
            .allow("Sheet1.drawings.charts.0.*"),
        // A histogram bins the values: one series, no categories.
        Entry::new("histogram", "C1:C6", rb("Insert", "Charts", "Histogram"))
            .set("Sheet1.drawings.charts.0.kind", "\"Histogram\"")
            .set(
                "Sheet1.drawings.charts.0.series.0.values",
                "\"Sheet1!$C$2:$C$6\"",
            )
            .allow("Sheet1.drawings.charts.0.*"),
        // Combo: the first series as columns, the next as a line.
        Entry::new(
            "combo chart",
            "B1:D6",
            rb("Insert", "Charts", "Combo Chart"),
        )
        .set("Sheet1.drawings.charts.0.kind", "\"Combo\"")
        .set(
            "Sheet1.drawings.charts.0.series.0.values",
            "\"Sheet1!$C$2:$C$6\"",
        )
        .set(
            "Sheet1.drawings.charts.0.series.1.values",
            "\"Sheet1!$D$2:$D$6\"",
        )
        .allow("Sheet1.drawings.charts.0.*"),
        // Sparklines: one per row of the selection, in the column to its right.
        Entry::new(
            "line sparklines",
            "C2:D6",
            rb("Insert", "Sparklines", "Line Sparkline"),
        )
        .set("Sheet1.drawings.sparklines.0.kind", "\"Line\"")
        .set(
            "Sheet1.drawings.sparklines.0.items.0.data",
            "\"Sheet1!C2:D2\"",
        )
        .set("Sheet1.drawings.sparklines.0.items.0.location.row", "1")
        .set("Sheet1.drawings.sparklines.0.items.0.location.col", "4")
        .set(
            "Sheet1.drawings.sparklines.0.items.4.data",
            "\"Sheet1!C6:D6\"",
        )
        .allow("Sheet1.drawings.sparklines.0.*"),
        Entry::new(
            "column sparklines",
            "C2:D6",
            rb("Insert", "Sparklines", "Column Sparkline"),
        )
        .set("Sheet1.drawings.sparklines.0.kind", "\"Column\"")
        .allow("Sheet1.drawings.sparklines.0.*"),
        Entry::new(
            "win loss sparklines",
            "C2:D6",
            rb("Insert", "Sparklines", "Win/Loss Sparkline"),
        )
        .set("Sheet1.drawings.sparklines.0.kind", "\"WinLoss\"")
        .allow("Sheet1.drawings.sparklines.0.*"),
        Entry::new(
            "table",
            "B3",
            Act::Dialog {
                ribbon: ("Insert", "Tables", "Table"),
                fill: |app| {
                    let d = dialog!(app, CreateTable);
                    assert!(d.has_headers, "headers detected");
                },
                ok: "OK",
            },
        )
        .set("Sheet1.tables.0.name", "\"Table1\"")
        .set("Sheet1.tables.0.header_row", "true")
        .set("Sheet1.tables.0.range.start.row", "0")
        .set("Sheet1.tables.0.range.start.col", "0")
        .set("Sheet1.tables.0.range.end.row", "5")
        .set("Sheet1.tables.0.range.end.col", "4")
        .set("Sheet1.tables.0.columns.0.name", "\"Region\"")
        .set("Sheet1.tables.0.columns.4.name", "\"Total\"")
        .allow("Sheet1.tables.0.*")
        .print(),
        Entry::new(
            "table by key",
            "B3",
            Act::Custom {
                prep: no_prep,
                act: |h| {
                    key(h, "Ctrl+T")?;
                    click_label(h, "OK")
                },
            },
        )
        .set("Sheet1.tables.0.name", "\"Table1\"")
        .allow("Sheet1.tables.0.*"),
        // A PivotTable from the region around the active cell, on a new sheet before it.
        Entry::new("pivot table", "B3", rb("Insert", "Tables", "PivotTable"))
            .set("workbook.sheet_order.0", "\"Sheet2\"")
            .set("workbook.sheet_order.1", "\"Sheet1\"")
            .set("workbook.active", "0")
            .set("Sheet2.analysis.pivots.0.name", "\"PivotTable1\"")
            .set("Sheet2.analysis.pivots.0.source", "\"Sheet1!$A$1:$E$6\"")
            // The new sheet's defaults, the pivot's layout and its output cells.
            .allow("Sheet2.*")
            .allow("Sheet2!*"),
        Entry::new(
            "hyperlink",
            "A1",
            Act::Dialog {
                ribbon: ("Insert", "Links", "Link"),
                fill: |app| dialog!(app, Hyperlink).address = "https://example.com/".into(),
                ok: "OK",
            },
        )
        .set("Sheet1.hyperlinks.0.target", "\"https://example.com/\"")
        // The cell takes the Hyperlink style: underlined, in the link colour.
        .set("Sheet1!A1.style.underline", "1")
        .allow("Sheet1!A1.style.font_color.*")
        .allow("Sheet1.hyperlinks.0.*"),
        Entry::new(
            "hyperlink to a place by key",
            "H2",
            Act::Custom {
                prep: no_prep,
                act: |h| {
                    key(h, "Ctrl+K")?;
                    let d = dialog!(h.state_mut(), Hyperlink);
                    d.to = sp_ui::dialogs::tools2::LinkTo::Place;
                    d.reference = "D5".into();
                    d.text = "Go".into();
                    h.run_steps(1);
                    click_label(h, "OK")
                },
            },
        )
        .put("Sheet1!H2", "Go")
        .set("Sheet1!H2.style.underline", "1")
        .allow("Sheet1!H2.style.font_color.*")
        .allow("Sheet1.hyperlinks.0.*"),
        Entry::new(
            "new note",
            "B2",
            Act::Custom {
                prep: |h| click_tab(h, "Review"),
                act: |h| {
                    click_in_ribbon(h, "Notes", "New Note")?;
                    h.run_steps(3);
                    dialog!(h.state_mut(), NewComment).text = "A note".into();
                    h.run_steps(1);
                    click_label(h, "Save")
                },
            },
        )
        .set("Sheet1.comments.0.text", "\"A note\"")
        .set("Sheet1.comments.0.kind", "\"Note\"")
        .allow("Sheet1.comments.0.*"),
        Entry::new(
            "edit note by key",
            "A1",
            Act::Custom {
                prep: no_prep,
                act: |h| {
                    key(h, "Shift+F2")?;
                    dialog!(h.state_mut(), NewComment).text = "changed".into();
                    h.run_steps(1);
                    click_label(h, "Save")
                },
            },
        )
        .setup("insert.note", json!({"cell": "A1", "text": "old"}))
        .set("Sheet1.comments.0.text", "\"changed\""),
        // A threaded comment from the Insert tab, and by key; it stays a thread on reopen.
        thread_on_c3(Entry::new(
            "new comment",
            "C3",
            Act::Dialog {
                ribbon: ("Insert", "Comments", "New Comment"),
                fill: |app| dialog!(app, NewComment).text = "Check this".into(),
                ok: "Post",
            },
        )),
        thread_on_c3(Entry::new(
            "new comment by key",
            "C3",
            Act::Custom {
                prep: no_prep,
                act: |h| {
                    key(h, "Ctrl+Shift+F2")?;
                    dialog!(h.state_mut(), NewComment).text = "Check this".into();
                    h.run_steps(1);
                    click_label(h, "Post")
                },
            },
        )),
        Entry::new("shape", "B2", rb("Insert", "Illustrations", "Shapes"))
            .allow("Sheet1.objects.0.*")
            .then(
                |s| match s.wb.sheets[0].objects.first().and_then(|o| o.shape()) {
                    Some(_) => Ok(()),
                    None => Err("no shape inserted".into()),
                },
            ),
        Entry::new("text box", "B2", rb("Insert", "Text", "Text Box"))
            .allow("Sheet1.objects.0.*")
            .then(
                |s| match s.wb.sheets[0].objects.first().and_then(|o| o.shape()) {
                    Some(_) => Ok(()),
                    None => Err("no text box inserted".into()),
                },
            ),
        Entry::new("equation", "B2", rb("Insert", "Symbols", "Equation"))
            .allow("Sheet1.objects.0.*")
            .then(
                |s| match s.wb.sheets[0].objects.first().and_then(|o| o.shape()) {
                    Some(_) => Ok(()),
                    None => Err("no equation inserted".into()),
                },
            ),
        Entry::new(
            "symbol",
            "H2",
            Act::Custom {
                prep: |h| click_tab(h, "Insert"),
                act: |h| {
                    click_in_ribbon(h, "Symbols", "Symbol…")?;
                    h.run_steps(3);
                    let d = dialog!(h.state_mut(), Symbol);
                    d.set = d
                        .sets
                        .iter()
                        .position(|(_, syms)| syms.iter().any(|x| x == "€"))
                        .ok_or("no € in the symbol sets")?;
                    h.run_steps(2);
                    // Clicking a symbol enters it and closes the dialog.
                    click_new(h, "€", &[])
                },
            },
        )
        .put("Sheet1!H2", "€"),
        // Checkboxes: empty cells become FALSE and draw as boxes.
        Entry::new(
            "checkbox",
            "H2:H3",
            rb("Insert", "Cell Controls", "Checkbox"),
        )
        .cells("Sheet1!H2:H3", "style.checkbox", "true")
        .cells("Sheet1!H2:H3", "value", "false")
        .cells("Sheet1!H2:H3", "input", "\"FALSE\""),
        // Header & Footer: the page layout view, to type into the header.
        Entry::new(
            "header and footer",
            "A1",
            rb("Insert", "Text", "Header & Footer"),
        )
        .set("view.print.mode", "\"PageLayout\"")
        .view_only(),
    ]
}

// ---------------------------------------------------------------------------------------
// Page Layout

fn layout_tab() -> Vec<Entry> {
    vec![
        Entry::new(
            "margins wide",
            "A1",
            Act::Pick("Page Layout", "Page Setup", "Margins", "Wide"),
        )
        .set("Sheet1.page_setup.margins.top", "1.0")
        .set("Sheet1.page_setup.margins.bottom", "1.0")
        .set("Sheet1.page_setup.margins.left", "1.0")
        .set("Sheet1.page_setup.margins.right", "1.0")
        .set("Sheet1.page_setup.margins.header", "0.5")
        .set("Sheet1.page_setup.margins.footer", "0.5")
        .print(),
        Entry::new(
            "margins narrow",
            "A1",
            Act::Pick("Page Layout", "Page Setup", "Margins", "Narrow"),
        )
        .set("Sheet1.page_setup.margins.top", "0.75")
        .set("Sheet1.page_setup.margins.bottom", "0.75")
        .set("Sheet1.page_setup.margins.left", "0.25")
        .set("Sheet1.page_setup.margins.right", "0.25")
        .set("Sheet1.page_setup.margins.header", "0.3")
        .set("Sheet1.page_setup.margins.footer", "0.3"),
        Entry::new(
            "custom margins",
            "A1",
            Act::Custom {
                prep: |h| click_tab(h, "Page Layout"),
                act: |h| {
                    click_in_ribbon(h, "Page Setup", "Margins")?;
                    h.run_steps(2);
                    click_new(h, "Custom Margins…", &[])?;
                    dialog!(h.state_mut(), CustomMargins).values[2] = "1.5".into();
                    h.run_steps(1);
                    click_label(h, "OK")
                },
            },
        )
        .set("Sheet1.page_setup.margins.left", "1.5"),
        Entry::new(
            "orientation",
            "A1",
            rb("Page Layout", "Page Setup", "Orientation"),
        )
        .set("Sheet1.page_setup.orientation", "\"Landscape\"")
        .print(),
        Entry::new(
            "paper size",
            "A1",
            Act::Pick("Page Layout", "Page Setup", "Size", "A4"),
        )
        .set("Sheet1.page_setup.paper_size", "9")
        .print(),
        Entry::new(
            "set print area",
            "A1:C4",
            rb("Page Layout", "Page Setup", "Set Print Area"),
        )
        .set("workbook.names.0.name", "\"_xlnm.Print_Area\"")
        .set("workbook.names.0.formula", "\"Sheet1!$A$1:$C$4\"")
        .allow("workbook.names.0.*")
        .print(),
        Entry::new(
            "clear print area",
            "A1",
            rb("Page Layout", "Page Setup", "Clear Print Area"),
        )
        .setup("layout.print_area.set", json!({"range": "A1:C4"}))
        .gone("workbook.names.0.*"),
        // A break goes above the active row (column A: no column break).
        Entry::new(
            "insert page break",
            "A4",
            rb("Page Layout", "Page Setup", "Insert Page Break"),
        )
        .set("Sheet1.page_setup.row_breaks.0", "3")
        .print(),
        Entry::new(
            "insert page break at a cell",
            "C4",
            rb("Page Layout", "Page Setup", "Insert Page Break"),
        )
        .set("Sheet1.page_setup.row_breaks.0", "3")
        .set("Sheet1.page_setup.col_breaks.0", "2"),
        Entry::new(
            "remove page break",
            "A4",
            rb("Page Layout", "Page Setup", "Remove Page Break"),
        )
        .setup("layout.page_break.insert", json!({"cell": "A4"}))
        .gone("Sheet1.page_setup.row_breaks.0"),
        Entry::new(
            "reset page breaks",
            "A1",
            rb("Page Layout", "Page Setup", "Reset All Page Breaks"),
        )
        .setup("layout.page_break.insert", json!({"cell": "A4"}))
        .setup("layout.page_break.insert", json!({"cell": "C2"}))
        .gone("Sheet1.page_setup.row_breaks.*")
        .gone("Sheet1.page_setup.col_breaks.*"),
        Entry::new(
            "scale to fit",
            "A1",
            Act::Dialog {
                ribbon: ("Page Layout", "Scale to Fit", "Scale to Fit"),
                fill: |app| {
                    let d = dialog!(app, ScaleToFit);
                    d.width = "1".into();
                    d.height = "Automatic".into();
                },
                ok: "OK",
            },
        )
        .set("Sheet1.page_setup.fit_width", "1")
        .allow("Sheet1.page_setup.fit_height")
        .print(),
        Entry::new(
            "print gridlines",
            "A1",
            rb("Page Layout", "Sheet Options", "Print Gridlines"),
        )
        .set("Sheet1.page_setup.print_gridlines", "true")
        .print(),
        Entry::new(
            "print headings",
            "A1",
            rb("Page Layout", "Sheet Options", "Print Headings"),
        )
        .set("Sheet1.page_setup.print_headings", "true")
        .print(),
        Entry::new(
            "view gridlines",
            "A1",
            rb("Page Layout", "Sheet Options", "View Gridlines"),
        )
        .set("Sheet1.show_gridlines", "false"),
        Entry::new(
            "view headings",
            "A1",
            rb("Page Layout", "Sheet Options", "View Headings"),
        )
        .set("view.show_headings", "false")
        .view_only(),
        // A theme brings its colours and fonts; cells in the body font take the new one.
        Entry::new(
            "theme",
            "A1",
            Act::Pick("Page Layout", "Themes", "Themes", "Harbor"),
        )
        .set("workbook.theme.name", "\"Harbor\"")
        .set("Sheet1!A1.style.font_name", "\"Verdana\"")
        .set("Sheet1!E6.style.font_name", "\"Verdana\"")
        // Every other cell in the body font gets the new body font too.
        .set("Sheet1!*.style.font_name", "\"Verdana\"")
        // The theme's colour slots and fonts.
        .allow("workbook.theme.*"),
    ]
}

// ---------------------------------------------------------------------------------------
// Keyboard: navigation, selection, sheets, editing and undo

/// A check that the selection is `$r` with the active cell `$a`.
macro_rules! sel {
    ($r:literal, $a:literal) => {
        |s: &Session| sel_is(s, $r, $a)
    };
}

/// The target does not undo this (sheet switches, undo itself): skip the undo check.
fn no_undo(mut e: Entry) -> Entry {
    e.undo = false;
    e
}

/// A key that only moves the selection: nothing in the workbook changes.
fn nav(name: &'static str, select: &'static str, sc: &'static str) -> Entry {
    Entry::new(name, select, Act::Key(sc)).view_only()
}

fn keyboard_nav() -> Vec<Entry> {
    vec![
        nav("move up", "C3", "Up").then(sel!("C2", "C2")),
        nav("move down", "C3", "Down").then(sel!("C4", "C4")),
        nav("move left", "C3", "Left").then(sel!("B3", "B3")),
        nav("move right", "C3", "Right").then(sel!("D3", "D3")),
        // Ctrl+arrows stop at the edge of the data.
        nav("edge up", "C5", "Ctrl+Up").then(sel!("C1", "C1")),
        nav("edge down", "C2", "Ctrl+Down").then(sel!("C6", "C6")),
        nav("edge left", "D3", "Ctrl+Left").then(sel!("A3", "A3")),
        nav("edge right", "A3", "Ctrl+Right").then(sel!("E3", "E3")),
        nav("home of row", "D4", "Home").then(sel!("A4", "A4")),
        nav("home of sheet", "D4", "Ctrl+Home").then(sel!("A1", "A1")),
        // The last used cell: the last used row (6) and column (G, the total).
        nav("end of sheet", "A1", "Ctrl+End").then(sel!("G6", "G6")),
        // A page down / right moves by a screenful (the window's size) and scrolls.
        nav("page down", "A1", "PageDown")
            .allow("view.scroll.*")
            .then(|s| {
                let a = s.sel.active;
                if a.col == 0 && a.row >= 10 && s.view.scroll.row > 0 {
                    Ok(())
                } else {
                    Err(format!(
                        "page down went to {} (scroll {:?})",
                        a.to_a1(),
                        s.view.scroll
                    ))
                }
            }),
        Entry::new(
            "page up",
            "A1",
            Act::Custom {
                prep: |h| key(h, "PageDown"),
                act: |h| key(h, "PageUp"),
            },
        )
        .set("view.scroll.row", "0")
        .view_only()
        .then(sel!("A1", "A1")),
        nav("page right", "A1", "Alt+PageDown")
            .allow("view.scroll.*")
            .then(|s| {
                let a = s.sel.active;
                if a.row == 0 && a.col >= 5 && s.view.scroll.col > 0 {
                    Ok(())
                } else {
                    Err(format!(
                        "page right went to {} (scroll {:?})",
                        a.to_a1(),
                        s.view.scroll
                    ))
                }
            }),
        Entry::new(
            "page left",
            "A1",
            Act::Custom {
                prep: |h| key(h, "Alt+PageDown"),
                act: |h| key(h, "Alt+PageUp"),
            },
        )
        .set("view.scroll.col", "0")
        .view_only()
        .then(sel!("A1", "A1")),
        nav("enter moves down", "A1", "Enter").then(sel!("A2", "A2")),
        // Inside a selection, Enter and Tab move the active cell and keep the selection.
        nav("enter within a selection", "A1:B2", "Enter").then(sel!("A1:B2", "A2")),
        nav("tab moves right", "A1", "Tab").then(sel!("B1", "B1")),
        nav("extend up", "C3", "Shift+Up").then(sel!("C2:C3", "C3")),
        nav("extend down", "C3", "Shift+Down").then(sel!("C3:C4", "C3")),
        nav("extend left", "C3", "Shift+Left").then(sel!("B3:C3", "C3")),
        nav("extend right", "C3", "Shift+Right").then(sel!("C3:D3", "C3")),
        nav("extend to edge up", "C5", "Ctrl+Shift+Up").then(sel!("C1:C5", "C5")),
        nav("extend to edge down", "C2", "Ctrl+Shift+Down").then(sel!("C2:C6", "C2")),
        nav("extend to edge left", "D3", "Ctrl+Shift+Left").then(sel!("A3:D3", "D3")),
        nav("extend to edge right", "A3", "Ctrl+Shift+Right").then(sel!("A3:E3", "A3")),
        // Ctrl+A inside data selects the data's region first.
        nav("select all", "B3", "Ctrl+A").then(sel!("A1:E6", "B3")),
        // Whole rows / columns print as `3:3` / `B:B`.
        nav("select row", "B3", "Shift+Space").then(sel!("3:3", "B3")),
        nav("select column", "B3", "Ctrl+Space").then(sel!("B:B", "B3")),
        nav("select current region", "B3", "Ctrl+Shift+*").then(sel!("A1:E6", "B3")),
        // Nothing hidden: the visible cells are the selection.
        nav("select visible cells", "A2:B3", "Alt+;").then(sel!("A2:B3", "A2")),
        // A mouse drag selects a range.
        Entry::new(
            "drag to select",
            "A1",
            Act::Custom {
                prep: no_prep,
                act: |h| {
                    let (a, b) = (cell_center(h, "B2"), cell_center(h, "C4"));
                    h.hover_at(a);
                    h.run_steps(1);
                    h.drag_at(a);
                    h.run_steps(1);
                    h.hover_at(b);
                    h.run_steps(1);
                    h.drop_at(b);
                    h.run_steps(2);
                    Ok(())
                },
            },
        )
        .view_only()
        .then(sel!("B2:C4", "B2")),
        // The Name Box: type a reference, Enter.
        Entry::new(
            "name box",
            "A1",
            Act::Custom {
                prep: no_prep,
                act: |h| {
                    use egui_kittest::kittest::Queryable;
                    let n = h
                        .query_all_by_role(egui::accesskit::Role::TextInput)
                        .find(|n| n.value().as_deref() == Some("A1"))
                        .ok_or("no name box showing A1")?;
                    n.click();
                    h.run_steps(1);
                    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
                    h.run_steps(1);
                    h.event(egui::Event::Text("D5".into()));
                    h.run_steps(1);
                    h.key_press(egui::Key::Enter);
                    h.run_steps(3);
                    Ok(())
                },
            },
        )
        .view_only()
        .then(sel!("D5", "D5")),
        // Sheets: switching is not undone (as in the target) but is saved (active tab).
        no_undo(
            Entry::new("next sheet", "A1", Act::Key("Ctrl+PageDown"))
                .setup("home.cells.insert_sheet", json!({"name": "Two"}))
                .set("workbook.active", "1"),
        ),
        no_undo(
            Entry::new("previous sheet", "A1", Act::Key("Ctrl+PageUp"))
                .setup("home.cells.insert_sheet", json!({"name": "Two"}))
                .setup("sheet.activate", json!({"sheet": "Sheet1"}))
                .set("workbook.active", "0"),
        ),
        no_undo(
            Entry::new(
                "click a sheet tab",
                "A1",
                Act::Custom {
                    prep: no_prep,
                    act: |h| {
                        use egui_kittest::kittest::Queryable;
                        h.query_all_by_label("Two")
                            .max_by(|a, b| a.rect().top().total_cmp(&b.rect().top()))
                            .ok_or("no tab Two")?
                            .click();
                        h.run_steps(2);
                        Ok(())
                    },
                },
            )
            .setup("home.cells.insert_sheet", json!({"name": "Two"}))
            .setup("sheet.activate", json!({"sheet": "Sheet1"}))
            .set("workbook.active", "0"),
        ),
    ]
}

/// Type `text` into the cell editor (which the first character opens).
fn type_text(h: &mut Harness<'_, App>, text: &str) {
    h.event(egui::Event::Text(text.into()));
    h.run_steps(1);
}

/// Days since 1899-12-30 for today (UTC; the check allows a day either side).
fn today_serial() -> f64 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    (secs / 86_400) as f64 + 25_569.0
}

fn value_at(s: &Session, a1: &str) -> Option<f64> {
    match s.wb.sheets[0].value(CellAddr::parse(a1).ok()?) {
        sp_core::Value::Number(n) => Some(*n),
        _ => None,
    }
}

fn keyboard_edit() -> Vec<Entry> {
    vec![
        Entry::new("type a formula", "H2", Act::Type("H2", "=C2+1")).formula(
            "Sheet1!H2",
            "=C2+1",
            "11.0",
        ),
        // Enter commits and moves one cell down.
        Entry::new("type a number", "H3", Act::Type("H3", "42"))
            .put("Sheet1!H3", "42")
            .then(sel!("H4", "H4")),
        // F2 edits the cell's contents with the caret at the end.
        Entry::new(
            "edit with F2",
            "A2",
            Act::Custom {
                prep: no_prep,
                act: |h| {
                    key(h, "F2")?;
                    type_text(h, "ern");
                    key(h, "Enter")
                },
            },
        )
        .put("Sheet1!A2", "Eastern"),
        // Escape throws the typing away.
        Entry::new(
            "cancel an edit",
            "A2",
            Act::Custom {
                prep: no_prep,
                act: |h| {
                    type_text(h, "zzz");
                    key(h, "Escape")
                },
            },
        )
        .then(|s| {
            if s.view.editing {
                Err("still editing".into())
            } else {
                Ok(())
            }
        }),
        // Ctrl+Enter enters the typing in every selected cell.
        Entry::new(
            "enter in all selected cells",
            "H1:H3",
            Act::Custom {
                prep: no_prep,
                act: |h| {
                    type_text(h, "x");
                    key(h, "Ctrl+Enter")
                },
            },
        )
        .put("Sheet1!H1", "x")
        .put("Sheet1!H2", "x")
        .put("Sheet1!H3", "x"),
        // Alt+Enter breaks the line; the cell wraps to show it.
        Entry::new(
            "line break in a cell",
            "H2",
            Act::Custom {
                prep: no_prep,
                act: |h| {
                    type_text(h, "a");
                    key(h, "Alt+Enter")?;
                    type_text(h, "b");
                    key(h, "Enter")
                },
            },
        )
        .put("Sheet1!H2", "a\nb")
        .set("Sheet1!H2.style.wrap", "true"),
        // F4 while typing a reference makes it absolute.
        Entry::new(
            "absolute reference with F4",
            "H2",
            Act::Custom {
                prep: no_prep,
                act: |h| {
                    type_text(h, "=C2");
                    key(h, "F4")?;
                    key(h, "Enter")
                },
            },
        )
        .formula("Sheet1!H2", "=$C$2", "10.0"),
        // Ctrl+Shift+Enter: a legacy array formula over the selection.
        Entry::new(
            "array formula",
            "H2:H3",
            Act::Custom {
                prep: no_prep,
                act: |h| {
                    type_text(h, "=C2:C3*2");
                    key(h, "Ctrl+Shift+Enter")
                },
            },
        )
        // One formula over a fixed area (the file's t="array" ref="H2:H3"); H3 holds a
        // part of its result.
        .formula("Sheet1!H2", "=C2:C3*2", "20.0")
        .set("Sheet1!H2.array", "\"H2:H3\"")
        .set("Sheet1!H3.value", "8.0"),
        // Ctrl+; enters today's date as a constant, shown as a date.
        Entry::new("insert date", "H2", Act::Key("Ctrl+;"))
            .set("Sheet1!H2.style.num_fmt", "\"m/d/yyyy\"")
            .allow("Sheet1!H2.input")
            .allow("Sheet1!H2.value")
            .then(|s| match value_at(s, "H2") {
                Some(v) if (v - today_serial()).abs() <= 1.0 && v.fract() == 0.0 => Ok(()),
                other => Err(format!("H2 holds {other:?}, not today")),
            }),
        Entry::new("insert time", "H2", Act::Key("Ctrl+Shift+;"))
            .set("Sheet1!H2.style.num_fmt", "\"h:mm AM/PM\"")
            .allow("Sheet1!H2.input")
            .allow("Sheet1!H2.value")
            .then(|s| match value_at(s, "H2") {
                Some(v) if (0.0..1.0).contains(&v) => Ok(()),
                other => Err(format!("H2 holds {other:?}, not a time of day")),
            }),
        // Ctrl+' copies the formula above exactly (not adjusted); Ctrl+Shift+" its value.
        Entry::new("copy formula from above", "E7", Act::Key("Ctrl+'")).formula(
            "Sheet1!E7",
            "=C6*D6",
            "4.5",
        ),
        Entry::new("copy value from above", "E7", Act::Key("Ctrl+Shift+\""))
            .put("Sheet1!E7", "4.5"),
        // Space on a checkbox cell ticks it.
        Entry::new("tick a checkbox", "H2", Act::Key("Space"))
            .setup("insert.checkbox", json!({"range": "H2"}))
            .set("Sheet1!H2.value", "true")
            .set("Sheet1!H2.input", "\"TRUE\""),
        // Undo / redo / repeat by key.
        no_undo(
            Entry::new(
                "undo",
                "A1",
                Act::Custom {
                    prep: |h| key(h, "Ctrl+B"),
                    act: |h| key(h, "Ctrl+Z"),
                },
            )
            .gone("Sheet1!A1.style.bold"),
        ),
        Entry::new(
            "redo",
            "A1",
            Act::Custom {
                prep: |h| {
                    key(h, "Ctrl+B")?;
                    key(h, "Ctrl+Z")
                },
                act: |h| key(h, "Ctrl+Y"),
            },
        )
        .set("Sheet1!A1.style.bold", "true"),
        // F4 repeats the last action on the new selection.
        Entry::new(
            "repeat",
            "A1",
            Act::Custom {
                prep: |h| {
                    key(h, "Ctrl+I")?;
                    click_cell(h, "B2");
                    Ok(())
                },
                act: |h| key(h, "F4"),
            },
        )
        .set("Sheet1!B2.style.italic", "true"),
        Entry::new(
            "print titles",
            "A1",
            Act::Dialog {
                ribbon: ("Page Layout", "Page Setup", "Print Titles"),
                fill: |app| {
                    let d = dialog!(app, Params);
                    let f = d.fields.iter_mut().find(|f| f.key == "rows").unwrap();
                    f.value = "1:1".into();
                },
                ok: "OK",
            },
        )
        .set("workbook.names.0.name", "\"_xlnm.Print_Titles\"")
        .set("workbook.names.0.formula", "\"Sheet1!$1:$1\"")
        .allow("workbook.names.0.*")
        .print(),
    ]
}

// ---------------------------------------------------------------------------------------
// The run, and the coverage gate

fn entries() -> Vec<Entry> {
    let mut v = Vec::new();
    v.extend(home_font());
    v.extend(home_alignment());
    v.extend(home_number());
    v.extend(home_clipboard());
    v.extend(home_cells());
    v.extend(home_editing());
    v.extend(home_styles());
    v.extend(grid_menus());
    v.extend(tab_menus());
    v.extend(insert_tab());
    v.extend(layout_tab());
    v.extend(keyboard_nav());
    v.extend(keyboard_edit());
    v
}

/// Entries whose command does not show in the command log (navigation and selection are
/// not logged; dialog-opening commands log the command the dialog's OK runs): the entry
/// name and the command it proves.
const UNLOGGED: &[(&str, &str)] = &[
    ("format cells font tab", "home.font.dialog"),
    ("format cells font by key", "home.font.dialog"),
    ("orientation", "home.align.orientation"),
    ("number format dialog", "home.number.format"),
    ("format cells dialog", "home.number.dialog"),
    ("format cells by key, border", "home.number.dialog"),
    ("find all", "edit.find_all"),
    ("highlight cells rule", "home.styles.cf.highlight_rules"),
    ("top bottom rule", "home.styles.cf.top_bottom"),
    ("manage rules delete", "home.styles.cf.manage"),
    ("go to by key", "home.editing.goto"),
    ("go to special blanks", "home.editing.goto_special"),
    ("print titles", "layout.print_titles"),
    ("move up", "nav.move_up"),
    ("move down", "nav.move_down"),
    ("move left", "nav.move_left"),
    ("move right", "nav.move_right"),
    ("edge up", "nav.edge_up"),
    ("edge down", "nav.edge_down"),
    ("edge left", "nav.edge_left"),
    ("edge right", "nav.edge_right"),
    ("home of row", "nav.home_row"),
    ("home of sheet", "nav.home_sheet"),
    ("end of sheet", "nav.end_sheet"),
    ("page down", "nav.page_down"),
    ("page up", "nav.page_up"),
    ("page right", "nav.page_right"),
    ("page left", "nav.page_left"),
    ("enter moves down", "nav.enter_next"),
    ("enter within a selection", "nav.enter_next"),
    ("tab moves right", "nav.tab_next"),
    ("extend up", "select.extend_up"),
    ("extend down", "select.extend_down"),
    ("extend left", "select.extend_left"),
    ("extend right", "select.extend_right"),
    ("extend to edge up", "select.extend_edge_up"),
    ("extend to edge down", "select.extend_edge_down"),
    ("extend to edge left", "select.extend_edge_left"),
    ("extend to edge right", "select.extend_edge_right"),
    ("select all", "select.all"),
    ("select row", "select.row"),
    ("select column", "select.column"),
    ("select current region", "select.current_region"),
    ("select visible cells", "select.visible_cells"),
    // The grid selects directly on a drag; select.range is that action's command form.
    ("drag to select", "select.range"),
    ("name box", "nav.goto_cell"),
    // F2 and Escape are the editor's own (the editor opens and closes, no command runs).
    ("edit with F2", "cell.edit"),
    ("cancel an edit", "cell.cancel_edit"),
    // Editor helpers run as queries on the text being typed.
    ("line break in a cell", "cell.newline"),
    ("absolute reference with F4", "cell.toggle_absolute"),
];

/// Commands the app gets wrong in a way too large to fix here: (id, why).
const KNOWN_GAPS: &[(&str, &str)] = &[
    (
        "home.font.borders",
        "the Borders button applies All Borders; the target's split button applies its last \
         border (Bottom Border at first) and drops down a preset gallery the app lacks",
    ),
    (
        "home.cells.insert_cells",
        "partial selections insert whole rows; the target asks (shift right / down) and \
         shifts only those cells (whole-row selections are proved)",
    ),
    (
        "home.cells.delete_cells",
        "partial selections delete whole rows; the target asks (shift left / up) and \
         shifts only those cells (whole-row selections are proved)",
    ),
    (
        "home.editing.select_formulas",
        "no multi-area selections: formulas in several blocks select their bounding box \
         (one block is proved)",
    ),
    (
        "insert.slicer",
        "slicers are not saved: the .xlsx writer has no slicer parts, so a slicer is lost \
         on save and reopen",
    ),
    (
        "cell.autofill",
        "the grid has no fill handle: AutoFill (drag or double-click the handle) has no \
         user path; Fill Down/Right/Series are proved",
    ),
    (
        "cell.autocomplete",
        "typing does not complete from the column's entries (the editor never asks)",
    ),
    (
        "cell.pick_from_list",
        "Alt+Down runs the query but the shell shows no pick list, so nothing can be picked",
    ),
    (
        "layout.headings_view",
        "headings are one session-wide view flag, not the sheet's showRowColHeaders: they \
         apply to every sheet and are not saved (the toggle itself is proved)",
    ),
    (
        "sheet.select_all_sheets",
        "no sheet grouping: the command only lists the sheets and no menu offers it",
    ),
];

/// Commands that change no spreadsheet property: (id, why).
const NOT_PROPERTY: &[(&str, &str)] = &[];

const PREFIXES: [&str; 8] = [
    "home.", "insert.", "layout.", "cell.", "edit.", "nav.", "select.", "sheet.",
];

struct Outcome {
    proved: Proved,
    failed: Vec<&'static str>,
    names: Vec<&'static str>,
}

/// One run of every entry, shared by the tests below.
fn outcome() -> &'static Outcome {
    static O: OnceLock<Outcome> = OnceLock::new();
    O.get_or_init(|| {
        let all = entries();
        let (proved, failed) = check_all(&all);
        Outcome {
            proved,
            failed,
            names: all.iter().map(|e| e.name).collect(),
        }
    })
}

#[test]
fn every_entry_changes_exactly_its_properties() {
    let o = outcome();
    assert!(
        o.failed.is_empty(),
        "{} entries failed: {:?}",
        o.failed.len(),
        o.failed
    );
}

/// Coverage may only grow: raise these as entries are added.
const COMMANDS_FLOOR_A: usize = 204;
const PROPERTY_KINDS_FLOOR_A: usize = 313;

#[test]
fn coverage_gate() {
    let o = outcome();
    let mut proved: std::collections::BTreeSet<String> = o
        .proved
        .commands
        .iter()
        .filter(|c| PREFIXES.iter().any(|p| c.starts_with(p)))
        .cloned()
        .collect();
    for (name, id) in UNLOGGED {
        assert!(
            o.names.contains(name),
            "UNLOGGED names a missing entry '{name}'"
        );
        if !o.failed.contains(name) {
            proved.insert((*id).to_string());
        }
    }
    let ours: Vec<&'static str> = sp_engine::registry()
        .all()
        .iter()
        .map(|c| c.id)
        .filter(|id| PREFIXES.iter().any(|p| id.starts_with(p)))
        // Native file dialogs (Pictures, Background) belong to the desktop app.
        .filter(|id| !sp_ui::shell_hook::DESKTOP_INTERCEPTED.contains(id))
        .collect();
    let listed = |id: &str| {
        KNOWN_GAPS.iter().any(|(k, _)| *k == id) || NOT_PROPERTY.iter().any(|(k, _)| *k == id)
    };
    let missing: Vec<&&str> = ours
        .iter()
        .filter(|id| !proved.contains(**id) && !listed(id))
        .collect();
    println!(
        "part A: {} of {} commands proved by {} entries; {} known gaps; {} not properties",
        proved.len(),
        ours.len(),
        o.names.len(),
        KNOWN_GAPS.len(),
        NOT_PROPERTY.len()
    );
    let unproved: Vec<&&str> = ours.iter().filter(|id| !proved.contains(**id)).collect();
    println!("not proved (listed above or missing): {unproved:?}");
    assert!(
        proved.len() >= COMMANDS_FLOOR_A,
        "commands proved {} < floor {COMMANDS_FLOOR_A}",
        proved.len()
    );
    assert!(
        o.proved.changed.len() >= PROPERTY_KINDS_FLOOR_A,
        "property kinds {} < floor {PROPERTY_KINDS_FLOOR_A}",
        o.proved.changed.len()
    );
    assert!(
        missing.is_empty(),
        "{} commands neither proved nor listed: {missing:?}",
        missing.len()
    );
}
