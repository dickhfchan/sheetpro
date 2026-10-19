//! Cell styles, interned in a `StyleTable`.

use serde::{Deserialize, Serialize};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default, PartialOrd, Ord,
)]
pub struct StyleId(pub u32);

impl StyleId {
    pub const DEFAULT: StyleId = StyleId(0);
}

/// sRGB colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Color(pub u8, pub u8, pub u8);

impl Color {
    pub const BLACK: Color = Color(0, 0, 0);
    pub const WHITE: Color = Color(255, 255, 255);

    /// `FF112233` / `112233` / `#112233`.
    pub fn from_hex(s: &str) -> Option<Color> {
        let s = s.trim_start_matches('#');
        let s = if s.len() == 8 { s.get(2..)? } else { s };
        if s.len() != 6 {
            return None;
        }
        let p = |i: usize| u8::from_str_radix(s.get(i..i + 2)?, 16).ok();
        Some(Color(p(0)?, p(2)?, p(4)?))
    }

    pub fn to_argb_hex(self) -> String {
        format!("FF{:02X}{:02X}{:02X}", self.0, self.1, self.2)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum HAlign {
    #[default]
    General,
    Left,
    Center,
    Right,
    Fill,
    Justify,
    CenterAcross,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum VAlign {
    Top,
    Center,
    #[default]
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum BorderStyle {
    #[default]
    None,
    Thin,
    Medium,
    Thick,
    Dashed,
    Dotted,
    Double,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct Border {
    pub style: BorderStyle,
    pub color: Option<Color>,
}

/// Every visual attribute of a cell. `Default` is the workbook's normal style.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Style {
    pub font_name: String,
    /// Size in half-points scaled by 10 to stay hashable (11pt = 110).
    pub font_size_tenths: u32,
    pub bold: bool,
    pub italic: bool,
    pub underline: u8,
    pub strike: bool,
    pub font_color: Option<Color>,
    pub fill: Option<Color>,
    pub h_align: HAlign,
    pub v_align: VAlign,
    pub wrap: bool,
    pub indent: u8,
    pub rotation: i16,
    pub num_fmt: String,
    pub border_left: Border,
    pub border_right: Border,
    pub border_top: Border,
    pub border_bottom: Border,
    pub locked: bool,
    pub hidden_formula: bool,
    /// Cell checkbox: TRUE/FALSE values draw as a checked / unchecked box.
    #[serde(default)]
    pub checkbox: bool,
    /// Theme reference behind `font_color` (applies while `font_color` is unchanged).
    #[serde(default)]
    pub font_theme: Option<crate::theme::ThemeColor>,
    /// Theme reference behind `fill`.
    #[serde(default)]
    pub fill_theme: Option<crate::theme::ThemeColor>,
}

pub const DEFAULT_FONT: &str = "Calibri";

impl Default for Style {
    fn default() -> Self {
        Style {
            font_name: DEFAULT_FONT.to_string(),
            font_size_tenths: 110,
            bold: false,
            italic: false,
            underline: 0,
            strike: false,
            font_color: None,
            fill: None,
            h_align: HAlign::General,
            v_align: VAlign::Bottom,
            wrap: false,
            indent: 0,
            rotation: 0,
            num_fmt: "General".to_string(),
            border_left: Border::default(),
            border_right: Border::default(),
            border_top: Border::default(),
            border_bottom: Border::default(),
            locked: true,
            hidden_formula: false,
            checkbox: false,
            font_theme: None,
            fill_theme: None,
        }
    }
}

impl Style {
    pub fn font_size(&self) -> f64 {
        f64::from(self.font_size_tenths) / 10.0
    }
}

/// Interning table: equal styles share an id. Id 0 is always the default style.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StyleTable {
    styles: Vec<Style>,
}

impl Default for StyleTable {
    fn default() -> Self {
        StyleTable {
            styles: vec![Style::default()],
        }
    }
}

impl StyleTable {
    pub fn get(&self, id: StyleId) -> &Style {
        static FALLBACK: std::sync::OnceLock<Style> = std::sync::OnceLock::new();
        self.styles
            .get(id.0 as usize)
            .unwrap_or_else(|| FALLBACK.get_or_init(Style::default))
    }

    pub fn intern(&mut self, s: Style) -> StyleId {
        if let Some(i) = self.styles.iter().position(|x| *x == s) {
            return StyleId(i as u32);
        }
        self.styles.push(s);
        StyleId((self.styles.len() - 1) as u32)
    }

    /// Apply `f` to a copy of style `id` and intern the result.
    pub fn derive(&mut self, id: StyleId, f: impl FnOnce(&mut Style)) -> StyleId {
        let mut s = self.get(id).clone();
        f(&mut s);
        self.intern(s)
    }

    pub fn len(&self) -> usize {
        self.styles.len()
    }

    /// Change every style in place (ids keep pointing at the changed styles).
    pub fn update_all(&mut self, mut f: impl FnMut(&mut Style)) {
        for s in &mut self.styles {
            f(s);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.styles.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (StyleId, &Style)> {
        self.styles
            .iter()
            .enumerate()
            .map(|(i, s)| (StyleId(i as u32), s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interning() {
        let mut t = StyleTable::default();
        let b = t.derive(StyleId::DEFAULT, |s| s.bold = true);
        let b2 = t.derive(StyleId::DEFAULT, |s| s.bold = true);
        assert_eq!(b, b2);
        assert_eq!(t.len(), 2);
        assert_eq!(t.derive(b, |s| s.bold = false), StyleId::DEFAULT);
        assert!(t.get(StyleId(999)).font_name == DEFAULT_FONT);
    }

    #[test]
    fn colors() {
        assert_eq!(Color::from_hex("FF102030"), Some(Color(16, 32, 48)));
        assert_eq!(Color(16, 32, 48).to_argb_hex(), "FF102030");
        assert_eq!(Color::from_hex("zz"), None);
    }
}
