//! Smaller dialogs: Paste Special, Insert Function, Name Manager / Define Name, one-value
//! prompts (column width, row height, sheet names), Zoom, Unhide Sheet, Tab Color, and a
//! generic params form for any command that needs params and has no dedicated dialog.

use super::widgets::{list_box, swatch_grid};
use super::{Dialog, Flow, enter_pressed, error_line, label, ok_cancel, window};
use crate::App;
use crate::theme::{LIGHT as T, hex};
use egui::{Context, RichText, Vec2};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sp_engine::{Json, Session, catalog, registry};

fn done(r: Result<Json, String>, error: &mut Option<String>) -> Flow {
    match r {
        Ok(_) => Flow::Close,
        Err(e) => {
            *error = Some(e);
            Flow::Keep
        }
    }
}

fn sheet_name(s: &Session, i: usize) -> String {
    s.wb.sheets
        .get(i)
        .map(|x| x.name.clone())
        .unwrap_or_default()
}

// ── Paste Special ────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PasteSpecial {
    /// all | formulas | values | formats
    pub mode: String,
    pub transpose: bool,
    pub error: Option<String>,
}

impl Default for PasteSpecial {
    fn default() -> Self {
        PasteSpecial {
            mode: "all".into(),
            transpose: false,
            error: None,
        }
    }
}

impl PasteSpecial {
    pub fn params(&self) -> Json {
        let mode = if self.transpose {
            "transpose"
        } else {
            self.mode.as_str()
        };
        json!({ "mode": mode })
    }

    pub fn ui(&mut self, ctx: &Context, app: &mut App) -> Flow {
        let ((ok, cancel), esc) = window(ctx, "Paste Special", 320.0, |ui| {
            ui.label(RichText::new("Paste").strong());
            ui.add_enabled_ui(!self.transpose, |ui| {
                for (k, n) in [
                    ("all", "All"),
                    ("formulas", "Formulas"),
                    ("values", "Values"),
                    ("formats", "Formats"),
                ] {
                    ui.radio_value(&mut self.mode, k.to_string(), n);
                }
            });
            ui.add_space(6.0);
            ui.checkbox(&mut self.transpose, "Transpose");
            if app.session.clipboard.is_none() {
                label(ui, "The clipboard is empty: copy or cut cells first.");
            }
            error_line(ui, &self.error);
            ok_cancel(ui, "OK")
        });
        if cancel || esc {
            return Flow::Close;
        }
        if ok {
            return done(
                app.try_exec("home.clipboard.paste_special", self.params()),
                &mut self.error,
            );
        }
        Flow::Keep
    }
}

// ── Insert Function ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct InsertFunction {
    pub search: String,
    /// Empty = all categories.
    pub category: String,
    pub selected: Option<String>,
    pub error: Option<String>,
}

impl InsertFunction {
    pub fn names(&self, app: &mut App) -> Vec<String> {
        let p = if self.category.is_empty() {
            json!({})
        } else {
            json!({ "category": self.category })
        };
        let mut v: Vec<String> = app
            .query("formulas.insert_function", p)
            .ok()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        let q = self.search.trim().to_ascii_uppercase();
        if !q.is_empty() {
            v.retain(|n| n.contains(&q));
        }
        v.sort();
        v.dedup();
        v
    }

    /// Put `=NAME(` (or `NAME(` after an existing formula) in the edit buffer.
    pub fn insert(app: &mut App, name: &str) {
        let editing = app.editing_cell.is_some();
        let text = if editing && app.edit_buf.starts_with('=') {
            format!("{}{name}(", app.edit_buf)
        } else {
            format!("={name}(")
        };
        app.begin_edit(Some(text));
    }

