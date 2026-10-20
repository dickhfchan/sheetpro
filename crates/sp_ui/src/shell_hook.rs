//! Hooks for the desktop app (apps/sheetpro), kept out of the core shell:
//!
//! - **Command interception**: the desktop registers command ids it handles natively (file
//!   dialogs, unsaved-changes prompts, options window…). Ribbon activations of those ids are
//!   queued here instead of running, and the desktop drains the queue each frame. Headless
//!   users (screenshots, tests) register nothing, so behaviour is unchanged for them.
//! - **Split panes**: when `view.split` is on, the grid area shows two panes over the same
//!   sheet with independent scroll positions (`ViewState::window.split_scroll`). A frozen
//!   sheet shows no split (freeze wins; `view.split` itself unfreezes).
//! - **App preferences** that change how the shell draws (gridline colour, R1C1 headings),
//!   pushed from the app settings with [`apply_settings`].

use crate::App;
use crate::theme::LIGHT as T;
use sp_core::{CellAddr, col_to_letters};
use sp_engine::settings::Settings;
use std::cell::{Cell, RefCell};

thread_local! {
    static INTERCEPTED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static PENDING: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static PREFS: Cell<ShellPrefs> = const { Cell::new(ShellPrefs { gridline: None, r1c1: false }) };
}

/// The app preferences the shell reads while drawing.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ShellPrefs {
    /// Gridline colour; `None` = the theme's.
    pub gridline: Option<egui::Color32>,
    /// R1C1 reference style: numbered column headings and R1C1 Name Box.
    pub r1c1: bool,
}

/// Use these app settings when drawing on this thread (the desktop calls this at start-up
/// and whenever the options change).
pub fn apply_settings(s: &Settings) {
    let prefs = ShellPrefs {
        gridline: s
            .gridline_rgb()
            .map(|[r, g, b]| egui::Color32::from_rgb(r, g, b)),
        r1c1: s.r1c1,
    };
    PREFS.with(|p| p.set(prefs));
}

pub fn prefs() -> ShellPrefs {
    PREFS.with(Cell::get)
}

/// Gridline colour: the preference, else `theme`.
pub(crate) fn gridline(theme: egui::Color32) -> egui::Color32 {
    prefs().gridline.unwrap_or(theme)
}

/// Column heading text: letters, or the column number in R1C1 style.
pub(crate) fn col_label(c: u32) -> String {
    if prefs().r1c1 {
        c.saturating_add(1).to_string()
    } else {
        col_to_letters(c)
    }
}

/// Name Box text for one cell.
pub(crate) fn cell_label(a: CellAddr) -> String {
    if prefs().r1c1 {
        format!("R{}C{}", a.row.saturating_add(1), a.col.saturating_add(1))
    } else {
        a.to_a1()
    }
}

/// Name Box input: `R5C3` becomes `C5` so Go To understands it; anything else is unchanged.
pub(crate) fn name_box_reference(text: &str) -> String {
    let t = text.trim();
    let upper = t.to_ascii_uppercase();
    let parsed = upper.strip_prefix('R').and_then(|rest| {
        let (r, c) = rest.split_once('C')?;
        let row: u32 = r.parse().ok()?;
        let col: u32 = c.parse().ok()?;
        (row >= 1 && col >= 1).then(|| CellAddr::new(row - 1, col - 1))
    });
    match parsed {
        Some(a) if a.row < sp_core::MAX_ROWS && a.col < sp_core::MAX_COLS => a.to_a1(),
        _ => t.to_string(),
    }
}

/// Route ribbon activations of these command ids to [`take_pending`] (this thread only).
/// Command ids the desktop app handles natively (file dialogs, closing, options) when the
/// ribbon activates them; it registers them with [`set_intercepted`].
pub const DESKTOP_INTERCEPTED: [&str; 16] = [
    "file.export.pdf",
    "insert.picture",
    "layout.background",
    "file.new",
    "file.open",
    "file.close",
    "file.save",
    "file.save_as",
    "file.save_copy",
    "file.export.csv",
    "file.exit",
    "file.recent",
    "file.options",
    "file.inspect",
    "data.get_from_text",
    "app.report_problem",
];

pub fn set_intercepted(ids: &[&str]) {
    INTERCEPTED.with(|i| *i.borrow_mut() = ids.iter().map(|s| s.to_string()).collect());
}

/// Called by `App::activate_command`: true when the desktop handles `id` itself.
pub fn intercept(id: &str) -> bool {
    let hit = INTERCEPTED.with(|i| i.borrow().iter().any(|x| x == id));
    if hit {
        PENDING.with(|p| p.borrow_mut().push(id.to_string()));
    }
    hit
}

/// Command ids activated since the last call.
pub fn take_pending() -> Vec<String> {
    PENDING.with(|p| std::mem::take(&mut *p.borrow_mut()))
}

const DIVIDER: f32 = 5.0;
const MIN_PANE: f32 = 40.0;

