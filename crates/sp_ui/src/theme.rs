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
    /// Cell borders whose colour is "automatic".
    pub border_auto: Color32,
    pub formula_bar_bg: Color32,
    pub status_bg: Color32,
    pub status_text: Color32,
    pub tab_bg: Color32,
    pub tab_active_bg: Color32,
    pub error_text: Color32,
    pub dialog_bg: Color32,
    pub dialog_border: Color32,
    pub dialog_backdrop: Color32,
    pub dialog_title: Color32,
    pub dialog_label: Color32,
    pub list_bg: Color32,
    pub list_border: Color32,
    pub list_selected: Color32,
    pub preview_bg: Color32,
    pub swatch_border: Color32,
    /// Chart objects: plot background, frame, axis lines, gridlines, titles, tick labels.
    pub chart_bg: Color32,
    pub chart_border: Color32,
    pub chart_axis: Color32,
    pub chart_grid: Color32,
    pub chart_text: Color32,
    pub chart_subtext: Color32,
    /// AutoFilter / validation drop-down buttons drawn in cells.
    pub dropdown_bg: Color32,
    pub dropdown_border: Color32,
    pub dropdown_glyph: Color32,
    /// Funnel glyph of a column with an active filter.
    pub filter_active: Color32,
    /// Corner marks: notes (triangle) and threaded comments.
    pub note_mark: Color32,
    pub comment_mark: Color32,
    pub note_bg: Color32,
    pub note_border: Color32,
    pub note_text: Color32,
    pub comment_bg: Color32,
    pub comment_author: Color32,
    /// Data-validation input message box.
    pub tip_bg: Color32,
    pub tip_border: Color32,
    /// Selected chart frame and its resize handles.
    pub handle_fill: Color32,
    pub handle_border: Color32,
    pub drag_ghost: Color32,
    /// Alert dialog icons (stop / warning / information).
    pub alert_stop: Color32,
    pub alert_warning: Color32,
    pub alert_info: Color32,
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
    border_auto: Color32::BLACK,
    formula_bar_bg: Color32::WHITE,
    status_bg: Color32::from_rgb(243, 243, 243),
    status_text: Color32::from_rgb(68, 68, 68),
    tab_bg: Color32::from_rgb(233, 233, 233),
    tab_active_bg: Color32::WHITE,
    error_text: Color32::from_rgb(192, 0, 0),
    dialog_bg: Color32::from_rgb(250, 250, 250),
    dialog_border: Color32::from_rgb(160, 160, 160),
    dialog_backdrop: Color32::from_rgba_premultiplied(0, 0, 0, 60),
    dialog_title: Color32::from_rgb(32, 32, 32),
    dialog_label: Color32::from_rgb(80, 80, 80),
    list_bg: Color32::WHITE,
    list_border: Color32::from_rgb(196, 199, 197),
    list_selected: Color32::from_rgb(211, 227, 217),
    preview_bg: Color32::WHITE,
    swatch_border: Color32::from_rgb(140, 140, 140),
    chart_bg: Color32::WHITE,
    chart_border: Color32::from_rgb(200, 200, 200),
    chart_axis: Color32::from_rgb(170, 170, 170),
    chart_grid: Color32::from_rgb(228, 228, 228),
    chart_text: Color32::from_rgb(64, 64, 64),
    chart_subtext: Color32::from_rgb(96, 96, 96),
    dropdown_bg: Color32::from_rgb(246, 246, 246),
    dropdown_border: Color32::from_rgb(160, 160, 160),
    dropdown_glyph: Color32::from_rgb(70, 70, 70),
    filter_active: Color32::from_rgb(33, 115, 70),
    note_mark: Color32::from_rgb(210, 40, 40),
    comment_mark: Color32::from_rgb(120, 70, 170),
    note_bg: Color32::from_rgb(255, 253, 214),
    note_border: Color32::from_rgb(150, 150, 120),
    note_text: Color32::from_rgb(30, 30, 30),
    comment_bg: Color32::WHITE,
    comment_author: Color32::from_rgb(33, 115, 70),
    tip_bg: Color32::from_rgb(255, 255, 238),
    tip_border: Color32::from_rgb(120, 120, 120),
    handle_fill: Color32::WHITE,
    handle_border: Color32::from_rgb(90, 90, 90),
    drag_ghost: Color32::from_rgb(33, 115, 70),
    alert_stop: Color32::from_rgb(200, 40, 40),
    alert_warning: Color32::from_rgb(220, 160, 20),
    alert_info: Color32::from_rgb(40, 110, 200),
};

