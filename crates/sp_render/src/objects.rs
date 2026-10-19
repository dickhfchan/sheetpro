//! Geometry for pictures, shapes and text boxes: object rectangles from anchors, preset
//! shape outlines (own math, sized to the object box), rotation/flip, triangulation for
//! filling concave outlines, arrow heads, resolved fill/line/text paint, and bounded image
//! decoding. No UI library here: the shell and the page scene draw what this returns.

use crate::Align;
use crate::chart::{col_at_px, col_offset_px, row_at_px, row_offset_px};
use sp_core::CellAddr;
use sp_model::chart::{Anchor, EMU_PER_PX};
use sp_model::objects::{
    AnchorKind, DrawObject, Fill, ImageData, ImageFormat, LineFormat, ObjColor, ObjectAnchor,
    ObjectKind, Placement, RunFont, Shape, TextBody, resolve_color,
};
use sp_model::{Color, Sheet, VAlign};
use std::sync::Arc;

/// Most pixels decoded from one image (larger images are refused, not decoded).
pub const MAX_DECODE_PIXELS: u64 = 40_000_000;
/// Largest width or height accepted.
pub const MAX_IMAGE_SIDE: u32 = 30_000;

// ---------------------------------------------------------------------------------------
// Anchors

/// The object's rectangle in px at 100% zoom relative to the top-left of `origin`.
pub fn object_rect(sheet: &Sheet, origin: CellAddr, a: &ObjectAnchor) -> (f32, f32, f32, f32) {
    let clamp = |v: f64| (v.clamp(0.0, 1e7)) as f32;
    match a.kind {
        AnchorKind::TwoCell => crate::chart::anchor_rect(sheet, origin, &a.cells),
        AnchorKind::OneCell => {
            let x = col_offset_px(sheet, origin.col, a.cells.from.col) + a.cells.from_off.0 as f32;
            let y = row_offset_px(sheet, origin.row, a.cells.from.row) + a.cells.from_off.1 as f32;
            (x, y, clamp(a.ext.0), clamp(a.ext.1))
        }
        AnchorKind::Absolute => {
            let x = col_offset_px(sheet, origin.col, 0) + clamp(a.pos.0);
            let y = row_offset_px(sheet, origin.row, 0) + clamp(a.pos.1);
            (x, y, clamp(a.ext.0), clamp(a.ext.1))
        }
    }
}

/// An anchor of `kind`/`placement` covering the rectangle `(x, y, w, h)` px measured from
/// the sheet's top-left (A1) at 100% zoom.
pub fn anchor_from_rect(
    sheet: &Sheet,
    kind: AnchorKind,
    placement: Placement,
    (x, y, w, h): (f64, f64, f64, f64),
) -> ObjectAnchor {
    let fin = |v: f64| {
        if v.is_finite() {
            v.clamp(0.0, 1e8)
        } else {
            0.0
        }
    };
    let (x, y, w, h) = (fin(x), fin(y), fin(w), fin(h));
    let (fc, fx) = col_at_px(sheet, 0, x);
    let (fr, fy) = row_at_px(sheet, 0, y);
    let (tc, tx) = col_at_px(sheet, fc, fx + w);
    let (tr, ty) = row_at_px(sheet, fr, fy + h);
    ObjectAnchor {
        kind,
        placement,
        cells: Anchor {
            from: CellAddr::new(fr, fc),
            from_off: (fx, fy),
            to: CellAddr::new(tr, tc),
            to_off: (tx, ty),
        },
        ext: (w, h),
        pos: (x, y),
    }
}

/// The object's rectangle measured from A1 (px, 100% zoom), as f64.
pub fn object_rect_abs(sheet: &Sheet, a: &ObjectAnchor) -> (f64, f64, f64, f64) {
    let (x, y, w, h) = object_rect(sheet, CellAddr::default(), a);
    (f64::from(x), f64::from(y), f64::from(w), f64::from(h))
}

// ---------------------------------------------------------------------------------------
// Preset geometry

/// One outline of a shape, in px inside the object box.
#[derive(Debug, Clone, PartialEq)]
pub struct Path {
    pub pts: Vec<(f32, f32)>,
    pub closed: bool,
    /// Filled with the shape fill (open paths and arrow heads use the line colour).
    pub filled: bool,
}

/// Preset names `insert.shapes` offers (others in files draw as far as known).
pub const PRESETS: &[(&str, &str)] = &[
    ("rect", "Rectangle"),
    ("roundRect", "Rounded Rectangle"),
    ("ellipse", "Oval"),
    ("triangle", "Isosceles Triangle"),
    ("rtTriangle", "Right Triangle"),
    ("diamond", "Diamond"),
    ("pentagon", "Pentagon"),
    ("hexagon", "Hexagon"),
    ("octagon", "Octagon"),
    ("parallelogram", "Parallelogram"),
    ("trapezoid", "Trapezoid"),
    ("plus", "Cross"),
    ("homePlate", "Arrow: Pentagon"),
    ("chevron", "Arrow: Chevron"),
    ("rightArrow", "Arrow: Right"),
    ("leftArrow", "Arrow: Left"),
    ("upArrow", "Arrow: Up"),
    ("downArrow", "Arrow: Down"),
    ("leftRightArrow", "Arrow: Left-Right"),
    ("wedgeRectCallout", "Speech Bubble: Rectangle"),
    ("star4", "Star: 4 Points"),
    ("star5", "Star: 5 Points"),
    ("line", "Line"),
    ("straightConnector1", "Line Arrow"),
];

/// Whether `insert.shapes` knows this preset.
pub fn known_preset(name: &str) -> bool {
    PRESETS.iter().any(|(p, _)| *p == name)
}

/// Display name of a preset (for default object names).
pub fn preset_label(name: &str) -> &'static str {
    PRESETS
        .iter()
        .find(|(p, _)| *p == name)
        .map_or("Shape", |(_, l)| l)
}

/// Line presets: open paths, no fill.
pub fn is_line_preset(name: &str) -> bool {
    matches!(
        name,
        "line" | "straightConnector1" | "bentConnector3" | "curvedConnector3"
    )
}

fn adj(shape_adj: &[(String, i64)], name: &str, default: i64, lo: i64, hi: i64) -> f32 {
    let v = shape_adj
        .iter()
        .find(|(n, _)| n == name)
        .map_or(default, |(_, v)| *v);
    v.clamp(lo, hi) as f32 / 100_000.0
}

fn arc(out: &mut Vec<(f32, f32)>, c: (f32, f32), r: (f32, f32), a0: f32, a1: f32, n: usize) {
    for i in 0..=n {
        let t = a0 + (a1 - a0) * i as f32 / n as f32;
        out.push((c.0 + r.0 * t.cos(), c.1 + r.1 * t.sin()));
    }
}

fn regular(w: f32, h: f32, n: usize, start: f32) -> Vec<(f32, f32)> {
    (0..n)
        .map(|i| {
            let t = start + std::f32::consts::TAU * i as f32 / n as f32;
            (w / 2.0 + w / 2.0 * t.cos(), h / 2.0 + h / 2.0 * t.sin())
        })
        .collect()
}

fn star(w: f32, h: f32, n: usize, inner: f32) -> Vec<(f32, f32)> {
    let start = -std::f32::consts::FRAC_PI_2;
    (0..2 * n)
        .map(|i| {
            let t = start + std::f32::consts::PI * i as f32 / n as f32;
            let r = if i % 2 == 0 { 1.0 } else { inner };
            (
                w / 2.0 + w / 2.0 * r * t.cos(),
                h / 2.0 + h / 2.0 * r * t.sin(),
            )
        })
        .collect()
}