    pub fn ui(&mut self, ctx: &Context, app: &mut App) -> Flow {
        let names = self.names(app);
        let mut chosen = None;
        let ((ok, cancel), esc) = window(ctx, "Insert Function", 420.0, |ui| {
            label(ui, "Search for a function:");
            ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text("Type a name, e.g. SUM")
                    .desired_width(420.0),
            );
            ui.horizontal(|ui| {
                label(ui, "Category:");
                let cur = if self.category.is_empty() {
                    "All"
                } else {
                    self.category.as_str()
                };
                egui::ComboBox::new("if-cat", "")
                    .selected_text(cur.to_string())
                    .width(220.0)
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(self.category.is_empty(), "All")
                            .clicked()
                        {
                            self.category.clear();
                        }
                        for g in catalog::functions() {
                            if ui
                                .selectable_label(self.category == g.category, &g.category)
                                .clicked()
                            {
                                self.category.clone_from(&g.category);
                            }
                        }
                    });
            });
            label(ui, "Select a function:");
            let sel = self
                .selected
                .as_ref()
                .and_then(|s| names.iter().position(|n| n == s));
            if let Some((i, dbl)) = list_box(ui, "if-list", Vec2::new(420.0, 200.0), &names, sel) {
                self.selected = names.get(i).cloned();
                if dbl {
                    chosen = self.selected.clone();
                }
            }
            if let Some(n) = &self.selected {
                ui.label(RichText::new(format!("{n}(…)")).monospace().strong());
            }
            label(ui, &format!("{} function(s)", names.len()));
            error_line(ui, &self.error);
            ok_cancel(ui, "OK")
        });
        if cancel || esc {
            return Flow::Close;
        }
        if ok && chosen.is_none() {
            match &self.selected {
                Some(n) => chosen = Some(n.clone()),
                None => self.error = Some("Pick a function first.".into()),
            }
        }
        if let Some(n) = chosen {
            InsertFunction::insert(app, &n);
            return Flow::Close;
        }
        Flow::Keep
    }
}

// ── Name Manager / Define Name ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NameRow {
    pub name: String,
    pub refers_to: String,
    pub scope: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NameManager {
    pub names: Vec<NameRow>,
    pub selected: Option<usize>,
    pub error: Option<String>,
}

fn list_names(app: &mut App, p: Json) -> Result<Vec<NameRow>, String> {
    app.query("formulas.name_manager", p)
        .and_then(|v| serde_json::from_value(v).map_err(|e| e.to_string()))
}

impl NameManager {
    pub fn new(app: &mut App) -> Self {
        NameManager {
            names: list_names(app, json!({})).unwrap_or_default(),
            ..Default::default()
        }
    }

    pub fn ui(&mut self, ctx: &Context, app: &mut App) -> Flow {
        let mut act = None;
        let (_, esc) = window(ctx, "Name Manager", 560.0, |ui| {
            ui.horizontal(|ui| {
                if ui.button("New…").clicked() {
                    act = Some("new");
                }
                if ui
                    .add_enabled(self.selected.is_some(), egui::Button::new("Edit…"))
                    .clicked()
                {
                    act = Some("edit");
                }
                if ui
                    .add_enabled(self.selected.is_some(), egui::Button::new("Delete"))
                    .clicked()
                {
                    act = Some("delete");
                }
            });
            ui.add_space(4.0);
            let row = |a: &str, b: &str, c: &str| format!("{a:<20} {b:<28} {c}");
            ui.label(
                RichText::new(row("Name", "Refers To", "Scope"))
                    .monospace()
                    .color(T.dialog_label),
            );
            let items: Vec<String> = self
                .names
                .iter()
                .map(|n| row(&n.name, &n.refers_to, &n.scope))
                .collect();
            ui.style_mut().override_font_id = Some(egui::FontId::monospace(12.0));
            if let Some((i, dbl)) = list_box(
                ui,
                "nm-list",
                Vec2::new(560.0, 200.0),
                &items,
                self.selected,
            ) {
                self.selected = Some(i);
                if dbl {
                    act = Some("edit");
                }
            }
            if self.names.is_empty() {
                label(ui, "No names are defined in this workbook.");
            }
            error_line(ui, &self.error);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Close").clicked() {
                    act = Some("close");
                }
            });
        });
        if esc {
            return Flow::Close;
        }
        match act {
            Some("close") => return Flow::Close,
            Some("new") => {
                app.ui.dialog = Some(Dialog::DefineName(DefineName::new(app, None)));
                return Flow::Close;
            }
            Some("edit") => {
                if let Some(n) = self.selected.and_then(|i| self.names.get(i)).cloned() {
                    app.ui.dialog = Some(Dialog::DefineName(DefineName::new(app, Some(n))));
                    return Flow::Close;
                }
            }
            Some("delete") => {
                if let Some(n) = self.selected.and_then(|i| self.names.get(i)).cloned() {
                    match app.try_exec("formulas.name_manager", json!({ "delete": n.name })) {
                        Ok(v) => {
                            self.names = serde_json::from_value(v).unwrap_or_default();
                            self.selected = None;
                        }
                        Err(e) => self.error = Some(e),
                    }
                }
            }
            _ => {}
        }
        Flow::Keep
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DefineName {
    pub name: String,
    /// "Workbook" or a sheet name.
    pub scope: String,
    pub refers_to: String,
    /// Name being edited (renaming deletes the old one).
    pub editing: Option<String>,
    pub back_to_manager: bool,
    pub error: Option<String>,
}