/// Default colours for chart series and sparklines, in order (cycled past the end).
pub const SERIES_PALETTE: [Color32; 8] = [
    Color32::from_rgb(46, 110, 190),
    Color32::from_rgb(232, 126, 44),
    Color32::from_rgb(86, 158, 80),
    Color32::from_rgb(196, 66, 72),
    Color32::from_rgb(128, 96, 178),
    Color32::from_rgb(38, 160, 168),
    Color32::from_rgb(214, 172, 36),
    Color32::from_rgb(118, 118, 118),
];

/// The `i`-th series colour.
pub fn series_color(i: usize) -> Color32 {
    SERIES_PALETTE
        .get(i % SERIES_PALETTE.len())
        .copied()
        .unwrap_or(Color32::GRAY)
}

/// Colour choices offered by the fill / font / tab colour pickers: a row of standard hues,
/// then tints and shades. These are cell data values the user picks, not chrome colours.
pub const SWATCHES: [Color32; 40] = [
    Color32::from_rgb(0, 0, 0),
    Color32::from_rgb(68, 68, 68),
    Color32::from_rgb(127, 127, 127),
    Color32::from_rgb(191, 191, 191),
    Color32::from_rgb(255, 255, 255),
    Color32::from_rgb(68, 84, 106),
    Color32::from_rgb(68, 114, 196),
    Color32::from_rgb(237, 125, 49),
    Color32::from_rgb(192, 0, 0),
    Color32::from_rgb(255, 0, 0),
    Color32::from_rgb(255, 192, 0),
    Color32::from_rgb(255, 255, 0),
    Color32::from_rgb(146, 208, 80),
    Color32::from_rgb(0, 176, 80),
    Color32::from_rgb(0, 176, 240),
    Color32::from_rgb(0, 112, 192),
    Color32::from_rgb(0, 32, 96),
    Color32::from_rgb(112, 48, 160),
    Color32::from_rgb(33, 115, 70),
    Color32::from_rgb(165, 165, 165),
    Color32::from_rgb(242, 242, 242),
    Color32::from_rgb(214, 220, 228),
    Color32::from_rgb(218, 227, 243),
    Color32::from_rgb(251, 229, 214),
    Color32::from_rgb(255, 230, 230),
    Color32::from_rgb(255, 242, 204),
    Color32::from_rgb(255, 255, 204),
    Color32::from_rgb(226, 239, 218),
    Color32::from_rgb(222, 235, 247),
    Color32::from_rgb(237, 226, 246),
    Color32::from_rgb(217, 217, 217),
    Color32::from_rgb(173, 185, 202),
    Color32::from_rgb(180, 199, 231),
    Color32::from_rgb(248, 203, 173),
    Color32::from_rgb(255, 153, 153),
    Color32::from_rgb(255, 230, 153),
    Color32::from_rgb(198, 224, 180),
    Color32::from_rgb(189, 215, 238),
    Color32::from_rgb(197, 165, 222),
    Color32::from_rgb(169, 208, 142),
];

/// `RRGGBB` hex for a colour, as command params expect.
pub fn hex(c: Color32) -> String {
    format!("{:02X}{:02X}{:02X}", c.r(), c.g(), c.b())
}

/// Parse `RRGGBB` (as stored in UI state) back to a colour.
pub fn from_hex(s: &str) -> Option<Color32> {
    sp_model::Color::from_hex(s).map(color)
}

pub fn color(c: sp_model::Color) -> Color32 {
    Color32::from_rgb(c.0, c.1, c.2)
}