/// A right-pointing block arrow in a `w × h` box.
fn right_arrow(w: f32, h: f32, shaft: f32, head: f32) -> Vec<(f32, f32)> {
    let ss = h.min(w);
    let hx = (w - ss * head).clamp(0.0, w);
    let t = h * shaft / 2.0;
    let (cy, y0, y1) = (h / 2.0, h / 2.0 - t, h / 2.0 + t);
    vec![
        (0.0, y0),
        (hx, y0),
        (hx, 0.0),
        (w, cy),
        (hx, h),
        (hx, y1),
        (0.0, y1),
    ]
}

fn swap(pts: Vec<(f32, f32)>) -> Vec<(f32, f32)> {
    pts.into_iter().map(|(x, y)| (y, x)).collect()
}

/// Outline(s) of a preset shape in a `w × h` box (unknown presets draw as a rectangle).
pub fn preset_paths(preset: &str, adjust: &[(String, i64)], w: f32, h: f32) -> Vec<Path> {
    let w = if w.is_finite() {
        w.clamp(0.0, 1e6)
    } else {
        0.0
    };
    let h = if h.is_finite() {
        h.clamp(0.0, 1e6)
    } else {
        0.0
    };
    let closed = |pts: Vec<(f32, f32)>| Path {
        pts,
        closed: true,
        filled: true,
    };
    let ss = w.min(h);
    let pts = match preset {
        "line" | "straightConnector1" | "bentConnector3" | "curvedConnector3" => {
            return vec![Path {
                pts: vec![(0.0, 0.0), (w, h)],
                closed: false,
                filled: false,
            }];
        }
        "roundRect" => {
            let r = ss * adj(adjust, "adj", 16_667, 0, 50_000);
            let mut v = Vec::new();
            use std::f32::consts::{FRAC_PI_2, PI};
            arc(&mut v, (w - r, r), (r, r), -FRAC_PI_2, 0.0, 6);
            arc(&mut v, (w - r, h - r), (r, r), 0.0, FRAC_PI_2, 6);
            arc(&mut v, (r, h - r), (r, r), FRAC_PI_2, PI, 6);
            arc(&mut v, (r, r), (r, r), PI, PI + FRAC_PI_2, 6);
            v
        }
        "ellipse" => regular(w, h, 64, 0.0),
        "triangle" => {
            let x = w * adj(adjust, "adj", 50_000, 0, 100_000);
            vec![(x, 0.0), (w, h), (0.0, h)]
        }
        "rtTriangle" => vec![(0.0, 0.0), (w, h), (0.0, h)],
        "diamond" => vec![(w / 2.0, 0.0), (w, h / 2.0), (w / 2.0, h), (0.0, h / 2.0)],
        "pentagon" => {
            // Regular pentagon stretched to the box, apex up.
            let v = regular(1.0, 1.0, 5, -std::f32::consts::FRAC_PI_2);
            let (miny, maxy) = v
                .iter()
                .fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p.1), b.max(p.1)));
            let span = (maxy - miny).max(1e-6);
            v.into_iter()
                .map(|(x, y)| (x * w, (y - miny) / span * h))
                .collect()
        }
        "hexagon" => {
            let d = ss * adj(adjust, "adj", 25_000, 0, 50_000);
            vec![
                (d, 0.0),
                (w - d, 0.0),
                (w, h / 2.0),
                (w - d, h),
                (d, h),
                (0.0, h / 2.0),
            ]
        }
        "octagon" => {
            let d = ss * adj(adjust, "adj", 29_289, 0, 50_000);
            vec![
                (d, 0.0),
                (w - d, 0.0),
                (w, d),
                (w, h - d),
                (w - d, h),
                (d, h),
                (0.0, h - d),
                (0.0, d),
            ]
        }
        "parallelogram" => {
            let d = ss * adj(adjust, "adj", 25_000, 0, 100_000).min(w / ss.max(1e-6));
            vec![(d, 0.0), (w, 0.0), (w - d, h), (0.0, h)]
        }
        "trapezoid" => {
            let d = (ss * adj(adjust, "adj", 25_000, 0, 50_000)).min(w / 2.0);
            vec![(d, 0.0), (w - d, 0.0), (w, h), (0.0, h)]
        }
        "plus" => {
            let d = ss * adj(adjust, "adj", 25_000, 0, 50_000);
            vec![
                (d, 0.0),
                (w - d, 0.0),
                (w - d, d),
                (w, d),
                (w, h - d),
                (w - d, h - d),
                (w - d, h),
                (d, h),
                (d, h - d),
                (0.0, h - d),
                (0.0, d),
                (d, d),
            ]
        }
        "homePlate" => {
            let d = (ss * adj(adjust, "adj", 50_000, 0, 100_000)).min(w);
            vec![(0.0, 0.0), (w - d, 0.0), (w, h / 2.0), (w - d, h), (0.0, h)]
        }
        "chevron" => {
            let d = (ss * adj(adjust, "adj", 50_000, 0, 100_000)).min(w / 2.0);
            vec![
                (0.0, 0.0),
                (w - d, 0.0),
                (w, h / 2.0),
                (w - d, h),
                (0.0, h),
                (d, h / 2.0),
            ]
        }
        "rightArrow" => right_arrow(
            w,
            h,
            adj(adjust, "adj1", 50_000, 0, 100_000),
            adj(adjust, "adj2", 50_000, 0, 100_000),
        ),
        "leftArrow" => right_arrow(
            w,
            h,
            adj(adjust, "adj1", 50_000, 0, 100_000),
            adj(adjust, "adj2", 50_000, 0, 100_000),
        )
        .into_iter()
        .map(|(x, y)| (w - x, y))
        .collect(),
        "downArrow" => swap(right_arrow(
            h,
            w,
            adj(adjust, "adj1", 50_000, 0, 100_000),
            adj(adjust, "adj2", 50_000, 0, 100_000),
        )),
        "upArrow" => swap(right_arrow(
            h,
            w,
            adj(adjust, "adj1", 50_000, 0, 100_000),
            adj(adjust, "adj2", 50_000, 0, 100_000),
        ))
        .into_iter()
        .map(|(x, y)| (x, h - y))
        .collect(),
        "leftRightArrow" => {
            let t = h * adj(adjust, "adj1", 50_000, 0, 100_000) / 2.0;
            let d = (ss * adj(adjust, "adj2", 50_000, 0, 100_000)).min(w / 2.0);
            let (y0, y1) = (h / 2.0 - t, h / 2.0 + t);
            vec![
                (0.0, h / 2.0),
                (d, 0.0),
                (d, y0),
                (w - d, y0),
                (w - d, 0.0),
                (w, h / 2.0),
                (w - d, h),
                (w - d, y1),
                (d, y1),
                (d, h),
            ]
        }
        "wedgeRectCallout" | "wedgeRoundRectCallout" | "wedgeEllipseCallout" => {
            let tx = w / 2.0 + w * adj(adjust, "adj1", -20_833, -1_000_000, 1_000_000);
            let ty = h / 2.0 + h * adj(adjust, "adj2", 62_500, -1_000_000, 1_000_000);
            callout(w, h, (tx, ty))
        }
        "star4" => star(w, h, 4, 2.0 * adj(adjust, "adj", 12_500, 0, 50_000)),
        "star5" => star(w, h, 5, 2.0 * adj(adjust, "adj", 19_098, 0, 50_000)),
        "star6" => star(w, h, 6, 2.0 * adj(adjust, "adj", 28_868, 0, 50_000)),
        _ => vec![(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)],
    };
    vec![closed(pts)]
}

