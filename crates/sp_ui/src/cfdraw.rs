//! Conditional-format decorations: in-cell data bars and icon-set icons, drawn as simple
//! vector shapes (SheetPro's own designs). Icon colours are the tokens below.

use egui::epaint::{Mesh, PathShape, PathStroke};
use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, Vec2};
use sp_render::DisplayCell;
use sp_render::condfmt::IconDraw;

/// Icon palette tokens.
pub const ICON_RED: Color32 = Color32::from_rgb(214, 62, 52);
pub const ICON_AMBER: Color32 = Color32::from_rgb(240, 180, 30);
pub const ICON_GREEN: Color32 = Color32::from_rgb(64, 160, 78);
pub const ICON_GRAY: Color32 = Color32::from_rgb(128, 128, 128);
pub const ICON_DARK: Color32 = Color32::from_rgb(40, 40, 40);
pub const ICON_BLUE: Color32 = Color32::from_rgb(60, 110, 190);
pub const ICON_LIGHT: Color32 = Color32::from_rgb(225, 225, 225);
pub const ICON_INK: Color32 = Color32::WHITE;

fn rgb(c: sp_model::Color) -> Color32 {
    Color32::from_rgb(c.0, c.1, c.2)
}

/// Draw the data bar (behind text) and icon of a cell, if it has them.
pub fn draw(p: &Painter, rect: Rect, d: &DisplayCell, zoom: f32) {
    let pc = p.with_clip_rect(rect);
    if let Some(bar) = d.data_bar {
        let inner = rect.shrink2(Vec2::new(2.0, 2.0 * zoom.max(0.5)));
        let w = inner.width() * bar.frac.clamp(0.0, 1.0);
        let r = Rect::from_min_size(inner.min, Vec2::new(w.max(0.0), inner.height()));
        let c = rgb(bar.color);
        if bar.gradient {
            let fade = c.lerp_to_gamma(Color32::WHITE, 0.85);
            let mut m = Mesh::default();
            m.colored_vertex(r.left_top(), c);
            m.colored_vertex(r.right_top(), fade);
            m.colored_vertex(r.right_bottom(), fade);
            m.colored_vertex(r.left_bottom(), c);
            m.add_triangle(0, 1, 2);
            m.add_triangle(0, 2, 3);
            pc.add(Shape::mesh(m));
            pc.rect_stroke(r, 0.0, Stroke::new(1.0, c), egui::StrokeKind::Inside);
        } else {
            pc.rect_filled(r, 0.0, c);
        }
    }
    if let Some(icon) = &d.icon {
        let s = (rect.height() - 4.0).clamp(4.0, 16.0 * zoom.max(0.5));
        let at = Rect::from_min_size(
            Pos2::new(rect.min.x + 2.0, rect.center().y - s / 2.0),
            Vec2::splat(s),
        );
        draw_icon(&pc, at, icon);
    }
}

/// Bucket colour on a red → amber → green ramp for `index` of `count`.
fn ramp(index: u8, count: u8) -> Color32 {
    let top = count.saturating_sub(1).max(1);
    match (u32::from(index) * 2).cmp(&u32::from(top)) {
        std::cmp::Ordering::Less => ICON_RED,
        std::cmp::Ordering::Equal => ICON_AMBER,
        std::cmp::Ordering::Greater => ICON_GREEN,
    }
}

fn poly(p: &Painter, pts: Vec<Pos2>, fill: Color32) {
    p.add(Shape::Path(PathShape::convex_polygon(
        pts,
        fill,
        PathStroke::NONE,
    )));
}

