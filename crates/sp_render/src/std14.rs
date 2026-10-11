//! The PDF standard-14 text faces (Helvetica, Times, Courier and their bold/italic
//! variants): glyph advance widths for layout and the WinAnsi (Windows-1252) encoding the PDF
//! writer uses for them. Printing measures text with these so wrap, alignment and overflow on
//! paper match what a PDF viewer draws.
//!
//! Widths are in 1/1000 em. The printable-ASCII tables hold each face's published metrics;
//! other WinAnsi characters reuse the width of the closest ASCII glyph (accented letters take
//! their base letter), which is within a few percent. Times bold-italic reuses Times bold.
//!
//! **Fallback for characters outside WinAnsi** (CJK, Greek, Cyrillic, emoji…): they are
//! written as `?` (see [`to_winansi`]) because the standard faces carry no such glyphs and
//! SheetPro does not embed fonts. Measuring uses the same substitution so layout matches.

/// Font family of a standard face.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Family {
    Helvetica,
    Times,
    Courier,
}

/// One of the twelve text faces (Symbol/ZapfDingbats are not used).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Face {
    pub family: Family,
    pub bold: bool,
    pub italic: bool,
}

impl Default for Face {
    fn default() -> Self {
        Face {
            family: Family::Helvetica,
            bold: false,
            italic: false,
        }
    }
}

/// Font names (lowercase fragments) printed with Times.
const SERIF_HINTS: [&str; 8] = [
    "times", "cambria", "georgia", "garamond", "serif", "book", "palatino", "century",
];
/// Font names printed with Courier.
const MONO_HINTS: [&str; 7] = [
    "courier", "consolas", "mono", "menlo", "monaco", "console", "typewriter",
];

impl Face {
    /// The standard face standing in for a cell font name.
    pub fn for_font(name: &str, bold: bool, italic: bool) -> Face {
        let lower = name.to_ascii_lowercase();
        let family = if MONO_HINTS.iter().any(|m| lower.contains(m)) {
            Family::Courier
        } else if SERIF_HINTS.iter().any(|m| lower.contains(m)) && !lower.contains("sans") {
            Family::Times
        } else {
            Family::Helvetica
        };
        Face {
            family,
            bold,
            italic,
        }
    }

    /// 0..12, stable: family × 4 + bold × 2 + italic.
    pub fn index(self) -> usize {
        let f = match self.family {
            Family::Helvetica => 0,
            Family::Times => 1,
            Family::Courier => 2,
        };
        f * 4 + usize::from(self.bold) * 2 + usize::from(self.italic)
    }

    /// All twelve faces in [`Face::index`] order.
    pub fn all() -> Vec<Face> {
        let mut v = Vec::with_capacity(12);
        for family in [Family::Helvetica, Family::Times, Family::Courier] {
            for bold in [false, true] {
                for italic in [false, true] {
                    v.push(Face {
                        family,
                        bold,
                        italic,
                    });
                }
            }
        }
        v
    }

    /// The PDF `BaseFont` name.
    pub fn base_font(self) -> &'static str {
        match (self.family, self.bold, self.italic) {
            (Family::Helvetica, false, false) => "Helvetica",
            (Family::Helvetica, true, false) => "Helvetica-Bold",
            (Family::Helvetica, false, true) => "Helvetica-Oblique",
            (Family::Helvetica, true, true) => "Helvetica-BoldOblique",
            (Family::Times, false, false) => "Times-Roman",
            (Family::Times, true, false) => "Times-Bold",
            (Family::Times, false, true) => "Times-Italic",
            (Family::Times, true, true) => "Times-BoldItalic",
            (Family::Courier, false, false) => "Courier",
            (Family::Courier, true, false) => "Courier-Bold",
            (Family::Courier, false, true) => "Courier-Oblique",
            (Family::Courier, true, true) => "Courier-BoldOblique",
        }
    }

    /// Ascent and descent (positive) as fractions of the font size.
    pub fn ascent_descent(self) -> (f32, f32) {
        match self.family {
            Family::Helvetica => (0.718, 0.207),
            Family::Times => (0.683, 0.217),
            Family::Courier => (0.629, 0.157),
        }
    }
}

