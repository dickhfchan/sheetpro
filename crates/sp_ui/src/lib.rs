//! L6 shell: a thin egui front end over `sp_engine::Session`. Every action goes through
//! `Session::execute`; this crate owns only transient view state (edit buffer, hover…).

pub mod cfdraw;
pub mod chart;
pub mod dialogs;
pub mod grid;
mod menus;
pub mod theme;

#[cfg(feature = "screenshot")]
pub mod screenshot;

use egui::{Align2, Color32, FontId, Key, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use serde_json::json;
use sp_core::{CellAddr, CellRange, Value, col_to_letters};
use sp_engine::{Json, Session, catalog, registry};
use sp_render::{Align, layout};

use theme::{LIGHT as T, color};

pub const RIBBON_TABS: [&str; 8] = [
    "File",
    "Home",
    "Insert",
    "Page Layout",
    "Formulas",
    "Data",
    "Review",
    "View",
];

const HEADER_W: f32 = 46.0;
const HEADER_H: f32 = 22.0;

pub struct App {
    pub session: Session,
    /// Text in the formula bar / in-cell editor.
    pub edit_buf: String,
    pub(crate) editing_cell: Option<(usize, CellAddr)>,
    focus_editor: bool,
    drag_anchor: Option<CellAddr>,
    pub(crate) last_error: Option<String>,
    /// Open dialog and its fields (serializable, so scripts and tests can drive it).
    pub ui: dialogs::UiState,
}

impl App {
    pub fn new(session: Session) -> Self {
        let mut a = App {
            session,
            edit_buf: String::new(),
            editing_cell: None,
            focus_editor: false,
            drag_anchor: None,
            last_error: None,
            ui: dialogs::UiState::default(),
        };
        a.sync_buffer();
        a
    }

    /// Run a command; failures go to the status bar instead of panicking or vanishing.
    pub fn exec(&mut self, id: &str, params: Json) {
        match self.session.execute(id, &params) {
            Ok(_) => self.last_error = None,
            Err(e) => self.last_error = Some(e.to_string()),
        }
        if self.editing_cell.is_none() {
            self.sync_buffer();
        }
    }

    pub(crate) fn sync_buffer(&mut self) {
        let s = &self.session;
        self.edit_buf =
            s.wb.sheet(s.sheet_index())
                .and_then(|sh| sh.get(s.sel.active))
                .map(|c| c.input_text())
                .unwrap_or_default();
    }

    pub(crate) fn begin_edit(&mut self, initial: Option<String>) {
        let at = (self.session.sheet_index(), self.session.sel.active);
        self.editing_cell = Some(at);
        if let Some(t) = initial {
            self.edit_buf = t;
        } else {
            self.sync_buffer();
        }
        self.focus_editor = true;
        self.session.view.editing = true;
    }

    pub(crate) fn commit_edit(&mut self, then: Option<&str>) {
        if let Some((sheet, at)) = self.editing_cell.take() {
            let cell = format!(
                "{}!{}",
                sp_core::addr::quote_sheet_name(
                    &self
                        .session
                        .wb
                        .sheets
                        .get(sheet)
                        .map(|s| s.name.clone())
                        .unwrap_or_default()
                ),
                at.to_a1()
            );
            let input = self.edit_buf.clone();
            self.exec("cell.set", json!({ "cell": cell, "input": input }));
            if self.last_error.is_some() {
                // Keep the editor open so the user can fix the formula.
                self.editing_cell = Some((sheet, at));
                return;
            }
        }
        self.session.view.editing = false;
        if let Some(next) = then {
            self.exec(next, json!({}));
        }
        self.sync_buffer();
    }

    fn cancel_edit(&mut self) {
        self.editing_cell = None;
        self.session.view.editing = false;
        self.sync_buffer();
    }

    /// The whole window: ribbon, formula bar, grid, sheet tabs, status bar.
    pub fn show(&mut self, ui: &mut egui::Ui) {
        // The spreadsheet surface is light-themed regardless of the OS appearance.
        if ui.ctx().global_style().visuals.dark_mode {
            ui.ctx().set_theme(egui::Theme::Light);
        }
        if !self.session.view.full_screen {
            egui::Panel::top("ribbon")
                .frame(egui::Frame::NONE.fill(T.ribbon_bg))
                .show(ui, |ui| self.ribbon(ui));
        }
        if self.session.view.show_formula_bar {
            egui::Panel::top("formula_bar")
                .frame(
                    egui::Frame::NONE
                        .fill(T.formula_bar_bg)
                        .inner_margin(egui::Margin::symmetric(6, 4)),
                )
                .show(ui, |ui| self.formula_bar(ui));
        }
        egui::Panel::bottom("status")
            .frame(
                egui::Frame::NONE
                    .fill(T.status_bg)
                    .inner_margin(egui::Margin::symmetric(8, 3)),
            )
            .show(ui, |ui| self.status_bar(ui));
        egui::Panel::bottom("tabs")
            .frame(
                egui::Frame::NONE
                    .fill(T.tab_bg)
                    .inner_margin(egui::Margin::symmetric(4, 2)),
            )
            .show(ui, |ui| self.sheet_tabs(ui));
        egui::CentralPanel::no_frame().show(ui, |ui| self.grid(ui));
        self.keyboard(ui);
        let ctx = ui.ctx().clone();
        self.show_dialog(&ctx);
    }

    fn ribbon(&mut self, ui: &mut egui::Ui) {
        // Tab strip.
        egui::Frame::NONE
            .fill(T.ribbon_tab_bg)
            .inner_margin(egui::Margin::symmetric(8, 2))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("SheetPro")
                            .color(Color32::WHITE)
                            .strong(),
                    );
                    ui.add_space(12.0);
                    for tab in RIBBON_TABS {
                        let active = self.session.view.ribbon_tab == tab;
                        let text = egui::RichText::new(tab)
                            .color(if active { T.accent } else { Color32::WHITE })
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
        // Groups for the active tab, generated from the catalog.
        let tab = self.session.view.ribbon_tab.clone();
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
        egui::Frame::NONE
            .inner_margin(egui::Margin::symmetric(6, 4))
            .show(ui, |ui| {
                egui::ScrollArea::horizontal().show(ui, |ui| {
                    ui.horizontal_top(|ui| {
                        for (gname, items) in &groups {
                            ui.vertical(|ui| {
                                ui.set_max_height(84.0);
                                egui::Grid::new(format!("grp-{gname}"))
                                    .spacing([2.0, 1.0])
                                    .show(ui, |ui| {
                                        for (i, item) in items.iter().enumerate() {
                                            self.ribbon_item(ui, item);
                                            if i % 3 == 2 {
                                                ui.end_row();
                                            }
                                        }
                                    });
                                ui.label(
                                    egui::RichText::new(gname).size(10.0).color(T.group_label),
                                );
                            });
                            ui.separator();
                        }
                    });
                });
            });
    }

    fn formula_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let r = self.session.sel.range();
            let name = if r.is_single() {
                self.session.sel.active.to_a1()
            } else {
                format!("{}R x {}C", r.rows(), r.cols())
            };
            let mut name_box = name.clone();
            let nb = ui.add(
                egui::TextEdit::singleline(&mut name_box)
                    .desired_width(90.0)
                    .font(FontId::proportional(13.0)),
            );
            if nb.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) && name_box != name {
                self.exec("nav.goto_cell", json!({ "reference": name_box }));
            }
            ui.label(egui::RichText::new("fx").italics().color(T.group_label));
            let id = egui::Id::new("formula_editor");
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.edit_buf)
                    .id(id)
                    .desired_width(f32::INFINITY)
                    .font(FontId::monospace(13.0)),
            );
            if self.focus_editor {
                resp.request_focus();
                self.focus_editor = false;
            }
            if resp.gained_focus() && self.editing_cell.is_none() {
                self.editing_cell = Some((self.session.sheet_index(), self.session.sel.active));
                self.session.view.editing = true;
            }
            if resp.lost_focus() && self.editing_cell.is_some() {
                let (enter, tab, esc) = ui.input(|i| {
                    (
                        i.key_pressed(Key::Enter),
                        i.key_pressed(Key::Tab),
                        i.key_pressed(Key::Escape),
                    )
                });
                if esc {
                    self.cancel_edit();
                } else if enter {
                    self.commit_edit(Some("nav.enter_next"));
                } else if tab {
                    self.commit_edit(Some("nav.tab_next"));
                } else {
                    self.commit_edit(None);
                }
            }
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            match &self.last_error {
                Some(e) => ui.label(egui::RichText::new(e).color(T.error_text).size(11.5)),
                None => ui.label(
                    egui::RichText::new(if self.session.view.editing {
                        "Edit"
                    } else {
                        &self.session.status
                    })
                    .color(T.status_text)
                    .size(11.5),
                ),
            };
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(format!("{:.0}%", self.session.view.zoom * 100.0))
                        .size(11.5)
                        .color(T.status_text),
                );
                // Sum / Average / Count of the selection.
                let s = &self.session;
                let r = s.sel.range();
                if !r.is_single()
                    && let Some(sh) = s.wb.sheet(s.sheet_index())
                {
                    let (mut sum, mut n, mut count) = (0.0, 0usize, 0usize);
                    for (_, c) in sh.cells_in(r) {
                        if !c.value.is_empty() {
                            count += 1;
                        }
                        if let Value::Number(x) = c.value {
                            sum += x;
                            n += 1;
                        }
                    }
                    if count > 0 {
                        let mut t = format!("Count: {count}");
                        if n > 0 {
                            t = format!(
                                "Average: {}    {t}    Sum: {}",
                                sp_core::value::format_general(sum / n as f64),
                                sp_core::value::format_general(sum)
                            );
                        }
                        ui.label(egui::RichText::new(t).size(11.5).color(T.status_text));
                    }
                }
            });
        });
    }

    fn sheet_tabs(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let active = self.session.sheet_index();
            let names: Vec<(usize, String, Option<sp_model::Color>)> = self
                .session
                .wb
                .sheets
                .iter()
                .enumerate()
                .filter(|(_, s)| s.visibility == sp_model::Visibility::Visible)
                .map(|(i, s)| (i, s.name.clone(), s.tab_color))
                .collect();
            for (i, name, tc) in names {
                let is_active = i == active;
                let mut text = egui::RichText::new(&name).size(12.0).color(if is_active {
                    T.accent
                } else {
                    T.ribbon_text
                });
                if is_active {
                    text = text.strong();
                }
                let b = egui::Button::new(text)
                    .fill(if is_active { T.tab_active_bg } else { T.tab_bg })
                    .stroke(Stroke::new(1.0, tc.map_or(T.header_border, color)));
                let r = ui.add(b);
                if r.clicked() || r.secondary_clicked() {
                    self.exec("sheet.activate", json!({ "sheet": i }));
                }
                if r.double_clicked() {
                    self.ui.dialog = Some(dialogs::Dialog::Prompt(
                        dialogs::misc::Prompt::rename_sheet(&self.session, Some(i)),
                    ));
                }
                r.context_menu(|ui| self.tab_menu(ui, i, &name));
            }
            if ui
                .add(egui::Button::new("+").small())
                .on_hover_text("New sheet")
                .clicked()
            {
                self.exec("home.cells.insert_sheet", json!({}));
            }
        });
    }

    fn grid(&mut self, ui: &mut egui::Ui) {
        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, T.grid_bg);
        let s = &self.session;
        let Some(sheet) = s.wb.sheet(s.sheet_index()) else {
            return;
        };
        let zoom = s.view.zoom;
        let (hw, hh) = if s.view.show_headings {
            (HEADER_W * zoom.max(0.6), HEADER_H * zoom.max(0.6))
        } else {
            (0.0, 0.0)
        };
        let cells_origin = rect.min + Vec2::new(hw, hh);
        let view = layout(
            &s.wb,
            sheet,
            s.view.scroll,
            rect.width() - hw,
            rect.height() - hh,
            zoom,
            s.view.show_formulas,
        );
        // Tell the engine how much fits, for Page Up/Down.
        let page_rows = view.rows.len().saturating_sub(1).max(1) as u32;
        let page_cols = view.cols.len().saturating_sub(1).max(1) as u32;
        let sel = s.sel.range();
        let active = s.sel.active;
        let families = painter.fonts(|f| f.families());
        let font = |size: f32| grid::font_id(size, zoom, egui::FontFamily::Proportional);

        let col_rect = |c: u32| {
            view.cols
                .iter()
                .find(|x| x.0 == c)
                .map(|(_, x, w)| (cells_origin.x + x, *w))
        };
        let row_rect = |r: u32| {
            view.rows
                .iter()
                .find(|x| x.0 == r)
                .map(|(_, y, h)| (cells_origin.y + y, *h))
        };
        let cell_rect = |a: CellAddr| -> Option<Rect> {
            let (x, w) = col_rect(a.col)?;
            let (y, h) = row_rect(a.row)?;
            Some(Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, h)))
        };
        let merge_rects: Vec<(CellRange, Rect)> = view
            .merges
            .iter()
            .map(|m| {
                (
                    m.range,
                    Rect::from_min_size(cells_origin + Vec2::new(m.x, m.y), Vec2::new(m.w, m.h)),
                )
            })
            .collect();
        let in_merge = |a: CellAddr| merge_rects.iter().any(|(m, _)| m.contains(a));

        // Fills first, then gridlines, then merged areas (hiding their inner gridlines), then
        // text, then borders on top.
        for d in &view.cells {
            if let (Some(f), Some(r)) = (d.fill, cell_rect(d.addr)) {
                painter.rect_filled(r, 0.0, color(f));
            }
            if let Some(r) = cell_rect(d.addr) {
                cfdraw::draw(&painter, r, d, zoom);
            }
        }
        if sheet.show_gridlines && s.view.show_gridlines {
            for (_, y, h) in &view.rows {
                painter.hline(
                    rect.x_range(),
                    cells_origin.y + y + h,
                    Stroke::new(1.0, T.gridline),
                );
            }
            for (_, x, w) in &view.cols {
                painter.vline(
                    cells_origin.x + x + w,
                    rect.y_range(),
                    Stroke::new(1.0, T.gridline),
                );
            }
        }
        for (m, (_, mr)) in view.merges.iter().zip(&merge_rects) {
            painter.rect_filled(mr.shrink(0.5), 0.0, grid::merge_fill(m.cell.as_ref()));
        }
        // Selection highlight.
        let sel_rect = {
            let tl = cell_rect(CellAddr::new(
                sel.start.row.max(view.rows.first().map_or(0, |r| r.0)),
                sel.start.col.max(view.cols.first().map_or(0, |c| c.0)),
            ));
            let br_row = view
                .rows
                .iter()
                .rev()
                .find(|r| r.0 <= sel.end.row)
                .map(|r| r.0);
            let br_col = view
                .cols
                .iter()
                .rev()
                .find(|c| c.0 <= sel.end.col)
                .map(|c| c.0);
            match (
                tl,
                br_row
                    .zip(br_col)
                    .and_then(|(r, c)| cell_rect(CellAddr::new(r, c))),
            ) {
                (Some(a), Some(b)) if a.min.x <= b.max.x && a.min.y <= b.max.y => Some(a.union(b)),
                _ => None,
            }
        };
        if let Some(r) = sel_rect
            && !sel.is_single()
        {
            painter.rect_filled(r, 0.0, T.selection_fill);
        }
        let cell_area = Rect::from_min_max(cells_origin, rect.max);
        let blocks = |c: u32, row: u32| {
            let a = CellAddr::new(row, c);
            in_merge(a) || sheet.get(a).is_some_and(|x| !x.value.is_empty())
        };
        for d in &view.cells {
            let Some(r) = cell_rect(d.addr) else { continue };
            // Unwrapped, unrotated text overflows into empty neighbours, like the target app.
            let mut clip = r;
            if !d.wrap && d.rotation == 0 && !d.is_number {
                if matches!(d.align, Align::Left | Align::Center) {
                    let mut c = d.addr.col.saturating_add(1);
                    while let Some((x, w)) = col_rect(c) {
                        if blocks(c, d.addr.row) {
                            break;
                        }
                        clip.max.x = x + w;
                        c = c.saturating_add(1);
                    }
                }
                if matches!(d.align, Align::Right | Align::Center) {
                    let mut c = d.addr.col;
                    while let Some(prev) = c.checked_sub(1)
                        && let Some((x, _)) = col_rect(prev)
                        && !blocks(prev, d.addr.row)
                    {
                        clip.min.x = x;
                        c = prev;
                    }
                }
            }
            grid::draw_cell_text(
                &painter,
                &grid::CellText {
                    d,
                    rect: r,
                    clip: clip.intersect(cell_area),
                    zoom,
                    families: &families,
                },
            );
        }
        for (m, (_, mr)) in view.merges.iter().zip(&merge_rects) {
            if let Some(d) = &m.cell {
                grid::draw_cell_text(
                    &painter,
                    &grid::CellText {
                        d,
                        rect: *mr,
                        clip: mr.intersect(cell_area),
                        zoom,
                        families: &families,
                    },
                );
            }
        }
        {
            let bp = painter.with_clip_rect(cell_area.expand(1.0));
            for seg in &view.borders {
                grid::draw_border(&bp, cells_origin, seg);
            }
        }
        if let Some(r) = sel_rect {
            painter.rect_stroke(
                r,
                0.0,
                Stroke::new(1.5, T.selection_border),
                StrokeKind::Inside,
            );
        }
        let active_rect = merge_rects
            .iter()
            .find(|(m, _)| m.contains(active))
            .map(|(_, r)| *r)
            .or_else(|| cell_rect(active));
        if let Some(r) = active_rect {
            painter.rect_stroke(
                r,
                0.0,
                Stroke::new(2.0, T.selection_border),
                StrokeKind::Inside,
            );
            // Fill handle.
            painter.rect_filled(
                Rect::from_center_size(r.max, Vec2::splat(5.0)),
                0.0,
                T.selection_border,
            );
        }
        chart::paint_objects(&painter, s, &view, cells_origin, cell_area);
        // Headings.
        if s.view.show_headings {
            let top = Rect::from_min_max(rect.min, Pos2::new(rect.max.x, rect.min.y + hh));
            let left = Rect::from_min_max(rect.min, Pos2::new(rect.min.x + hw, rect.max.y));
            painter.rect_filled(top, 0.0, T.header_bg);
            painter.rect_filled(left, 0.0, T.header_bg);
            for (c, x, w) in &view.cols {
                let r = Rect::from_min_size(
                    Pos2::new(cells_origin.x + x, rect.min.y),
                    Vec2::new(*w, hh),
                );
                if *c >= sel.start.col && *c <= sel.end.col {
                    painter.rect_filled(r, 0.0, T.header_bg_selected);
                }
                painter.vline(r.max.x, r.y_range(), Stroke::new(1.0, T.header_border));
                painter.with_clip_rect(r).text(
                    r.center(),
                    Align2::CENTER_CENTER,
                    col_to_letters(*c),
                    font(9.0),
                    T.header_text,
                );
            }
            for (row, y, h) in &view.rows {
                let r = Rect::from_min_size(
                    Pos2::new(rect.min.x, cells_origin.y + y),
                    Vec2::new(hw, *h),
                );
                if *row >= sel.start.row && *row <= sel.end.row {
                    painter.rect_filled(r, 0.0, T.header_bg_selected);
                }
                painter.hline(r.x_range(), r.max.y, Stroke::new(1.0, T.header_border));
                painter.with_clip_rect(r).text(
                    r.center(),
                    Align2::CENTER_CENTER,
                    (row + 1).to_string(),
                    font(9.0),
                    T.header_text,
                );
            }
            painter.hline(
                rect.x_range(),
                rect.min.y + hh,
                Stroke::new(1.0, T.header_border),
            );
            painter.vline(
                rect.min.x + hw,
                rect.y_range(),
                Stroke::new(1.0, T.header_border),
            );
        }

        // Pointer → cell.
        let hit = |p: Pos2| -> Option<CellAddr> {
            let r = view
                .rows
                .iter()
                .find(|(_, y, h)| p.y >= cells_origin.y + y && p.y < cells_origin.y + y + h)?
                .0;
            let c = view
                .cols
                .iter()
                .find(|(_, x, w)| p.x >= cells_origin.x + x && p.x < cells_origin.x + x + w)?
                .0;
            Some(CellAddr::new(r, c))
        };
        let shift = ui.input(|i| i.modifiers.shift);
        if resp.drag_started() || resp.clicked() {
            if let Some(a) = resp.interact_pointer_pos().and_then(hit) {
                if self.editing_cell.is_some() {
                    self.commit_edit(None);
                }
                if shift {
                    let anchor = self.session.sel.anchor;
                    self.session.sel = sp_engine::Selection {
                        active: anchor,
                        anchor,
                        cursor: a,
                    };
                } else {
                    self.session.select(a);
                }
                self.drag_anchor = Some(self.session.sel.anchor);
                self.sync_buffer();
            }
        } else if resp.dragged()
            && let (Some(anchor), Some(a)) =
                (self.drag_anchor, resp.interact_pointer_pos().and_then(hit))
        {
            self.session.sel = sp_engine::Selection {
                active: anchor,
                anchor,
                cursor: a,
            };
        }
        if resp.double_clicked() {
            self.begin_edit(None);
        }
        // Right-click outside the selection selects the cell under the pointer first.
        if resp.secondary_clicked()
            && let Some(a) = resp.interact_pointer_pos().and_then(hit)
            && !self.session.sel.range().contains(a)
        {
            if self.editing_cell.is_some() {
                self.commit_edit(None);
            }
            self.session.select(a);
            self.sync_buffer();
        }
        resp.context_menu(|ui| self.grid_menu(ui));
        let scroll = ui.input(|i| i.smooth_scroll_delta);
        if resp.hovered() && (scroll.y.abs() > 0.5 || scroll.x.abs() > 0.5) {
            let v = &mut self.session.view;
            let dr = (-scroll.y / 20.0).round() as i64;
            let dc = (-scroll.x / 64.0).round() as i64;
            v.scroll = v.scroll.offset(dr, dc).unwrap_or(v.scroll);
        }
        self.session.view.page_rows = page_rows;
        self.session.view.page_cols = page_cols;
        // In-cell editor overlay shows the buffer over the active cell.
        if self.editing_cell.is_some()
            && let Some(r) = cell_rect(active)
        {
            let w = r.width().max(120.0);
            let er = Rect::from_min_size(r.min, Vec2::new(w, r.height()));
            painter.rect_filled(er, 0.0, T.grid_bg);
            painter.rect_stroke(
                er,
                0.0,
                Stroke::new(2.0, T.selection_border),
                StrokeKind::Inside,
            );
            painter.with_clip_rect(er).text(
                Pos2::new(er.min.x + 3.0, er.center().y),
                Align2::LEFT_CENTER,
                &self.edit_buf,
                font(11.0),
                T.cell_text,
            );
        }
    }

    /// Map key presses to catalog shortcuts → commands. Typing starts editing.
    fn keyboard(&mut self, ui: &mut egui::Ui) {
        if ui.memory(|m| m.focused().is_some()) || self.ui.dialog.is_some() {
            return; // A text field or a dialog owns the keyboard.
        }
        let events = ui.input(|i| i.events.clone());
        for ev in events {
            match ev {
                egui::Event::Text(t) if self.editing_cell.is_none() => {
                    self.begin_edit(Some(t));
                }
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    if let Some(sc) = shortcut_string(key, modifiers) {
                        self.on_shortcut(&sc);
                    }
                }
                egui::Event::Copy => self.exec("home.clipboard.copy", json!({})),
                egui::Event::Cut => self.exec("home.clipboard.cut", json!({})),
                egui::Event::Paste(_) => self.exec("home.clipboard.paste", json!({})),
                _ => {}
            }
        }
    }

    pub fn on_shortcut(&mut self, sc: &str) {
        match sc {
            "F2" => return self.begin_edit(None),
            "Delete" | "Backspace" => return self.exec("home.editing.clear_contents", json!({})),
            "Enter" => return self.exec("nav.enter_next", json!({})),
            _ => {}
        }
        if let Some(cmd) = registry().by_shortcut(sc) {
            self.activate_command(cmd.id);
        }
    }
}