/// Rectangle with a wedge from the side nearest `tip` out to `tip`.
fn callout(w: f32, h: f32, tip: (f32, f32)) -> Vec<(f32, f32)> {
    let (dx, dy) = (
        (tip.0 - w / 2.0) / w.max(1e-6),
        (tip.1 - h / 2.0) / h.max(1e-6),
    );
    let inside = dx.abs() <= 0.5 && dy.abs() <= 0.5;
    let corners = [(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)];
    if inside {
        return corners.to_vec();
    }
    // Side index: 0 top, 1 right, 2 bottom, 3 left (edge from corner i to i+1).
    let side = if dy.abs() >= dx.abs() {
        if dy < 0.0 { 0 } else { 2 }
    } else if dx > 0.0 {
        1
    } else {
        3
    };
    let along = |len: f32, pos: f32| {
        let half = len / 12.0;
        let c = pos.clamp(half * 2.0, (len - half * 2.0).max(half * 2.0));
        (c - half, c + half)
    };
    let mut out = Vec::new();
    for (i, c) in corners.iter().enumerate() {
        out.push(*c);
        if i != side {
            continue;
        }
        match side {
            0 => {
                let (a, b) = along(w, tip.0);
                out.extend([(a, 0.0), tip, (b, 0.0)]);
            }
            1 => {
                let (a, b) = along(h, tip.1);
                out.extend([(w, a), tip, (w, b)]);
            }
            2 => {
                let (a, b) = along(w, tip.0);
                out.extend([(b, h), tip, (a, h)]);
            }
            _ => {
                let (a, b) = along(h, tip.1);
                out.extend([(0.0, b), tip, (0.0, a)]);
            }
        }
    }
    out
}

/// Flip and rotate (clockwise degrees) points of a `w × h` box about its centre, then
/// offset by `(ox, oy)`.
pub fn transform(
    pts: &[(f32, f32)],
    (w, h): (f32, f32),
    rot_deg: f64,
    flip_h: bool,
    flip_v: bool,
    (ox, oy): (f32, f32),
) -> Vec<(f32, f32)> {
    let r = if rot_deg.is_finite() {
        (rot_deg % 360.0).to_radians() as f32
    } else {
        0.0
    };
    let (s, c) = r.sin_cos();
    let (cx, cy) = (w / 2.0, h / 2.0);
    pts.iter()
        .map(|&(x, y)| {
            let x = if flip_h { w - x } else { x };
            let y = if flip_v { h - y } else { y };
            let (dx, dy) = (x - cx, y - cy);
            (ox + cx + dx * c - dy * s, oy + cy + dx * s + dy * c)
        })
        .collect()
}

/// Rotate one point about `(cx, cy)` by `rot_deg` clockwise.
pub fn rotate_point((x, y): (f32, f32), (cx, cy): (f32, f32), rot_deg: f64) -> (f32, f32) {
    let r = if rot_deg.is_finite() {
        (rot_deg % 360.0).to_radians() as f32
    } else {
        0.0
    };
    let (s, c) = r.sin_cos();
    let (dx, dy) = (x - cx, y - cy);
    (cx + dx * c - dy * s, cy + dx * s + dy * c)
}

fn area2(p: &[(f32, f32)]) -> f32 {
    let n = p.len();
    (0..n)
        .map(|i| {
            let a = p.get(i).copied().unwrap_or_default();
            let b = p.get((i + 1) % n).copied().unwrap_or_default();
            a.0 * b.1 - b.0 * a.1
        })
        .sum()
}

fn cross(o: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0)
}

fn in_tri(p: (f32, f32), a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> bool {
    let d1 = cross(a, b, p);
    let d2 = cross(b, c, p);
    let d3 = cross(c, a, p);
    !((d1 < 0.0 || d2 < 0.0 || d3 < 0.0) && (d1 > 0.0 || d2 > 0.0 || d3 > 0.0))
}

/// Ear-clipping triangulation of a simple polygon (indices into `pts`). Degenerate input
/// falls back to a fan so something is always drawn.
pub fn triangulate(pts: &[(f32, f32)]) -> Vec<[usize; 3]> {
    let n = pts.len();
    if !(3..=4096).contains(&n) {
        return Vec::new();
    }
    let mut idx: Vec<usize> = (0..n).collect();
    if area2(pts) < 0.0 {
        idx.reverse();
    }
    let at = |i: usize| pts.get(i).copied().unwrap_or_default();
    let mut out = Vec::with_capacity(n);
    let mut guard = 0usize;
    while idx.len() > 3 && guard < n * n {
        guard += 1;
        let m = idx.len();
        let mut clipped = false;
        for k in 0..m {
            let (ia, ib, ic) = (
                idx.get((k + m - 1) % m).copied().unwrap_or(0),
                idx.get(k).copied().unwrap_or(0),
                idx.get((k + 1) % m).copied().unwrap_or(0),
            );
            let (a, b, c) = (at(ia), at(ib), at(ic));
            if cross(a, b, c) <= 0.0 {
                continue; // reflex or flat
            }
            let blocked = idx
                .iter()
                .filter(|&&j| j != ia && j != ib && j != ic)
                .any(|&j| in_tri(at(j), a, b, c));
            if blocked {
                continue;
            }
            out.push([ia, ib, ic]);
            idx.remove(k);
            clipped = true;
            break;
        }
        if !clipped {
            // Self-intersecting or collinear leftovers: fan the rest.
            let first = idx.first().copied().unwrap_or(0);
            for w in idx.windows(2).skip(1) {
                if let [b, c] = w {
                    out.push([first, *b, *c]);
                }
            }
            return out;
        }
    }
    if let [a, b, c] = idx.as_slice() {
        out.push([*a, *b, *c]);
    }
    out
}

/// A filled arrow head at `tip`, pointing away from `from`, for a line of `width` px.
pub fn arrow_head(kind: &str, from: (f32, f32), tip: (f32, f32), width: f32) -> Option<Path> {
    let (dx, dy) = (tip.0 - from.0, tip.1 - from.1);
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1e-3 || kind == "none" {
        return None;
    }
    let (ux, uy) = (dx / len, dy / len);
    let (px, py) = (-uy, ux);
    let size = (width * 3.0).max(6.0).min(len);
    let back = (tip.0 - ux * size, tip.1 - uy * size);
    let half = size * 0.5;
    let pts = match kind {
        "oval" => {
            let c = (tip.0 - ux * half, tip.1 - uy * half);
            (0..16)
                .map(|i| {
                    let t = std::f32::consts::TAU * i as f32 / 16.0;
                    (c.0 + half * t.cos(), c.1 + half * t.sin())
                })
                .collect()
        }
        "diamond" => vec![
            tip,
            (
                back.0 + ux * half + px * half,
                back.1 + uy * half + py * half,
            ),
            back,
            (
                back.0 + ux * half - px * half,
                back.1 + uy * half - py * half,
            ),
        ],
        "stealth" => vec![
            tip,
            (back.0 + px * half, back.1 + py * half),
            (tip.0 - ux * size * 0.6, tip.1 - uy * size * 0.6),
            (back.0 - px * half, back.1 - py * half),
        ],
        _ => vec![
            tip,
            (back.0 + px * half, back.1 + py * half),
            (back.0 - px * half, back.1 - py * half),
        ],
    };
    Some(Path {
        pts,
        closed: true,
        filled: true,
    })
}

// ---------------------------------------------------------------------------------------
// Paint

/// Resolved stroke of an outline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stroke {
    pub color: Color,
    pub alpha: f32,
    /// px at 100% zoom.
    pub width: f32,
    /// (on, off) dash lengths in multiples of the width.
    pub dash: Option<(f32, f32)>,
}

/// Fill, outline and default text colour of an object.
#[derive(Debug, Clone, PartialEq)]
pub struct ObjPaint {
    pub fill: Option<(Color, f32)>,
    pub line: Option<Stroke>,
    pub text: Color,
}

fn dash_pattern(name: &str) -> Option<(f32, f32)> {
    Some(match name {
        "solid" => return None,
        "dot" | "sysDot" => (1.0, 1.5),
        "sysDash" => (3.0, 1.0),
        "dash" => (4.0, 3.0),
        "lgDash" => (8.0, 3.0),
        "dashDot" | "lgDashDot" | "lgDashDotDot" | "sysDashDot" | "sysDashDotDot" => (4.0, 2.0),
        _ => (4.0, 3.0),
    })
}

