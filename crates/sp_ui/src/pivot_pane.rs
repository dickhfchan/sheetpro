//! PivotTable Fields pane: a side panel shown while the active cell is inside a pivot.
//! It lists the source fields with check boxes and the Filters / Columns / Rows / Values
//! areas; every edit runs the `pivot.field` command (so it is undoable and scriptable).
//! Fields can be dragged from the list into an area, between areas, within an area (to
//! reorder) and back onto the list (to remove); each entry also has a menu.

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

/// What is being dragged in the pane.
#[derive(Debug, Clone, PartialEq)]
enum PaneDrag {
    /// A source field, from the field list (`from: None`) or an axis area at an index.
    Field {
        name: String,
        from: Option<(&'static str, usize)>,
    },
    /// Entry `i` of the Values area.
    Value(usize),
}

/// The `pivot.field` edit for dropping `drag` on `area` ("none" = the field list) at entry
/// `position` of that area (counted in the area as shown, before the move); `None` when the
/// drop changes nothing.
fn drop_edit(drag: &PaneDrag, area: &str, position: usize) -> Option<Json> {
    // Moving down inside the same list: the entry leaves its old slot first.
    let adjust = |from: usize| {
        if from < position {
            position - 1
        } else {
            position
        }
    };
    match drag {
        PaneDrag::Field { name, from } => match (area, from) {
            ("none", None) => None,
            ("none", Some(_)) => Some(json!({ "field": name, "area": "none" })),
            (a, Some((fa, fi))) if *fa == a => {
                let at = adjust(*fi);
                (at != *fi).then(|| json!({ "field": name, "area": a, "position": at }))
            }
            // From an axis to Values is a move; from the field list it adds.
            ("values", Some(_)) => {
                Some(json!({ "field": name, "area": "values", "position": position, "move": true }))
            }
            (a, _) => Some(json!({ "field": name, "area": a, "position": position })),
        },
        PaneDrag::Value(i) => match area {
            "none" => Some(json!({ "value": i, "remove": true })),
            "values" => {
                let at = adjust(*i);
                (at != *i).then(|| json!({ "value": i, "area": "values", "position": at }))
            }
            a => Some(json!({ "value": i, "area": a, "position": position })),
        },
    }
}

/// Index at which a drop at pointer `y` lands among entries laid out at `rows` (their
/// screen rects, top to bottom).
fn drop_index(rows: &[egui::Rect], y: f32) -> usize {
    rows.iter().filter(|r| r.center().y < y).count()
}

/// Pointer travel (px) after which a press on an entry becomes a drag.
const DRAG_THRESHOLD: f32 = 6.0;

/// Let the entry `resp` (a checkbox or menu button) also be dragged, carrying `payload`.
/// The widget keeps sensing only clicks (so a click still toggles / opens its menu); a
/// press on it that travels past `DRAG_THRESHOLD` starts the drag.
fn drag_source(ui: &Ui, resp: egui::Response, payload: PaneDrag, label: &str) -> egui::Response {
    let ctx = ui.ctx();
    // Pressed on this entry (and still held) and moved far enough.
    let dragging = ctx.input(|i| {
        i.pointer.primary_down()
            && i.pointer
                .press_origin()
                .zip(i.pointer.interact_pos())
                .is_some_and(|(a, b)| {
                    resp.interact_rect.contains(a) && a.distance(b) > DRAG_THRESHOLD
                })
    });
    if dragging && ui.is_enabled() {
        egui::DragAndDrop::set_payload(ctx, payload);
        if let Some(p) = ctx.pointer_interact_pos() {
            let layer = egui::LayerId::new(egui::Order::Tooltip, resp.id.with("drag-ghost"));
            let painter = ctx.layer_painter(layer);
            let g = painter.layout_no_wrap(
                label.to_string(),
                egui::FontId::proportional(11.0),
                T.dialog_title,
            );
            let r = egui::Rect::from_min_size(p + egui::vec2(10.0, 4.0), g.size())
                .expand2(egui::vec2(5.0, 2.0));
            painter.rect_filled(r, 2.0, T.list_bg);
            painter.rect_stroke(
                r,
                2.0,
                egui::Stroke::new(1.0, T.list_border),
                egui::StrokeKind::Inside,
            );
            painter.galley(r.min + egui::vec2(5.0, 2.0), g, T.dialog_title);
        }
        ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
    }
    resp
}

/// Outline an area (or the list) while something droppable hovers it.
fn drop_highlight(ui: &Ui, resp: &egui::Response) {
    if resp.dnd_hover_payload::<PaneDrag>().is_some() {
        ui.painter().rect_stroke(
            resp.rect,
            2.0,
            egui::Stroke::new(2.0, T.selection_border),
            egui::StrokeKind::Inside,
        );
    }
}

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
        let list = egui::Frame::NONE
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
                            let payload = PaneDrag::Field {
                                name: f.name.clone(),
                                from: None,
                            };
                            let r = ui.checkbox(&mut on, label);
                            let r = drag_source(ui, r, payload, &f.name);
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
            })
            .response;
        drop_highlight(ui, &list);
        if let Some(drag) = list.dnd_release_payload::<PaneDrag>()
            && let Some(e) = drop_edit(&drag, "none", 0)
        {
            edits.push(e);
        }
        ui.add_space(6.0);
        ui.label(
            RichText::new("Drag fields between areas below:")
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

/// One area list ("Rows", "Values"…): each entry has a menu and can be dragged; the box
/// takes drops.
fn area_box(ui: &mut Ui, d: &PaneData, area: &'static str, title: &str, edits: &mut Vec<Json>) {
    let w = (PANE_WIDTH - 30.0) / 2.0;
    ui.vertical(|ui| {
        ui.set_width(w);
        ui.label(RichText::new(title).size(11.0).color(T.dialog_label));
        let mut rows: Vec<egui::Rect> = Vec::new();
        let frame = egui::Frame::NONE
            .fill(T.list_bg)
            .stroke(egui::Stroke::new(1.0, T.list_border))
            .inner_margin(egui::Margin::same(3))
            .show(ui, |ui| {
                ui.set_min_size(egui::vec2(w - 8.0, 70.0));
                if area == "values" {
                    let n = d.values.len();
                    for (i, (caption, func)) in d.values.iter().enumerate() {
                        let r = ui.menu_button(RichText::new(caption).size(11.0), |ui| {
                            value_menu(ui, i, n, func, edits);
                        });
                        let r = drag_source(ui, r.response, PaneDrag::Value(i), caption);
                        rows.push(r.rect);
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
                    let payload = PaneDrag::Field {
                        name: name.clone(),
                        from: Some((area, i)),
                    };
                    let r = ui.menu_button(RichText::new(name).size(11.0), |ui| {
                        field_menu(ui, name, area, i, n, edits);
                    });
                    let r = drag_source(ui, r.response, payload, name);
                    rows.push(r.rect);
                }
            })
            .response;
        drop_highlight(ui, &frame);
        if let Some(drag) = frame.dnd_release_payload::<PaneDrag>() {
            let y = ui
                .ctx()
                .pointer_interact_pos()
                .map_or(f32::INFINITY, |p| p.y);
            if let Some(e) = drop_edit(&drag, area, drop_index(&rows, y)) {
                edits.push(e);
            }
        }
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
            push(ui, json!({ "field": name, "area": a, "move": true }));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn field(name: &str, from: Option<(&'static str, usize)>) -> PaneDrag {
        PaneDrag::Field {
            name: name.into(),
            from,
        }
    }

    #[test]
    fn drops_become_pivot_field_edits() {
        // List → area, at the drop position.
        assert_eq!(
            drop_edit(&field("Product", None), "cols", 0),
            Some(json!({"field": "Product", "area": "cols", "position": 0}))
        );
        // List → list: nothing.
        assert_eq!(drop_edit(&field("Product", None), "none", 0), None);
        // Area → list removes it.
        assert_eq!(
            drop_edit(&field("Region", Some(("rows", 0))), "none", 0),
            Some(json!({"field": "Region", "area": "none"}))
        );
        // Reorder in place: moving entry 0 below entry 1 lands at 1; dropping it where it
        // is changes nothing.
        assert_eq!(
            drop_edit(&field("Region", Some(("rows", 0))), "rows", 2),
            Some(json!({"field": "Region", "area": "rows", "position": 1}))
        );
        assert_eq!(
            drop_edit(&field("Region", Some(("rows", 0))), "rows", 1),
            None
        );
        assert_eq!(
            drop_edit(&field("Year", Some(("rows", 2))), "rows", 0),
            Some(json!({"field": "Year", "area": "rows", "position": 0}))
        );
        // To Values: from an axis it moves, from the list it adds.
        assert_eq!(
            drop_edit(&field("Region", Some(("rows", 0))), "values", 0),
            Some(json!({"field": "Region", "area": "values", "position": 0, "move": true}))
        );
        assert_eq!(
            drop_edit(&field("Region", None), "values", 0),
            Some(json!({"field": "Region", "area": "values", "position": 0}))
        );
        // Values: reorder, move out to an axis, remove.
        assert_eq!(
            drop_edit(&PaneDrag::Value(1), "values", 0),
            Some(json!({"value": 1, "area": "values", "position": 0}))
        );
        assert_eq!(drop_edit(&PaneDrag::Value(0), "values", 0), None);
        assert_eq!(
            drop_edit(&PaneDrag::Value(0), "filters", 1),
            Some(json!({"value": 0, "area": "filters", "position": 1}))
        );
        assert_eq!(
            drop_edit(&PaneDrag::Value(0), "none", 0),
            Some(json!({"value": 0, "remove": true}))
        );
    }

    #[test]
    fn drop_index_counts_entries_above() {
        let r = |y: f32| egui::Rect::from_min_size(egui::pos2(0.0, y), egui::vec2(50.0, 20.0));
        let rows = [r(0.0), r(24.0), r(48.0)];
        assert_eq!(drop_index(&rows, -5.0), 0);
        assert_eq!(drop_index(&rows, 15.0), 1);
        assert_eq!(drop_index(&rows, 40.0), 2);
        assert_eq!(drop_index(&rows, 500.0), 3);
        assert_eq!(drop_index(&[], 10.0), 0);
    }
}
