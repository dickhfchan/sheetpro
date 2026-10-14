//! In-grid objects drawn over the cells and their pointer handling: AutoFilter buttons on
//! header cells (sheet filter and tables), note / comment corner marks with hover popups,
//! the data-validation list arrow and input message, and chart selection with move/resize
//! handles. Clicks open popups (see `dialogs::grid_popups`) or run commands; nothing here
//! mutates the workbook directly.

use crate::App;
use crate::dialogs::Dialog;
use crate::dialogs::grid_popups::{CommentCard, FilterDropdown, ValidationList};
use crate::theme::LIGHT as T;
use egui::{FontId, LayerId, Order, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, Vec2};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sp_core::{CellAddr, CellRange};
use sp_model::{CommentKind, ValidationKind};
use sp_render::GridView;

/// Cell geometry of the current frame.
pub(crate) struct Geo<'a> {
    pub view: &'a GridView,
    pub origin: Pos2,
    pub area: Rect,
}

impl Geo<'_> {
    pub fn cell_rect(&self, a: CellAddr) -> Option<Rect> {
        let (_, x, w) = self.view.cols.iter().find(|c| c.0 == a.col)?;
        let (_, y, h) = self.view.rows.iter().find(|r| r.0 == a.row)?;
        Some(Rect::from_min_size(
            self.origin + Vec2::new(*x, *y),
            Vec2::new(*w, *h),
        ))
    }

    pub fn cell_at(&self, p: Pos2) -> Option<CellAddr> {
        let r = self
            .view
            .rows
            .iter()
            .find(|(_, y, h)| p.y >= self.origin.y + y && p.y < self.origin.y + y + h)?
            .0;
        let c = self
            .view
            .cols
            .iter()
            .find(|(_, x, w)| p.x >= self.origin.x + x && p.x < self.origin.x + x + w)?
            .0;
        Some(CellAddr::new(r, c))
    }
}

/// Something clickable drawn over the grid this frame.
#[derive(Debug, Clone)]
pub(crate) enum Hot {
    Filter {
        rect: Rect,
        col: u32,
        range: CellRange,
        table: Option<String>,
    },
    ListArrow {
        rect: Rect,
        cell: CellAddr,
    },
    CommentMark {
        rect: Rect,
        cell: CellAddr,
    },
    Chart {
        rect: Rect,
        idx: usize,
    },
}

impl Hot {
    fn rect(&self) -> Rect {
        match self {
            Hot::Filter { rect, .. }
            | Hot::ListArrow { rect, .. }
            | Hot::CommentMark { rect, .. }
            | Hot::Chart { rect, .. } => *rect,
        }
    }
}

/// Which part of a selected chart a drag grabbed: the body or one of 8 handles
/// (`(dx, dy)` in -1..=1: which edges the handle moves).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ChartDrag {
    pub idx: usize,
    pub handle: Option<(i8, i8)>,
    pub start: [f32; 2],
    pub now: [f32; 2],
    pub rect: [f32; 4],
}

impl ChartDrag {
    /// The chart's rectangle after applying the drag so far.
    pub fn ghost(&self) -> Rect {
        let [x0, y0, x1, y1] = self.rect;
        let (dx, dy) = (self.now[0] - self.start[0], self.now[1] - self.start[1]);
        let mut r = Rect::from_min_max(Pos2::new(x0, y0), Pos2::new(x1, y1));
        match self.handle {
            None => r = r.translate(Vec2::new(dx, dy)),
            Some((hx, hy)) => {
                if hx < 0 {
                    r.min.x = (r.min.x + dx).min(r.max.x - 12.0);
                } else if hx > 0 {
                    r.max.x = (r.max.x + dx).max(r.min.x + 12.0);
                }
                if hy < 0 {
                    r.min.y = (r.min.y + dy).min(r.max.y - 12.0);
                } else if hy > 0 {
                    r.max.y = (r.max.y + dy).max(r.min.y + 12.0);
                }
            }
        }
        r
    }
}

const HANDLES: [(i8, i8); 8] = [
    (-1, -1),
    (0, -1),
    (1, -1),
    (-1, 0),
    (1, 0),
    (-1, 1),
    (0, 1),
    (1, 1),
];

