//! Grid cell painting: resolved borders and formatted cell text (wrap, vertical alignment,
//! indent, rotation, font family). Pure drawing over the `sp_render` model; colours come from
//! `theme`.

use crate::theme::{LIGHT as T, color};
use egui::emath::Rot2;
use egui::epaint::TextShape;
use egui::text::LayoutJob;
use egui::{Color32, FontFamily, FontId, Painter, Pos2, Rect, Shape, Stroke, Vec2};
use sp_model::{BorderStyle, VAlign};
use sp_render::{Align, BorderSeg, DisplayCell, INDENT_PX};

/// Horizontal text padding inside a cell, px.
const PAD_X: f32 = 3.0;
/// Vertical text padding inside a cell, px.
const PAD_Y: f32 = 1.0;
/// Visual size of 1pt of font in egui px at 100% zoom (11pt ≈ 12.5px matches the column fit).
const PT_TO_PX: f32 = 1.14;

/// Font names the default egui fonts can stand in for with their monospace face.
const MONO_HINTS: [&str; 7] = [
    "courier",
    "consolas",
    "mono",
    "menlo",
    "monaco",
    "lucida console",
    "hack",
];

/// Map a stored font name to an egui family: an exactly-registered family if the app loaded
/// one by that name, the monospace face for typewriter fonts, the proportional face otherwise.
pub fn font_family(name: &str, known: &[FontFamily]) -> FontFamily {
    let named = FontFamily::Name(name.into());
    if known.contains(&named) {
        return named;
    }
    let lower = name.to_ascii_lowercase();
    if MONO_HINTS.iter().any(|m| lower.contains(m)) {
        FontFamily::Monospace
    } else {
        FontFamily::Proportional
    }
}

pub fn font_id(size_pt: f32, zoom: f32, family: FontFamily) -> FontId {
    FontId::new((size_pt * zoom * PT_TO_PX).max(1.0), family)
}

/// Draw one resolved border segment; `origin` is the top-left of the cell area.
pub fn draw_border(p: &Painter, origin: Pos2, seg: &BorderSeg) {
    let c = seg.color.map_or(T.border_auto, color);
    let a = origin + Vec2::new(seg.x, seg.y);
    let (dir, normal) = if seg.horizontal {
        (Vec2::new(1.0, 0.0), Vec2::new(0.0, 1.0))
    } else {
        (Vec2::new(0.0, 1.0), Vec2::new(1.0, 0.0))
    };
    let b = a + dir * seg.len;
    // Solid lines wider than 1px overhang by half their width so corners close.
    let solid = |w: f32| {
        let ext = dir * ((w - 1.0) / 2.0).max(0.0);
        p.line_segment([a - ext, b + ext], Stroke::new(w, c));
    };
    match seg.style {
        BorderStyle::None => {}
        BorderStyle::Thin => solid(1.0),
        BorderStyle::Medium => solid(2.0),
        BorderStyle::Thick => solid(3.0),
        BorderStyle::Dashed => p.extend(Shape::dashed_line(&[a, b], Stroke::new(1.0, c), 3.0, 1.5)),
        BorderStyle::Dotted => p.extend(Shape::dotted_line(&[a, b], c, 2.5, 0.7)),
        BorderStyle::Double => {
            let ext = dir;
            for off in [-normal, normal] {
                p.line_segment([a - ext + off, b + ext + off], Stroke::new(1.0, c));
            }
        }
    }
}

/// Inputs for painting one cell's text.
pub struct CellText<'a> {
    pub d: &'a DisplayCell,
    /// The cell (or merged area) the text is aligned in.
    pub rect: Rect,
    /// Where the text may paint (the cell, its overflow span, or a merge), already clipped.
    pub clip: Rect,
    pub zoom: f32,
    pub families: &'a [FontFamily],
}

