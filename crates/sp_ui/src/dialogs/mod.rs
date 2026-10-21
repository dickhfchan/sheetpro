//! Modal dialogs that collect params and run commands. The shell stays thin: every OK path
//! calls `Session::execute`; nothing here mutates the workbook directly.
//!
//! Open/closed state and every field live in [`UiState`], which is serializable, so a
//! screenshot script (or a test) can open any dialog by name with pre-filled fields:
//! `App::open_dialog("find {\"what\":\"total\"}")`.

pub mod cf_dialog;
pub mod chart_format;
pub mod find;
pub mod format_cells;
pub mod grid_popups;
pub mod misc;
pub mod params_form;
pub mod sort;
pub mod spelling;
pub mod table_dialog;
pub mod tools;
pub mod widgets;

use crate::App;
use crate::theme::LIGHT as T;
use egui::{Context, Key, RichText, Stroke};
use serde::{Deserialize, Serialize};
use sp_engine::{Json, registry};

/// Shell state that is not part of the session: which dialog is open and its fields.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UiState {
    pub dialog: Option<Dialog>,
    /// Selected chart / drag in progress over the grid.
    pub grid: crate::overlays::GridUi,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Dialog {
    FormatCells(Box<format_cells::FormatCells>),
    Find(find::FindReplace),
    GoTo(find::GoTo),
    Sort(sort::Sort),
    PasteSpecial(misc::PasteSpecial),
    InsertFunction(misc::InsertFunction),
    NameManager(misc::NameManager),
    DefineName(misc::DefineName),
    Prompt(misc::Prompt),
    Zoom(misc::Zoom),
    UnhideSheet(misc::UnhideSheet),
    TabColor(misc::TabColor),
    Params(params_form::Params),
    Filter(Box<grid_popups::FilterDropdown>),
    ValidationList(grid_popups::ValidationList),
    Comment(grid_popups::CommentCard),
    ValidationAlert(grid_popups::ValidationAlert),
    ChartFormat(chart_format::ChartFormat),
    CondFormat(Box<cf_dialog::CfDialog>),
    CreateTable(table_dialog::CreateTable),
    TableDesign(Box<table_dialog::TableDesign>),
    Spelling(spelling::Spelling),
    DataValidation(Box<tools::DataValidation>),
    GoalSeek(tools::GoalSeek),
    NewComment(tools::NewComment),
    GoToSpecial(tools::GoToSpecial),
}

/// Names accepted by [`App::open_dialog`] (and `sp screenshot --dialog`).
pub const DIALOG_NAMES: [&str; 43] = [
    "spelling",
    "data_validation",
    "goal_seek",
    "new_note",
    "new_comment",
    "goto_special",
    "cf_new",
    "cf_highlight",
    "cf_top_bottom",
    "cf_manage",
    "create_table",
    "format_as_table",
    "table_design",
    "cf_formula",
    "filter",
    "validation_list",
    "comment",
    "validation_alert",
    "chart_format",
    "format_cells",
    "number",
    "alignment",
    "font",
    "border",
    "fill",
    "protection",
    "find",
    "replace",
    "goto",
    "sort",
    "sort_custom",
    "paste_special",
    "insert_function",
    "name_manager",
    "define_name",
    "column_width",
    "row_height",
    "default_width",
    "rename_sheet",
    "insert_sheet",
    "zoom",
    "unhide_sheet",
    "tab_color",
];

/// Whether a dialog stays open after this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Keep,
    Close,
}

