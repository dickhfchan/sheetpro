//! Paper-oriented views over the sheet area: page break preview (page boundaries and
//! "Page N" watermarks over the grid), page layout (pages drawn as paper, with rulers) and
//! the print preview opened by `file.print`.
//!
//! Pagination and page layout come from `sp_render::paginate` via
//! `sp_engine::commands::print_cmds` (the same scenes the PDF writer draws); this module only
//! maps them to the egui painter.

use crate::theme::color;
use egui::epaint::TextShape;
use egui::{
    Align2, Color32, FontFamily, FontId, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, Vec2,
};
use sp_core::CellAddr;
use sp_engine::Session;
use sp_engine::commands::print_cmds::{ViewMode, print_job};
use sp_render::GridView;
use sp_render::chart::{col_offset_px, row_offset_px};
use sp_render::paginate::{self, EdgeKind, Item, PrintOptions};
use sp_render::std14::Family;

/// Colour tokens for the page views (kept here so the shared theme file stays untouched by
/// this feature; same role as `theme::Theme`).
struct PageTheme {
    /// Page boundary lines in page break preview.
    boundary: Color32,
    /// Veil over cells outside the print area in page break preview.
    outside: Color32,
    watermark: Color32,
    /// Workspace behind the paper.
    desk: Color32,
    paper: Color32,
    paper_edge: Color32,
    /// Margin guides in page layout view.
    margin: Color32,
    ruler_bg: Color32,
    ruler_tick: Color32,
    label: Color32,
}

const P: PageTheme = PageTheme {
    boundary: Color32::from_rgb(38, 84, 196),
    outside: Color32::from_rgba_premultiplied(28, 28, 28, 90),
    watermark: Color32::from_rgba_premultiplied(38, 38, 38, 160),
    desk: Color32::from_rgb(214, 216, 220),
    paper: Color32::WHITE,
    paper_edge: Color32::from_rgb(160, 162, 168),
    margin: Color32::from_rgb(200, 210, 228),
    ruler_bg: Color32::from_rgb(246, 246, 246),
    ruler_tick: Color32::from_rgb(110, 110, 110),
    label: Color32::from_rgb(60, 60, 60),
};

/// Screen px per printed point at 100% zoom (96 px per 72 pt).
const PX_PER_PT: f32 = 96.0 / 72.0;
/// Most pages drawn in one frame of page layout view.
const MAX_VISIBLE_PAGES: usize = 12;
const RULER: f32 = 18.0;
const GAP: f32 = 24.0;

/// Draw the page layout view or the print preview over `rect` when one is active. Returns
/// `true` when it drew (the caller then skips the normal grid).
pub fn paper(painter: &Painter, rect: Rect, s: &Session) -> bool {
    let pv = &s.view.print;
    if pv.preview {
        preview(painter, rect, s);
        true
    } else if pv.mode == ViewMode::PageLayout {
        page_layout(painter, rect, s);
        true
    } else {
        false
    }
}

/// Backstage › Print: one page of the job, fitted to the area, with "Page i of n".
fn preview(painter: &Painter, rect: Rect, s: &Session) {
    painter.rect_filled(rect, 0.0, P.desk);
    let pv = &s.view.print;
    let Ok(job) = print_job(s, &pv.preview_sheets, None) else {
        return;
    };
    let label_h = 28.0;
    if job.is_empty() {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "Nothing to print",
            FontId::proportional(16.0),
            P.label,
        );
        return;
    }
    let i = pv.preview_page.min(job.len().saturating_sub(1));
    let Some(scene) = job.scene(&s.wb, i) else {
        return;
    };
    let avail = rect.shrink(16.0);
    let avail = Rect::from_min_max(avail.min, Pos2::new(avail.max.x, avail.max.y - label_h));
    let fit = (avail.width() / scene.width.max(1.0)).min(avail.height() / scene.height.max(1.0));
    let k = (fit * pv.preview_zoom.clamp(0.1, 4.0)).max(0.05);
    let size = Vec2::new(scene.width, scene.height) * k;
    let page = Rect::from_min_size(
        Pos2::new(avail.center().x - size.x / 2.0, avail.min.y),
        size,
    );
    draw_paper(painter, page);
    draw_items(
        &painter.with_clip_rect(page.intersect(rect)),
        page.min,
        k,
        &scene.items,
    );
    painter.text(
        Pos2::new(rect.center().x, rect.max.y - label_h / 2.0 - 4.0),
        Align2::CENTER_CENTER,
        format!("Page {} of {}", i + 1, job.len()),
        FontId::proportional(13.0),
        P.label,
    );
}