/// Paint a cell's text honouring alignment, wrap, indent, rotation and font decorations.
pub fn draw_cell_text(p: &Painter, ct: &CellText<'_>) {
    let d = ct.d;
    if d.text.is_empty() {
        return;
    }
    let fg = d.color.map_or(T.cell_text, color);
    let font = font_id(d.font_size, ct.zoom, font_family(&d.font_name, ct.families));
    let indent = f32::from(d.indent) * INDENT_PX * ct.zoom;
    let (pad_l, pad_r) = match d.align {
        Align::Left => (PAD_X + indent, PAD_X),
        Align::Right => (PAD_X, PAD_X + indent),
        Align::Center => (PAD_X, PAD_X),
    };
    let inner = Rect::from_min_max(
        ct.rect.min + Vec2::new(pad_l, PAD_Y),
        ct.rect.max - Vec2::new(pad_r, PAD_Y),
    );
    let deg = d.rotation.clamp(-90, 90);
    // Excel's positive angles turn text counter-clockwise; egui's turn clockwise.
    let angle = -f32::from(deg).to_radians();
    let rot = Rot2::from_angle(angle);
    let wrap_width = if !d.wrap {
        f32::INFINITY
    } else if deg == 0 {
        inner.width().max(1.0)
    } else if deg.abs() == 90 {
        inner.height().max(1.0)
    } else {
        // Slanted wrapped text: wrap to the cell height along the slant.
        (inner.height() / f32::from(deg).to_radians().sin().abs()).max(1.0)
    };
    let halign = match d.align {
        Align::Left => egui::Align::Min,
        Align::Center => egui::Align::Center,
        Align::Right => egui::Align::Max,
    };
    let layout = |text: String| {
        let mut job = LayoutJob::simple(text, font.clone(), fg, wrap_width);
        job.halign = halign;
        p.layout_job(job)
    };
    let mut galley = layout(d.text.clone());
    // Numbers that don't fit show ### like the target app.
    if d.is_number && !d.wrap && deg == 0 && galley.size().x > ct.rect.width() - 4.0 {
        let n = ((ct.rect.width() - 4.0) / (7.0 * ct.zoom)).max(1.0) as usize;
        galley = layout("#".repeat(n.min(255)));
    }
    // Bounding box of the (rotated) text relative to the galley origin.
    let g = galley.rect;
    let corners = [
        g.left_top(),
        g.right_top(),
        g.left_bottom(),
        g.right_bottom(),
    ];
    let turned = corners.map(|c| Pos2::ZERO + rot * c.to_vec2());
    let bbox = Rect::from_points(&turned);
    let size = bbox.size();
    let x = match d.align {
        Align::Left => inner.min.x,
        Align::Center => inner.center().x - size.x / 2.0,
        Align::Right => inner.max.x - size.x,
    };
    let y = match d.v_align {
        VAlign::Top => inner.min.y,
        VAlign::Center => inner.center().y - size.y / 2.0,
        VAlign::Bottom => inner.max.y - size.y,
    };
    let pos = Pos2::new(x, y) - bbox.min.to_vec2();
    let pc = p.with_clip_rect(ct.clip);
    let shape = |at: Pos2| TextShape::new(at, galley.clone(), fg).with_angle(angle);
    pc.add(shape(pos));
    if d.bold {
        // egui has no synthetic bold for the default font: overdraw with a 0.6px offset.
        pc.add(shape(pos + Vec2::new(0.6, 0.0)));
    }
    if d.underline || d.strike {
        let to_screen = |v: Vec2| pos + rot * v;
        for row in &galley.rows {
            let r = row.rect();
            if r.width() <= 0.0 {
                continue;
            }
            let mut lines: Vec<f32> = Vec::new();
            if d.underline {
                lines.push(r.max.y - 1.0);
            }
            if d.strike {
                lines.push(r.center().y);
            }
            for ly in lines {
                pc.line_segment(
                    [
                        to_screen(Vec2::new(r.min.x, ly)),
                        to_screen(Vec2::new(r.max.x, ly)),
                    ],
                    Stroke::new(1.0, fg),
                );
            }
        }
    }
}

/// Background for a merged area (its anchor's fill or the grid colour).
pub fn merge_fill(d: Option<&DisplayCell>) -> Color32 {
    d.and_then(|d| d.fill).map_or(T.grid_bg, color)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_family_mapping() {
        let known = vec![FontFamily::Name("Fancy".into())];
        assert_eq!(
            font_family("Fancy", &known),
            FontFamily::Name("Fancy".into())
        );
        assert_eq!(font_family("Courier New", &known), FontFamily::Monospace);
        assert_eq!(font_family("Consolas", &known), FontFamily::Monospace);
        assert_eq!(font_family("Calibri", &known), FontFamily::Proportional);
        assert_eq!(font_family("", &[]), FontFamily::Proportional);
    }
}