fn line_stroke(l: &LineFormat, style: Option<(u32, &ObjColor)>) -> Option<Stroke> {
    let (color, alpha) = match &l.fill {
        Fill::None => return None,
        Fill::Solid(c) => resolve_color(c),
        Fill::Default => match style {
            Some((idx, c)) if idx > 0 => resolve_color(c),
            _ => return None,
        },
    };
    let default_emu = match style.map(|s| s.0) {
        Some(2) => 12_700,
        Some(3) => 19_050,
        _ => 9_525,
    };
    let width = (l.width.unwrap_or(default_emu).clamp(0, 20_000_000) as f64 / EMU_PER_PX) as f32;
    Some(Stroke {
        color,
        alpha,
        width: width.max(0.5),
        dash: l.dash.as_deref().and_then(dash_pattern),
    })
}

/// Resolve an object's paint through its own formatting and style references.
pub fn object_paint(o: &DrawObject) -> ObjPaint {
    match &o.kind {
        ObjectKind::Picture(p) => ObjPaint {
            fill: None,
            line: line_stroke(&p.line, None),
            text: Color::BLACK,
        },
        ObjectKind::Shape(s) => shape_paint(s),
    }
}

pub fn shape_paint(s: &Shape) -> ObjPaint {
    let style = s.style.as_ref();
    let fill = if s.connector || is_line_preset(&s.preset) {
        None
    } else {
        match &s.fill {
            Fill::None => None,
            Fill::Solid(c) => Some(resolve_color(c)),
            Fill::Default => style
                .filter(|st| st.fill.0 > 0)
                .map(|st| resolve_color(&st.fill.1)),
        }
    };
    let line = line_stroke(&s.line, style.map(|st| (st.line.0, &st.line.1)));
    let text = style.map_or(Color::BLACK, |st| resolve_color(&st.font.1).0);
    ObjPaint { fill, line, text }
}

// ---------------------------------------------------------------------------------------
// Text

/// One paragraph ready to draw: the first run's formatting applies to the whole line.
#[derive(Debug, Clone, PartialEq)]
pub struct TextPara {
    pub text: String,
    /// Points.
    pub size: f32,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub color: Color,
    pub align: Align,
    pub font: String,
}

/// Paragraphs of a text body with defaults filled in (11 pt, `default_color`).
pub fn text_paras(t: &TextBody, default_color: Color) -> Vec<TextPara> {
    t.paras
        .iter()
        .map(|p| {
            let f: Option<&RunFont> = p.runs.first().map(|r| &r.font).or(p.end.as_ref());
            let size = f
                .and_then(|f| f.size)
                .map_or(11.0, |s| (s.clamp(100, 400_000) as f32) / 100.0);
            TextPara {
                text: p.text(),
                size,
                bold: f.is_some_and(|f| f.bold),
                italic: f.is_some_and(|f| f.italic),
                underline: f.is_some_and(|f| f.underline),
                color: f
                    .and_then(|f| f.color.as_ref())
                    .map_or(default_color, |c| resolve_color(c).0),
                align: match p.align.as_deref() {
                    Some("ctr") => Align::Center,
                    Some("r") => Align::Right,
                    _ => Align::Left,
                },
                font: f
                    .and_then(|f| f.latin.clone())
                    .unwrap_or_else(|| "Calibri".into()),
            }
        })
        .collect()
}

/// Text insets (left, top, right, bottom) in px at 100% zoom.
pub fn text_insets(t: &TextBody) -> (f32, f32, f32, f32) {
    let get = |k: &str, d: i64| {
        let emu = t
            .body_attr(k)
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(d)
            .clamp(0, 10_000_000);
        (emu as f64 / EMU_PER_PX) as f32
    };
    (
        get("lIns", 91_440),
        get("tIns", 45_720),
        get("rIns", 91_440),
        get("bIns", 45_720),
    )
}

/// Vertical anchoring of the text block.
pub fn text_v_align(t: &TextBody) -> VAlign {
    match t.body_attr("anchor") {
        Some("ctr") => VAlign::Center,
        Some("b") => VAlign::Bottom,
        _ => VAlign::Top,
    }
}

/// Whether lines wrap at the box width (`wrap="none"` turns it off).
pub fn text_wraps(t: &TextBody) -> bool {
    t.body_attr("wrap") != Some("none")
}

// ---------------------------------------------------------------------------------------
// Cell checkboxes

/// `Some(checked)` when cell `a` shows a checkbox (checkbox format on a TRUE/FALSE value).
pub fn checkbox_state(wb: &sp_model::Workbook, sheet: &Sheet, a: CellAddr) -> Option<bool> {
    let c = sheet.get(a)?;
    if !wb.styles.get(c.style).checkbox {
        return None;
    }
    match c.value {
        sp_core::Value::Bool(b) => Some(b),
        _ => None,
    }
}

/// The checkbox square inside a cell box `(x, y, w, h)`: centred (or aligned like the
/// cell), side ~ 11 px × `zoom`.
pub fn checkbox_rect(
    (x, y, w, h): (f32, f32, f32, f32),
    zoom: f32,
    align: sp_model::HAlign,
) -> (f32, f32, f32, f32) {
    let side = (11.0 * zoom).min(h - 2.0).min(w - 2.0).max(1.0);
    let cx = match align {
        sp_model::HAlign::Left => x + 3.0 * zoom + side / 2.0,
        sp_model::HAlign::Right => x + w - 3.0 * zoom - side / 2.0,
        _ => x + w / 2.0,
    };
    (cx - side / 2.0, y + (h - side) / 2.0, side, side)
}

/// The check mark polyline inside a checkbox square.
pub fn check_mark((x, y, w, h): (f32, f32, f32, f32)) -> Vec<(f32, f32)> {
    vec![
        (x + w * 0.2, y + h * 0.52),
        (x + w * 0.42, y + h * 0.74),
        (x + w * 0.8, y + h * 0.28),
    ]
}

// ---------------------------------------------------------------------------------------
// Images

/// Decoded RGBA pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decoded {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, straight alpha.
    pub rgba: Arc<[u8]>,
}

fn image_format(f: ImageFormat) -> image::ImageFormat {
    match f {
        ImageFormat::Png => image::ImageFormat::Png,
        ImageFormat::Jpeg => image::ImageFormat::Jpeg,
        ImageFormat::Gif => image::ImageFormat::Gif,
        ImageFormat::Bmp => image::ImageFormat::Bmp,
    }
}

fn limits(max_pixels: u64) -> image::Limits {
    let mut l = image::Limits::default();
    l.max_image_width = Some(MAX_IMAGE_SIDE);
    l.max_image_height = Some(MAX_IMAGE_SIDE);
    l.max_alloc = Some(max_pixels.saturating_mul(8).max(1 << 20));
    l
}

/// Format and pixel size from the file header, without decoding.
pub fn probe(bytes: &[u8]) -> Option<(ImageFormat, u32, u32)> {
    let fmt = ImageFormat::sniff(bytes)?;
    let run = || {
        let mut r = image::ImageReader::with_format(std::io::Cursor::new(bytes), image_format(fmt));
        r.limits(limits(MAX_DECODE_PIXELS));
        r.into_dimensions().ok()
    };
    let (w, h) = std::panic::catch_unwind(run).ok()??;
    Some((fmt, w, h))
}