/// Page layout view: the active sheet's pages as paper, top to bottom, starting with the page
/// that holds the scroll position; optional rulers along the top and left.
fn page_layout(painter: &Painter, rect: Rect, s: &Session) {
    painter.rect_filled(rect, 0.0, P.desk);
    let Ok(job) = print_job(s, "active", None) else {
        return;
    };
    let k = s.view.zoom.clamp(0.1, 4.0) * PX_PER_PT;
    let ruler = if s.view.print.ruler { RULER } else { 0.0 };
    let area = Rect::from_min_max(rect.min + Vec2::new(ruler, ruler), rect.max);
    let p = painter.with_clip_rect(area.intersect(painter.clip_rect()));
    // First page: the one whose body shows the scroll cell (else the first).
    let first = job
        .sheets
        .first()
        .and_then(|(pg, _)| pg.page_of(s.view.scroll))
        .unwrap_or(0);
    let mut y = area.min.y + GAP;
    let mut first_page: Option<(Rect, f32, f32)> = None;
    for i in (first..job.len()).take(MAX_VISIBLE_PAGES) {
        if y > area.max.y {
            break;
        }
        let Some(scene) = job.scene(&s.wb, i) else {
            break;
        };
        let size = Vec2::new(scene.width, scene.height) * k;
        let x = (area.center().x - size.x / 2.0).max(area.min.x + GAP);
        let page = Rect::from_min_size(Pos2::new(x, y), size);
        draw_paper(&p, page);
        // Margin guides (dashed), from the sheet's page setup.
        if let Some((pg, _)) = job.sheets.first() {
            let m = pg.setup.margins;
            let inner = Rect::from_min_max(
                page.min + Vec2::new(m.left as f32 * 72.0, m.top as f32 * 72.0) * k,
                page.max - Vec2::new(m.right as f32 * 72.0, m.bottom as f32 * 72.0) * k,
            );
            if inner.is_positive() {
                dashed_rect(&p, inner, Stroke::new(1.0, P.margin), 4.0, 3.0);
            }
            if first_page.is_none() {
                first_page = Some((page, m.left as f32 * 72.0 * k, m.top as f32 * 72.0 * k));
            }
        }
        draw_items(
            &p.with_clip_rect(page.intersect(area)),
            page.min,
            k,
            &scene.items,
        );
        y = page.max.y + GAP;
    }
    if ruler > 0.0 {
        let (page, ml, mt) = first_page.unwrap_or((area, 0.0, 0.0));
        draw_rulers(painter, rect, area, page, k, (ml, mt));
    }
}