/// `Ctrl+Shift+Down`-style name for a key press (Cmd counts as Ctrl on macOS).
pub fn shortcut_string(key: Key, m: egui::Modifiers) -> Option<String> {
    let name = match key {
        Key::ArrowUp => "Up".to_string(),
        Key::ArrowDown => "Down".to_string(),
        Key::ArrowLeft => "Left".to_string(),
        Key::ArrowRight => "Right".to_string(),
        Key::PageUp => "PageUp".to_string(),
        Key::PageDown => "PageDown".to_string(),
        Key::Home => "Home".to_string(),
        Key::End => "End".to_string(),
        Key::Enter => "Enter".to_string(),
        Key::Tab => "Tab".to_string(),
        Key::Escape => "Escape".to_string(),
        Key::Delete => "Delete".to_string(),
        Key::Backspace => "Backspace".to_string(),
        Key::Space => "Space".to_string(),
        k => {
            let n = k.name();
            if n.len() == 1 || n.starts_with('F') && n.len() <= 3 {
                n.to_string()
            } else {
                return None;
            }
        }
    };
    let mut s = String::new();
    if m.command || m.ctrl {
        s.push_str("Ctrl+");
    }
    if m.alt {
        s.push_str("Alt+");
    }
    if m.shift {
        s.push_str("Shift+");
    }
    s.push_str(&name);
    Some(s)
}

/// Selection as A1 text, for tests and the control channel.
pub fn selection_text(s: &Session) -> String {
    let r: CellRange = s.sel.range();
    r.to_a1()
}