impl DefineName {
    pub fn new(app: &App, row: Option<NameRow>) -> Self {
        match row {
            Some(r) => DefineName {
                editing: Some(r.name.clone()),
                name: r.name,
                scope: r.scope,
                refers_to: r.refers_to,
                back_to_manager: true,
                error: None,
            },
            None => {
                let s = &app.session;
                let r = s.sel.range();
                let abs = |a: sp_core::CellAddr| {
                    format!("${}${}", sp_core::col_to_letters(a.col), a.row + 1)
                };
                let body = if r.is_single() {
                    abs(r.start)
                } else {
                    format!("{}:{}", abs(r.start), abs(r.end))
                };
                let sheet = sp_core::addr::quote_sheet_name(&sheet_name(s, s.sheet_index()));
                DefineName {
                    scope: "Workbook".into(),
                    refers_to: format!("={sheet}!{body}"),
                    ..Default::default()
                }
            }
        }
    }

    pub fn params(&self) -> Json {
        let scope = if self.scope.is_empty() || self.scope == "Workbook" {
            "workbook".to_string()
        } else {
            self.scope.clone()
        };
        json!({"name": self.name.trim(), "refers_to": self.refers_to.trim(), "scope": scope})
    }

    pub fn ui(&mut self, ctx: &Context, app: &mut App) -> Flow {
        let title = if self.editing.is_some() {
            "Edit Name"
        } else {
            "New Name"
        };
        let sheets: Vec<String> = app
            .session
            .wb
            .sheets
            .iter()
            .map(|s| s.name.clone())
            .collect();
        let ((ok, cancel), esc) = window(ctx, title, 380.0, |ui| {
            egui::Grid::new("dn-grid")
                .num_columns(2)
                .spacing([8.0, 6.0])
                .show(ui, |ui| {
                    label(ui, "Name:");
                    ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(260.0));
                    ui.end_row();
                    label(ui, "Scope:");
                    egui::ComboBox::new("dn-scope", "")
                        .selected_text(self.scope.clone())
                        .width(250.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.scope,
                                "Workbook".to_string(),
                                "Workbook",
                            );
                            for s in &sheets {
                                ui.selectable_value(&mut self.scope, s.clone(), s);
                            }
                        });
                    ui.end_row();
                    label(ui, "Refers to:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.refers_to)
                            .desired_width(260.0)
                            .font(egui::FontId::monospace(12.5)),
                    );
                    ui.end_row();
                });
            error_line(ui, &self.error);
            ok_cancel(ui, "OK")
        });
        let back = |app: &mut App| {
            app.ui.dialog = Some(Dialog::NameManager(NameManager::new(app)));
        };
        if cancel || esc {
            if self.back_to_manager {
                back(app);
            }
            return Flow::Close;
        }
        if ok {
            if let Err(e) = app.try_exec("formulas.define_name", self.params()) {
                self.error = Some(e);
                return Flow::Keep;
            }
            if let Some(old) = &self.editing
                && !old.eq_ignore_ascii_case(self.name.trim())
            {
                let _ = app.try_exec("formulas.name_manager", json!({ "delete": old }));
            }
            if self.back_to_manager {
                back(app);
            }
            return Flow::Close;
        }
        Flow::Keep
    }
}