impl App {
    /// The grid, or two panes when the window is split.
    pub(crate) fn grid_area(&mut self, ui: &mut egui::Ui) {
        let Some(at) = self.session.view.window.split else {
            return self.grid(ui);
        };
        let frozen = self
            .session
            .wb
            .sheet(self.session.sheet_index())
            .is_some_and(|sh| sh.freeze.is_some());
        if frozen {
            // Freezing replaced the split (as in the target app); continue from the first
            // pane's position so no rows go missing between the panes.
            self.session.view.window.split = None;
            self.session.view.scroll = self.session.view.window.split_scroll;
            return self.grid(ui);
        }
        let rect = ui.available_rect_before_wrap();
        let w = &self.session.view.window;
        let (first_scroll, size) = (w.split_scroll, w.split_size.max(1));
        let by_rows = at.row > 0;
        // First pane: headings plus `size` rows (columns) from its own scroll position.
        let offs = self.first_pane_extent(first_scroll, size, by_rows);
        let (first, second, divider) = if by_rows {
            let y = (rect.min.y + offs).clamp(
                rect.min.y + MIN_PANE,
                (rect.max.y - MIN_PANE).max(rect.min.y + MIN_PANE),
            );
            (
                egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x, y)),
                egui::Rect::from_min_max(egui::pos2(rect.min.x, y + DIVIDER), rect.max),
                egui::Rect::from_min_max(
                    egui::pos2(rect.min.x, y),
                    egui::pos2(rect.max.x, y + DIVIDER),
                ),
            )
        } else {
            let x = (rect.min.x + offs).clamp(
                rect.min.x + MIN_PANE,
                (rect.max.x - MIN_PANE).max(rect.min.x + MIN_PANE),
            );
            (
                egui::Rect::from_min_max(rect.min, egui::pos2(x, rect.max.y)),
                egui::Rect::from_min_max(egui::pos2(x + DIVIDER, rect.min.y), rect.max),
                egui::Rect::from_min_max(
                    egui::pos2(x, rect.min.y),
                    egui::pos2(x + DIVIDER, rect.max.y),
                ),
            )
        };
        // First pane with its own scroll; the second (main) pane last, so its page size is
        // what the engine keeps for Page Up/Down.
        let main_scroll = self.session.view.scroll;
        let (page_rows, page_cols) = (self.session.view.page_rows, self.session.view.page_cols);
        self.session.view.scroll = first_scroll;
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(first)
                .id_salt("split_pane_1"),
            |ui| self.grid(ui),
        );
        // Wheel scrolling in the first pane moves only the first pane.
        self.session.view.window.split_scroll = self.session.view.scroll;
        self.session.view.scroll = main_scroll;
        self.session.view.page_rows = page_rows;
        self.session.view.page_cols = page_cols;
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(second)
                .id_salt("split_pane_2"),
            |ui| self.grid(ui),
        );
        ui.painter().rect_filled(divider, 0.0, T.header_border);
    }

    /// Height (width) in px of the first pane: column (row) headings plus `size` rows
    /// (columns) starting at `from`.
    fn first_pane_extent(&self, from: CellAddr, size: u32, by_rows: bool) -> f32 {
        let s = &self.session;
        let zoom = s.view.zoom.clamp(0.1, 4.0);
        let head = if s.view.show_headings {
            let base: f32 = if by_rows { 22.0 } else { 46.0 };
            base * zoom.max(0.6)
        } else {
            0.0
        };
        let Some(sheet) = s.wb.sheet(s.sheet_index()) else {
            return head;
        };
        let n = size.min(500);
        let px: f32 = if by_rows {
            (from.row..from.row.saturating_add(n))
                .map(|r| sp_render::row_height_px(sheet.row_height(r)))
                .sum()
        } else {
            (from.col..from.col.saturating_add(n))
                .map(|c| sp_render::col_width_px(sheet.col_width(c)))
                .sum()
        };
        head + px * zoom
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intercept_queues_only_registered_ids() {
        set_intercepted(&["file.open"]);
        assert!(intercept("file.open"));
        assert!(!intercept("home.font.bold"));
        assert_eq!(take_pending(), vec!["file.open".to_string()]);
        assert!(take_pending().is_empty());
        set_intercepted(&[]);
        assert!(!intercept("file.open"));
    }

    #[test]
    fn prefs_change_headings_names_and_gridlines() {
        let theme = egui::Color32::from_rgb(1, 2, 3);
        apply_settings(&Settings::default());
        assert_eq!(col_label(27), "AB");
        assert_eq!(cell_label(CellAddr::new(4, 2)), "C5");
        let mut st = Settings {
            r1c1: true,
            gridline_color: "#102030".into(),
            ..Default::default()
        };
        apply_settings(&st);
        assert_eq!(col_label(27), "28");
        assert_eq!(cell_label(CellAddr::new(4, 2)), "R5C3");
        assert_eq!(gridline(theme), egui::Color32::from_rgb(0x10, 0x20, 0x30));
        assert_eq!(name_box_reference("r5c3"), "C5");
        assert_eq!(name_box_reference("R0C1"), "R0C1");
        assert_eq!(name_box_reference("R99999999C1"), "R99999999C1");
        assert_eq!(name_box_reference("Sales"), "Sales");
        st.gridline_color = "bad".into();
        apply_settings(&st);
        assert_eq!(gridline(theme), theme);
        apply_settings(&Settings::default());
    }
}
