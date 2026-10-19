//! Workbook theme: 12 colour slots and the heading (major) / body (minor) fonts. Styles
//! refer to theme colours by index and tint ([`ThemeColor`]); renderers resolve them with
//! [`resolve_theme_color`]. The file part (`xl/theme/theme1.xml`) is read and written by
//! `sp_io::xlsx_theme`, which keeps everything else in it as loaded.
//!
//! The built-in themes are SheetPro's own (names and colours chosen for this project).

use crate::Color;
use serde::{Deserialize, Serialize};

/// Colour slots in file (scheme) order.
pub const SLOT_NAMES: [&str; 12] = [
    "dk1", "lt1", "dk2", "lt2", "accent1", "accent2", "accent3", "accent4", "accent5", "accent6",
    "hlink", "folHlink",
];

/// Slot labels for pickers, in scheme order.
pub const SLOT_LABELS: [&str; 12] = [
    "Text 1",
    "Background 1",
    "Text 2",
    "Background 2",
    "Accent 1",
    "Accent 2",
    "Accent 3",
    "Accent 4",
    "Accent 5",
    "Accent 6",
    "Hyperlink",
    "Followed Hyperlink",
];

/// A workbook theme.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Theme {
    pub name: String,
    /// Colour-scheme name.
    pub colors_name: String,
    /// The 12 slots in scheme order (see [`SLOT_NAMES`]).
    pub colors: [Color; 12],
    /// Font-scheme name.
    pub fonts_name: String,
    /// Heading font (latin typeface).
    pub major_font: String,
    /// Body font (latin typeface).
    pub minor_font: String,
}

impl Default for Theme {
    fn default() -> Self {
        builtin_themes()
            .into_iter()
            .next()
            .unwrap_or_else(|| Theme {
                name: "SheetPro".into(),
                colors_name: "SheetPro".into(),
                colors: [Color::BLACK; 12],
                fonts_name: "SheetPro".into(),
                major_font: "Cambria".into(),
                minor_font: crate::style::DEFAULT_FONT.into(),
            })
    }
}

/// A theme colour reference in a style: theme index (file numbering, see
/// [`slot_for_index`]), tint in millionths (-1_000_000..=1_000_000), and the sRGB value the
/// style field held when the reference was made. The reference only applies while the
/// style field still holds that value, so any later direct colour change wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ThemeColor {
    pub idx: u8,
    pub tint: i32,
    pub rgb: Color,
}

impl ThemeColor {
    pub fn tint_f64(&self) -> f64 {
        f64::from(self.tint) / 1_000_000.0
    }

    /// Whether the reference still describes `field` (the style's colour).
    pub fn applies_to(&self, field: Option<Color>) -> bool {
        field == Some(self.rgb)
    }
}

/// Scheme slot of a style theme index. SpreadsheetML numbers the first four with light
/// and dark swapped: 0 = lt1, 1 = dk1, 2 = lt2, 3 = dk2, then accents and links.
pub fn slot_for_index(idx: u32) -> Option<usize> {
    let slot = match idx {
        0 => 1,
        1 => 0,
        2 => 3,
        3 => 2,
        4..=11 => idx as usize,
        _ => return None,
    };
    Some(slot)
}

/// The colour of theme index `idx` (file numbering) with `tint` (-1..1: darker..lighter)
/// applied to its HSL lightness. Unknown indexes give black.
pub fn resolve_theme_color(theme: &Theme, idx: u32, tint: f64) -> Color {
    let base = slot_for_index(idx)
        .and_then(|s| theme.colors.get(s).copied())
        .unwrap_or(Color::BLACK);
    apply_tint(base, tint)
}

/// Lighten (tint > 0) or darken (tint < 0) a colour in HSL space.
pub fn apply_tint(c: Color, tint: f64) -> Color {
    if !tint.is_finite() || tint == 0.0 {
        return c;
    }
    let tint = tint.clamp(-1.0, 1.0);
    let (h, s, l) = rgb_to_hsl(c);
    let l = if tint < 0.0 {
        l * (1.0 + tint)
    } else {
        l * (1.0 - tint) + tint
    };
    hsl_to_rgb(h, s, l.clamp(0.0, 1.0))
}

fn rgb_to_hsl(c: Color) -> (f64, f64, f64) {
    let r = f64::from(c.0) / 255.0;
    let g = f64::from(c.1) / 255.0;
    let b = f64::from(c.2) / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d.abs() < 1e-12 {
        return (0.0, 0.0, l);
    }
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let h = if (max - r).abs() < 1e-12 {
        ((g - b) / d).rem_euclid(6.0)
    } else if (max - g).abs() < 1e-12 {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h / 6.0, s, l)
}