// ── One-value prompts ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Prompt {
    pub title: String,
    pub label: String,
    pub value: String,
    /// Command run on OK, with `key` = value plus `extra` params.
    pub cmd: String,
    pub key: String,
    pub numeric: bool,
    pub extra: Json,
    pub error: Option<String>,
}

fn trim_num(x: f64) -> String {
    let s = format!("{x:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

impl Prompt {
    pub fn column_width(s: &Session) -> Self {
        let w =
            s.wb.sheet(s.sheet_index())
                .map_or(8.43, |sh| sh.col_width(s.sel.active.col));
        Prompt {
            title: "Column Width".into(),
            label: "Column width:".into(),
            value: trim_num(w),
            cmd: "home.cells.column_width".into(),
            key: "width".into(),
            numeric: true,
            ..Default::default()
        }
    }

    pub fn row_height(s: &Session) -> Self {
        let h =
            s.wb.sheet(s.sheet_index())
                .map_or(15.0, |sh| sh.row_height(s.sel.active.row));
        Prompt {
            title: "Row Height".into(),
            label: "Row height:".into(),
            value: trim_num(h),
            cmd: "home.cells.row_height".into(),
            key: "height".into(),
            numeric: true,
            ..Default::default()
        }
    }

    pub fn default_width(s: &Session) -> Self {
        let w =
            s.wb.sheet(s.sheet_index())
                .map_or(8.43, |sh| sh.default_col_width);
        Prompt {
            title: "Standard Width".into(),
            label: "Standard column width:".into(),
            value: trim_num(w),
            cmd: "home.cells.default_width".into(),
            key: "width".into(),
            numeric: true,
            ..Default::default()
        }
    }

    pub fn rename_sheet(s: &Session, sheet: Option<usize>) -> Self {
        let name = sheet_name(s, sheet.unwrap_or_else(|| s.sheet_index()));
        Prompt {
            title: "Rename Sheet".into(),
            label: "Sheet name:".into(),
            value: name.clone(),
            cmd: "home.cells.rename_sheet".into(),
            key: "name".into(),
            extra: json!({ "sheet": name }),
            ..Default::default()
        }
    }

    pub fn insert_sheet() -> Self {
        Prompt {
            title: "Insert Sheet".into(),
            label: "Name for the new sheet:".into(),
            cmd: "home.cells.insert_sheet".into(),
            key: "name".into(),
            ..Default::default()
        }
    }

    pub fn params(&self) -> Result<Json, String> {
        let mut p = match &self.extra {
            Json::Object(m) => m.clone(),
            _ => serde_json::Map::new(),
        };
        let v = self.value.trim();
        if self.numeric {
            let n: f64 = v.parse().map_err(|_| format!("'{v}' is not a number"))?;
            p.insert(self.key.clone(), json!(n));
        } else if !(v.is_empty() && self.cmd == "home.cells.insert_sheet") {
            p.insert(self.key.clone(), json!(v));
        }
        Ok(Json::Object(p))
    }

    pub fn ui(&mut self, ctx: &Context, app: &mut App) -> Flow {
        let mut enter = false;
        let ((ok, cancel), esc) = window(ctx, &self.title.clone(), 280.0, |ui| {
            label(ui, &self.label);
            let r = ui.add(egui::TextEdit::singleline(&mut self.value).desired_width(280.0));
            if !ui.ctx().memory(|m| m.focused().is_some()) {
                r.request_focus();
            }
            enter = r.lost_focus() && enter_pressed(ui.ctx());
            error_line(ui, &self.error);
            ok_cancel(ui, "OK")
        });
        if cancel || esc {
            return Flow::Close;
        }
        if ok || enter {
            return match self.params() {
                Ok(p) => done(app.try_exec(&self.cmd, p), &mut self.error),
                Err(e) => {
                    self.error = Some(e);
                    Flow::Keep
                }
            };
        }
        Flow::Keep
    }
}

// ── Zoom ─────────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Zoom {
    /// "200" | "100" | "75" | "50" | "25" | "fit" | "custom"
    pub choice: String,
    pub custom: u32,
    pub error: Option<String>,
}

const ZOOM_PRESETS: [&str; 5] = ["200", "100", "75", "50", "25"];

impl Zoom {
    pub fn new(s: &Session) -> Self {
        let pct = (s.view.zoom * 100.0).round().clamp(10.0, 400.0) as u32;
        let p = pct.to_string();
        Zoom {
            choice: if ZOOM_PRESETS.contains(&p.as_str()) {
                p
            } else {
                "custom".into()
            },
            custom: pct,
            error: None,
        }
    }

    pub fn command(&self) -> (&'static str, Json) {
        match self.choice.as_str() {
            "fit" => ("view.zoom_selection", json!({})),
            "custom" => ("view.zoom", json!({ "percent": self.custom })),
            other => (
                "view.zoom",
                json!({ "percent": other.parse::<u32>().unwrap_or(100) }),
            ),
        }
    }

    pub fn ui(&mut self, ctx: &Context, app: &mut App) -> Flow {
        let ((ok, cancel), esc) = window(ctx, "Zoom", 220.0, |ui| {
            ui.label(RichText::new("Magnification").strong());
            for p in ZOOM_PRESETS {
                ui.radio_value(&mut self.choice, p.to_string(), format!("{p}%"));
            }
            ui.radio_value(&mut self.choice, "fit".to_string(), "Fit selection");
            ui.horizontal(|ui| {
                ui.radio_value(&mut self.choice, "custom".to_string(), "Custom:");
                let r = ui.add(
                    egui::DragValue::new(&mut self.custom)
                        .range(10..=400)
                        .suffix("%"),
                );
                if r.changed() {
                    self.choice = "custom".into();
                }
            });
            error_line(ui, &self.error);
            ok_cancel(ui, "OK")
        });
        if cancel || esc {
            return Flow::Close;
        }
        if ok {
            let (id, p) = self.command();
            return done(app.try_exec(id, p), &mut self.error);
        }
        Flow::Keep
    }
}

// ── Unhide Sheet ─────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UnhideSheet {
    pub hidden: Vec<String>,
    pub selected: usize,
    pub error: Option<String>,
}

