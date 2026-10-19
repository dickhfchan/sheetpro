//! PivotTable Fields pane: a side panel shown while the active cell is inside a pivot.
//! It lists the source fields with check boxes and the Filters / Columns / Rows / Values
//! areas; every edit runs the `pivot.field` command (so it is undoable and scriptable).

use crate::App;
use crate::theme::LIGHT as T;
use egui::{RichText, Ui};
use serde_json::{Value as Json, json};
use sp_engine::commands::pivot_cmds::{field_is_numeric, pivot_at};

/// Width of the pane, px.
pub const PANE_WIDTH: f32 = 250.0;

/// Summary functions offered for a value field.
const FUNCS: [(&str, &str); 6] = [
    ("sum", "Sum"),
    ("count", "Count"),
    ("average", "Average"),
    ("max", "Max"),
    ("min", "Min"),
    ("product", "Product"),
];

/// Areas as the command names them, with their headings, in pane order.
const AREAS: [(&str, &str); 4] = [
    ("filters", "Filters"),
    ("cols", "Columns"),
    ("rows", "Rows"),
    ("values", "Values"),
];

struct FieldRow {
    name: String,
    used: bool,
    filtered: bool,
    numeric: bool,
}

/// What the pane shows, copied out of the model so the UI can run commands freely.
struct PaneData {
    pivot: String,
    fields: Vec<FieldRow>,
    /// Field names per axis area (filters, cols, rows).
    axes: [Vec<String>; 3],
    /// (caption, summary function name) per value field.
    values: Vec<(String, &'static str)>,
}

impl App {
    /// `(sheet, pivot index)` of the pivot under the active cell.
    pub fn active_pivot(&self) -> Option<(usize, usize)> {
        let s = &self.session;
        let si = s.sheet_index();
        pivot_at(&s.wb, si, s.sel.active).map(|i| (si, i))
    }

    /// Whether the Fields pane is showing this frame.
    pub fn field_list_visible(&self) -> bool {
        !self.ui.grid.hide_field_list && self.active_pivot().is_some()
    }

    fn pane_data(&self) -> Option<PaneData> {
        let (si, pi) = self.active_pivot()?;
        let pv = self.session.wb.sheet(si)?.analysis.pivots.get(pi)?;
        let fields = pv
            .cache
            .fields
            .iter()
            .enumerate()
            .map(|(i, n)| FieldRow {
                name: n.clone(),
                used: pv.rows.contains(&i)
                    || pv.cols.contains(&i)
                    || pv.filters.contains(&i)
                    || pv.values.iter().any(|d| d.field == i),
                filtered: !pv.hidden_items(i).is_empty(),
                numeric: field_is_numeric(pv, i),
            })
            .collect();
        let names = |v: &[usize]| -> Vec<String> {
            v.iter().map(|f| pv.field_name(*f).to_string()).collect()
        };
        Some(PaneData {
            pivot: pv.name.clone(),
            fields,
            axes: [names(&pv.filters), names(&pv.cols), names(&pv.rows)],
            values: pv
                .values
                .iter()
                .enumerate()
                .map(|(i, d)| (pv.data_caption(i), d.func.name()))
                .collect(),
        })
    }

    /// The pane's contents (inside a right side panel).
    pub(crate) fn pivot_pane(&mut self, ui: &mut Ui) {
        let Some(d) = self.pane_data() else {
            return;
        };
        let mut edits: Vec<Json> = Vec::new();
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("PivotTable Fields")
                    .strong()
                    .size(13.0)
                    .color(T.dialog_title),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button("Close")
                    .on_hover_text("Close the field list")
                    .clicked()
                {
                    self.ui.grid.hide_field_list = true;
                }
            });
        });
        ui.label(
            RichText::new("Choose fields to add to report:")
                .size(11.0)
                .color(T.dialog_label),
        );
        egui::Frame::NONE
            .fill(T.list_bg)
            .stroke(egui::Stroke::new(1.0, T.list_border))
            .inner_margin(egui::Margin::same(4))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("pivot-fields-list")
                    .max_height(180.0)
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        for f in &d.fields {
                            let mut on = f.used;
                            let label = if f.filtered {
                                format!("{} (filtered)", f.name)
                            } else {
                                f.name.clone()
                            };
                            let r = ui.checkbox(&mut on, label);
                            let r = if f.numeric {
                                r.on_hover_text("Numbers: checking adds it to Values")
                            } else {
                                r
                            };
                            if r.changed() {
                                edits.push(json!({ "field": f.name, "check": on }));
                            }
                        }
                    });
            });
        ui.add_space(6.0);
        ui.label(
            RichText::new("Click a field below to move or remove it:")
                .size(11.0)
                .color(T.dialog_label),
        );
        egui::Grid::new("pivot-areas")
            .num_columns(2)
            .spacing([6.0, 6.0])
            .show(ui, |ui| {
                for (row, pair) in AREAS.chunks(2).enumerate() {
                    for (area, title) in pair {
                        ui.push_id((row, *area), |ui| {
                            area_box(ui, &d, area, title, &mut edits);
                        });
                    }
                    ui.end_row();
                }
            });
        for mut e in edits {
            if let Some(o) = e.as_object_mut() {
                o.insert("pivot".into(), json!(d.pivot));
            }
            self.exec("pivot.field", e);
        }
    }
}