/// The dialog a command opens instead of running with empty params.
pub fn dialog_for_command(id: &str) -> Option<&'static str> {
    Some(match id {
        "home.number.dialog" | "home.number.format" => "number",
        "home.font.dialog" | "home.font.family" | "home.font.size" | "home.font.font_color" => {
            "font"
        }
        "home.font.fill_color" => "fill",
        "home.align.orientation" => "alignment",
        "home.editing.find" => "find",
        "home.editing.replace" => "replace",
        "home.editing.goto" | "nav.goto_cell" => "goto",
        "data.sort" => "sort",
        "home.editing.sort_custom" => "sort_custom",
        "home.clipboard.paste_special" => "paste_special",
        "formulas.insert_function" | "formulas.library.more" => "insert_function",
        // Function Library menus: Insert Function, filtered to the menu's category.
        "formulas.library.financial" => r#"insert_function {"category": "Financial"}"#,
        "formulas.library.logical" => r#"insert_function {"category": "Logical"}"#,
        "formulas.library.text" => r#"insert_function {"category": "Text"}"#,
        "formulas.library.datetime" => r#"insert_function {"category": "Date & Time"}"#,
        "formulas.library.lookup" => r#"insert_function {"category": "Lookup & Reference"}"#,
        "formulas.library.math" => r#"insert_function {"category": "Math & Trig"}"#,
        "formulas.name_manager" => "name_manager",
        "formulas.define_name" => "define_name",
        "home.cells.column_width" => "column_width",
        "home.cells.row_height" => "row_height",
        "home.cells.default_width" => "default_width",
        "home.cells.rename_sheet" => "rename_sheet",
        "home.cells.unhide_sheet" => "unhide_sheet",
        "home.cells.tab_color" => "tab_color",
        "view.zoom" => "zoom",
        "home.styles.cf.new_rule" => "cf_new",
        "home.styles.cf.highlight_rules" => "cf_highlight",
        "home.styles.cf.top_bottom" => "cf_top_bottom",
        "home.styles.cf.manage" => "cf_manage",
        "insert.table" => "create_table",
        "home.styles.format_as_table" => "format_as_table",
        "table.design" => "table_design",
        "review.spelling" => "spelling",
        "data.validation" => "data_validation",
        "data.what_if.goal_seek" => "goal_seek",
        "insert.note" => "new_note",
        "insert.comment" | "review.comment.new" => "new_comment",
        "home.editing.goto_special" => "goto_special",
        // Every parameter is optional, but the command needs one of them to do anything.
        "insert.hyperlink" => "params:insert.hyperlink",
        "layout.margins" => "params:layout.margins",
        "layout.scale_to_fit" => "params:layout.scale_to_fit",
        "data.what_if.data_table" => "params:data.what_if.data_table",
        _ => return None,
    })
}

/// Top-level keys a params doc marks as required (`"key":` rather than `"key"?:`).
pub fn required_params(doc: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut chars = doc.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' | '[' => depth += 1,
            '}' | ']' => depth -= 1,
            '"' => {
                let mut s = String::new();
                while let Some(ch) = chars.next() {
                    match ch {
                        '\\' => {
                            chars.next();
                        }
                        '"' => break,
                        _ => s.push(ch),
                    }
                }
                while chars.peek().is_some_and(|c| c.is_whitespace()) {
                    chars.next();
                }
                if depth == 1 && chars.peek() == Some(&':') {
                    out.push(s);
                }
            }
            _ => {}
        }
    }
    out
}

/// The AutoFilter range (or table, by name) whose header row holds the active column.
fn filter_at_active(s: &sp_engine::Session) -> Option<(sp_core::CellRange, Option<String>)> {
    let sh = s.wb.sheet(s.sheet_index())?;
    let a = s.sel.active;
    let in_cols = |r: &sp_core::CellRange| a.col >= r.start.col && a.col <= r.end.col;
    if let Some(af) = sh.auto_filter.as_ref().filter(|af| in_cols(&af.range)) {
        return Some((af.range, None));
    }
    sh.tables
        .iter()
        .find(|t| t.range.contains(a) && t.header_row)
        .map(|t| (t.range, Some(t.name.clone())))
}

