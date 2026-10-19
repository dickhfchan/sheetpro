//! Floating objects over the grid: pictures (decoded once, cached as textures keyed by the
//! image bytes), preset shapes, text boxes and connector lines (geometry and paint from
//! `sp_render::objects`), interleaved with charts in z-order; slicers; cell checkboxes; and
//! the sheet background tiled behind the cells. Drawing only: clicks come back as
//! `overlays::Hot` entries and run commands there.

use crate::grid;
use crate::overlays::Hot;
use crate::theme::{LIGHT as T, color};
use egui::epaint::{Mesh, TextShape, Vertex};
use egui::text::LayoutJob;
use egui::{
    Color32, FontFamily, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, TextureHandle, TextureId,
    TextureOptions, Vec2,
};
use sp_engine::Session;
use sp_engine::commands::pivot_cmds::slicer_state;
use sp_model::VAlign;
use sp_model::objects::ImageData;
use sp_render::objects::{
    MAX_DECODE_PIXELS, ObjPrim, ObjText, ZItem, check_mark, checkbox_rect, decode, downscale,
    object_rect, object_scene, rotate_point, slicer_layout, triangulate, z_order,
};
use sp_render::{Align, GridView};
use std::collections::HashMap;
use std::sync::Arc;

/// Frames an unused texture survives before it is dropped.
const KEEP_FRAMES: u64 = 600;
/// Smallest background tile on screen (px) and most tiles drawn per frame.
const MIN_TILE: f32 = 16.0;
const MAX_TILES: usize = 20_000;
/// Largest texture side uploaded (bigger pictures are downscaled once, when decoded).
const MAX_TEXTURE_SIDE: u32 = 4096;

struct TexEntry {
    /// Keeps the bytes alive so the cache key (their address) can't be reused.
    _bytes: Arc<[u8]>,
    /// `None`: the image could not be decoded (drawn as a placeholder, not retried).
    tex: Option<TextureHandle>,
    last: u64,
}

/// Decoded pictures as egui textures, keyed by image identity (bytes address + length).
#[derive(Default)]
pub struct Textures {
    entries: HashMap<(usize, usize), TexEntry>,
    frame: u64,
    decodes: usize,
}

impl Textures {
    /// The texture for `img` (decoded on first use only).
    pub fn get(&mut self, ctx: &egui::Context, img: &ImageData) -> Option<TextureId> {
        let (ptr, len) = img.key();
        let key = (ptr, len);
        let frame = self.frame;
        if let Some(e) = self.entries.get_mut(&key) {
            e.last = frame;
            return e.tex.as_ref().map(TextureHandle::id);
        }
        self.decodes += 1;
        let max_side = ctx
            .input(|i| i.max_texture_side)
            .min(MAX_TEXTURE_SIDE as usize)
            .max(1) as u32;
        let tex = decode(img, MAX_DECODE_PIXELS).ok().and_then(|d| {
            let d = downscale(&d, max_side);
            let size = [d.width as usize, d.height as usize];
            if size[0] == 0 || size[1] == 0 || size[0] * size[1] * 4 != d.rgba.len() {
                return None;
            }
            let ci = egui::ColorImage::from_rgba_unmultiplied(size, &d.rgba);
            Some(ctx.load_texture(
                format!("sp-image-{ptr:x}-{len}"),
                ci,
                TextureOptions::LINEAR,
            ))
        });
        let id = tex.as_ref().map(TextureHandle::id);
        self.entries.insert(
            key,
            TexEntry {
                _bytes: img.bytes.clone(),
                tex,
                last: frame,
            },
        );
        id
    }

    /// Advance the frame counter and drop textures unused for a while.
    pub fn end_frame(&mut self) {
        let frame = self.frame;
        self.entries
            .retain(|_, e| frame.saturating_sub(e.last) <= KEEP_FRAMES);
        self.frame += 1;
    }