/// Validate image file bytes for embedding: a supported format whose header parses and
/// whose size is within the caps. Pixels are not decoded here.
pub fn image_data(bytes: Vec<u8>) -> Result<ImageData, String> {
    if bytes.len() > sp_model::objects::MAX_IMAGE_BYTES {
        return Err("image file is too large".into());
    }
    let (format, width, height) =
        probe(&bytes).ok_or("not a readable PNG, JPEG, GIF or BMP image")?;
    if width == 0 || height == 0 {
        return Err("image has no pixels".into());
    }
    if u64::from(width) * u64::from(height) > MAX_DECODE_PIXELS {
        return Err(format!(
            "image is too large ({width}×{height} pixels; at most {MAX_DECODE_PIXELS})"
        ));
    }
    Ok(ImageData {
        bytes: Arc::from(bytes),
        format,
        width,
        height,
    })
}

/// Image data for bytes read from a file (format from the signature; size best effort).
/// `None` when the bytes are not an image format we draw.
pub fn image_from_file(bytes: Arc<[u8]>) -> Option<ImageData> {
    let format = ImageFormat::sniff(&bytes)?;
    let (width, height) = probe(&bytes).map_or((0, 0), |(_, w, h)| (w, h));
    Some(ImageData {
        bytes,
        format,
        width,
        height,
    })
}

/// Decode to RGBA, refusing images over `max_pixels` and never panicking on hostile input.
pub fn decode(img: &ImageData, max_pixels: u64) -> Result<Decoded, String> {
    let fmt = ImageFormat::sniff(&img.bytes).ok_or("unsupported image format")?;
    let run = || -> Result<Decoded, String> {
        let mut r = image::ImageReader::with_format(
            std::io::Cursor::new(&img.bytes[..]),
            image_format(fmt),
        );
        r.limits(limits(max_pixels));
        let (w, h) = {
            let mut p = image::ImageReader::with_format(
                std::io::Cursor::new(&img.bytes[..]),
                image_format(fmt),
            );
            p.limits(limits(max_pixels));
            p.into_dimensions().map_err(|e| e.to_string())?
        };
        if u64::from(w) * u64::from(h) > max_pixels || w == 0 || h == 0 {
            return Err(format!("image is too large to draw ({w}×{h})"));
        }
        let d = r.decode().map_err(|e| e.to_string())?.to_rgba8();
        Ok(Decoded {
            width: d.width(),
            height: d.height(),
            rgba: Arc::from(d.into_raw()),
        })
    };
    std::panic::catch_unwind(run).map_err(|_| "image decoder failed".to_string())?
}

/// Encode RGBA pixels as a PNG file (sample images, tests); `None` on a size mismatch.
pub fn encode_png(width: u32, height: u32, rgba: Vec<u8>) -> Option<Vec<u8>> {
    let img = image::RgbaImage::from_raw(width, height, rgba)?;
    let mut buf = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .ok()?;
    Some(buf)
}

/// Nearest-neighbour downscale so neither side exceeds `max_side` (returns a clone when
/// already small enough).
pub fn downscale(d: &Decoded, max_side: u32) -> Decoded {
    let max_side = max_side.max(1);
    if d.width <= max_side && d.height <= max_side {
        return d.clone();
    }
    let s = (f64::from(max_side) / f64::from(d.width.max(d.height))).min(1.0);
    let nw = ((f64::from(d.width) * s).round() as u32).max(1);
    let nh = ((f64::from(d.height) * s).round() as u32).max(1);
    let mut out = Vec::with_capacity((nw as usize) * (nh as usize) * 4);
    for y in 0..nh {
        let sy = (u64::from(y) * u64::from(d.height) / u64::from(nh)) as usize;
        for x in 0..nw {
            let sx = (u64::from(x) * u64::from(d.width) / u64::from(nw)) as usize;
            let i = (sy * d.width as usize + sx) * 4;
            match d.rgba.get(i..i + 4) {
                Some(px) => out.extend_from_slice(px),
                None => out.extend_from_slice(&[0, 0, 0, 0]),
            }
        }
    }
    Decoded {
        width: nw,
        height: nh,
        rgba: Arc::from(out),
    }
}

/// Default display size (px at 100%) for an inserted image: its pixel size, scaled down to
/// fit `max` × `max`.
pub fn fit_size(img: &ImageData, max: f64) -> (f64, f64) {
    let (w, h) = (f64::from(img.width.max(1)), f64::from(img.height.max(1)));
    let s = (max / w.max(h)).min(1.0);
    (w * s, h * s)
}

// ---------------------------------------------------------------------------------------
// Object scenes (shared by the grid painter and the page scene)

/// One primitive of an object, in output units (screen px or page pt), with the object's
/// rotation and flip applied.
#[derive(Debug, Clone, PartialEq)]
pub enum ObjPrim {
    /// A filled outline; may be concave (fill non-zero, or [`triangulate`] first).
    Fill {
        pts: Vec<(f32, f32)>,
        color: Color,
        alpha: f32,
    },
    /// A stroked outline; `width` and `dash` lengths in output units.
    Stroke {
        pts: Vec<(f32, f32)>,
        closed: bool,
        color: Color,
        alpha: f32,
        width: f32,
        dash: Option<(f32, f32)>,
    },
    /// The picture mapped onto a parallelogram: where its top-left, top-right,
    /// bottom-right and bottom-left corners land.
    Image { corners: [(f32, f32); 4] },
}

/// An object's text: laid out unrotated inside `rect`, then turned `rotation` degrees
/// clockwise about `center` (text is not mirrored by flips).
#[derive(Debug, Clone, PartialEq)]
pub struct ObjText {
    pub rect: (f32, f32, f32, f32),
    pub center: (f32, f32),
    pub rotation: f64,
    pub paras: Vec<TextPara>,
    pub v_align: VAlign,
    pub wrap: bool,
}

/// Everything to draw for one object, bottom to top: `prims`, then `text`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ObjScene {
    pub prims: Vec<ObjPrim>,
    pub text: Option<ObjText>,
}

fn finite_or(v: f32, d: f32) -> f32 {
    if v.is_finite() { v } else { d }
}

/// Lay out `o` in the box `(x, y, w, h)` (output units). `scale` is output units per px
/// at 100% (the zoom on screen, pt per px on paper) and sizes line widths and arrow heads.
/// Hidden objects produce an empty scene.
pub fn object_scene(o: &DrawObject, (x, y, w, h): (f32, f32, f32, f32), scale: f32) -> ObjScene {
    let mut sc = ObjScene::default();
    if o.hidden {
        return sc;
    }
    let (x, y) = (
        finite_or(x, 0.0).clamp(-1e7, 1e7),
        finite_or(y, 0.0).clamp(-1e7, 1e7),
    );
    let (w, h) = (
        finite_or(w, 0.0).clamp(0.0, 1e6),
        finite_or(h, 0.0).clamp(0.0, 1e6),
    );
    let scale = if scale.is_finite() && scale > 0.0 {
        scale.min(100.0)
    } else {
        1.0
    };
    let paint = object_paint(o);
    let tf = |pts: &[(f32, f32)]| transform(pts, (w, h), o.rotation, o.flip_h, o.flip_v, (x, y));
    let stroke = |pts: Vec<(f32, f32)>, closed: bool, s: &Stroke| {
        let width = s.width * scale;
        ObjPrim::Stroke {
            pts,
            closed,
            color: s.color,
            alpha: s.alpha,
            width,
            dash: s.dash.map(|(on, off)| (on * width, off * width)),
        }
    };
    match &o.kind {
        ObjectKind::Picture(_) => {
            let c = tf(&[(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)]);
            if let [a, b, cc, d] = c.as_slice() {
                sc.prims.push(ObjPrim::Image {
                    corners: [*a, *b, *cc, *d],
                });
            }
            if let Some(s) = &paint.line {
                sc.prims.push(stroke(c, true, s));
            }
        }
        ObjectKind::Shape(s) => {
            let paths = preset_paths(&s.preset, &s.adjust, w, h);
            for p in &paths {
                let pts = tf(&p.pts);
                if p.filled
                    && let Some((c, a)) = paint.fill
                {
                    sc.prims.push(ObjPrim::Fill {
                        pts: pts.clone(),
                        color: c,
                        alpha: a,
                    });
                }
                if let Some(st) = &paint.line {
                    sc.prims.push(stroke(pts, p.closed, st));
                }
            }
            // Arrow heads on open outlines (`head` at the start, `tail` at the end).
            if let Some(st) = &paint.line {
                for p in paths.iter().filter(|p| !p.closed) {
                    let pts = tf(&p.pts);
                    let n = pts.len();
                    let ends = [
                        (s.line.head.as_deref(), pts.get(1), pts.first()),
                        (
                            s.line.tail.as_deref(),
                            n.checked_sub(2).and_then(|i| pts.get(i)),
                            pts.last(),
                        ),
                    ];
                    for (kind, from, tip) in ends {
                        if let (Some(k), Some(f), Some(t)) = (kind, from, tip)
                            && let Some(head) = arrow_head(k, *f, *t, st.width * scale)
                        {
                            sc.prims.push(ObjPrim::Fill {
                                pts: head.pts,
                                color: st.color,
                                alpha: st.alpha,
                            });
                        }
                    }
                }
            }
            if let Some(t) = &s.text
                && t.paras.iter().any(|p| !p.text().is_empty())
            {
                let (l, tp, r, b) = text_insets(t);
                let (l, tp, r, b) = (l * scale, tp * scale, r * scale, b * scale);
                sc.text = Some(ObjText {
                    rect: (x + l, y + tp, (w - l - r).max(0.0), (h - tp - b).max(0.0)),
                    center: (x + w / 2.0, y + h / 2.0),
                    rotation: if o.rotation.is_finite() {
                        o.rotation
                    } else {
                        0.0
                    },
                    paras: text_paras(t, paint.text),
                    v_align: text_v_align(t),
                    wrap: text_wraps(t),
                });
            }
        }
    }
    sc
}

