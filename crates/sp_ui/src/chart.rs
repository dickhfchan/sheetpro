//! Charts and sparklines over the grid: resolve data through the engine, lay out with
//! `sp_render::chart`, and map the primitives to the egui painter. Colours come from `theme`.

use crate::theme::{LIGHT as T, color, series_color};
use egui::epaint::TextShape;
use egui::{Color32, FontFamily, FontId, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, Vec2};
use sp_core::CellAddr;
use sp_engine::Session;
use sp_engine::commands::chart_cmds::{chart_data, sparkline_values};
use sp_render::GridView;
use sp_render::chart::{
    HAnchor, Paint, Prim, VAnchor, anchor_rect, chart_scene, col_offset_px, row_offset_px,
    sparkline_scene,
};

fn paint_color(p: Paint) -> Color32 {
    match p {
        Paint::Series(i) => series_color(i),
        Paint::Rgb(c) => color(c),
        Paint::Background => T.chart_bg,
        Paint::Border => T.chart_border,
        Paint::Axis => T.chart_axis,
        Paint::Gridline => T.chart_grid,
        Paint::Text => T.chart_text,
        Paint::SubText => T.chart_subtext,
    }
}

/// Paint primitives laid out in a box whose top-left is `o`.
fn draw_prims(p: &Painter, o: Pos2, prims: &[Prim]) {
    let at = |x: f32, y: f32| o + Vec2::new(x, y);
    for prim in prims {
        match prim {
            Prim::Rect {
                x,
                y,
                w,
                h,
                fill,
                stroke,
            } => {
                let r = Rect::from_min_size(at(*x, *y), Vec2::new(*w, *h));
                if let Some(f) = fill {
                    p.rect_filled(r, 0.0, paint_color(*f));
                }
                if let Some(s) = stroke {
                    p.rect_stroke(
                        r,
                        0.0,
                        Stroke::new(1.0, paint_color(*s)),
                        StrokeKind::Inside,
                    );
                }
            }
            Prim::Line { pts, width, paint } => {
                let pts: Vec<Pos2> = pts.iter().map(|(x, y)| at(*x, *y)).collect();
                p.add(Shape::line(pts, Stroke::new(*width, paint_color(*paint))));
            }
            Prim::Poly { pts, fill } => {
                let pts: Vec<Pos2> = pts.iter().map(|(x, y)| at(*x, *y)).collect();
                // A hairline in the fill colour hides anti-aliasing seams between pieces.
                let c = paint_color(*fill);
                p.add(Shape::convex_polygon(pts, c, Stroke::new(0.6, c)));
            }
            Prim::Circle { x, y, r, fill } => {
                p.circle_filled(at(*x, *y), *r, paint_color(*fill));
            }
            Prim::Text {
                x,
                y,
                text,
                size,
                h,
                v,
                paint,
                vertical,
                bold,
            } => {
                let c = paint_color(*paint);
                let galley = p.layout_no_wrap(
                    text.clone(),
                    FontId::new(size.max(1.0), FontFamily::Proportional),
                    c,
                );
                let sz = galley.size();
                // Box size on screen (swapped when rotated a quarter turn).
                let (bw, bh) = if *vertical {
                    (sz.y, sz.x)
                } else {
                    (sz.x, sz.y)
                };
                let left = match h {
                    HAnchor::Left => *x,
                    HAnchor::Center => x - bw / 2.0,
                    HAnchor::Right => x - bw,
                };
                let top = match v {
                    VAnchor::Top => *y,
                    VAnchor::Center => y - bh / 2.0,
                    VAnchor::Bottom => y - bh,
                };
                let (pos, angle) = if *vertical {
                    // Rotated 90° counter-clockwise: the galley origin is its bottom-left.
                    (at(left, top + bh), -std::f32::consts::FRAC_PI_2)
                } else {
                    (at(left, top), 0.0)
                };
                let shape = |q: Pos2| TextShape::new(q, galley.clone(), c).with_angle(angle);
                p.add(shape(pos));
                if *bold {
                    p.add(shape(pos + Vec2::new(0.5, 0.0)));
                }
            }
        }
    }
}