fn handle_rect(r: Rect, h: (i8, i8)) -> Rect {
    let x = match h.0 {
        -1 => r.min.x,
        0 => r.center().x,
        _ => r.max.x,
    };
    let y = match h.1 {
        -1 => r.min.y,
        0 => r.center().y,
        _ => r.max.y,
    };
    Rect::from_center_size(Pos2::new(x, y), Vec2::splat(8.0))
}

/// Down-arrow drop-down button (AutoFilter / validation list).
pub(crate) fn dropdown_button(p: &Painter, r: Rect, active: bool) {
    p.rect_filled(r, 2.0, T.dropdown_bg);
    p.rect_stroke(
        r,
        2.0,
        Stroke::new(1.0, T.dropdown_border),
        StrokeKind::Inside,
    );
    let c = r.center();
    let s = (r.width() * 0.22).max(2.5);
    if active {
        // Funnel: a filter is applied to this column.
        let top = c.y - s;
        p.add(Shape::convex_polygon(
            vec![
                Pos2::new(c.x - s * 1.3, top),
                Pos2::new(c.x + s * 1.3, top),
                Pos2::new(c.x + s * 0.3, c.y + s * 0.1),
                Pos2::new(c.x - s * 0.3, c.y + s * 0.1),
            ],
            T.filter_active,
            Stroke::NONE,
        ));
        p.rect_filled(
            Rect::from_min_max(
                Pos2::new(c.x - s * 0.3, c.y),
                Pos2::new(c.x + s * 0.3, c.y + s * 1.2),
            ),
            0.0,
            T.filter_active,
        );
    } else {
        p.add(Shape::convex_polygon(
            vec![
                Pos2::new(c.x - s, c.y - s * 0.5),
                Pos2::new(c.x + s, c.y - s * 0.5),
                Pos2::new(c.x, c.y + s * 0.6),
            ],
            T.dropdown_glyph,
            Stroke::NONE,
        ));
    }
}

/// A floating text box (note, comment preview, input message) next to `at`.
fn text_box(
    p: &Painter,
    at: Pos2,
    title: Option<&str>,
    body: &str,
    fill: egui::Color32,
    border: egui::Color32,
) -> Rect {
    let width = 200.0;
    let body_g = p.layout(
        body.to_string(),
        FontId::proportional(11.5),
        T.note_text,
        width - 12.0,
    );
    let title_g = title.map(|t| {
        p.layout(
            t.to_string(),
            FontId::proportional(11.5),
            T.comment_author,
            width - 12.0,
        )
    });
    let th = title_g.as_ref().map_or(0.0, |g| g.size().y + 2.0);
    let h = (body_g.size().y + th + 10.0).clamp(24.0, 400.0);
    let r = Rect::from_min_size(at, Vec2::new(width, h));
    p.rect_filled(r, 2.0, fill);
    p.rect_stroke(r, 2.0, Stroke::new(1.0, border), StrokeKind::Inside);
    let mut y = r.min.y + 5.0;
    if let Some(g) = title_g {
        p.galley(Pos2::new(r.min.x + 6.0, y), g, T.comment_author);
        y += th;
    }
    p.galley(Pos2::new(r.min.x + 6.0, y), body_g, T.note_text);
    r
}

fn origin_id() -> egui::Id {
    egui::Id::new("sp-cells-origin")
}

/// Screen position of the top-left of the cell area (row 1 / column A at no scroll) in the
/// last frame, for tests and scripts that need to point at a cell or chart.
pub fn cells_origin(ctx: &egui::Context) -> Option<Pos2> {
    ctx.data(|d| d.get_temp::<Pos2>(origin_id()))
}

