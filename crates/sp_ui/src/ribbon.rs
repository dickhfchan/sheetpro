//! The ribbon: tab strip plus the active tab's groups, generated from the catalog.
//!
//! Each group lays its controls out in columns of [`ROWS`] (top to bottom, then the next
//! column), every column as wide as its widest control, so labels never overlap. When the
//! groups are wider than the window, groups collapse from the right into a single drop-down
//! button that lists their controls (as the target app does), until the rest fit. Only if
//! every group is collapsed and it still does not fit does the strip scroll horizontally.
//!
//! The layout of the last frame is published in egui temp memory ([`group_rects`]) so tests
//! can check that no two groups overlap at a given window width.

use crate::App;
use crate::theme::LIGHT as T;
use egui::{Align, Align2, FontId, Layout, Rect, RichText, Stroke, Ui, Vec2};
use sp_engine::catalog;

/// Controls per column inside a group.
pub const ROWS: usize = 3;
/// Height of one control row.
const ROW_H: f32 = 20.0;
/// Height reserved for the group name under the controls.
const LABEL_H: f32 = 14.0;
/// Space between two columns of a group.
const COL_GAP: f32 = 10.0;
/// Space either side of the separator line between two groups.
const GROUP_PAD: f32 = 7.0;
/// Widest a single control may make its column.
const MAX_COL_W: f32 = 180.0;
/// Text size of ribbon controls (matches `App::ribbon_item`).
const ITEM_TEXT: f32 = 11.0;

/// Height of the group strip (controls + label).
pub const GROUPS_H: f32 = ROWS as f32 * ROW_H + LABEL_H;

/// One laid-out group (screen rectangle) of the last rendered frame.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupRect {
    pub name: String,
    pub rect: Rect,
    pub collapsed: bool,
}

fn rects_id() -> egui::Id {
    egui::Id::new("sp-ribbon-groups")
}

/// Group rectangles of the last frame (empty before the first frame).
pub fn group_rects(ctx: &egui::Context) -> Vec<GroupRect> {
    ctx.data(|d| d.get_temp::<Vec<GroupRect>>(rects_id()))
        .unwrap_or_default()
}