/// Draw order of a sheet's charts and objects, bottom to top. Object `i` sits above the
/// first `after_charts` charts (clamped to the chart count).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZItem {
    Chart(usize),
    Object(usize),
}

pub fn z_order(sheet: &Sheet) -> Vec<ZItem> {
    let n = sheet.drawings.charts.len();
    let mut objs: Vec<(usize, usize)> = sheet
        .objects
        .iter()
        .enumerate()
        .map(|(i, o)| (o.after_charts.min(n), i))
        .collect();
    objs.sort_by_key(|(after, i)| (*after, *i));
    let mut out = Vec::with_capacity(n + objs.len());
    let mut it = objs.into_iter().peekable();
    for c in 0..=n {
        while let Some((_, i)) = it.next_if(|(after, _)| *after <= c) {
            out.push(ZItem::Object(i));
        }
        if c < n {
            out.push(ZItem::Chart(c));
        }
    }
    out
}

// ---------------------------------------------------------------------------------------
// Slicer layout

/// Where a slicer's parts go inside its box, in output units.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SlicerLayout {
    /// Caption strip.
    pub header: (f32, f32, f32, f32),
    /// "Clear filter" button at the right of the header.
    pub clear: (f32, f32, f32, f32),
    /// One button per item that fits, in item order (left to right, then down).
    pub buttons: Vec<(f32, f32, f32, f32)>,
}

/// Lay out a slicer with `n` items in `columns` columns inside `(x, y, w, h)`; `scale` is
/// output units per px at 100%. Items that don't fit are left out.
pub fn slicer_layout(
    (x, y, w, h): (f32, f32, f32, f32),
    n: usize,
    columns: u32,
    scale: f32,
) -> SlicerLayout {
    let s = if scale.is_finite() && scale > 0.0 {
        scale.min(100.0)
    } else {
        1.0
    };
    let (w, h) = (finite_or(w, 0.0).max(0.0), finite_or(h, 0.0).max(0.0));
    let (pad, head_h, btn_h, gap) = (6.0 * s, 26.0 * s, 22.0 * s, 4.0 * s);
    let head_h = head_h.min(h);
    let side = (18.0 * s).min(head_h);
    let header = (x, y, w, head_h);
    let clear = (
        x + w - pad - side,
        y + (head_h - side) / 2.0,
        side.min(w),
        side,
    );
    let cols = columns.clamp(1, 20) as usize;
    let inner_w = (w - 2.0 * pad).max(0.0);
    let bw = ((inner_w - gap * (cols as f32 - 1.0)) / cols as f32).max(1.0);
    let bottom = y + h - pad;
    let mut buttons = Vec::new();
    for i in 0..n.min(10_000) {
        let (r, c) = (i / cols, i % cols);
        let by = y + head_h + r as f32 * (btn_h + gap);
        if by + btn_h > bottom + 0.01 {
            break;
        }
        buttons.push((x + pad + c as f32 * (bw + gap), by, bw, btn_h));
    }
    SlicerLayout {
        header,
        clear,
        buttons,
    }
}

// ---------------------------------------------------------------------------------------
// Images for PDF

/// An image ready for a PDF image XObject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdfImage {
    pub width: u32,
    pub height: u32,
    /// 1 (gray) or 3 (RGB) colour components.
    pub colors: u8,
    /// `data` is a baseline/progressive JPEG file (`/DCTDecode`); otherwise a zlib stream
    /// of PNG-predicted rows (`/FlateDecode` with `/Predictor 15`).
    pub dct: bool,
    pub data: Vec<u8>,
    /// Soft mask (alpha), as a 1-component predicted zlib stream; `None` when opaque.
    pub alpha: Option<Vec<u8>>,
}

/// Size and component count from a JPEG's start-of-frame marker (8-bit precision only).
pub fn jpeg_info(bytes: &[u8]) -> Option<(u32, u32, u8)> {
    if !bytes.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let mut i = 2usize;
    let at = |i: usize| bytes.get(i).copied();
    while i < bytes.len() {
        if at(i)? != 0xFF {
            return None;
        }
        let mut m = at(i + 1)?;
        while m == 0xFF {
            i += 1;
            m = at(i + 1)?;
        }
        i += 2;
        if m == 0xD8 || m == 0x01 || (0xD0..=0xD7).contains(&m) {
            continue; // markers without a length
        }
        if m == 0xD9 || m == 0xDA {
            return None; // end of image / scan before any frame header
        }
        let len = usize::from(u16::from_be_bytes([at(i)?, at(i + 1)?]));
        if len < 2 {
            return None;
        }
        let is_sof = matches!(m, 0xC0..=0xCF) && !matches!(m, 0xC4 | 0xC8 | 0xCC);
        if is_sof {
            let precision = at(i + 2)?;
            let height = u32::from(u16::from_be_bytes([at(i + 3)?, at(i + 4)?]));
            let width = u32::from(u16::from_be_bytes([at(i + 5)?, at(i + 6)?]));
            let comps = at(i + 7)?;
            if precision != 8 || width == 0 || height == 0 {
                return None;
            }
            return Some((width, height, comps));
        }
        i = i.checked_add(len)?;
    }
    None
}