impl App {
    /// Paint objects over the cells and return their hit areas (topmost last).
    pub(crate) fn paint_overlays(&self, ui: &egui::Ui, painter: &Painter, geo: &Geo) -> Vec<Hot> {
        ui.ctx()
            .data_mut(|d| d.insert_temp(origin_id(), geo.origin));
        let mut hots = Vec::new();
        let s = &self.session;
        let si = s.sheet_index();
        let Some(sheet) = s.wb.sheet(si) else {
            return hots;
        };
        let p = painter.with_clip_rect(geo.area.intersect(painter.clip_rect()));
        let button = |cell: Rect| {
            let side = (cell.height() - 3.0).clamp(10.0, 17.0);
            Rect::from_min_size(
                Pos2::new(cell.max.x - side - 1.5, cell.max.y - side - 1.5),
                Vec2::splat(side),
            )
        };
        // AutoFilter buttons on the header row.
        if let Some(af) = &sheet.auto_filter {
            for col in af.range.start.col..=af.range.end.col {
                let a = CellAddr::new(af.range.start.row, col);
                if let Some(cr) = geo.cell_rect(a) {
                    let r = button(cr);
                    let off = col - af.range.start.col;
                    let active = af.columns.iter().any(|c| c.col == off);
                    dropdown_button(&p, r, active);
                    hots.push(Hot::Filter {
                        rect: r,
                        col,
                        range: af.range,
                        table: None,
                    });
                }
            }
        }
        // Table header buttons.
        for t in &sheet.tables {
            if !(t.autofilter && t.header_row) {
                continue;
            }
            for col in t.range.start.col..=t.range.end.col {
                let a = CellAddr::new(t.range.start.row, col);
                if let Some(cr) = geo.cell_rect(a) {
                    let r = button(cr);
                    dropdown_button(&p, r, false);
                    let body_end = if t.totals_row {
                        t.range.end.row.saturating_sub(1)
                    } else {
                        t.range.end.row
                    };
                    hots.push(Hot::Filter {
                        rect: r,
                        col,
                        range: CellRange::new(
                            t.range.start,
                            CellAddr::new(body_end.max(t.range.start.row), t.range.end.col),
                        ),
                        table: Some(t.name.clone()),
                    });
                }
            }
        }
        // Note / comment corner marks.
        for c in &sheet.comments {
            let Some(cr) = geo.cell_rect(c.addr) else {
                continue;
            };
            let s7 = 7.0;
            let tr = cr.right_top();
            match c.kind {
                CommentKind::Note => {
                    p.add(Shape::convex_polygon(
                        vec![
                            Pos2::new(tr.x - s7, tr.y + 1.0),
                            Pos2::new(tr.x - 1.0, tr.y + 1.0),
                            Pos2::new(tr.x - 1.0, tr.y + s7),
                        ],
                        T.note_mark,
                        Stroke::NONE,
                    ));
                }
                CommentKind::Thread => {
                    let b =
                        Rect::from_min_size(Pos2::new(tr.x - 9.0, tr.y + 1.5), Vec2::new(7.5, 6.0));
                    p.rect_filled(b, 1.5, T.comment_mark);
                    p.add(Shape::convex_polygon(
                        vec![
                            Pos2::new(b.min.x + 1.5, b.max.y - 0.5),
                            Pos2::new(b.min.x + 4.0, b.max.y - 0.5),
                            Pos2::new(b.min.x + 1.5, b.max.y + 2.5),
                        ],
                        T.comment_mark,
                        Stroke::NONE,
                    ));
                }
            }
            hots.push(Hot::CommentMark {
                rect: Rect::from_min_max(
                    Pos2::new(tr.x - 11.0, tr.y),
                    Pos2::new(tr.x, tr.y + 10.0),
                ),
                cell: c.addr,
            });
        }
        // Validation: list arrow and input message for the active cell.
        let active = s.sel.active;
        if let (Some(rule), Some(cr)) = (sheet.validation_at(active), geo.cell_rect(active)) {
            if rule.kind == ValidationKind::List && rule.dropdown {
                let side = (cr.height() - 2.0).clamp(12.0, 18.0);
                let r = Rect::from_min_size(
                    Pos2::new(cr.max.x + 1.0, cr.max.y - side),
                    Vec2::splat(side),
                );
                dropdown_button(&p, r, false);
                hots.push(Hot::ListArrow {
                    rect: r,
                    cell: active,
                });
            }
            if rule.show_prompt && !rule.prompt.is_empty() && self.ui.dialog.is_none() {
                let title = (!rule.prompt_title.is_empty()).then_some(rule.prompt_title.as_str());
                text_box(
                    &p,
                    cr.right_bottom() + Vec2::new(8.0, 4.0),
                    title,
                    &rule.prompt,
                    T.tip_bg,
                    T.tip_border,
                );
            }
        }
        // Charts: selection frame, handles and drag ghost.
        for (idx, r) in crate::chart::chart_rects(s, geo.origin) {
            if !r.intersects(geo.area) {
                continue;
            }
            let selected = self.ui.grid.chart == Some(idx);
            let mut hit = r;
            if selected {
                p.rect_stroke(
                    r,
                    0.0,
                    Stroke::new(1.0, T.handle_border),
                    StrokeKind::Outside,
                );
                for h in HANDLES {
                    let hr = handle_rect(r, h);
                    p.rect_filled(hr, 1.0, T.handle_fill);
                    p.rect_stroke(
                        hr,
                        1.0,
                        Stroke::new(1.0, T.handle_border),
                        StrokeKind::Inside,
                    );
                }
                hit = r.expand(5.0);
            }
            hots.push(Hot::Chart { rect: hit, idx });
        }
        if let Some(d) = &self.ui.grid.drag {
            p.rect_stroke(
                d.ghost(),
                0.0,
                Stroke::new(1.5, T.drag_ghost),
                StrokeKind::Inside,
            );
        }
        // Notes shown permanently (Show All Notes / a note set visible), then hover popups,
        // on a layer above the grid.
        let top = ui
            .ctx()
            .layer_painter(LayerId::new(Order::Middle, egui::Id::new("sp-notes")))
            .with_clip_rect(geo.area);
        for c in &sheet.comments {
            if c.kind == CommentKind::Note
                && c.visible
                && let Some(cr) = geo.cell_rect(c.addr)
            {
                text_box(
                    &top,
                    cr.right_top() + Vec2::new(12.0, 0.0),
                    Some(&c.author),
                    &c.text,
                    T.note_bg,
                    T.note_border,
                );
            }
        }
        let hover = ui.input(|i| i.pointer.hover_pos());
        if self.ui.dialog.is_none()
            && self.ui.grid.drag.is_none()
            && let Some(hp) = hover.filter(|p| geo.area.contains(*p))
            && let Some(a) = geo.cell_at(hp)
            && let Some(c) = sheet.comment_at(a)
            && !(c.kind == CommentKind::Note && c.visible)
            && let Some(cr) = geo.cell_rect(a)
        {
            let mut body = c.text.clone();
            if c.kind == CommentKind::Thread {
                for rep in c.replies.iter().take(3) {
                    body.push_str(&format!("\n— {}: {}", rep.author, rep.text));
                }
                if c.replies.len() > 3 {
                    body.push_str(&format!("\n… {} more", c.replies.len() - 3));
                }
            }
            let fill = if c.kind == CommentKind::Note {
                T.note_bg
            } else {
                T.comment_bg
            };
            text_box(
                &top,
                cr.right_top() + Vec2::new(12.0, 0.0),
                Some(&c.author),
                &body,
                fill,
                T.note_border,
            );
        }
        hots
    }