impl UnhideSheet {
    pub fn new(s: &Session) -> Self {
        UnhideSheet {
            hidden: s
                .wb
                .sheets
                .iter()
                .filter(|x| x.visibility != sp_model::Visibility::Visible)
                .map(|x| x.name.clone())
                .collect(),
            ..Default::default()
        }
    }

    pub fn ui(&mut self, ctx: &Context, app: &mut App) -> Flow {
        let ((ok, cancel), esc) = window(ctx, "Unhide", 280.0, |ui| {
            label(ui, "Unhide sheet:");
            if let Some((i, _)) = list_box(
                ui,
                "unhide",
                Vec2::new(280.0, 140.0),
                &self.hidden,
                Some(self.selected),
            ) {
                self.selected = i;
            }
            if self.hidden.is_empty() {
                label(ui, "No sheets are hidden.");
            }
            error_line(ui, &self.error);
            ok_cancel(ui, "OK")
        });
        if cancel || esc {
            return Flow::Close;
        }
        if ok {
            let Some(name) = self.hidden.get(self.selected).cloned() else {
                return Flow::Close;
            };
            return done(
                app.try_exec("home.cells.unhide_sheet", json!({ "sheet": name })),
                &mut self.error,
            );
        }
        Flow::Keep
    }
}

// ── Tab Color ────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TabColor {
    pub sheet: String,
    pub error: Option<String>,
}

impl TabColor {
    pub fn new(s: &Session, sheet: Option<usize>) -> Self {
        TabColor {
            sheet: sheet_name(s, sheet.unwrap_or_else(|| s.sheet_index())),
            error: None,
        }
    }