/// Zlib stream of 8-bit rows with PNG row filters (what PDF `/FlateDecode` with
/// `/Predictor 15` reads), made by the PNG encoder. `channels` is 1 or 3.
pub fn predicted_zlib(width: u32, height: u32, channels: u8, data: &[u8]) -> Option<Vec<u8>> {
    use image::ImageEncoder;
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};
    let ct = match channels {
        1 => image::ExtendedColorType::L8,
        3 => image::ExtendedColorType::Rgb8,
        _ => return None,
    };
    let need = u64::from(width) * u64::from(height) * u64::from(channels);
    if width == 0 || height == 0 || need != data.len() as u64 {
        return None;
    }
    let mut png = Vec::new();
    PngEncoder::new_with_quality(&mut png, CompressionType::Default, FilterType::Adaptive)
        .write_image(data, width, height, ct)
        .ok()?;
    // Concatenate the IDAT chunks: together they are one zlib stream.
    let mut out = Vec::new();
    let mut i = 8usize;
    while let Some(Ok(head)) = png.get(i..i + 8).map(<[u8; 8]>::try_from) {
        let [l0, l1, l2, l3, t0, t1, t2, t3] = head;
        let len = u32::from_be_bytes([l0, l1, l2, l3]) as usize;
        let body = png.get(i + 8..(i + 8).checked_add(len)?)?;
        if [t0, t1, t2, t3] == *b"IDAT" {
            out.extend_from_slice(body);
        }
        i = i.checked_add(12 + len)?;
    }
    (!out.is_empty()).then_some(out)
}

/// Largest side of an image embedded in a PDF (larger pictures are downscaled).
pub const PDF_MAX_IMAGE_SIDE: u32 = 4096;

