//! Find & Replace (Ctrl+F / Ctrl+H) and Go To (Ctrl+G).

use super::widgets::list_box;
use super::{Flow, enter_pressed, error_line, label, ok_cancel, window};
use crate::App;
use crate::theme::LIGHT as T;
use egui::{Context, RichText, Vec2};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sp_engine::Json;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Hit {
    pub sheet: String,
    pub cell: String,
    pub value: String,
    pub formula: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FindReplace {
    pub replace_mode: bool,
    pub what: String,
    pub with: String,
    pub match_case: bool,
    pub whole: bool,
    /// Look in: values instead of formulas.
    pub in_values: bool,
    /// Within: the whole workbook instead of the active sheet.
    pub in_workbook: bool,
    /// Search: by columns instead of by rows.
    pub by_columns: bool,
    pub show_options: bool,
    pub hits: Vec<Hit>,
    pub selected: Option<usize>,
    pub status: Option<String>,
    pub error: Option<String>,
    /// Run Find All right after opening (screenshot scripts).
    pub find_all_on_open: bool,
}

impl FindReplace {
    pub fn new(replace_mode: bool) -> Self {
        FindReplace {
            replace_mode,
            ..Default::default()
        }
    }

    /// Options shared by Find and Replace.
    fn search_params(&self) -> Json {
        json!({
            "what": self.what,
            "match_case": self.match_case,
            "whole": self.whole,
            "within": if self.in_workbook { "workbook" } else { "sheet" },
            "search": if self.by_columns { "columns" } else { "rows" },
        })
    }

    fn find_params(&self) -> Json {
        let mut p = self.search_params();
        if let Some(m) = p.as_object_mut() {
            m.insert(
                "look_in".into(),
                json!(if self.in_values { "values" } else { "formulas" }),
            );
        }
        p
    }

    fn replace_params(&self, all: bool) -> Json {
        let mut p = self.search_params();
        if let Some(m) = p.as_object_mut() {
            m.insert("with".into(), json!(self.with));
            if all {
                m.insert("all".into(), json!(true));
            }
        }
        p
    }

    pub fn find_next(&mut self, app: &mut App) {
        self.error = None;
        match app.try_exec("home.editing.find", self.find_params()) {
            Ok(r) => {
                self.status = r
                    .get("cell")
                    .and_then(Json::as_str)
                    .map(|c| format!("Found at {c}"));
            }
            Err(e) => self.error = Some(e),
        }
    }

    pub fn find_all(&mut self, app: &mut App) {
        self.error = None;
        match app.query("edit.find_all", self.find_params()) {
            Ok(r) => {
                self.hits = serde_json::from_value(r).unwrap_or_default();
                self.selected = None;
                self.status = Some(format!("{} cell(s) found", self.hits.len()));
            }
            Err(e) => {
                self.hits.clear();
                self.error = Some(e);
            }
        }
    }

    fn go_to_hit(&mut self, app: &mut App, i: usize) {
        if let Some(h) = self.hits.get(i) {
            let reference = format!("{}!{}", sp_core::addr::quote_sheet_name(&h.sheet), h.cell);
            if let Err(e) = app.try_exec("home.editing.goto", json!({ "reference": reference })) {
                self.error = Some(e);
            }
            self.selected = Some(i);
        }
    }

    pub fn ui(&mut self, ctx: &Context, app: &mut App) -> Flow {
        let mut close = false;
        let mut action: Option<&str> = None;
        let mut clicked_hit = None;
        let (_, esc) = window(ctx, "Find and Replace", 560.0, |ui| {
            ui.horizontal(|ui| {
                for (on, name) in [(false, "Find"), (true, "Replace")] {
                    let sel = self.replace_mode == on;
                    if ui
                        .add(egui::Button::selectable(
                            sel,
                            RichText::new(name).color(if sel { T.accent } else { T.ribbon_text }),
                        ))
                        .clicked()
                    {
                        self.replace_mode = on;
                    }
                }
            });
            ui.separator();
            egui::Grid::new("find-fields")
                .num_columns(2)
                .spacing([8.0, 6.0])
                .show(ui, |ui| {
                    label(ui, "Find what:");
                    let r = ui.add(egui::TextEdit::singleline(&mut self.what).desired_width(400.0));
                    if self.what.is_empty()
                        && !r.has_focus()
                        && !ui.ctx().memory(|m| m.focused().is_some())
                    {
                        r.request_focus();
                    }
                    if r.lost_focus() && enter_pressed(ui.ctx()) {
                        action = Some("next");
                    }
                    ui.end_row();
                    if self.replace_mode {
                        label(ui, "Replace with:");
                        ui.add(egui::TextEdit::singleline(&mut self.with).desired_width(400.0));
                        ui.end_row();
                    }
                });
            if self.show_options {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.checkbox(&mut self.match_case, "Match case");
                        ui.checkbox(&mut self.whole, "Match entire cell contents");
                    });
                    ui.add_space(24.0);
                    egui::Grid::new("find-options")
                        .num_columns(2)
                        .spacing([8.0, 4.0])
                        .show(ui, |ui| {
                            label(ui, "Within:");
                            egui::ComboBox::new("find-within", "")
                                .selected_text(if self.in_workbook {
                                    "Workbook"
                                } else {
                                    "Sheet"
                                })
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut self.in_workbook, false, "Sheet");
                                    ui.selectable_value(&mut self.in_workbook, true, "Workbook");
                                });
                            ui.end_row();
                            label(ui, "Search:");
                            egui::ComboBox::new("find-search", "")
                                .selected_text(if self.by_columns {
                                    "By Columns"
                                } else {
                                    "By Rows"
                                })
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut self.by_columns, false, "By Rows");
                                    ui.selectable_value(&mut self.by_columns, true, "By Columns");
                                });
                            ui.end_row();
                            label(ui, "Look in:");
                            egui::ComboBox::new("find-lookin", "")
                                .selected_text(if self.in_values { "Values" } else { "Formulas" })
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut self.in_values, false, "Formulas");
                                    ui.selectable_value(&mut self.in_values, true, "Values");
                                });
                            ui.end_row();
                        });
                });
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let opts = if self.show_options {
                    "Options <<"
                } else {
                    "Options >>"
                };
                if ui.button(opts).clicked() {
                    self.show_options = !self.show_options;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                    if ui.button("Find Next").clicked() {
                        action = Some("next");
                    }
                    if ui.button("Find All").clicked() {
                        action = Some("all");
                    }
                    if self.replace_mode {
                        if ui.button("Replace").clicked() {
                            action = Some("replace");
                        }
                        if ui.button("Replace All").clicked() {
                            action = Some("replace_all");
                        }
                    }
                });
            });
            error_line(ui, &self.error);
            if !self.hits.is_empty() {
                ui.add_space(4.0);
                let rows: Vec<String> = self
                    .hits
                    .iter()
                    .map(|h| {
                        format!(
                            "{:<14} {:<8} {:<20} {}",
                            truncate(&h.sheet, 14),
                            h.cell,
                            truncate(&h.value, 20),
                            h.formula.as_deref().unwrap_or("")
                        )
                    })
                    .collect();
                ui.label(
                    RichText::new(format!(
                        "{:<14} {:<8} {:<20} {}",
                        "Sheet", "Cell", "Value", "Formula"
                    ))
                    .monospace()
                    .color(T.dialog_label),
                );
                ui.style_mut().override_font_id = Some(egui::FontId::monospace(12.0));
                if let Some((i, _)) = list_box(
                    ui,
                    "find-hits",
                    Vec2::new(560.0, 130.0),
                    &rows,
                    self.selected,
                ) {
                    clicked_hit = Some(i);
                }
            }
            if let Some(s) = &self.status {
                ui.label(RichText::new(s).size(11.5).color(T.status_text));
            }
        });
        if let Some(i) = clicked_hit {
            self.go_to_hit(app, i);
        }
        match action {
            Some("next") => self.find_next(app),
            Some("all") => self.find_all(app),
            Some("replace") => {
                self.error = None;
                match app.try_exec("edit.replace_current", self.replace_params(false)) {
                    Ok(r) => {
                        self.status = Some(if r.get("replaced") == Some(&json!(true)) {
                            "Replaced 1 cell".into()
                        } else {
                            "Active cell did not match; moved to the next match".into()
                        });
                    }
                    Err(e) => self.error = Some(e),
                }
            }
            Some("replace_all") => {
                self.error = None;
                match app.try_exec("home.editing.replace", self.replace_params(true)) {
                    Ok(n) => {
                        self.status = Some(format!(
                            "All done. Made {} replacement(s).",
                            n.as_u64().unwrap_or(0)
                        ));
                        self.hits.clear();
                    }
                    Err(e) => self.error = Some(e),
                }
            }
            _ => {}
        }
        if close || esc {
            Flow::Close
        } else {
            Flow::Keep
        }
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(n.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GoTo {
    pub reference: String,
    /// (name, refers to) of defined names.
    pub names: Vec<(String, String)>,
    pub selected: Option<usize>,
    pub error: Option<String>,
}

impl GoTo {
    pub fn new(app: &mut App) -> Self {
        let names = app
            .query("formulas.name_manager", json!({}))
            .ok()
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
            .iter()
            .map(|n| {
                (
                    n.get("name")
                        .and_then(Json::as_str)
                        .unwrap_or("")
                        .to_string(),
                    n.get("refers_to")
                        .and_then(Json::as_str)
                        .unwrap_or("")
                        .to_string(),
                )
            })
            .collect();
        GoTo {
            names,
            ..Default::default()
        }
    }

    pub fn ui(&mut self, ctx: &Context, app: &mut App) -> Flow {
        let mut go = false;
        let ((ok, cancel), esc) = window(ctx, "Go To", 340.0, |ui| {
            label(ui, "Go to:");
            let items: Vec<String> = self
                .names
                .iter()
                .map(|(n, r)| format!("{n}   {r}"))
                .collect();
            if let Some((i, dbl)) = list_box(
                ui,
                "goto-names",
                Vec2::new(340.0, 150.0),
                &items,
                self.selected,
            ) {
                self.selected = Some(i);
                if let Some((n, _)) = self.names.get(i) {
                    self.reference.clone_from(n);
                }
                go = dbl;
            }
            ui.add_space(4.0);
            label(ui, "Reference:");
            let r = ui.add(egui::TextEdit::singleline(&mut self.reference).desired_width(340.0));
            if !ui.ctx().memory(|m| m.focused().is_some()) && self.reference.is_empty() {
                r.request_focus();
            }
            if r.lost_focus() && enter_pressed(ui.ctx()) {
                go = true;
            }
            error_line(ui, &self.error);
            ok_cancel(ui, "OK")
        });
        if cancel || esc {
            return Flow::Close;
        }
        if ok || go {
            return match app.try_exec(
                "home.editing.goto",
                json!({ "reference": self.reference.trim() }),
            ) {
                Ok(_) => Flow::Close,
                Err(e) => {
                    self.error = Some(e);
                    Flow::Keep
                }
            };
        }
        Flow::Keep
    }
}