    pub fn ui(&mut self, ctx: &Context, app: &mut App) -> Flow {
        let mut pick: Option<Option<String>> = None;
        let mut cancel = false;
        let (_, esc) = window(ctx, "Tab Color", 240.0, |ui| {
            label(ui, &format!("Sheet: {}", self.sheet));
            if let Some(c) = swatch_grid(ui, None) {
                pick = Some(Some(hex(c)));
            }
            ui.horizontal(|ui| {
                if ui.button("No Color").clicked() {
                    pick = Some(None);
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
            error_line(ui, &self.error);
        });
        if cancel || esc {
            return Flow::Close;
        }
        if let Some(c) = pick {
            return done(
                app.try_exec(
                    "home.cells.tab_color",
                    json!({ "sheet": self.sheet, "color": c }),
                ),
                &mut self.error,
            );
        }
        Flow::Keep
    }
}

// ── Generic params form ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Params {
    pub cmd: String,
    pub label: String,
    pub doc: String,
    /// JSON object typed by the user.
    pub text: String,
    pub error: Option<String>,
}

impl Params {
    pub fn new(id: &str) -> Result<Self, String> {
        let c = registry()
            .get(id)
            .ok_or_else(|| format!("unknown command '{id}'"))?;
        let fields: Vec<String> = super::required_params(c.params)
            .iter()
            .map(|k| format!("\"{k}\": \"\""))
            .collect();
        Ok(Params {
            cmd: id.to_string(),
            label: c.label.clone(),
            doc: c.params.to_string(),
            text: format!("{{{}}}", fields.join(", ")),
            error: None,
        })
    }

    pub fn ui(&mut self, ctx: &Context, app: &mut App) -> Flow {
        let ((ok, cancel), esc) = window(ctx, &self.label.clone(), 440.0, |ui| {
            label(ui, "Parameters (JSON):");
            ui.label(
                RichText::new(&self.doc)
                    .monospace()
                    .size(11.5)
                    .color(T.group_label),
            );
            ui.add(
                egui::TextEdit::multiline(&mut self.text)
                    .desired_width(440.0)
                    .desired_rows(3)
                    .font(egui::FontId::monospace(12.5)),
            );
            error_line(ui, &self.error);
            ok_cancel(ui, "Run")
        });
        if cancel || esc {
            return Flow::Close;
        }
        if ok {
            return match serde_json::from_str::<Json>(&self.text) {
                Ok(p) if p.is_object() => done(app.try_exec(&self.cmd, p), &mut self.error),
                _ => {
                    self.error = Some("Parameters must be a JSON object.".into());
                    Flow::Keep
                }
            };
        }
        Flow::Keep
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_params() {
        let s = Session::new();
        let mut p = Prompt::column_width(&s);
        p.value = "20".into();
        assert_eq!(p.params(), Ok(json!({"width": 20.0})));
        p.value = "wide".into();
        assert!(p.params().is_err());
        let mut r = Prompt::rename_sheet(&s, None);
        r.value = "Data".into();
        assert_eq!(r.params(), Ok(json!({"sheet": "Sheet1", "name": "Data"})));
        assert_eq!(Prompt::insert_sheet().params(), Ok(json!({})));
    }

    #[test]
    fn zoom_and_paste_params() {
        let mut z = Zoom::new(&Session::new());
        assert_eq!(z.choice, "100");
        z.choice = "custom".into();
        z.custom = 130;
        assert_eq!(z.command(), ("view.zoom", json!({"percent": 130})));
        z.choice = "fit".into();
        assert_eq!(z.command().0, "view.zoom_selection");
        let mut ps = PasteSpecial {
            mode: "values".into(),
            ..Default::default()
        };
        assert_eq!(ps.params(), json!({"mode": "values"}));
        ps.transpose = true;
        assert_eq!(ps.params(), json!({"mode": "transpose"}));
    }

    #[test]
    fn generic_params_template() {
        let p = Params::new("view.zoom").unwrap();
        assert_eq!(p.text, r#"{"percent": ""}"#);
        assert!(Params::new("nope").is_err());
    }
}
