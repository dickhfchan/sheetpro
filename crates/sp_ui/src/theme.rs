//! Theme tokens. Widgets take colours from here, never from literals.

use egui::Color32;

pub struct Theme {
    pub ribbon_bg: Color32,
    pub ribbon_tab_bg: Color32,
    pub ribbon_tab_active: Color32,
    pub ribbon_text: Color32,
    pub ribbon_text_disabled: Color32,
    pub group_label: Color32,
    pub accent: Color32,
    pub grid_bg: Color32,
    pub gridline: Color32,
    pub header_bg: Color32,
    pub header_bg_selected: Color32,
    pub header_text: Color32,
    pub header_border: Color32,
    pub selection_fill: Color32,
    pub selection_border: Color32,
    pub cell_text: Color32,
    pub formula_bar_bg: Color32,
    pub status_bg: Color32,
    pub status_text: Color32,
    pub tab_bg: Color32,
    pub tab_active_bg: Color32,
    pub error_text: Color32,
}

pub const LIGHT: Theme = Theme {
    ribbon_bg: Color32::from_rgb(243, 243, 243),
    ribbon_tab_bg: Color32::from_rgb(33, 115, 70),
    ribbon_tab_active: Color32::from_rgb(243, 243, 243),
    ribbon_text: Color32::from_rgb(38, 38, 38),
    ribbon_text_disabled: Color32::from_rgb(170, 170, 170),
    group_label: Color32::from_rgb(110, 110, 110),
    accent: Color32::from_rgb(33, 115, 70),
    grid_bg: Color32::WHITE,
    gridline: Color32::from_rgb(218, 220, 224),
    header_bg: Color32::from_rgb(248, 249, 250),
    header_bg_selected: Color32::from_rgb(211, 227, 217),
    header_text: Color32::from_rgb(68, 68, 68),
    header_border: Color32::from_rgb(196, 199, 197),
    selection_fill: Color32::from_rgba_premultiplied(33, 115, 70, 22),
    selection_border: Color32::from_rgb(33, 115, 70),
    cell_text: Color32::from_rgb(20, 20, 20),
    formula_bar_bg: Color32::WHITE,
    status_bg: Color32::from_rgb(243, 243, 243),
    status_text: Color32::from_rgb(68, 68, 68),
    tab_bg: Color32::from_rgb(233, 233, 233),
    tab_active_bg: Color32::WHITE,
    error_text: Color32::from_rgb(192, 0, 0),
};

pub fn color(c: sp_model::Color) -> Color32 {
    Color32::from_rgb(c.0, c.1, c.2)
}