/// Arrow pointing at `angle` (radians, 0 = right, -π/2 = up) inside `r`.
fn arrow(p: &Painter, r: Rect, angle: f32, c: Color32) {
    let ctr = r.center();
    let h = r.width() * 0.45;
    let rot = |x: f32, y: f32| {
        let (s, co) = angle.sin_cos();
        ctr + Vec2::new(x * co - y * s, x * s + y * co)
    };
    // Shaft and head as two convex parts.
    poly(
        p,
        vec![
            rot(-h, -h * 0.22),
            rot(0.0, -h * 0.22),
            rot(0.0, h * 0.22),
            rot(-h, h * 0.22),
        ],
        c,
    );
    poly(
        p,
        vec![
            rot(h, 0.0),
            rot(-0.05 * h, -h * 0.75),
            rot(-0.05 * h, h * 0.75),
        ],
        c,
    );
}

fn star(p: &Painter, r: Rect, fill: f32, c: Color32) {
    let ctr = r.center();
    let ro = r.width() * 0.5;
    let ri = ro * 0.45;
    let pts: Vec<Pos2> = (0..10)
        .map(|i| {
            let a = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 5.0;
            let rr = if i % 2 == 0 { ro } else { ri };
            ctr + Vec2::new(a.cos() * rr, a.sin() * rr)
        })
        .collect();
    // Concave outline: fan triangles from the centre.
    let mut m = Mesh::default();
    m.colored_vertex(ctr, c);
    let base = if fill >= 0.99 { c } else { ICON_LIGHT };
    for q in &pts {
        let col = if fill > 0.0 && q.x <= r.min.x + r.width() * fill {
            c
        } else {
            base
        };
        m.colored_vertex(*q, col);
    }
    for i in 0..10u32 {
        m.add_triangle(0, 1 + i, 1 + (i + 1) % 10);
    }
    p.add(Shape::mesh(m));
}

