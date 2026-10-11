//! Small widgets shared by dialogs, the ribbon and context menus: a framed list box, the
//! colour swatch grid, and a colour drop-down.

use crate::theme::{LIGHT as T, SWATCHES, from_hex, hex};
use egui::{Color32, RichText, Sense, Stroke, StrokeKind, Ui, Vec2};

/// Framed, scrollable single-select list. Returns the index clicked this frame and whether it
/// was a double-click.
pub fn list_box(
    ui: &mut Ui,
    id: &str,
    size: Vec2,
    items: &[String],
    selected: Option<usize>,
) -> Option<(usize, bool)> {
    let mut hit = None;
    egui::Frame::NONE
        .fill(T.list_bg)
        .stroke(Stroke::new(1.0, T.list_border))
        .inner_margin(egui::Margin::same(2))
        .show(ui, |ui| {
            ui.set_min_size(size);
            ui.set_max_width(size.x);
            egui::ScrollArea::vertical()
                .id_salt(id)
                .max_height(size.y)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.set_width(size.x - 8.0);
                    ui.visuals_mut().selection.bg_fill = T.list_selected;
                    ui.visuals_mut().selection.stroke.color = T.ribbon_text;
                    let layout = egui::Layout::top_down_justified(egui::Align::Min);
                    for (i, it) in items.iter().enumerate() {
                        let sel = selected == Some(i);
                        let r = ui
                            .with_layout(layout, |ui| {
                                ui.selectable_label(sel, RichText::new(it).size(12.5))
                            })
                            .inner;
                        if r.double_clicked() {
                            hit = Some((i, true));
                        } else if r.clicked() {
                            hit = Some((i, false));
                        }
                    }
                });
        });
    hit
}

/// The swatch grid. Returns the colour clicked this frame.
pub fn swatch_grid(ui: &mut Ui, current: Option<Color32>) -> Option<Color32> {
    let mut picked = None;
    egui::Grid::new(ui.next_auto_id())
        .spacing([3.0, 3.0])
        .show(ui, |ui| {
            for (i, c) in SWATCHES.iter().enumerate() {
                let (rect, resp) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::click());
                ui.painter().rect_filled(rect, 2.0, *c);
                let on = current == Some(*c);
                ui.painter().rect_stroke(
                    rect,
                    2.0,
                    Stroke::new(
                        if on || resp.hovered() { 2.0 } else { 1.0 },
                        if on { T.accent } else { T.swatch_border },
                    ),
                    StrokeKind::Inside,
                );
                if resp.on_hover_text(format!("#{}", hex(*c))).clicked() {
                    picked = Some(*c);
                }
                if i % 10 == 9 {
                    ui.end_row();
                }
            }
        });
    picked
}

/// A colour drop-down bound to `value` (`None` = automatic / no colour). Returns true when the
/// value changed.
pub fn color_combo(ui: &mut Ui, id: &str, value: &mut Option<String>, none_label: &str) -> bool {
    let cur = value.as_deref().and_then(from_hex);
    let mut changed = false;
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(22.0, 16.0), Sense::hover());
        ui.painter()
            .rect_filled(rect, 2.0, cur.unwrap_or(T.preview_bg));
        ui.painter().rect_stroke(
            rect,
            2.0,
            Stroke::new(1.0, T.swatch_border),
            StrokeKind::Inside,
        );
        let text = value
            .as_ref()
            .map_or_else(|| none_label.to_string(), |h| format!("#{h}"));
        egui::ComboBox::new(id, "")
            .selected_text(text)
            .width(120.0)
            .show_ui(ui, |ui| {
                if ui.button(none_label).clicked() {
                    *value = None;
                    changed = true;
                    ui.close();
                }
                if let Some(c) = swatch_grid(ui, cur) {
                    *value = Some(hex(c));
                    changed = true;
                    ui.close();
                }
            });
    });
    changed
}