/// One area list ("Rows", "Values"…) with a menu per entry.
fn area_box(ui: &mut Ui, d: &PaneData, area: &str, title: &str, edits: &mut Vec<Json>) {
    let w = (PANE_WIDTH - 30.0) / 2.0;
    ui.vertical(|ui| {
        ui.set_width(w);
        ui.label(RichText::new(title).size(11.0).color(T.dialog_label));
        egui::Frame::NONE
            .fill(T.list_bg)
            .stroke(egui::Stroke::new(1.0, T.list_border))
            .inner_margin(egui::Margin::same(3))
            .show(ui, |ui| {
                ui.set_min_size(egui::vec2(w - 8.0, 70.0));
                if area == "values" {
                    let n = d.values.len();
                    for (i, (caption, func)) in d.values.iter().enumerate() {
                        ui.menu_button(RichText::new(caption).size(11.0), |ui| {
                            value_menu(ui, i, n, func, edits);
                        });
                    }
                    return;
                }
                let names: &[String] = match area {
                    "filters" => d.axes.first(),
                    "cols" => d.axes.get(1),
                    _ => d.axes.get(2),
                }
                .map_or(&[], Vec::as_slice);
                let n = names.len();
                for (i, name) in names.iter().enumerate() {
                    ui.menu_button(RichText::new(name).size(11.0), |ui| {
                        field_menu(ui, name, area, i, n, edits);
                    });
                }
            });
    });
}

fn field_menu(ui: &mut Ui, name: &str, area: &str, i: usize, n: usize, edits: &mut Vec<Json>) {
    let mut push = |ui: &mut Ui, e: Json| {
        edits.push(e);
        ui.close();
    };
    if ui
        .add_enabled(i > 0, egui::Button::new("Move Up"))
        .clicked()
    {
        push(
            ui,
            json!({ "field": name, "area": area, "position": i - 1 }),
        );
    }
    if ui
        .add_enabled(i + 1 < n, egui::Button::new("Move Down"))
        .clicked()
    {
        push(
            ui,
            json!({ "field": name, "area": area, "position": i + 1 }),
        );
    }
    ui.separator();
    for (a, title) in AREAS {
        if a != area && ui.button(format!("Move to {title}")).clicked() {
            push(ui, json!({ "field": name, "area": a }));
        }
    }
    ui.separator();
    if ui.button("Remove Field").clicked() {
        push(ui, json!({ "field": name, "area": "none" }));
    }
}

fn value_menu(ui: &mut Ui, i: usize, n: usize, func: &str, edits: &mut Vec<Json>) {
    let mut push = |ui: &mut Ui, e: Json| {
        edits.push(e);
        ui.close();
    };
    if ui
        .add_enabled(i > 0, egui::Button::new("Move Up"))
        .clicked()
    {
        push(
            ui,
            json!({ "value": i, "area": "values", "position": i - 1 }),
        );
    }
    if ui
        .add_enabled(i + 1 < n, egui::Button::new("Move Down"))
        .clicked()
    {
        push(
            ui,
            json!({ "value": i, "area": "values", "position": i + 1 }),
        );
    }
    ui.separator();
    ui.label(RichText::new("Summarize Values By").size(11.0));
    for (f, title) in FUNCS {
        if ui.radio(func == f, title).clicked() {
            push(ui, json!({ "value": i, "func": f }));
        }
    }
    ui.separator();
    for (a, title) in AREAS {
        if a != "values" && ui.button(format!("Move to {title}")).clicked() {
            push(ui, json!({ "value": i, "area": a }));
        }
    }
    if ui.button("Remove Field").clicked() {
        push(ui, json!({ "value": i, "remove": true }));
    }
}
