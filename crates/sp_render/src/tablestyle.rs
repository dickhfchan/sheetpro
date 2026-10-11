//! Table styles: the banded look of a structured table, derived from its style name
//! (`TableStyleLight|Medium|Dark` + number) as a differential format per cell. The palette
//! is SheetPro's own.

use sp_core::CellAddr;
use sp_model::{Border, BorderStyle, Color, DiffStyle, Table};

/// Accent colours cycled by style number.
const ACCENTS: [Color; 7] = [
    Color(64, 64, 64),
    Color(70, 115, 190),
    Color(222, 120, 50),
    Color(150, 150, 150),
    Color(232, 178, 20),
    Color(88, 150, 205),
    Color(106, 166, 72),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Light,
    Medium,
    Dark,
}

/// (family, accent) for a style name; `None` = no styling.
pub fn parse_style(name: &str) -> Option<(Family, Color)> {
    let rest = name.strip_prefix("TableStyle")?;
    let (family, num) = [
        ("Light", Family::Light),
        ("Medium", Family::Medium),
        ("Dark", Family::Dark),
    ]
    .into_iter()
    .find_map(|(k, f)| rest.strip_prefix(k).map(|n| (f, n)))?;
    let n: usize = num.parse().ok()?;
    let accent = *ACCENTS.get(n.saturating_sub(1) % ACCENTS.len())?;
    Some((family, accent))
}

/// Blend towards white (`f > 0`) or black (`f < 0`).
pub fn tint(c: Color, f: f64) -> Color {
    let ch = |v: u8| {
        let v = f64::from(v);
        let out = if f >= 0.0 {
            v + (255.0 - v) * f
        } else {
            v * (1.0 + f)
        };
        out.round().clamp(0.0, 255.0) as u8
    };
    Color(ch(c.0), ch(c.1), ch(c.2))
}

/// The table style's contribution to cell `a` (inside `t.range`).
pub fn cell_style(t: &Table, a: CellAddr) -> Option<DiffStyle> {
    if !t.range.contains(a) {
        return None;
    }
    let (family, accent) = parse_style(&t.style)?;
    let mut d = DiffStyle::default();
    let header = t.header_row && a.row == t.range.start.row;
    let totals = t.totals_row && a.row == t.range.end.row;
    if header {
        d.bold = Some(true);
        match family {
            Family::Light => {}
            Family::Medium => {
                d.fill = Some(accent);
                d.font_color = Some(Color::WHITE);
            }
            Family::Dark => {
                d.fill = Some(tint(accent, -0.55));
                d.font_color = Some(Color::WHITE);
            }
        }
        return Some(d);
    }
    if totals {
        d.bold = Some(true);
        if family == Family::Dark {
            d.fill = Some(tint(accent, -0.35));
            d.font_color = Some(Color::WHITE);
        }
        return Some(d);
    }
    let body_row = a.row - t.range.start.row - u32::from(t.header_row);
    let body_col = a.col - t.range.start.col;
    let band = (t.banded_rows && body_row.is_multiple_of(2))
        || (t.banded_cols && body_col.is_multiple_of(2));
    match family {
        Family::Light | Family::Medium => {
            if band {
                d.fill = Some(tint(accent, 0.8));
            }
        }
        Family::Dark => {
            d.fill = Some(if band { tint(accent, -0.15) } else { accent });
            d.font_color = Some(Color::WHITE);
        }
    }
    if (t.first_col && a.col == t.range.start.col) || (t.last_col && a.col == t.range.end.col) {
        d.bold = Some(true);
    }
    (!d.is_empty()).then_some(d)
}

/// Horizontal rules of the table look: (top, bottom) border of cell `a`. The header of a
/// light style is underlined; the totals row has a double rule above it.
pub fn cell_edges(t: &Table, a: CellAddr) -> (Option<Border>, Option<Border>) {
    let Some((family, accent)) = parse_style(&t.style) else {
        return (None, None);
    };
    if !t.range.contains(a) {
        return (None, None);
    }
    let top = (t.totals_row && a.row == t.range.end.row).then_some(Border {
        style: BorderStyle::Double,
        color: Some(accent),
    });
    let bottom =
        (family == Family::Light && t.header_row && a.row == t.range.start.row).then_some(Border {
            style: BorderStyle::Thin,
            color: Some(accent),
        });
    (top, bottom)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sp_core::CellRange;

    #[test]
    fn styles() {
        assert_eq!(
            parse_style("TableStyleMedium2").map(|x| x.0),
            Some(Family::Medium)
        );
        assert!(parse_style("TableStyleMedium").is_none());
        assert!(parse_style("Fancy").is_none());
        assert!(parse_style("TableStyleDark999999999999999999999").is_none());
        let mut t = Table::new(
            1,
            "T",
            CellRange::parse("A1:B5").unwrap(),
            vec!["a".into(), "b".into()],
        );
        t.totals_row = true;
        let h = cell_style(&t, CellAddr::new(0, 0)).unwrap();
        assert_eq!(h.font_color, Some(Color::WHITE));
        assert!(cell_style(&t, CellAddr::new(1, 0)).unwrap().fill.is_some());
        assert!(cell_style(&t, CellAddr::new(2, 0)).is_none());
        assert_eq!(
            cell_style(&t, CellAddr::new(4, 1)).unwrap().bold,
            Some(true)
        );
        assert!(cell_style(&t, CellAddr::new(9, 9)).is_none());
        assert_eq!(tint(Color(0, 0, 0), 0.5), Color(128, 128, 128));
        assert_eq!(tint(Color(200, 100, 0), -0.5), Color(100, 50, 0));
    }
}