// Widths of ' ' (0x20) through '~' (0x7E).
#[rustfmt::skip]
const HELVETICA: [u16; 95] = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278,
    556, 556, 556, 556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556,
    1015, 667, 667, 722, 722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778,
    667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 278, 278, 278, 469, 556,
    333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500, 222, 833, 556, 556,
    556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584,
];
#[rustfmt::skip]
const HELVETICA_BOLD: [u16; 95] = [
    278, 333, 474, 556, 556, 889, 722, 238, 333, 333, 389, 584, 278, 333, 278, 278,
    556, 556, 556, 556, 556, 556, 556, 556, 556, 556, 333, 333, 584, 584, 584, 611,
    975, 722, 722, 722, 722, 667, 611, 778, 722, 278, 556, 722, 611, 833, 722, 778,
    667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 333, 278, 333, 584, 556,
    333, 556, 611, 556, 611, 556, 333, 611, 611, 278, 278, 556, 278, 889, 611, 611,
    611, 611, 389, 556, 333, 611, 556, 778, 556, 556, 500, 389, 280, 389, 584,
];
#[rustfmt::skip]
const TIMES: [u16; 95] = [
    250, 333, 408, 500, 500, 833, 778, 180, 333, 333, 500, 564, 250, 333, 250, 278,
    500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 278, 278, 564, 564, 564, 444,
    921, 722, 667, 667, 722, 611, 556, 722, 722, 333, 389, 722, 611, 889, 722, 722,
    556, 722, 667, 556, 611, 722, 722, 944, 722, 722, 611, 333, 278, 333, 469, 500,
    333, 444, 500, 444, 500, 444, 333, 500, 500, 278, 278, 500, 278, 778, 500, 500,
    500, 500, 333, 389, 278, 500, 500, 722, 500, 500, 444, 480, 200, 480, 541,
];
#[rustfmt::skip]
const TIMES_BOLD: [u16; 95] = [
    250, 333, 555, 500, 500, 1000, 833, 278, 333, 333, 500, 570, 250, 333, 250, 278,
    500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 333, 333, 570, 570, 570, 500,
    930, 722, 667, 722, 722, 667, 611, 778, 778, 389, 500, 778, 667, 944, 722, 778,
    611, 778, 722, 556, 667, 722, 722, 1000, 722, 722, 667, 333, 278, 333, 581, 500,
    333, 500, 556, 444, 556, 444, 333, 500, 556, 278, 333, 556, 278, 833, 556, 500,
    556, 556, 444, 389, 333, 556, 500, 722, 500, 500, 444, 394, 220, 394, 520,
];
#[rustfmt::skip]
const TIMES_ITALIC: [u16; 95] = [
    250, 333, 420, 500, 500, 833, 778, 214, 333, 333, 500, 675, 250, 333, 250, 278,
    500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 333, 333, 675, 675, 675, 500,
    920, 611, 611, 667, 722, 611, 611, 722, 722, 333, 444, 667, 556, 833, 667, 722,
    611, 722, 611, 500, 556, 722, 611, 833, 611, 556, 556, 389, 278, 389, 422, 500,
    333, 500, 500, 444, 500, 444, 278, 500, 500, 278, 278, 444, 278, 722, 500, 500,
    500, 500, 389, 389, 278, 500, 444, 667, 444, 444, 389, 400, 275, 400, 541,
];

/// WinAnsi code for `c` (`None` when the encoding has no such character).
pub fn winansi(c: char) -> Option<u8> {
    let u = u32::from(c);
    match u {
        0x20..=0x7E | 0xA0..=0xFF => u8::try_from(u).ok(),
        _ => Some(match c {
            '€' => 0x80,
            '‚' => 0x82,
            'ƒ' => 0x83,
            '„' => 0x84,
            '…' => 0x85,
            '†' => 0x86,
            '‡' => 0x87,
            'ˆ' => 0x88,
            '‰' => 0x89,
            'Š' => 0x8A,
            '‹' => 0x8B,
            'Œ' => 0x8C,
            'Ž' => 0x8E,
            '‘' => 0x91,
            '’' => 0x92,
            '“' => 0x93,
            '”' => 0x94,
            '•' => 0x95,
            '–' => 0x96,
            '—' => 0x97,
            '˜' => 0x98,
            '™' => 0x99,
            'š' => 0x9A,
            '›' => 0x9B,
            'œ' => 0x9C,
            'ž' => 0x9E,
            'Ÿ' => 0x9F,
            _ => return None,
        }),
    }
}

/// `c` as written: tabs become spaces, other characters outside WinAnsi become `?`.
pub fn printable(c: char) -> char {
    if c == '\t' {
        ' '
    } else if winansi(c).is_some() {
        c
    } else {
        '?'
    }
}