/// Prepare an embedded image for PDF: 8-bit gray/RGB JPEGs pass through as-is, anything
/// else is decoded (bounded) to RGB plus an alpha mask when not opaque.
pub fn pdf_image(img: &ImageData) -> Option<PdfImage> {
    if img.format == ImageFormat::Jpeg
        && let Some((width, height, comps)) = jpeg_info(&img.bytes)
        && (comps == 1 || comps == 3)
    {
        return Some(PdfImage {
            width,
            height,
            colors: comps,
            dct: true,
            data: img.bytes.to_vec(),
            alpha: None,
        });
    }
    let d = downscale(&decode(img, MAX_DECODE_PIXELS).ok()?, PDF_MAX_IMAGE_SIDE);
    let n = (d.width as usize).checked_mul(d.height as usize)?;
    let mut rgb = Vec::with_capacity(n.checked_mul(3)?);
    let mut alpha = Vec::with_capacity(n);
    for [r, g, b, a] in d.rgba.as_chunks::<4>().0 {
        rgb.extend_from_slice(&[*r, *g, *b]);
        alpha.push(*a);
    }
    let opaque = alpha.iter().all(|a| *a == 255);
    Some(PdfImage {
        width: d.width,
        height: d.height,
        colors: 3,
        dct: false,
        data: predicted_zlib(d.width, d.height, 3, &rgb)?,
        alpha: if opaque {
            None
        } else {
            Some(predicted_zlib(d.width, d.height, 1, &alpha)?)
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny valid PNG (2×1: red, transparent) built with the encoder.
    pub(crate) fn png_2x1() -> Vec<u8> {
        let mut buf = Vec::new();
        let img = image::RgbaImage::from_raw(2, 1, vec![255, 0, 0, 255, 0, 0, 0, 0]).unwrap();
        img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        buf
    }

    #[test]
    fn decode_png() {
        let d = image_data(png_2x1()).unwrap();
        assert_eq!((d.width, d.height, d.format), (2, 1, ImageFormat::Png));
        let px = decode(&d, 100).unwrap();
        assert_eq!(&px.rgba[..4], &[255, 0, 0, 255]);
        assert!(decode(&d, 1).is_err(), "over the pixel cap");
    }

    #[test]
    fn hostile_images_are_refused_without_panic() {
        let good = png_2x1();
        // Truncations of a valid file.
        for n in 0..good.len() {
            let v = good.get(..n).unwrap().to_vec();
            if let Ok(d) = image_data(v) {
                let _ = decode(&d, 1000);
            }
        }
        // A PNG header claiming 100000×100000 pixels.
        let mut huge = good.clone();
        // IHDR width/height live at bytes 16..24.
        huge.splice(16..24, [0, 1, 0x86, 0xA0, 0, 1, 0x86, 0xA0]);
        assert!(image_data(huge.clone()).is_err());
        let fake = ImageData {
            bytes: Arc::from(huge),
            format: ImageFormat::Png,
            width: 1,
            height: 1,
        };
        assert!(decode(&fake, MAX_DECODE_PIXELS).is_err());
        // Signatures with garbage after them.
        for sig in [
            &b"\xFF\xD8\xFF\xE0garbage"[..],
            b"GIF89a\xFF\xFF\xFF\xFF\x00\x00",
            b"BM\xFF\xFF\xFF\xFF\x00\x00\x00\x00\x36\x00\x00\x00\x28\x00\x00\x00\xFF\xFF\xFF\x7F\xFF\xFF\xFF\x7F\x01\x00\x20\x00",
            b"\x89PNG\r\n\x1a\n",
        ] {
            let _ = image_data(sig.to_vec());
            let fake = ImageData {
                bytes: Arc::from(sig.to_vec()),
                format: ImageFormat::Png,
                width: 1,
                height: 1,
            };
            assert!(decode(&fake, MAX_DECODE_PIXELS).is_err());
        }
        assert!(image_data(b"<svg/>".to_vec()).is_err());
    }

    #[test]
    fn downscale_caps_side() {
        let d = decode(&image_data(png_2x1()).unwrap(), 100).unwrap();
        let s = downscale(&d, 1);
        assert_eq!((s.width, s.height), (1, 1));
        assert_eq!(s.rgba.len(), 4);
    }

    #[test]
    fn presets_stay_in_box_and_triangulate() {
        for (p, _) in PRESETS {
            let paths = preset_paths(p, &[], 120.0, 60.0);
            assert!(!paths.is_empty(), "{p}");
            for path in &paths {
                for &(x, y) in &path.pts {
                    if p.starts_with("wedge") {
                        continue; // the callout tip points outside the box
                    }
                    assert!(
                        (-0.01..=120.01).contains(&x) && (-0.01..=60.01).contains(&y),
                        "{p}: {x},{y}"
                    );
                }
                if path.filled {
                    let tris = triangulate(&path.pts);
                    assert_eq!(tris.len(), path.pts.len() - 2, "{p}");
                }
            }
        }
        // Hostile sizes and adjust values don't panic.
        let adj = vec![
            ("adj".to_string(), i64::MAX),
            ("adj1".to_string(), i64::MIN),
        ];
        for (p, _) in PRESETS {
            let _ = preset_paths(p, &adj, f32::NAN, f32::INFINITY);
            let _ = preset_paths(p, &adj, 0.0, 0.0);
        }
        assert!(triangulate(&[(0.0, 0.0), (1.0, 1.0)]).is_empty());
    }

    #[test]
    fn concave_star_triangulates_inside() {
        let p = &preset_paths("star5", &[], 100.0, 100.0)[0];
        let tris = triangulate(&p.pts);
        let area: f32 = tris
            .iter()
            .map(|t| {
                let (a, b, c) = (p.pts[t[0]], p.pts[t[1]], p.pts[t[2]]);
                cross(a, b, c).abs() / 2.0
            })
            .sum();
        assert!((area - area2(&p.pts).abs() / 2.0).abs() < 1.0);
    }

    #[test]
    fn transform_rotates_about_centre() {
        let r = transform(&[(0.0, 0.0)], (10.0, 10.0), 90.0, false, false, (0.0, 0.0));
        assert!((r[0].0 - 10.0).abs() < 1e-4 && r[0].1.abs() < 1e-4);
        let f = transform(&[(0.0, 0.0)], (10.0, 4.0), 0.0, true, true, (1.0, 1.0));
        assert_eq!(f[0], (11.0, 5.0));
        let _ = transform(
            &[(0.0, 0.0)],
            (1.0, 1.0),
            f64::NAN,
            false,
            false,
            (0.0, 0.0),
        );
    }

    #[test]
    fn anchors_round_trip_through_rects() {
        let mut sh = Sheet::new("S");
        sh.col_widths.insert(1, 20.0);
        for kind in [
            AnchorKind::TwoCell,
            AnchorKind::OneCell,
            AnchorKind::Absolute,
        ] {
            let a = anchor_from_rect(&sh, kind, Placement::Move, (90.0, 35.0, 200.0, 80.0));
            let (x, y, w, h) = object_rect_abs(&sh, &a);
            assert!(
                (x - 90.0).abs() < 0.01 && (y - 35.0).abs() < 0.01,
                "{kind:?}"
            );
            assert!(
                (w - 200.0).abs() < 0.01 && (h - 80.0).abs() < 0.01,
                "{kind:?}"
            );
        }
        let a = anchor_from_rect(
            &sh,
            AnchorKind::TwoCell,
            Placement::Move,
            (f64::NAN, -5.0, f64::INFINITY, 1.0),
        );
        let _ = object_rect_abs(&sh, &a);
    }

    #[test]
    fn paint_resolves_style_refs() {
        let s = Shape {
            preset: "rect".into(),
            style: Some(sp_model::objects::StyleRefs::shape_default()),
            ..Shape::default()
        };
        let p = shape_paint(&s);
        assert_eq!(p.fill.map(|f| f.0), Some(Color(0x44, 0x72, 0xC4)));
        let l = p.line.unwrap();
        assert!((l.width - 12700.0 / 9525.0).abs() < 1e-3);
        assert_eq!(p.text, Color::WHITE);
        let line = Shape {
            preset: "line".into(),
            connector: true,
            fill: Fill::Solid(ObjColor::rgb(Color::BLACK)),
            ..Shape::default()
        };
        assert!(shape_paint(&line).fill.is_none());
        assert!(arrow_head("triangle", (0.0, 0.0), (10.0, 0.0), 1.0).is_some());
        assert!(arrow_head("triangle", (0.0, 0.0), (0.0, 0.0), 1.0).is_none());
    }

    fn obj(kind: ObjectKind) -> DrawObject {
        DrawObject {
            kind,
            ..DrawObject::default()
        }
    }

    #[test]
    fn scenes_for_shapes_pictures_and_lines() {
        let rect = obj(ObjectKind::Shape(Shape {
            preset: "rect".into(),
            style: Some(sp_model::objects::StyleRefs::shape_default()),
            text: Some(TextBody::from_plain("Hi", &RunFont::default(), None)),
            ..Shape::default()
        }));
        let sc = object_scene(&rect, (10.0, 20.0, 100.0, 50.0), 2.0);
        assert!(matches!(sc.prims[0], ObjPrim::Fill { .. }));
        match &sc.prims[1] {
            ObjPrim::Stroke { width, closed, .. } => {
                assert!(*closed);
                assert!((width - 2.0 * 12700.0 / 9525.0).abs() < 1e-3, "{width}");
            }
            p => panic!("{p:?}"),
        }
        let t = sc.text.unwrap();
        assert_eq!(t.paras[0].text, "Hi");
        assert_eq!(t.center, (60.0, 45.0));
        assert!(t.rect.0 > 10.0 && t.rect.2 < 100.0);
        // Hidden objects draw nothing.
        let mut hidden = rect.clone();
        hidden.hidden = true;
        assert_eq!(
            object_scene(&hidden, (0.0, 0.0, 9.0, 9.0), 1.0),
            ObjScene::default()
        );
        // Pictures: corners follow rotation and flips.
        let mut pic = obj(ObjectKind::Picture(sp_model::objects::Picture::default()));
        pic.rotation = 90.0;
        let sc = object_scene(&pic, (0.0, 0.0, 10.0, 10.0), 1.0);
        let ObjPrim::Image { corners } = sc.prims[0] else {
            panic!("no image")
        };
        assert!((corners[0].0 - 10.0).abs() < 1e-4 && corners[0].1.abs() < 1e-4);
        pic.rotation = 0.0;
        pic.flip_h = true;
        let sc = object_scene(&pic, (0.0, 0.0, 10.0, 10.0), 1.0);
        let ObjPrim::Image { corners } = sc.prims[0] else {
            panic!("no image")
        };
        assert_eq!(corners[0], (10.0, 0.0));
        // A connector with an end arrow: line, then a filled head at (w, h).
        let line = obj(ObjectKind::Shape(Shape {
            preset: "straightConnector1".into(),
            connector: true,
            style: Some(sp_model::objects::StyleRefs::line_default()),
            line: LineFormat {
                tail: Some("triangle".into()),
                ..LineFormat::default()
            },
            ..Shape::default()
        }));
        let sc = object_scene(&line, (0.0, 0.0, 40.0, 0.0), 1.0);
        assert_eq!(sc.prims.len(), 2, "{:?}", sc.prims);
        let ObjPrim::Fill { pts, .. } = &sc.prims[1] else {
            panic!("no head")
        };
        assert_eq!(pts[0], (40.0, 0.0));
        // Hostile boxes don't panic.
        let _ = object_scene(&rect, (f32::NAN, f32::INFINITY, -5.0, f32::MAX), f32::NAN);
    }

    #[test]
    fn z_order_interleaves_charts() {
        let mut sh = Sheet::new("S");
        sh.drawings.charts = vec![Default::default(), Default::default()];
        let a = DrawObject {
            after_charts: 1,
            ..DrawObject::default()
        };
        let b = DrawObject {
            after_charts: 9,
            ..DrawObject::default()
        };
        sh.objects = vec![DrawObject::default(), a, b];
        assert_eq!(
            z_order(&sh),
            vec![
                ZItem::Object(0),
                ZItem::Chart(0),
                ZItem::Object(1),
                ZItem::Chart(1),
                ZItem::Object(2)
            ]
        );
    }

    #[test]
    fn slicer_buttons_fit_the_box() {
        let l = slicer_layout((0.0, 0.0, 150.0, 120.0), 10, 2, 1.0);
        // 26 header, rows of 22 + 4, 6 padding: three rows fit in 120.
        assert_eq!(l.buttons.len(), 6);
        let (x0, y0, w0, _) = l.buttons[0];
        let (x1, y1, _, _) = l.buttons[1];
        assert_eq!(y0, y1, "items run left to right first");
        assert!(x1 > x0 + w0);
        for (x, y, w, h) in &l.buttons {
            assert!(*x >= 0.0 && x + w <= 150.01 && *y >= 26.0 && y + h <= 120.0);
        }
        assert!(l.clear.0 + l.clear.2 <= 150.0);
        let _ = slicer_layout((0.0, 0.0, f32::NAN, -1.0), usize::MAX, 0, 0.0);
    }

    #[test]
    fn pdf_images_pass_jpeg_and_encode_others() {
        // PNG with transparency: RGB + soft mask, zlib streams.
        let png = image_data(png_2x1()).unwrap();
        let p = pdf_image(&png).unwrap();
        assert_eq!((p.width, p.height, p.colors, p.dct), (2, 1, 3, false));
        assert_eq!(p.data.first(), Some(&0x78), "zlib header");
        assert!(p.alpha.is_some());
        // JPEG: passes through untouched.
        let mut jpg = Vec::new();
        image::RgbImage::from_pixel(5, 3, image::Rgb([10, 200, 30]))
            .write_to(
                &mut std::io::Cursor::new(&mut jpg),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        assert_eq!(jpeg_info(&jpg), Some((5, 3, 3)));
        let j = pdf_image(&image_data(jpg.clone()).unwrap()).unwrap();
        assert!(j.dct && j.alpha.is_none());
        assert_eq!(j.data, jpg);
        // Garbage and truncation never panic.
        for n in 0..jpg.len() {
            let _ = jpeg_info(&jpg[..n]);
        }
        assert_eq!(jpeg_info(b"\xFF\xD8\xFF\xC0\x00\x01"), None);
        assert!(predicted_zlib(2, 2, 3, &[0; 5]).is_none());
        assert!(predicted_zlib(0, 2, 1, &[]).is_none());
    }
}