    /// Pointer handling for overlay objects. Returns true when the pointer event was
    /// consumed (the cell under it must not be selected).
    pub(crate) fn overlay_input(&mut self, resp: &egui::Response, hots: &[Hot], geo: &Geo) -> bool {
        let pos = resp.interact_pointer_pos();
        let top_hit = |p: Pos2| hots.iter().rev().find(|h| h.rect().contains(p)).cloned();
        // Chart drag in progress.
        if let Some(mut d) = self.ui.grid.drag {
            if let Some(p) = pos {
                d.now = [p.x, p.y];
            }
            if resp.drag_stopped() || !resp.dragged() {
                self.ui.grid.drag = None;
                let moved = (d.now[0] - d.start[0]).abs() + (d.now[1] - d.start[1]).abs() > 3.0;
                if moved
                    && let Some(anchor) =
                        crate::chart::rect_to_anchor(&self.session, geo.origin, d.ghost())
                {
                    self.exec(
                        "insert.chart.recommended",
                        json!({ "chart": d.idx, "anchor": anchor }),
                    );
                }
            } else {
                self.ui.grid.drag = Some(d);
            }
            return true;
        }
        if resp.drag_started()
            && let Some(p) = pos
            && let Some(Hot::Chart { idx, .. }) = top_hit(p)
        {
            let rect = crate::chart::chart_rects(&self.session, geo.origin)
                .into_iter()
                .find(|(i, _)| *i == idx)
                .map(|(_, r)| r);
            if let Some(r) = rect {
                let handle = if self.ui.grid.chart == Some(idx) {
                    HANDLES
                        .iter()
                        .find(|h| handle_rect(r, **h).expand(2.0).contains(p))
                        .copied()
                } else {
                    None
                };
                self.ui.grid.chart = Some(idx);
                self.ui.grid.drag = Some(ChartDrag {
                    idx,
                    handle,
                    start: [p.x, p.y],
                    now: [p.x, p.y],
                    rect: [r.min.x, r.min.y, r.max.x, r.max.y],
                });
                return true;
            }
        }
        if !(resp.clicked() || resp.double_clicked() || resp.drag_started()) {
            return false;
        }
        let Some(p) = pos else { return false };
        let hit = top_hit(p);
        if !matches!(hit, Some(Hot::Chart { .. })) {
            self.ui.grid.chart = None;
        }
        match hit {
            Some(Hot::Chart { idx, .. }) => {
                self.ui.grid.chart = Some(idx);
                if resp.double_clicked() {
                    match crate::dialogs::chart_format::ChartFormat::new(&self.session, idx) {
                        Some(d) => self.ui.dialog = Some(Dialog::ChartFormat(d)),
                        None => self.last_error = Some("no such chart".into()),
                    }
                }
                true
            }
            Some(Hot::Filter {
                rect,
                col,
                range,
                table,
            }) => {
                if resp.clicked() {
                    let mut d = FilterDropdown::new(&self.session, range, col, table);
                    d.pos = Some([rect.min.x, rect.max.y + 1.0]);
                    self.ui.dialog = Some(Dialog::Filter(Box::new(d)));
                }
                true
            }
            Some(Hot::ListArrow { rect, cell }) => {
                if resp.clicked() {
                    let mut d = ValidationList::new(&self.session, cell);
                    d.pos = Some([rect.max.x, rect.max.y + 1.0]);
                    self.ui.dialog = Some(Dialog::ValidationList(d));
                }
                true
            }
            Some(Hot::CommentMark { rect, cell }) => {
                if resp.clicked() {
                    if self.editing_cell.is_some() {
                        self.commit_edit(None);
                    }
                    self.session.select(cell);
                    self.sync_buffer();
                    let mut d = CommentCard::new(&self.session, cell);
                    d.pos = Some([rect.max.x + 6.0, rect.min.y]);
                    self.ui.dialog = Some(Dialog::Comment(d));
                }
                true
            }
            None => false,
        }
    }