    /// How many images were decoded so far (each decode happens once per image).
    pub fn decodes(&self) -> usize {
        self.decodes
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

fn rgba(c: sp_model::Color, alpha: f32) -> Color32 {
    let a = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color32::from_rgba_unmultiplied(c.0, c.1, c.2, a)
}

fn pos(p: (f32, f32)) -> Pos2 {
    Pos2::new(p.0, p.1)
}

/// Screen rectangles of the active sheet's objects (index = position in `Sheet::objects`),
/// measured like charts from the top-left of the scrollable pane.
pub fn object_rects(s: &Session, cells_origin: Pos2) -> Vec<(usize, Rect)> {
    let Some(sheet) = s.wb.sheet(s.sheet_index()) else {
        return Vec::new();
    };
    let zoom = s.view.zoom.clamp(0.1, 4.0);
    let (origin, frozen) = crate::chart::pane_origin(s, sheet);
    sheet
        .objects
        .iter()
        .enumerate()
        .map(|(i, o)| {
            let (x, y, w, h) = object_rect(sheet, origin, &o.anchor);
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

/// Screen rectangles of the active sheet's slicers.
pub fn slicer_rects(s: &Session, cells_origin: Pos2) -> Vec<(usize, Rect)> {
    let Some(sheet) = s.wb.sheet(s.sheet_index()) else {
        return Vec::new();
    };
    let zoom = s.view.zoom.clamp(0.1, 4.0);
    let (origin, frozen) = crate::chart::pane_origin(s, sheet);
    sheet
        .analysis
        .slicers
        .iter()
        .enumerate()
        .map(|(i, sl)| {
            let (x, y, w, h) = sp_render::chart::anchor_rect(sheet, origin, &sl.anchor);
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

/// The sheet background picture, tiled at its pixel size × zoom from the sheet's top-left
/// (so it scrolls with the cells). Drawn before any cell fill.
pub fn paint_background(
    p: &Painter,
    tex: &mut Textures,
    s: &Session,
    cells_origin: Pos2,
    cell_area: Rect,
) {
    let Some(sheet) = s.wb.sheet(s.sheet_index()) else {
        return;
    };
    let Some(img) = &sheet.background else {
        return;
    };
    let Some(id) = tex.get(p.ctx(), img) else {
        return;
    };
    let zoom = s.view.zoom.clamp(0.1, 4.0);
    // Tiles of at least MIN_TILE px keep the quad count bounded for tiny images.
    let tile = |px: u32| (px.max(1) as f32 * zoom).max(MIN_TILE);
    let (tw, th) = (tile(img.width), tile(img.height));
    let (origin, frozen) = crate::chart::pane_origin(s, sheet);
    // Screen position of A1's top-left (left of / above the visible area when scrolled).
    let ox =
        cells_origin.x + frozen.x - sp_render::chart::col_offset_px(sheet, 0, origin.col) * zoom;
    let oy =
        cells_origin.y + frozen.y - sp_render::chart::row_offset_px(sheet, 0, origin.row) * zoom;
    let r = cell_area;
    // First tile at or before the area's top-left, aligned to the A1 grid of tiles.
    let x0 = ox + ((r.min.x - ox) / tw).floor() * tw;
    let y0 = oy + ((r.min.y - oy) / th).floor() * th;
    if !(x0.is_finite() && y0.is_finite()) {
        return;
    }
    // Explicit quads (one per tile): a single repeating UV rectangle is not honoured by
    // every renderer.
    let mut m = Mesh::with_texture(id);
    let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
    let mut y = y0;
    while y < r.max.y && m.vertices.len() < 4 * MAX_TILES {
        let mut x = x0;
        while x < r.max.x && m.vertices.len() < 4 * MAX_TILES {
            m.add_rect_with_uv(
                Rect::from_min_size(Pos2::new(x, y), Vec2::new(tw, th)),
                uv,
                Color32::WHITE,
            );
            x += tw;
        }
        y += th;
    }
    p.with_clip_rect(cell_area.intersect(p.clip_rect())).add(m);
}

/// Fill a (possibly concave) outline: convex ones as one anti-aliased polygon, others
/// triangulated with a hairline edge in the fill colour.
pub(crate) fn fill_outline(p: &Painter, pts: &[Pos2], c: Color32) {
    if pts.len() < 3 {
        return;
    }
    if is_convex(pts) {
        p.add(Shape::convex_polygon(pts.to_vec(), c, Stroke::NONE));
        return;
    }
    let raw: Vec<(f32, f32)> = pts.iter().map(|q| (q.x, q.y)).collect();
    let mut mesh = Mesh::default();
    for q in pts {
        mesh.colored_vertex(*q, c);
    }
    for [a, b, cc] in triangulate(&raw) {
        mesh.add_triangle(a as u32, b as u32, cc as u32);
    }
    p.add(mesh);
    p.add(Shape::closed_line(pts.to_vec(), Stroke::new(0.6, c)));
}

fn is_convex(pts: &[Pos2]) -> bool {
    let n = pts.len();
    let mut sign = 0.0f32;
    for i in 0..n {
        let (Some(a), Some(b), Some(c)) = (pts.get(i), pts.get((i + 1) % n), pts.get((i + 2) % n))
        else {
            return false;
        };
        let z = (b.x - a.x) * (c.y - b.y) - (b.y - a.y) * (c.x - b.x);
        if z.abs() < 1e-6 {
            continue;
        }
        if sign == 0.0 {
            sign = z.signum();
        } else if z.signum() != sign {
            return false;
        }
    }
    true
}

pub(crate) fn draw_image(p: &Painter, id: TextureId, corners: &[(f32, f32); 4]) {
    let mut m = Mesh::with_texture(id);
    let uvs = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
    for (c, uv) in corners.iter().zip(uvs) {
        m.vertices.push(Vertex {
            pos: pos(*c),
            uv: Pos2::new(uv.0, uv.1),
            color: Color32::WHITE,
        });
    }
    m.add_triangle(0, 1, 2);
    m.add_triangle(0, 2, 3);
    p.add(m);
}

pub(crate) fn draw_missing(p: &Painter, corners: &[(f32, f32); 4]) {
    let pts: Vec<Pos2> = corners.iter().map(|c| pos(*c)).collect();
    p.add(Shape::convex_polygon(
        pts.clone(),
        T.picture_missing_bg,
        Stroke::new(1.0, T.picture_missing_mark),
    ));
    if let [a, b, c, d] = pts.as_slice() {
        let s = Stroke::new(1.0, T.picture_missing_mark);
        p.line_segment([*a, *c], s);
        p.line_segment([*b, *d], s);
    }
}

/// Paint an object's text block (see `ObjText`), rotated with the object.
fn draw_text(p: &Painter, t: &ObjText, zoom: f32, families: &[FontFamily]) {
    let (x, y, w, h) = t.rect;
    let wrap_w = if t.wrap { w.max(1.0) } else { f32::INFINITY };
    let laid: Vec<_> = t
        .paras
        .iter()
        .map(|para| {
            let font = grid::font_id(para.size, zoom, grid::font_family(&para.font, families));
            let c = color(para.color);
            let text = if para.text.is_empty() {
                " ".to_string()
            } else {
                para.text.clone()
            };
            let mut job = LayoutJob::simple(text, font, c, wrap_w);
            job.halign = match para.align {
                Align::Left => egui::Align::Min,
                Align::Center => egui::Align::Center,
                Align::Right => egui::Align::Max,
            };
            (p.layout_job(job), para, c)
        })
        .collect();
    let total: f32 = laid.iter().map(|(g, _, _)| g.size().y).sum();
    let mut top = match t.v_align {
        VAlign::Top => y,
        VAlign::Center => y + (h - total) / 2.0,
        VAlign::Bottom => y + h - total,
    };
    let rad = (t.rotation as f32).to_radians();
    for (g, para, c) in laid {
        let gw = g.rect.width();
        let left = match para.align {
            Align::Left => x,
            Align::Center => x + (w - gw) / 2.0,
            Align::Right => x + w - gw,
        };
        // Galley origin (unrotated), then turned about the object's centre.
        let origin = (left - g.rect.min.x, top - g.rect.min.y);
        let at = pos(rotate_point(origin, t.center, t.rotation));
        let to_screen = |v: Vec2| {
            pos(rotate_point(
                (origin.0 + v.x, origin.1 + v.y),
                t.center,
                t.rotation,
            ))
        };
        let shape = |q: Pos2| TextShape::new(q, g.clone(), c).with_angle(rad);
        p.add(shape(at));
        if para.bold {
            p.add(shape(at + Vec2::angled(rad) * 0.6));
        }
        if para.underline {
            for row in &g.rows {
                let r = row.rect();
                if r.width() > 0.0 {
                    p.line_segment(
                        [
                            to_screen(Vec2::new(r.min.x, r.max.y - 1.0)),
                            to_screen(Vec2::new(r.max.x, r.max.y - 1.0)),
                        ],
                        Stroke::new(1.0, c),
                    );
                }
            }
        }
        top += g.size().y;
    }
}

/// Paint one object in its screen rectangle.
pub fn paint_object(
    p: &Painter,
    tex: &mut Textures,
    o: &sp_model::objects::DrawObject,
    r: Rect,
    zoom: f32,
    families: &[FontFamily],
) {
    let sc = object_scene(o, (r.min.x, r.min.y, r.width(), r.height()), zoom);
    for prim in &sc.prims {
        match prim {
            ObjPrim::Fill { pts, color, alpha } => {
                let v: Vec<Pos2> = pts.iter().map(|q| pos(*q)).collect();
                fill_outline(p, &v, rgba(*color, *alpha));
            }
            ObjPrim::Stroke {
                pts,
                closed,
                color,
                alpha,
                width,
                dash,
            } => {
                let mut v: Vec<Pos2> = pts.iter().map(|q| pos(*q)).collect();
                let stroke = Stroke::new(width.max(0.5), rgba(*color, *alpha));
                match dash {
                    Some((on, off)) => {
                        if *closed && let Some(f) = v.first().copied() {
                            v.push(f);
                        }
                        p.extend(Shape::dashed_line(&v, stroke, on.max(1.0), off.max(1.0)));
                    }
                    None if *closed => {
                        p.add(Shape::closed_line(v, stroke));
                    }
                    None => {
                        p.add(Shape::line(v, stroke));
                    }
                }
            }
            ObjPrim::Image { corners } => {
                let id = o.picture().and_then(|pic| tex.get(p.ctx(), &pic.image));
                match id {
                    Some(id) => draw_image(p, id, corners),
                    None => draw_missing(p, corners),
                }
            }
        }
    }
    if let Some(t) = &sc.text {
        draw_text(p, t, zoom, families);
    }
}

/// Paint the active sheet's charts and objects in z-order and its slicers on top, clipped
/// to the cell area. Returns the slicers' and objects' hit areas (topmost last).
pub(crate) fn paint_drawings(
    painter: &Painter,
    tex: &mut Textures,
    s: &Session,
    view: &GridView,
    cells_origin: Pos2,
    cell_area: Rect,
) -> Vec<Hot> {
    let mut hots = Vec::new();
    let si = s.sheet_index();
    let Some(sheet) = s.wb.sheet(si) else {
        return hots;
    };
    crate::chart::paint_sparklines(painter, s, view, cells_origin, cell_area);
    let zoom = s.view.zoom.clamp(0.1, 4.0);
    let p = painter.with_clip_rect(cell_area.intersect(painter.clip_rect()));
    let families = p.fonts(|f| f.families());
    let charts = if sheet.drawings.charts.is_empty() {
        Vec::new()
    } else {
        crate::chart::chart_rects(s, cells_origin)
    };
    let objs = if sheet.objects.is_empty() {
        Vec::new()
    } else {
        object_rects(s, cells_origin)
    };
    for z in z_order(sheet) {
        match z {
            ZItem::Chart(i) => {
                if let Some((_, r)) = charts.get(i) {
                    crate::chart::paint_chart(&p, s, i, *r, cell_area);
                }
            }
            ZItem::Object(i) => {
                let (Some(o), Some((_, r))) = (sheet.objects.get(i), objs.get(i)) else {
                    continue;
                };
                if o.hidden {
                    continue;
                }
                // Rotation can swing corners outside the box: test the circumscribed square.
                let reach = r.size().length() / 2.0;
                let bound = Rect::from_center_size(r.center(), Vec2::splat(2.0 * reach));
                if !bound.intersects(cell_area) {
                    continue;
                }
                paint_object(&p, tex, o, *r, zoom, &families);
                hots.push(Hot::Object { rect: *r, idx: i });
            }
        }
    }
    paint_slicers(&p, s, cells_origin, &mut hots);
    hots
}

fn paint_slicers(p: &Painter, s: &Session, cells_origin: Pos2, hots: &mut Vec<Hot>) {
    let Some(sheet) = s.wb.sheet(s.sheet_index()) else {
        return;
    };
    if sheet.analysis.slicers.is_empty() {
        return;
    }
    let zoom = s.view.zoom.clamp(0.1, 4.0);
    let font = |pt: f32| grid::font_id(pt, zoom, FontFamily::Proportional);
    for (i, r) in slicer_rects(s, cells_origin) {
        let Some(sl) = sheet.analysis.slicers.get(i) else {
            continue;
        };
        if r.width() < 4.0 || r.height() < 4.0 || !r.intersects(p.clip_rect()) {
            continue;
        }
        let items = sheet
            .analysis
            .pivot(&sl.pivot)
            .map(|pv| slicer_state(pv, sl.field))
            .unwrap_or_default();
        let lay = slicer_layout(
            (r.min.x, r.min.y, r.width(), r.height()),
            items.len(),
            sl.columns,
            zoom,
        );
        let pc = p.with_clip_rect(r.intersect(p.clip_rect()));
        pc.rect_filled(r, 2.0 * zoom, T.slicer_bg);
        pc.rect_stroke(
            r,
            2.0 * zoom,
            Stroke::new(1.0, T.slicer_border),
            StrokeKind::Inside,
        );
        hots.push(Hot::Slicer {
            rect: r,
            slicer: i,
            item: None,
        });
        // Caption.
        let (hx, hy, _, hh) = lay.header;
        let caption = if sl.caption.is_empty() {
            &sl.name
        } else {
            &sl.caption
        };
        let clear_r = Rect::from_min_size(
            Pos2::new(lay.clear.0, lay.clear.1),
            Vec2::new(lay.clear.2, lay.clear.3),
        );
        pc.with_clip_rect(
            Rect::from_min_max(Pos2::new(hx, hy), Pos2::new(clear_r.min.x - 2.0, hy + hh))
                .intersect(pc.clip_rect()),
        )
        .text(
            Pos2::new(hx + 8.0 * zoom, hy + hh / 2.0),
            egui::Align2::LEFT_CENTER,
            caption,
            font(11.0),
            T.slicer_caption,
        );
        // Clear-filter button: a funnel with a cross, live when something is filtered out.
        let filtered = items.iter().any(|it| !it.selected);
        draw_clear_icon(&pc, clear_r, filtered);
        if filtered {
            hots.push(Hot::SlicerClear {
                rect: clear_r,
                slicer: i,
            });
        }
        for (it, b) in items.iter().zip(&lay.buttons) {
            let br = Rect::from_min_size(Pos2::new(b.0, b.1), Vec2::new(b.2, b.3));
            let (fill, border) = if it.selected && it.has_data {
                (T.slicer_selected_bg, T.slicer_selected_border)
            } else {
                (T.slicer_item_bg, T.slicer_item_border)
            };
            pc.rect_filled(br, 2.0 * zoom, fill);
            pc.rect_stroke(br, 2.0 * zoom, Stroke::new(1.0, border), StrokeKind::Inside);
            let tc = if it.has_data {
                T.slicer_item_text
            } else {
                T.slicer_nodata_text
            };
            pc.with_clip_rect(br.shrink(2.0).intersect(pc.clip_rect()))
                .text(
                    Pos2::new(br.min.x + 6.0 * zoom, br.center().y),
                    egui::Align2::LEFT_CENTER,
                    &it.label,
                    font(10.0),
                    tc,
                );
            hots.push(Hot::Slicer {
                rect: br,
                slicer: i,
                item: Some(it.label.clone()),
            });
        }
    }
}

fn draw_clear_icon(p: &Painter, r: Rect, live: bool) {
    let c = if live {
        T.slicer_clear
    } else {
        T.slicer_clear_disabled
    };
    let s = r.width().min(r.height());
    let o = r.min;
    let at = |x: f32, y: f32| o + Vec2::new(x * s, y * s);
    let funnel = vec![
        at(0.1, 0.2),
        at(0.65, 0.2),
        at(0.45, 0.5),
        at(0.45, 0.85),
        at(0.3, 0.75),
        at(0.3, 0.5),
    ];
    p.add(Shape::closed_line(
        funnel,
        Stroke::new(1.0, T.slicer_caption),
    ));
    let st = Stroke::new(1.3, c);
    p.line_segment([at(0.6, 0.55), at(0.9, 0.85)], st);
    p.line_segment([at(0.9, 0.55), at(0.6, 0.85)], st);
}

/// Draw a cell checkbox in `cell` and return the clickable square.
pub fn paint_checkbox(
    p: &Painter,
    cell: Rect,
    checked: bool,
    font_color: Option<sp_model::Color>,
    zoom: f32,
    align: sp_model::HAlign,
) -> Rect {
    let (x, y, w, h) = checkbox_rect(
        (cell.min.x, cell.min.y, cell.width(), cell.height()),
        zoom,
        align,
    );
    let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, h));
    let c = font_color.map_or(T.checkbox, color);
    p.rect_stroke(r, 1.5 * zoom, Stroke::new(1.0, c), StrokeKind::Inside);
    if checked {
        let pts: Vec<Pos2> = check_mark((x, y, w, h)).into_iter().map(pos).collect();
        p.add(Shape::line(pts, Stroke::new((1.6 * zoom).max(1.0), c)));
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convexity() {
        let sq = [
            Pos2::new(0.0, 0.0),
            Pos2::new(1.0, 0.0),
            Pos2::new(1.0, 1.0),
            Pos2::new(0.0, 1.0),
        ];
        assert!(is_convex(&sq));
        let dart = [
            Pos2::new(0.0, 0.0),
            Pos2::new(2.0, 1.0),
            Pos2::new(0.0, 2.0),
            Pos2::new(0.5, 1.0),
        ];
        assert!(!is_convex(&dart));
    }
}