/// Draw the active sheet's sparklines (in their cells) and charts (at their anchors),
/// clipped to the cell area.
pub fn paint_objects(
    painter: &Painter,
    s: &Session,
    view: &GridView,
    cells_origin: Pos2,
    cell_area: Rect,
) {
    let si = s.sheet_index();
    let Some(sheet) = s.wb.sheet(si) else {
        return;
    };
    if sheet.drawings.is_empty() {
        return;
    }
    let zoom = s.view.zoom.clamp(0.1, 4.0);
    let p = painter.with_clip_rect(cell_area.intersect(painter.clip_rect()));
    // Sparklines: only cells on screen.
    for g in &sheet.drawings.sparklines {
        for sp in &g.items {
            let (Some(col), Some(row)) = (
                view.cols.iter().find(|c| c.0 == sp.location.col),
                view.rows.iter().find(|r| r.0 == sp.location.row),
            ) else {
                continue;
            };
            let r = Rect::from_min_size(
                cells_origin + Vec2::new(col.1, row.1),
                Vec2::new(col.2, row.2),
            )
            .shrink2(Vec2::new(3.0 * zoom, 3.0 * zoom));
            let vals = sparkline_values(&s.wb, si, sp);
            let prims = sparkline_scene(g, &vals, r.width(), r.height());
            draw_prims(
                &p.with_clip_rect(r.expand(2.0).intersect(p.clip_rect())),
                r.min,
                &prims,
            );
        }
    }
    for (i, r) in chart_rects(s, cells_origin) {
        if !r.intersects(cell_area) || r.width() < 2.0 || r.height() < 2.0 {
            continue;
        }
        let Some(c) = sheet.drawings.charts.get(i) else {
            continue;
        };
        let data = chart_data(&s.wb, si, c);
        let prims = chart_scene(&data, r.width(), r.height(), zoom);
        draw_prims(&p, r.min, &prims);
    }
}

/// Top-left cell of the scrollable pane and the frozen panes' size in px (at `zoom`).
fn pane_origin(s: &Session, sheet: &sp_model::Sheet) -> (CellAddr, Vec2) {
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
    (origin, frozen)
}

/// Screen rectangles of the active sheet's charts, in drawing order (index = chart number).
/// Anchors are measured from the top-left of the scrollable pane.
pub fn chart_rects(s: &Session, cells_origin: Pos2) -> Vec<(usize, Rect)> {
    let Some(sheet) = s.wb.sheet(s.sheet_index()) else {
        return Vec::new();
    };
    let zoom = s.view.zoom.clamp(0.1, 4.0);
    let (origin, frozen) = pane_origin(s, sheet);
    sheet
        .drawings
        .charts
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let (x, y, w, h) = anchor_rect(sheet, origin, &c.anchor);
            (
                i,
                Rect::from_min_size(
                    cells_origin + frozen + Vec2::new(x, y) * zoom,
                    Vec2::new(w, h) * zoom,
                ),
            )
        })
        .collect()
}

/// The cell range (as a chart `anchor` param) whose grid lines are nearest to `r`.
pub fn rect_to_anchor(s: &Session, cells_origin: Pos2, r: Rect) -> Option<String> {
    let sheet = s.wb.sheet(s.sheet_index())?;
    let zoom = s.view.zoom.clamp(0.1, 4.0);
    let (origin, frozen) = pane_origin(s, sheet);
    let base = cells_origin + frozen;
    let to_px = |v: f32| f64::from(v / zoom);
    // Nearest grid line at or after a pixel offset (`col_at_px` gives the cell and the
    // offset inside it).
    let snap_col = |px: f64| {
        let (c, off) = sp_render::chart::col_at_px(sheet, origin.col, px);
        let w = f64::from(sp_render::col_width_px(sheet.col_width(c)));
        if off > w / 2.0 {
            c.saturating_add(1)
        } else {
            c
        }
    };
    let snap_row = |px: f64| {
        let (r, off) = sp_render::chart::row_at_px(sheet, origin.row, px);
        let h = f64::from(sp_render::row_height_px(sheet.row_height(r)));
        if off > h / 2.0 {
            r.saturating_add(1)
        } else {
            r
        }
    };
    let c0 = snap_col(to_px(r.min.x - base.x));
    let r0 = snap_row(to_px(r.min.y - base.y));
    let c1 = snap_col(to_px(r.max.x - base.x)).max(c0.saturating_add(1));
    let r1 = snap_row(to_px(r.max.y - base.y)).max(r0.saturating_add(1));
    let max_c = sp_core::MAX_COLS - 1;
    let max_r = sp_core::MAX_ROWS - 1;
    let range = sp_core::CellRange::new(
        CellAddr::new(r0.min(max_r - 1), c0.min(max_c - 1)),
        CellAddr::new(
            r1.saturating_sub(1).min(max_r - 1),
            c1.saturating_sub(1).min(max_c - 1),
        ),
    );
    Some(range.to_a1())
}