fn hsl_to_rgb(h: f64, s: f64, l: f64) -> Color {
    let to = |v: f64| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    if s <= 0.0 {
        let v = to(l);
        return Color(v, v, v);
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let hue = |t: f64| {
        let t = t.rem_euclid(1.0);
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    Color(to(hue(h + 1.0 / 3.0)), to(hue(h)), to(hue(h - 1.0 / 3.0)))
}

fn hex(s: &str) -> Color {
    Color::from_hex(s).unwrap_or(Color::BLACK)
}

/// (name, 12 slot colours in scheme order).
const PALETTES: [(&str, [&str; 12]); 6] = [
    (
        "SheetPro",
        [
            "1F1F1F", "FFFFFF", "25384A", "EEF1EC", "217346", "2E6EBE", "E07B39", "8A5BB0",
            "C4424A", "2A9DA8", "1D6FB8", "7A4E9C",
        ],
    ),
    (
        "Harbor",
        [
            "102030", "FFFFFF", "1B3A57", "E4EEF4", "0B6E99", "13A3A3", "F2A541", "D1495B",
            "5C6BC0", "7FB069", "0B6E99", "5C6BC0",
        ],
    ),
    (
        "Meadow",
        [
            "1E2B1F", "FFFFFF", "2F4A2C", "F1F5E8", "5B8C3A", "A3B83A", "D9A441", "8C5E3C",
            "3E7C6B", "B5654A", "3E7C6B", "8C5E3C",
        ],
    ),
    (
        "Ember",
        [
            "2B1B17", "FFFFFF", "5A2E22", "FBF0E6", "C8553D", "F28F3B", "E0A030", "8E3B46",
            "588B8B", "2D3047", "C8553D", "8E3B46",
        ],
    ),
    (
        "Slate",
        [
            "202124", "FFFFFF", "3C4043", "F1F3F4", "4A6FA5", "9DA3A8", "E2856E", "59A96A",
            "B57EDC", "F2C14E", "4A6FA5", "B57EDC",
        ],
    ),
    (
        "Orchid",
        [
            "26162B", "FFFFFF", "4B2C5E", "F6EEF8", "9B4DCA", "D45D9E", "4DA1C9", "F08A5D",
            "6BAA75", "C9A227", "9B4DCA", "D45D9E",
        ],
    ),
];

/// (name, heading font, body font).
pub const FONT_SCHEMES: [(&str, &str, &str); 7] = [
    ("SheetPro", "Cambria", "Calibri"),
    ("Harbor", "Georgia", "Verdana"),
    ("Meadow", "Trebuchet MS", "Trebuchet MS"),
    ("Ember", "Arial Black", "Arial"),
    ("Slate", "Segoe UI", "Segoe UI"),
    ("Orchid", "Century Gothic", "Century Gothic"),
    ("Classic", "Times New Roman", "Arial"),
];

/// Built-in colour schemes: (name, colours).
pub fn builtin_color_schemes() -> Vec<(&'static str, [Color; 12])> {
    PALETTES
        .iter()
        .map(|(n, cols)| (*n, cols.map(hex)))
        .collect()
}

/// Built-in themes (colours and fonts of the same name).
pub fn builtin_themes() -> Vec<Theme> {
    builtin_color_schemes()
        .into_iter()
        .map(|(name, colors)| {
            let (_, major, minor) = FONT_SCHEMES
                .iter()
                .find(|f| f.0 == name)
                .copied()
                .unwrap_or(("SheetPro", "Cambria", "Calibri"));
            Theme {
                name: name.into(),
                colors_name: name.into(),
                colors,
                fonts_name: name.into(),
                major_font: major.into(),
                minor_font: minor.into(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_mapping_swaps_light_and_dark() {
        let t = Theme::default();
        assert_eq!(resolve_theme_color(&t, 1, 0.0), t.colors[0]);
        assert_eq!(resolve_theme_color(&t, 0, 0.0), t.colors[1]);
        assert_eq!(resolve_theme_color(&t, 4, 0.0), t.colors[4]);
        assert_eq!(resolve_theme_color(&t, 11, 0.0), t.colors[11]);
        assert_eq!(resolve_theme_color(&t, 99, 0.0), Color::BLACK);
    }

    #[test]
    fn tints_lighten_and_darken() {
        let c = Color(0x44, 0x72, 0xC4);
        assert_eq!(apply_tint(c, 0.0), c);
        let light = apply_tint(c, 0.8);
        let dark = apply_tint(c, -0.5);
        assert!(light.0 > c.0 && light.1 > c.1 && light.2 > c.2);
        assert!(dark.0 < c.0 && dark.1 < c.1 && dark.2 < c.2);
        assert_eq!(apply_tint(Color::BLACK, 1.0), Color::WHITE);
        assert_eq!(apply_tint(Color::WHITE, -1.0), Color::BLACK);
        // Greys stay grey; hostile tints are clamped.
        let g = apply_tint(Color(128, 128, 128), 0.5);
        assert!(g.0 == g.1 && g.1 == g.2);
        assert_eq!(apply_tint(c, f64::NAN), c);
        let _ = apply_tint(c, 1e9);
    }

    #[test]
    fn hsl_round_trips() {
        for c in [
            Color(0x21, 0x73, 0x46),
            Color(255, 0, 0),
            Color(10, 200, 250),
        ] {
            let (h, s, l) = rgb_to_hsl(c);
            assert_eq!(hsl_to_rgb(h, s, l), c);
        }
    }

    #[test]
    fn builtins_are_distinct_and_complete() {
        let t = builtin_themes();
        assert_eq!(t.len(), 6);
        assert_eq!(Theme::default().name, "SheetPro");
        let mut names: Vec<_> = t.iter().map(|x| x.name.clone()).collect();
        names.dedup();
        assert_eq!(names.len(), 6);
        assert!(t.iter().all(|x| x.colors[1] == Color::WHITE));
        let r = ThemeColor {
            idx: 4,
            tint: 0,
            rgb: Color(1, 2, 3),
        };
        assert!(r.applies_to(Some(Color(1, 2, 3))));
        assert!(!r.applies_to(None));
    }
}