/// Inch rulers: ticks every 1/8 inch from the first page's top-left; the margin band is
/// shaded like the target app's ruler.
fn draw_rulers(painter: &Painter, rect: Rect, area: Rect, page: Rect, k: f32, margin: (f32, f32)) {
    let top = Rect::from_min_max(
        Pos2::new(area.min.x, rect.min.y),
        Pos2::new(rect.max.x, area.min.y),
    );
    let left = Rect::from_min_max(
        Pos2::new(rect.min.x, area.min.y),
        Pos2::new(area.min.x, rect.max.y),
    );
    painter.rect_filled(top, 0.0, P.ruler_bg);
    painter.rect_filled(left, 0.0, P.ruler_bg);
    painter.rect_filled(
        Rect::from_min_max(rect.min, Pos2::new(area.min.x, area.min.y)),
        0.0,
        P.desk,
    );
    let inch = 72.0 * k;
    let eighth = inch / 8.0;
    if eighth < 1.0 {
        return;
    }
    let font = FontId::proportional(9.0);
    // Shade the margins on the rulers.
    painter.rect_filled(
        Rect::from_min_max(
            Pos2::new(page.min.x.max(top.min.x), top.min.y),
            Pos2::new((page.min.x + margin.0).min(top.max.x), top.max.y),
        ),
        0.0,
        P.desk,
    );
    painter.rect_filled(
        Rect::from_min_max(
            Pos2::new(left.min.x, page.min.y.max(left.min.y)),
            Pos2::new(left.max.x, (page.min.y + margin.1).min(left.max.y)),
        ),
        0.0,
        P.desk,
    );
    let tp = painter.with_clip_rect(top);
    let lp = painter.with_clip_rect(left);
    let stroke = Stroke::new(1.0, P.ruler_tick);
    // Horizontal ticks (bounded by the visible width).
    let n = ((top.max.x - page.min.x) / eighth).clamp(0.0, 4000.0) as i32;
    for t in 0..=n {
        let x = page.min.x + t as f32 * eighth;
        let len = if t % 8 == 0 {
            RULER * 0.6
        } else if t % 4 == 0 {
            RULER * 0.4
        } else {
            RULER * 0.2
        };
        tp.vline(x, (top.max.y - len)..=top.max.y, stroke);
        if t % 8 == 0 && t > 0 {
            tp.text(
                Pos2::new(x + 2.0, top.min.y + 1.0),
                Align2::LEFT_TOP,
                (t / 8).to_string(),
                font.clone(),
                P.ruler_tick,
            );
        }
    }
    let n = ((left.max.y - page.min.y) / eighth).clamp(0.0, 4000.0) as i32;
    for t in 0..=n {
        let y = page.min.y + t as f32 * eighth;
        let len = if t % 8 == 0 {
            RULER * 0.6
        } else if t % 4 == 0 {
            RULER * 0.4
        } else {
            RULER * 0.2
        };
        lp.hline((left.max.x - len)..=left.max.x, y, stroke);
        if t % 8 == 0 && t > 0 {
            lp.text(
                Pos2::new(left.min.x + 1.0, y + 2.0),
                Align2::LEFT_TOP,
                (t / 8).to_string(),
                font.clone(),
                P.ruler_tick,
            );
        }
    }
}

fn draw_paper(p: &Painter, page: Rect) {
    p.rect_filled(page.translate(Vec2::new(2.0, 2.0)), 0.0, P.paper_edge);
    p.rect_filled(page, 0.0, P.paper);
    p.rect_stroke(
        page,
        0.0,
        Stroke::new(1.0, P.paper_edge),
        StrokeKind::Outside,
    );
}

fn dashed_rect(p: &Painter, r: Rect, stroke: Stroke, on: f32, off: f32) {
    let pts = [
        r.left_top(),
        r.right_top(),
        r.right_bottom(),
        r.left_bottom(),
        r.left_top(),
    ];
    p.extend(Shape::dashed_line(&pts, stroke, on, off));
}