    /// Delete / Escape while a chart is selected act on the chart.
    pub(crate) fn chart_key(&mut self, sc: &str) -> bool {
        let Some(idx) = self.ui.grid.chart else {
            return false;
        };
        match sc {
            "Delete" | "Backspace" => {
                self.exec(
                    "insert.chart.recommended",
                    json!({ "chart": idx, "delete": true }),
                );
                self.ui.grid.chart = None;
                true
            }
            "Escape" => {
                self.ui.grid.chart = None;
                true
            }
            _ => false,
        }
    }

    /// Screen anchor for a popup opened by name (screenshots, tests): below the header
    /// cell's button, or next to the active cell.
    pub(crate) fn place_popups(&mut self, geo: &Geo) {
        let Some(d) = self.ui.dialog.as_mut() else {
            return;
        };
        match d {
            Dialog::Filter(f) if f.pos.is_none() => {
                let a = CellAddr::new(f.header_row(), f.col);
                if let Some(r) = geo.cell_rect(a) {
                    f.pos = Some([r.max.x - 18.0, r.max.y + 1.0]);
                }
            }
            Dialog::ValidationList(v) if v.pos.is_none() => {
                if let Some(r) = geo.cell_rect(v.cell) {
                    v.pos = Some([r.min.x, r.max.y + 1.0]);
                }
            }
            Dialog::Comment(c) if c.pos.is_none() => {
                if let Some(r) = geo.cell_rect(c.cell) {
                    c.pos = Some([r.max.x + 6.0, r.min.y]);
                }
            }
            _ => {}
        }
    }
}

/// Grid view state that belongs to the shell: the selected chart and a drag in progress.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GridUi {
    pub chart: Option<usize>,
    #[serde(skip)]
    pub drag: Option<ChartDrag>,
}