impl Dialog {
    /// Build a dialog by name, seeded from the session (active cell style, selection…).
    pub fn create(name: &str, app: &mut App) -> Result<Dialog, String> {
        use format_cells::{FcTab, FormatCells};
        let fc =
            |app: &App, tab| Dialog::FormatCells(Box::new(FormatCells::new(&app.session, tab)));
        Ok(match name {
            "format_cells" | "number" => fc(app, FcTab::Number),
            "alignment" => fc(app, FcTab::Alignment),
            "font" => fc(app, FcTab::Font),
            "border" => fc(app, FcTab::Border),
            "fill" => fc(app, FcTab::Fill),
            "protection" => fc(app, FcTab::Protection),
            "find" => Dialog::Find(find::FindReplace::new(false)),
            "replace" => Dialog::Find(find::FindReplace::new(true)),
            "goto" => Dialog::GoTo(find::GoTo::new(app)),
            "sort" => Dialog::Sort(sort::Sort::new(app, "data.sort")),
            "sort_custom" => Dialog::Sort(sort::Sort::new(app, "home.editing.sort_custom")),
            "paste_special" => Dialog::PasteSpecial(misc::PasteSpecial::default()),
            "insert_function" => Dialog::InsertFunction(misc::InsertFunction::default()),
            "name_manager" => Dialog::NameManager(misc::NameManager::new(app)),
            "define_name" => Dialog::DefineName(misc::DefineName::new(app, None)),
            "column_width" => Dialog::Prompt(misc::Prompt::column_width(&app.session)),
            "row_height" => Dialog::Prompt(misc::Prompt::row_height(&app.session)),
            "default_width" => Dialog::Prompt(misc::Prompt::default_width(&app.session)),
            "rename_sheet" => Dialog::Prompt(misc::Prompt::rename_sheet(&app.session, None)),
            "insert_sheet" => Dialog::Prompt(misc::Prompt::insert_sheet()),
            "zoom" => Dialog::Zoom(misc::Zoom::new(&app.session)),
            "unhide_sheet" => Dialog::UnhideSheet(misc::UnhideSheet::new(&app.session)),
            "tab_color" => Dialog::TabColor(misc::TabColor::new(&app.session, None)),
            "filter" => {
                let (range, table) = filter_at_active(&app.session)
                    .ok_or("no AutoFilter or table header at the active column")?;
                let col = app.session.sel.active.col;
                Dialog::Filter(Box::new(grid_popups::FilterDropdown::new(
                    &app.session,
                    range,
                    col,
                    table,
                )))
            }
            "validation_list" => Dialog::ValidationList(grid_popups::ValidationList::new(
                &app.session,
                app.session.sel.active,
            )),
            "comment" => Dialog::Comment(grid_popups::CommentCard::new(
                &app.session,
                app.session.sel.active,
            )),
            "validation_alert" => {
                let s = &app.session;
                let (si, at) = (s.sheet_index(), s.sel.active);
                let msg = s
                    .wb
                    .sheet(si)
                    .and_then(|sh| sh.validation_at(at))
                    .map(|r| r.error.clone())
                    .filter(|m| !m.is_empty())
                    .unwrap_or_else(|| {
                        "This value doesn't match the data validation restrictions defined for this cell.".into()
                    });
                Dialog::ValidationAlert(grid_popups::ValidationAlert::new(s, si, at, "", &msg))
            }
            "chart_format" => {
                let idx = app.ui.grid.chart.unwrap_or(0);
                Dialog::ChartFormat(
                    chart_format::ChartFormat::new(&app.session, idx)
                        .ok_or("no chart on this sheet")?,
                )
            }
            "cf_new" | "cf_highlight" | "cf_top_bottom" | "cf_formula" => {
                use cf_dialog::{CAT_CONTAINS, CAT_FORMULA, CAT_TOP, CfDialog};
                let cat = match name {
                    "cf_top_bottom" => CAT_TOP,
                    "cf_formula" => CAT_FORMULA,
                    _ => CAT_CONTAINS,
                };
                Dialog::CondFormat(Box::new(CfDialog::new_rule(&app.session, cat)))
            }
            "cf_manage" => Dialog::CondFormat(Box::new(cf_dialog::CfDialog::manager(&app.session))),
            "create_table" => {
                Dialog::CreateTable(table_dialog::CreateTable::new(&app.session, "insert.table"))
            }
            // Restyles the table at the active cell, else creates one with a chosen style.
            "format_as_table" => match table_dialog::TableDesign::at_active(&app.session) {
                Some(d) => Dialog::TableDesign(Box::new(d)),
                None => Dialog::CreateTable(table_dialog::CreateTable::new(
                    &app.session,
                    "home.styles.format_as_table",
                )),
            },
            "table_design" => Dialog::TableDesign(Box::new(
                table_dialog::TableDesign::new(&app.session).ok_or("no table on this sheet")?,
            )),
            "spelling" => Dialog::Spelling(spelling::Spelling::new(app)),
            "data_validation" => {
                Dialog::DataValidation(Box::new(tools::DataValidation::new(&app.session)))
            }
            "goal_seek" => Dialog::GoalSeek(tools::GoalSeek::new(&app.session)),
            "new_note" => Dialog::NewComment(tools::NewComment::new(&app.session, true)),
            "new_comment" => Dialog::NewComment(tools::NewComment::new(&app.session, false)),
            "goto_special" => Dialog::GoToSpecial(tools::GoToSpecial::default()),
            other => match other.strip_prefix("params:") {
                Some(id) => Dialog::Params(params_form::Params::new(id, &app.session)?),
                None => return Err(format!("unknown dialog '{other}'")),
            },
        })
    }