/// One icon of a set.
pub fn draw_icon(p: &Painter, r: Rect, icon: &IconDraw) {
    let set = icon.set.to_ascii_lowercase();
    let (i, n) = (
        icon.index.min(icon.count.saturating_sub(1)),
        icon.count.max(2),
    );
    let top = n - 1;
    let frac = f32::from(i) / f32::from(top);
    let ctr = r.center();
    let rad = r.width() * 0.45;
    let gray = set.contains("gray");
    let c = if gray { ICON_GRAY } else { ramp(i, n) };
    if set.contains("arrows") {
        // Lowest points down, highest up; intermediate buckets slant.
        let a = std::f32::consts::FRAC_PI_2 - frac * std::f32::consts::PI;
        arrow(p, r, a, c);
    } else if set.contains("triangles") {
        if i == top {
            poly(
                p,
                vec![
                    ctr + Vec2::new(0.0, -rad),
                    ctr + Vec2::new(rad, rad * 0.6),
                    ctr + Vec2::new(-rad, rad * 0.6),
                ],
                ICON_GREEN,
            );
        } else if i == 0 {
            poly(
                p,
                vec![
                    ctr + Vec2::new(-rad, -rad * 0.6),
                    ctr + Vec2::new(rad, -rad * 0.6),
                    ctr + Vec2::new(0.0, rad),
                ],
                ICON_RED,
            );
        } else {
            p.rect_filled(
                Rect::from_center_size(ctr, Vec2::new(rad * 1.6, rad * 0.5)),
                0.0,
                ICON_AMBER,
            );
        }
    } else if set.contains("flags") {
        let pole = Rect::from_min_size(
            r.min + Vec2::new(r.width() * 0.2, 1.0),
            Vec2::new(1.5, r.height() - 2.0),
        );
        p.rect_filled(pole, 0.0, ICON_DARK);
        poly(
            p,
            vec![
                Pos2::new(pole.max.x, pole.min.y),
                Pos2::new(r.max.x - 1.0, pole.min.y + r.height() * 0.22),
                Pos2::new(pole.max.x, pole.min.y + r.height() * 0.45),
            ],
            c,
        );
    } else if set.contains("symbols") {
        p.circle_filled(ctr, rad, c);
        let s = Stroke::new((rad * 0.28).max(1.0), ICON_INK);
        let k = rad * 0.45;
        if i == top {
            p.line_segment(
                [ctr + Vec2::new(-k, 0.0), ctr + Vec2::new(-k * 0.2, k * 0.8)],
                s,
            );
            p.line_segment(
                [
                    ctr + Vec2::new(-k * 0.2, k * 0.8),
                    ctr + Vec2::new(k, -k * 0.8),
                ],
                s,
            );
        } else if i == 0 {
            p.line_segment([ctr + Vec2::new(-k, -k), ctr + Vec2::new(k, k)], s);
            p.line_segment([ctr + Vec2::new(-k, k), ctr + Vec2::new(k, -k)], s);
        } else {
            p.line_segment([ctr + Vec2::new(0.0, -k), ctr + Vec2::new(0.0, k * 0.3)], s);
            p.circle_filled(ctr + Vec2::new(0.0, k * 0.85), s.width * 0.5, ICON_INK);
        }
    } else if set.contains("signs") {
        match i {
            0 => poly(
                p,
                vec![
                    ctr + Vec2::new(0.0, -rad),
                    ctr + Vec2::new(rad, 0.0),
                    ctr + Vec2::new(0.0, rad),
                    ctr + Vec2::new(-rad, 0.0),
                ],
                c,
            ),
            x if x == top => {
                p.circle_filled(ctr, rad, c);
            }
            _ => poly(
                p,
                vec![
                    ctr + Vec2::new(0.0, -rad),
                    ctr + Vec2::new(rad, rad * 0.8),
                    ctr + Vec2::new(-rad, rad * 0.8),
                ],
                c,
            ),
        }
    } else if set.contains("rating") {
        let bars = u32::from(top).max(1);
        let w = r.width() / (bars as f32 * 1.5);
        for b in 0..bars {
            let h = r.height() * (0.3 + 0.7 * (b + 1) as f32 / bars as f32);
            let x = r.min.x + b as f32 * w * 1.5;
            let br = Rect::from_min_max(Pos2::new(x, r.max.y - h), Pos2::new(x + w, r.max.y));
            p.rect_filled(
                br,
                0.0,
                if b < u32::from(i) {
                    ICON_BLUE
                } else {
                    ICON_LIGHT
                },
            );
        }
    } else if set.contains("quarters") {
        p.circle_filled(ctr, rad, ICON_INK);
        p.circle_stroke(ctr, rad, Stroke::new(1.0, ICON_DARK));
        if frac > 0.0 {
            let steps = 24;
            let mut pts = vec![ctr];
            for s in 0..=steps {
                let a = -std::f32::consts::FRAC_PI_2
                    + frac * std::f32::consts::TAU * s as f32 / steps as f32;
                pts.push(ctr + Vec2::new(a.cos() * rad, a.sin() * rad));
            }
            let mut m = Mesh::default();
            for q in &pts {
                m.colored_vertex(*q, ICON_DARK);
            }
            for k in 1..pts.len().saturating_sub(1) {
                let k = u32::try_from(k).unwrap_or(0);
                m.add_triangle(0, k, k + 1);
            }
            p.add(Shape::mesh(m));
        }
    } else if set.contains("stars") {
        star(p, r, frac, ICON_AMBER);
    } else if set.contains("boxes") {
        let cell = r.width() / 2.0;
        for k in 0..4u8 {
            let x = r.min.x + f32::from(k % 2) * cell;
            let y = r.min.y + f32::from(k / 2) * cell;
            let on = k < i;
            p.rect_filled(
                Rect::from_min_size(Pos2::new(x, y), Vec2::splat(cell - 1.0)),
                0.0,
                if on { ICON_BLUE } else { ICON_LIGHT },
            );
        }
    } else if set.contains("redtoblack") {
        let c = ICON_DARK.lerp_to_gamma(ICON_RED, frac);
        p.circle_filled(ctr, rad, c);
    } else {
        // Traffic lights and anything unknown: a coloured disc.
        let c = if n == 4 && i == 0 { ICON_DARK } else { c };
        p.circle_filled(ctr, rad, c);
        p.circle_stroke(ctr, rad, Stroke::new(0.5, ICON_DARK));
    }
}