/// Paint page-scene items (points, top-left origin) at `o`, `k` px per point.
fn draw_items(p: &Painter, o: Pos2, k: f32, items: &[Item]) {
    let at = |x: f32, y: f32| o + Vec2::new(x, y) * k;
    for it in items {
        match it {
            Item::Fill {
                x,
                y,
                w,
                h,
                color: c,
            } => {
                p.rect_filled(
                    Rect::from_min_size(at(*x, *y), Vec2::new(*w, *h) * k),
                    0.0,
                    color(*c),
                );
            }
            Item::Line {
                pts,
                width,
                color: c,
                dash,
                closed,
            } => {
                let mut v: Vec<Pos2> = pts.iter().map(|(x, y)| at(*x, *y)).collect();
                if *closed && let Some(f) = v.first().copied() {
                    v.push(f);
                }
                let stroke = Stroke::new((width * k).max(0.5), color(*c));
                match dash {
                    Some((on, off)) => {
                        p.extend(Shape::dashed_line(
                            &v,
                            stroke,
                            (on * k).max(1.0),
                            (off * k).max(1.0),
                        ));
                    }
                    None => {
                        p.add(Shape::line(v, stroke));
                    }
                }
            }
            Item::Poly { pts, color: c } => {
                let v: Vec<Pos2> = pts.iter().map(|(x, y)| at(*x, *y)).collect();
                let c = color(*c);
                p.add(Shape::convex_polygon(v, c, Stroke::new(0.5, c)));
            }
            Item::Circle { x, y, r, color: c } => {
                p.circle_filled(at(*x, *y), r * k, color(*c));
            }
            Item::Text {
                x,
                y,
                text,
                face,
                size,
                color: c,
                angle,
            } => {
                let px = size * k;
                if px < 1.0 {
                    continue;
                }
                let family = if face.family == Family::Courier {
                    FontFamily::Monospace
                } else {
                    FontFamily::Proportional
                };
                let c = color(*c);
                let galley = p.layout_no_wrap(text.clone(), FontId::new(px, family), c);
                // (x, y) is the baseline start; egui places the galley by its top-left.
                let ascent = galley
                    .rows
                    .first()
                    .map_or(px * 0.8, |r| r.row.size.y * 0.78);
                let rad = -angle.to_radians();
                let base = at(*x, *y);
                // The galley rotates about its top-left; put its baseline start on `base`.
                let pos = base + Vec2::new(ascent * rad.sin(), -ascent * rad.cos());
                let shape = |q: Pos2| TextShape::new(q, galley.clone(), c).with_angle(rad);
                p.add(shape(pos));
                if face.bold {
                    p.add(shape(pos + Vec2::new(0.4, 0.0)));
                }
            }
            Item::Clip { x, y, w, h, items } => {
                let r = Rect::from_min_size(at(*x, *y), Vec2::new(*w, *h) * k);
                draw_items(&p.with_clip_rect(r.intersect(p.clip_rect())), o, k, items);
            }
        }
    }
}

/// Page break preview overlay on the normal grid: veil outside the print area, blue page
/// boundaries (solid at area edges and manual breaks, dashed at automatic breaks) and a
/// "Page N" watermark in each page.
pub fn break_preview(
    painter: &Painter,
    s: &Session,
    view: &GridView,
    cells_origin: Pos2,
    cell_area: Rect,
) {
    if s.view.print.mode != ViewMode::PageBreakPreview || s.view.print.preview {
        return;
    }
    let si = s.sheet_index();
    let Some(sheet) = s.wb.sheet(si) else {
        return;
    };
    let pg = paginate::paginate(&s.wb, si, &PrintOptions::default());
    let zoom = s.view.zoom.clamp(0.1, 4.0);
    let freeze = sheet.freeze.unwrap_or_default();
    let origin = CellAddr::new(
        s.view.scroll.row.max(freeze.row),
        s.view.scroll.col.max(freeze.col),
    );
    let frozen = Vec2::new(
        col_offset_px(sheet, 0, freeze.col),
        row_offset_px(sheet, 0, freeze.row),
    ) * zoom;
    // Visible lines use the laid-out positions; others are measured from the scroll origin.
    let col_x = |c: u32| -> f32 {
        view.cols.iter().find(|x| x.0 == c).map_or_else(
            || cells_origin.x + frozen.x + col_offset_px(sheet, origin.col, c) * zoom,
            |(_, x, _)| cells_origin.x + x,
        )
    };
    let row_y = |r: u32| -> f32 {
        view.rows.iter().find(|x| x.0 == r).map_or_else(
            || cells_origin.y + frozen.y + row_offset_px(sheet, origin.row, r) * zoom,
            |(_, y, _)| cells_origin.y + y,
        )
    };
    let p = painter.with_clip_rect(cell_area.intersect(painter.clip_rect()));
    let screen = |rows: (u32, u32), cols: (u32, u32)| {
        Rect::from_min_max(
            Pos2::new(col_x(cols.0), row_y(rows.0)),
            Pos2::new(
                col_x(cols.1.saturating_add(1)),
                row_y(rows.1.saturating_add(1)),
            ),
        )
    };
    let areas: Vec<Rect> = pg
        .areas
        .iter()
        .map(|a| screen((a.start.row, a.end.row), (a.start.col, a.end.col)))
        .collect();
    veil_outside(&p, cell_area, &areas);
    let bounds = pg.bounds();
    for b in bounds.iter().take(2000) {
        let r = screen(b.rows, b.cols);
        if !r.intersects(cell_area) {
            continue;
        }
        let vis = r.intersect(cell_area);
        let size = (vis.width() / 4.5)
            .min(vis.height() / 3.0)
            .min(120.0 * zoom.max(0.5));
        if size >= 10.0 {
            p.text(
                vis.center(),
                Align2::CENTER_CENTER,
                format!("Page {}", b.number),
                FontId::proportional(size),
                P.watermark,
            );
        }
        let edge = |a: Pos2, z: Pos2, kind: EdgeKind| {
            if kind == EdgeKind::Auto {
                p.extend(Shape::dashed_line(
                    &[a, z],
                    Stroke::new(2.0, P.boundary),
                    6.0,
                    4.0,
                ));
            } else {
                p.line_segment([a, z], Stroke::new(3.0, P.boundary));
            }
        };
        edge(r.left_top(), r.right_top(), b.top);
        edge(r.left_bottom(), r.right_bottom(), b.bottom);
        edge(r.left_top(), r.left_bottom(), b.left);
        edge(r.right_top(), r.right_bottom(), b.right);
    }
}