    fn ui(&mut self, ctx: &Context, app: &mut App) -> Flow {
        match self {
            Dialog::FormatCells(d) => d.ui(ctx, app),
            Dialog::Find(d) => d.ui(ctx, app),
            Dialog::GoTo(d) => d.ui(ctx, app),
            Dialog::Sort(d) => d.ui(ctx, app),
            Dialog::PasteSpecial(d) => d.ui(ctx, app),
            Dialog::InsertFunction(d) => d.ui(ctx, app),
            Dialog::NameManager(d) => d.ui(ctx, app),
            Dialog::DefineName(d) => d.ui(ctx, app),
            Dialog::Prompt(d) => d.ui(ctx, app),
            Dialog::Zoom(d) => d.ui(ctx, app),
            Dialog::UnhideSheet(d) => d.ui(ctx, app),
            Dialog::TabColor(d) => d.ui(ctx, app),
            Dialog::Params(d) => d.ui(ctx, app),
            Dialog::Filter(d) => d.ui(ctx, app),
            Dialog::ValidationList(d) => d.ui(ctx, app),
            Dialog::Comment(d) => d.ui(ctx, app),
            Dialog::ValidationAlert(d) => d.ui(ctx, app),
            Dialog::ChartFormat(d) => d.ui(ctx, app),
            Dialog::CondFormat(d) => d.ui(ctx, app),
            Dialog::CreateTable(d) => d.ui(ctx, app),
            Dialog::TableDesign(d) => d.ui(ctx, app),
            Dialog::Spelling(d) => d.ui(ctx, app),
            Dialog::DataValidation(d) => d.ui(ctx, app),
            Dialog::GoalSeek(d) => d.ui(ctx, app),
            Dialog::NewComment(d) => d.ui(ctx, app),
            Dialog::GoToSpecial(d) => d.ui(ctx, app),
        }
    }

    /// Work a freshly opened (possibly script-prefilled) dialog does once.
    fn after_open(&mut self, app: &mut App) {
        if let Dialog::Find(d) = self
            && d.find_all_on_open
        {
            d.find_all_on_open = false;
            d.find_all(app);
        }
    }
}

impl App {
    /// Open a dialog by name, optionally followed by a JSON object that overrides its fields,
    /// e.g. `find {"what": "x", "find_all_on_open": true}`. Used by shortcuts, the ribbon,
    /// context menus, tests and `sp screenshot --dialog`.
    pub fn open_dialog(&mut self, spec: &str) -> Result<(), String> {
        let spec = spec.trim();
        let (name, rest) = match spec.find(|c: char| c.is_whitespace() || c == '{') {
            Some(i) => (
                spec.get(..i).unwrap_or(spec),
                spec.get(i..).unwrap_or("").trim(),
            ),
            None => (spec, ""),
        };
        let mut d = Dialog::create(name, self)?;
        if !rest.is_empty() {
            let patch: Json =
                serde_json::from_str(rest).map_err(|e| format!("dialog fields: {e}"))?;
            let mut cur = serde_json::to_value(&d).map_err(|e| e.to_string())?;
            if let (Some(obj), Some(p)) = (cur.as_object_mut(), patch.as_object()) {
                for (k, v) in p {
                    if k != "kind" {
                        obj.insert(k.clone(), v.clone());
                    }
                }
            }
            d = serde_json::from_value(cur).map_err(|e| format!("dialog fields: {e}"))?;
        }
        if self.editing_cell.is_some() && !matches!(d, Dialog::InsertFunction(_)) {
            self.commit_edit(None);
        }
        d.after_open(self);
        self.ui.dialog = Some(d);
        Ok(())
    }

    /// Name of the open dialog's kind (for tests / the control channel).
    pub fn dialog_kind(&self) -> Option<String> {
        let d = self.ui.dialog.as_ref()?;
        serde_json::to_value(d)
            .ok()?
            .get("kind")?
            .as_str()
            .map(str::to_string)
    }

    /// The command a ribbon button, menu item or shortcut stands for: a dialog when the
    /// command has one or needs params, otherwise run it straight away.
    pub fn activate_command(&mut self, id: &str) {
        if crate::shell_hook::intercept(id) {
            return;
        }
        if let Some(name) = dialog_for_command(id) {
            if let Err(e) = self.open_dialog(name) {
                self.last_error = Some(e);
            }
            return;
        }
        let doc = registry().get(id).map_or("", |c| c.params);
        if !required_params(doc).is_empty() {
            if let Err(e) = self.open_dialog(&format!("params:{id}")) {
                self.last_error = Some(e);
            }
            return;
        }
        self.exec(id, serde_json::json!({}));
    }