/// Encode text for a standard face (one byte per character, see [`printable`]).
pub fn to_winansi(text: &str) -> Vec<u8> {
    text.chars()
        .filter_map(|c| winansi(printable(c)))
        .collect()
}

/// The ASCII glyph whose width stands in for a non-ASCII WinAnsi character.
fn ascii_proxy(c: char) -> char {
    match c {
        'À'..='Å' => 'A',
        'Ç' => 'C',
        'È'..='Ë' => 'E',
        'Ì'..='Ï' => 'I',
        'Ð' => 'D',
        'Ñ' => 'N',
        'Ò'..='Ö' | 'Ø' | 'Œ' => 'O',
        'Ù'..='Ü' => 'U',
        'Ý' | 'Ÿ' => 'Y',
        'Þ' => 'P',
        'Š' => 'S',
        'Ž' => 'Z',
        'à'..='å' | 'ª' => 'a',
        'ç' => 'c',
        'è'..='ë' => 'e',
        'ì'..='ï' => 'i',
        'ð' | 'ò'..='ö' | 'ø' | 'º' | 'œ' => 'o',
        'ñ' => 'n',
        'ù'..='ü' | 'µ' => 'u',
        'ý' | 'ÿ' => 'y',
        'þ' => 'p',
        'ß' => 'b',
        'š' => 's',
        'ž' => 'z',
        'ƒ' => 'f',
        '¡' => '!',
        '¿' => '?',
        '‘' | '’' | '‚' | '´' | '¸' | '‹' | '›' => '\'',
        '“' | '”' | '„' | '¨' | '«' | '»' => '"',
        '¢' | '£' | '¤' | '¥' | '€' | '–' | '§' | '¶' | '†' | '‡' => '0',
        '±' | '×' | '÷' | '¬' | '¯' | '˜' | 'ˆ' => '+',
        '¦' => '|',
        '°' | '¹' | '²' | '³' | '•' | '·' => '*',
        '¼' | '½' | '¾' | '‰' | '…' | '—' | '™' | 'Æ' | '©' | '®' => 'W',
        'æ' => 'm',
        '\u{A0}' => ' ',
        _ => 'n',
    }
}

/// Advance width of `c` in 1/1000 em.
pub fn char_width(face: Face, c: char) -> u16 {
    if face.family == Family::Courier {
        return 600;
    }
    let c = printable(c);
    let c = if (' '..='~').contains(&c) {
        c
    } else {
        ascii_proxy(c)
    };
    let table: &[u16; 95] = match (face.family, face.bold, face.italic) {
        (Family::Helvetica, false, _) => &HELVETICA,
        (Family::Helvetica, true, _) => &HELVETICA_BOLD,
        (Family::Times, false, false) => &TIMES,
        (Family::Times, false, true) => &TIMES_ITALIC,
        (Family::Times, true, _) | (Family::Courier, _, _) => &TIMES_BOLD,
    };
    let i = (u32::from(c).saturating_sub(0x20)) as usize;
    table.get(i).copied().unwrap_or(556)
}

/// Width of `text` in points at `size` points.
pub fn text_width(face: Face, text: &str, size: f32) -> f32 {
    let em: u32 = text.chars().map(|c| u32::from(char_width(face, c))).sum();
    em as f32 * size / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths_and_faces() {
        let h = Face::default();
        assert_eq!(char_width(h, 'A'), 667);
        assert_eq!(char_width(h, 'i'), 222);
        assert_eq!(char_width(h, 'é'), char_width(h, 'e'));
        let c = Face::for_font("Courier New", false, false);
        assert_eq!(c.family, Family::Courier);
        assert!((text_width(c, "abcd", 10.0) - 24.0).abs() < 1e-4);
        assert_eq!(Face::for_font("Times New Roman", true, false).base_font(), "Times-Bold");
        assert_eq!(Face::for_font("Calibri", false, true).base_font(), "Helvetica-Oblique");
        assert_eq!(Face::for_font("Calibri", false, true).family, Family::Helvetica);
        let all = Face::all();
        for (i, f) in all.iter().enumerate() {
            assert_eq!(f.index(), i);
        }
    }

    #[test]
    fn winansi_mapping() {
        assert_eq!(winansi('€'), Some(0x80));
        assert_eq!(winansi('é'), Some(0xE9));
        assert_eq!(winansi('中'), None);
        assert_eq!(to_winansi("a中€"), vec![b'a', b'?', 0x80]);
        // Fallback glyphs measure like '?'.
        assert_eq!(char_width(Face::default(), '中'), char_width(Face::default(), '?'));
    }
}