/// The active tab's groups in catalog order: `(group name, items)`.
pub fn tab_groups(tab: &str) -> Vec<(String, Vec<&'static catalog::CatalogItem>)> {
    let mut groups: Vec<(String, Vec<&'static catalog::CatalogItem>)> = Vec::new();
    for item in catalog::commands() {
        let mut parts = item.loc.split(" › ");
        let (Some(t), g) = (parts.next(), parts.next()) else {
            continue;
        };
        if t != tab {
            continue;
        }
        let g = g.unwrap_or(t).to_string();
        match groups.iter_mut().find(|(name, _)| *name == g) {
            Some((_, v)) => v.push(item),
            None => groups.push((g, vec![item])),
        }
    }
    groups
}

fn text_w(ui: &Ui, text: &str, size: f32) -> f32 {
    ui.painter()
        .layout_no_wrap(text.to_string(), FontId::proportional(size), T.ribbon_text)
        .size()
        .x
}

/// Width a control needs (mirrors the widgets `App::ribbon_item` draws).
fn item_w(ui: &Ui, item: &catalog::CatalogItem) -> f32 {
    let spacing = ui.spacing();
    let combo_extra = spacing.icon_width + spacing.button_padding.x * 2.0 + 6.0;
    let w = match item.id.as_str() {
        "home.font.family" => 118.0 + combo_extra,
        "home.font.size" => 44.0 + combo_extra,
        "home.font.fill_color" | "home.font.font_color" => {
            text_w(ui, &item.label, ITEM_TEXT) + spacing.button_padding.x * 2.0
        }
        _ => text_w(ui, &item.label, ITEM_TEXT),
    };
    (w + 2.0).min(MAX_COL_W)
}

/// Column widths of an expanded group.
fn column_widths(ui: &Ui, items: &[&catalog::CatalogItem]) -> Vec<f32> {
    items
        .chunks(ROWS)
        .map(|col| col.iter().map(|i| item_w(ui, i)).fold(0.0, f32::max))
        .collect()
}

fn collapsed_label(name: &str) -> String {
    format!("{name} ⏷")
}

/// Width of the "more" button that ends a partly collapsed group.
const MORE_W: f32 = 22.0;

/// Measured sizes of one group: its column widths, its name label and its collapsed button.
#[derive(Debug, Clone, Default)]
pub struct GroupSize {
    pub cols: Vec<f32>,
    pub label: f32,
    pub collapsed: f32,
}

impl GroupSize {
    /// Width with the first `k` columns shown (all: no "more" button; none: collapsed).
    pub fn width(&self, k: usize) -> f32 {
        if k == 0 && !self.cols.is_empty() {
            return self.collapsed;
        }
        let k = k.min(self.cols.len());
        let shown: f32 =
            self.cols.iter().take(k).sum::<f32>() + COL_GAP * k.saturating_sub(1) as f32;
        let more = if k < self.cols.len() {
            COL_GAP + MORE_W
        } else {
            0.0
        };
        (shown + more).max(self.label)
    }
}

fn strip_width(sizes: &[GroupSize], shown: &[usize]) -> f32 {
    sizes
        .iter()
        .zip(shown)
        .map(|(g, k)| g.width(*k) + 2.0 * GROUP_PAD + 1.0)
        .sum()
}

/// How many columns of each group to show so the strip fits `avail` px. Shrinks the group
/// showing the most columns first (the rightmost on ties), one column at a time; a group with
/// no columns left collapses to its drop-down button.
pub fn plan(sizes: &[GroupSize], avail: f32) -> Vec<usize> {
    let mut shown: Vec<usize> = sizes.iter().map(|g| g.cols.len()).collect();
    while strip_width(sizes, &shown) > avail {
        let Some((i, _)) = shown
            .iter()
            .enumerate()
            .filter(|(_, k)| **k > 0)
            .max_by_key(|(i, k)| (**k, *i))
        else {
            break;
        };
        if let Some(k) = shown.get_mut(i) {
            *k -= 1;
        }
    }
    shown
}

impl App {
    pub(crate) fn ribbon(&mut self, ui: &mut Ui) {
        self.ribbon_tabs(ui);
        let tab = self.session.view.ribbon_tab.clone();
        let groups = tab_groups(&tab);
        // Measure every group, then decide how many columns of each fit.
        let label_font = 10.0;
        let sizes: Vec<GroupSize> = groups
            .iter()
            .map(|(name, items)| GroupSize {
                cols: column_widths(ui, items),
                label: text_w(ui, name, label_font),
                collapsed: text_w(ui, &collapsed_label(name), 12.0) + 12.0,
            })
            .collect();
        let margin = 6.0;
        let avail = (ui.available_width() - 2.0 * margin).max(0.0);
        let shown = plan(&sizes, avail);

        let mut rects = Vec::with_capacity(groups.len());
        egui::Frame::NONE
            .inner_margin(egui::Margin::symmetric(margin as i8, 4))
            .show(ui, |ui| {
                egui::ScrollArea::horizontal()
                    .id_salt("ribbon-scroll")
                    .show(ui, |ui| {
                        ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            for (gi, (name, items)) in groups.iter().enumerate() {
                                let size = sizes.get(gi).cloned().unwrap_or_default();
                                let k = shown.get(gi).copied().unwrap_or(0);
                                let w = size.width(k);
                                let collapsed = k == 0;
                                ui.add_space(GROUP_PAD);
                                let (rect, _) = ui.allocate_exact_size(
                                    Vec2::new(w, GROUPS_H),
                                    egui::Sense::hover(),
                                );
                                let mut child = ui.new_child(
                                    egui::UiBuilder::new()
                                        .max_rect(rect)
                                        .layout(Layout::left_to_right(Align::Min)),
                                );
                                child.set_clip_rect(rect.intersect(ui.clip_rect()));
                                if collapsed {
                                    self.collapsed_group(&mut child, rect, name, items);
                                } else {
                                    self.expanded_group(&mut child, &size.cols, k, name, items);
                                    ui.painter().text(
                                        rect.center_bottom(),
                                        Align2::CENTER_BOTTOM,
                                        name,
                                        FontId::proportional(label_font),
                                        T.group_label,
                                    );
                                }
                                rects.push(GroupRect {
                                    name: name.clone(),
                                    rect,
                                    collapsed,
                                });
                                ui.add_space(GROUP_PAD);
                                let (sep, _) = ui.allocate_exact_size(
                                    Vec2::new(1.0, GROUPS_H),
                                    egui::Sense::hover(),
                                );
                                ui.painter().vline(
                                    sep.center().x,
                                    sep.y_range().shrink(4.0),
                                    Stroke::new(1.0, T.ribbon_separator),
                                );
                            }
                        });
                    });
            });
        ui.ctx().data_mut(|d| d.insert_temp(rects_id(), rects));
    }

    fn ribbon_tabs(&mut self, ui: &mut Ui) {
        egui::Frame::NONE
            .fill(T.ribbon_tab_bg)
            .inner_margin(egui::Margin::symmetric(8, 2))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new("SheetPro").color(T.ribbon_tab_text).strong());
                    ui.add_space(12.0);
                    for tab in crate::RIBBON_TABS {
                        let active = self.session.view.ribbon_tab == tab;
                        let text = RichText::new(tab)
                            .color(if active { T.accent } else { T.ribbon_tab_text })
                            .size(12.5);
                        let b = egui::Button::new(text)
                            .fill(if active {
                                T.ribbon_tab_active
                            } else {
                                T.ribbon_tab_bg
                            })
                            .stroke(Stroke::NONE);
                        if ui.add(b).clicked() {
                            self.session.view.ribbon_tab = tab.to_string();
                        }
                    }
                });
            });
    }

    /// Columns of `ROWS` controls, each column as wide as its widest control. Only the first
    /// `shown` columns are drawn; the rest go into a trailing "more" drop-down.
    fn expanded_group(
        &mut self,
        ui: &mut Ui,
        widths: &[f32],
        shown: usize,
        name: &str,
        items: &[&catalog::CatalogItem],
    ) {
        let split = (shown * ROWS).min(items.len());
        let (visible, rest) = items.split_at(split);
        for (ci, col) in visible.chunks(ROWS).enumerate() {
            if ci > 0 {
                ui.add_space(COL_GAP);
            }
            let w = widths.get(ci).copied().unwrap_or(60.0);
            ui.allocate_ui_with_layout(
                Vec2::new(w, ROWS as f32 * ROW_H),
                Layout::top_down(Align::Min),
                |ui| {
                    ui.set_width(w);
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for item in col {
                        ui.allocate_ui_with_layout(
                            Vec2::new(w, ROW_H),
                            Layout::left_to_right(Align::Center),
                            |ui| {
                                ui.set_height(ROW_H);
                                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                                self.ribbon_item(ui, item);
                            },
                        );
                    }
                },
            );
        }
        if !rest.is_empty() {
            ui.add_space(COL_GAP);
            ui.allocate_ui_with_layout(
                Vec2::new(MORE_W, ROWS as f32 * ROW_H),
                Layout::top_down(Align::Center),
                |ui| {
                    ui.add_space(ROW_H * 0.5);
                    ui.menu_button(RichText::new("⏷").size(12.0).color(T.ribbon_text), |ui| {
                        ui.set_min_width(160.0);
                        for item in rest {
                            ui.horizontal(|ui| self.ribbon_item(ui, item));
                        }
                    })
                    .response
                    .on_hover_text(format!("More {name} commands"));
                },
            );
        }
    }

    /// A collapsed group: one drop-down button that lists the group's controls.
    fn collapsed_group(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        name: &str,
        items: &[&catalog::CatalogItem],
    ) {
        let h = 26.0;
        ui.add_space(0.0);
        let top = (rect.height() - h) / 2.0;
        ui.allocate_ui_with_layout(
            Vec2::new(rect.width(), rect.height()),
            Layout::top_down(Align::Center),
            |ui| {
                ui.add_space(top.max(0.0));
                ui.menu_button(
                    RichText::new(collapsed_label(name))
                        .size(12.0)
                        .color(T.ribbon_text),
                    |ui| {
                        ui.set_min_width(160.0);
                        for item in items {
                            ui.horizontal(|ui| self.ribbon_item(ui, item));
                        }
                    },
                )
                .response
                .on_hover_text(format!("{name} ({} commands)", items.len()));
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(cols: &[f32]) -> GroupSize {
        GroupSize {
            cols: cols.to_vec(),
            label: 20.0,
            collapsed: 30.0,
        }
    }

    #[test]
    fn plan_shrinks_widest_group_first_until_it_fits() {
        let sizes = [g(&[50.0, 50.0]), g(&[50.0, 50.0, 50.0]), g(&[40.0])];
        let full = strip_width(&sizes, &[2, 3, 1]);
        assert_eq!(plan(&sizes, full), vec![2, 3, 1]);
        // A little too narrow: the group with the most columns gives one up.
        assert_eq!(plan(&sizes, full - 1.0), vec![2, 2, 1]);
        // Fits whatever plan returns unless everything is collapsed.
        for avail in [0.0, 100.0, 200.0, 300.0, 400.0] {
            let p = plan(&sizes, avail);
            assert!(strip_width(&sizes, &p) <= avail || p.iter().all(|k| *k == 0));
        }
        assert!(plan(&[], 10.0).is_empty());
        // Widths: collapsed, partial (with "more" button), full.
        let s = g(&[50.0, 60.0]);
        assert_eq!(s.width(0), 30.0);
        assert_eq!(s.width(1), 50.0 + COL_GAP + MORE_W);
        assert_eq!(s.width(2), 50.0 + COL_GAP + 60.0);
    }

    #[test]
    fn every_tab_has_groups() {
        for tab in crate::RIBBON_TABS {
            assert!(!tab_groups(tab).is_empty(), "{tab}");
        }
    }
}