    /// Run a command for a dialog: errors come back to the dialog instead of the status bar.
    pub fn try_exec(&mut self, id: &str, params: Json) -> Result<Json, String> {
        let r = self.session.execute(id, &params).map_err(|e| e.to_string());
        if r.is_ok() {
            self.last_error = None;
        }
        if self.editing_cell.is_none() {
            self.sync_buffer();
        }
        r
    }

    /// Run a read-only command (lists, lookups) without making it the Repeat target.
    pub fn query(&mut self, id: &str, params: Json) -> Result<Json, String> {
        let cmd = registry()
            .get(id)
            .ok_or_else(|| format!("unknown command '{id}'"))?;
        (cmd.spec.run)(&mut self.session, &params).map_err(|e| e.to_string())
    }

    pub(crate) fn show_dialog(&mut self, ctx: &Context) {
        let Some(mut d) = self.ui.dialog.take() else {
            return;
        };
        let flow = d.ui(ctx, self);
        if flow == Flow::Keep && self.ui.dialog.is_none() {
            self.ui.dialog = Some(d);
        }
    }
}

/// Modal frame with a title. Returns the body's value and whether Escape was pressed.
pub(crate) fn window<R>(
    ctx: &Context,
    title: &str,
    width: f32,
    body: impl FnOnce(&mut egui::Ui) -> R,
) -> (R, bool) {
    let style = ctx.global_style();
    let frame = egui::Frame::popup(&style)
        .fill(T.dialog_bg)
        .stroke(Stroke::new(1.0, T.dialog_border))
        .corner_radius(6.0)
        .inner_margin(egui::Margin::same(14));
    let r = egui::Modal::new(egui::Id::new("sp-dialog"))
        .frame(frame)
        .backdrop_color(T.dialog_backdrop)
        .show(ctx, |ui| {
            ui.set_width(width);
            ui.visuals_mut().selection.bg_fill = T.list_selected;
            ui.visuals_mut().selection.stroke.color = T.ribbon_text;
            ui.label(
                RichText::new(title)
                    .size(15.0)
                    .strong()
                    .color(T.dialog_title),
            );
            ui.add_space(4.0);
            ui.separator();
            ui.add_space(4.0);
            body(ui)
        });
    let esc = ctx.input(|i| i.key_pressed(Key::Escape));
    (r.inner, esc)
}

/// Right-aligned OK / Cancel row. Returns (ok, cancel).
pub(crate) fn ok_cancel(ui: &mut egui::Ui, ok_label: &str) -> (bool, bool) {
    ui.add_space(6.0);
    let mut out = (false, false);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        out.1 = ui
            .add(egui::Button::new("Cancel").min_size(egui::vec2(72.0, 24.0)))
            .clicked();
        out.0 = ui
            .add(
                egui::Button::new(RichText::new(ok_label).color(egui::Color32::WHITE))
                    .fill(T.accent)
                    .min_size(egui::vec2(72.0, 24.0)),
            )
            .clicked();
    });
    out
}

/// Error line inside a dialog.
pub(crate) fn error_line(ui: &mut egui::Ui, e: &Option<String>) {
    if let Some(e) = e {
        ui.label(RichText::new(e).color(T.error_text).size(12.0));
    }
}

pub(crate) fn enter_pressed(ctx: &Context) -> bool {
    ctx.input(|i| i.key_pressed(Key::Enter))
}

pub(crate) fn label(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.label(RichText::new(text).color(T.dialog_label))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_params_skips_optional_and_nested() {
        assert_eq!(
            required_params(r#"{"range"?: "A1", "size": 14}"#),
            vec!["size".to_string()]
        );
        assert_eq!(
            required_params(
                r#"{"range"?: "A1:D20", "keys": [{"column": "B", "order": "asc|desc"}], "header"?: bool}"#
            ),
            vec!["keys".to_string()]
        );
        assert!(required_params(r#"{"range"?: "A1:B2"}"#).is_empty());
        assert!(required_params("{}").is_empty());
        assert_eq!(
            required_params(r#"{"sheet"?: "Sheet1", "color": "FF0000" | null}"#),
            vec!["color".to_string()]
        );
    }

    #[test]
    fn every_dialog_name_builds() {
        let mut app = App::new(sp_engine::Session::new());
        for n in DIALOG_NAMES {
            // These need an object at the active cell (filter header, chart).
            let needs_context = matches!(n, "filter" | "chart_format" | "table_design");
            assert_eq!(Dialog::create(n, &mut app).is_ok(), !needs_context, "{n}");
        }
        assert!(Dialog::create("params:view.zoom", &mut app).is_ok());
        assert!(Dialog::create("nope", &mut app).is_err());
    }
}