/// Grey out everything in `area` not covered by one of `holes`.
fn veil_outside(p: &Painter, area: Rect, holes: &[Rect]) {
    for r in veil_rects(area, holes) {
        p.rect_filled(r, 0.0, P.outside);
    }
}

/// The parts of `area` outside every hole, as vertical strips between the holes' x edges.
fn veil_rects(area: Rect, holes: &[Rect]) -> Vec<Rect> {
    let holes: Vec<Rect> = holes
        .iter()
        .map(|h| h.intersect(area))
        .filter(|h| h.is_positive())
        .collect();
    let mut xs: Vec<f32> = vec![area.min.x, area.max.x];
    for h in &holes {
        xs.push(h.min.x);
        xs.push(h.max.x);
    }
    xs.sort_by(f32::total_cmp);
    xs.dedup();
    let mut out = Vec::new();
    for w in xs.windows(2) {
        let (Some(&x0), Some(&x1)) = (w.first(), w.get(1)) else {
            continue;
        };
        if x1 - x0 < 0.5 {
            continue;
        }
        let mid = (x0 + x1) / 2.0;
        let mut spans: Vec<(f32, f32)> = holes
            .iter()
            .filter(|h| h.min.x <= mid && mid <= h.max.x)
            .map(|h| (h.min.y, h.max.y))
            .collect();
        spans.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut y = area.min.y;
        for (a, b) in spans {
            if a > y {
                out.push(Rect::from_min_max(Pos2::new(x0, y), Pos2::new(x1, a)));
            }
            y = y.max(b);
        }
        if y < area.max.y {
            out.push(Rect::from_min_max(
                Pos2::new(x0, y),
                Pos2::new(x1, area.max.y),
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn total(v: &[Rect]) -> f32 {
        v.iter().map(Rect::area).sum()
    }

    #[test]
    fn veil_covers_area_minus_holes() {
        let area = Rect::from_min_size(Pos2::ZERO, Vec2::splat(100.0));
        assert!((total(&veil_rects(area, &[])) - 10_000.0).abs() < 1e-3);
        let hole = Rect::from_min_size(Pos2::new(10.0, 10.0), Vec2::splat(20.0));
        let v = veil_rects(area, &[hole]);
        assert!((total(&v) - 9_600.0).abs() < 1e-3);
        assert!(v.iter().all(|r| !r.intersect(hole).is_positive()));
        // Overlapping and degenerate holes.
        let v = veil_rects(
            area,
            &[hole, hole.translate(Vec2::new(10.0, 10.0)), Rect::NOTHING],
        );
        assert!((total(&v) - (10_000.0 - 700.0)).abs() < 1e-3);
        // A hole bigger than the area leaves nothing.
        assert!(veil_rects(area, &[area.expand(10.0)]).is_empty());
    }
}
