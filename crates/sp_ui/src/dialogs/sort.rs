//! Sort dialog (Data › Sort…, Home › Custom Sort…): sort levels, header row option.

use super::{Flow, error_line, label, ok_cancel, window};
use crate::App;
use crate::theme::LIGHT as T;
use egui::{Context, RichText};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sp_core::{CellAddr, CellRange, col_to_letters};
use sp_engine::Session;

/// Most sort levels the dialog offers.
const MAX_LEVELS: usize = 64;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SortLevel {
    /// Column letter, e.g. "B".
    pub column: String,
    pub descending: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Sort {
    /// Command the OK button runs: `data.sort` or `home.editing.sort_custom`.
    pub cmd: String,
    /// Range being sorted (A1), header row included.
    pub range: String,
    pub header: bool,
    pub levels: Vec<SortLevel>,
    pub selected: usize,
    pub error: Option<String>,
}

fn cell_text(s: &Session, a: CellAddr) -> String {
    s.wb.sheet(s.sheet_index())
        .and_then(|sh| sh.get(a))
        .map(|c| c.value.to_display())
        .unwrap_or_default()
}

/// A first row of text above a second row holding something else reads as a header.
fn guess_header(s: &Session, r: CellRange) -> bool {
    if r.rows() < 2 {
        return false;
    }
    let Some(sh) = s.wb.sheet(s.sheet_index()) else {
        return false;
    };
    let is_text = |a: CellAddr| {
        sh.get(a)
            .is_some_and(|c| matches!(c.value, sp_core::Value::Text(_)))
    };
    let cols = r.start.col..=r.end.col.min(r.start.col.saturating_add(256));
    let first_all_text = cols.clone().all(|c| is_text(CellAddr::new(r.start.row, c)));
    let second_has_other = cols.clone().any(|c| {
        let a = CellAddr::new(r.start.row + 1, c);
        !is_text(a) && sh.get(a).is_some_and(|x| !x.value.is_empty())
    });
    first_all_text && second_has_other
}

impl Sort {
    pub fn new(app: &mut App, cmd: &str) -> Self {
        // Like the target app, a single-cell selection grows to the surrounding data block.
        if app.session.sel.range().is_single() {
            let _ = app.try_exec("select.current_region", json!({}));
        }
        let s = &app.session;
        let r = s.sel.range();
        let col = s.sel.active.col.clamp(r.start.col, r.end.col);
        Sort {
            cmd: cmd.to_string(),
            range: r.to_a1(),
            header: guess_header(s, r),
            levels: vec![SortLevel {
                column: col_to_letters(col),
                descending: false,
            }],
            selected: 0,
            error: None,
        }
    }

    fn columns(&self, s: &Session) -> Vec<(String, String)> {
        let Ok(r) = CellRange::parse(&self.range) else {
            return Vec::new();
        };
        (r.start.col..=r.end.col.min(r.start.col.saturating_add(255)))
            .map(|c| {
                let letter = col_to_letters(c);
                let head = cell_text(s, CellAddr::new(r.start.row, c));
                let label = if self.header && !head.is_empty() {
                    head
                } else {
                    format!("Column {letter}")
                };
                (letter, label)
            })
            .collect()
    }

    /// The command and params OK runs.
    pub fn command(&self) -> (String, serde_json::Value) {
        let keys: Vec<_> = self
            .levels
            .iter()
            .map(
                |l| json!({"column": l.column, "order": if l.descending { "desc" } else { "asc" }}),
            )
            .collect();
        if self.cmd == "home.editing.sort_custom" {
            // This command has no header flag: sort the rows below the header instead.
            let range = match CellRange::parse(&self.range) {
                Ok(r) if self.header && r.rows() > 1 => {
                    CellRange::new(CellAddr::new(r.start.row + 1, r.start.col), r.end).to_a1()
                }
                _ => self.range.clone(),
            };
            (self.cmd.clone(), json!({"range": range, "keys": keys}))
        } else {
            (
                "data.sort".into(),
                json!({"range": self.range, "keys": keys, "header": self.header}),
            )
        }
    }

    pub fn ui(&mut self, ctx: &Context, app: &mut App) -> Flow {
        let cols = self.columns(&app.session);
        let ((ok, cancel), esc) = window(ctx, "Sort", 520.0, |ui| {
            ui.horizontal(|ui| {
                let can_add = self.levels.len() < MAX_LEVELS.min(cols.len().max(1));
                if ui
                    .add_enabled(can_add, egui::Button::new("+ Add Level"))
                    .clicked()
                {
                    let used: Vec<&str> = self.levels.iter().map(|l| l.column.as_str()).collect();
                    let next = cols
                        .iter()
                        .find(|(l, _)| !used.contains(&l.as_str()))
                        .or(cols.first())
                        .map(|(l, _)| l.clone())
                        .unwrap_or_default();
                    self.levels.push(SortLevel {
                        column: next,
                        descending: false,
                    });
                    self.selected = self.levels.len() - 1;
                }
                if ui
                    .add_enabled(self.levels.len() > 1, egui::Button::new("Delete Level"))
                    .clicked()
                    && self.selected < self.levels.len()
                {
                    self.levels.remove(self.selected);
                    self.selected = self.selected.min(self.levels.len().saturating_sub(1));
                }
                if ui
                    .add_enabled(self.selected > 0, egui::Button::new("Move Up"))
                    .clicked()
                {
                    self.levels.swap(self.selected, self.selected - 1);
                    self.selected -= 1;
                }
                if ui
                    .add_enabled(
                        self.selected + 1 < self.levels.len(),
                        egui::Button::new("Move Down"),
                    )
                    .clicked()
                {
                    self.levels.swap(self.selected, self.selected + 1);
                    self.selected += 1;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.checkbox(&mut self.header, "My data has headers");
                });
            });
            ui.add_space(4.0);
            label(ui, &format!("Range: {}", self.range));
            egui::Frame::group(ui.style())
                .fill(T.list_bg)
                .show(ui, |ui| {
                    ui.set_width(500.0);
                    egui::Grid::new("sort-levels")
                        .num_columns(3)
                        .spacing([10.0, 6.0])
                        .show(ui, |ui| {
                            label(ui, "");
                            label(ui, "Column");
                            label(ui, "Order");
                            ui.end_row();
                            for (i, lv) in self.levels.iter_mut().enumerate() {
                                let name = if i == 0 { "Sort by" } else { "Then by" };
                                if ui
                                    .add(egui::Button::selectable(self.selected == i, name))
                                    .clicked()
                                {
                                    self.selected = i;
                                }
                                let cur = cols.iter().find(|(l, _)| *l == lv.column).map_or_else(
                                    || format!("Column {}", lv.column),
                                    |(_, n)| n.clone(),
                                );
                                egui::ComboBox::new(("sort-col", i), "")
                                    .selected_text(RichText::new(cur))
                                    .width(200.0)
                                    .show_ui(ui, |ui| {
                                        for (l, n) in &cols {
                                            if ui.selectable_label(lv.column == *l, n).clicked() {
                                                lv.column.clone_from(l);
                                            }
                                        }
                                    });
                                egui::ComboBox::new(("sort-ord", i), "")
                                    .selected_text(if lv.descending { "Z to A" } else { "A to Z" })
                                    .width(110.0)
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(&mut lv.descending, false, "A to Z");
                                        ui.selectable_value(&mut lv.descending, true, "Z to A");
                                    });
                                ui.end_row();
                            }
                        });
                });
            error_line(ui, &self.error);
            ok_cancel(ui, "OK")
        });
        if cancel || esc {
            return Flow::Close;
        }
        if ok {
            let (id, p) = self.command();
            return match app.try_exec(&id, p) {
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
