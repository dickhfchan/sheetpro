//! Floating drawing objects other than charts: pictures, shapes, text boxes and connector
//! lines, plus the data they carry (embedded image bytes, DrawingML colours, text). Pure
//! data; geometry lives in `sp_render::objects`, file I/O in `sp_io::xlsx_drawings`.
//!
//! Objects share the cell anchor type charts use (`chart::Anchor`) and add how they follow
//! the cells (move and size / move only / fixed) and which DrawingML anchor element they
//! were stored in (two-cell, one-cell, absolute).

use crate::Color;
use crate::chart::Anchor;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Largest image file accepted (bytes).
pub const MAX_IMAGE_BYTES: usize = 64 << 20;
/// Most objects per sheet.
pub const MAX_OBJECTS: usize = 10_000;
/// Longest text stored in one shape (characters).
pub const MAX_TEXT_CHARS: usize = 32_767;

// ---------------------------------------------------------------------------------------
// Anchors

/// The DrawingML anchor element an object is stored in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AnchorKind {
    /// `twoCellAnchor`: both corners on cells (`ObjectAnchor::cells`).
    #[default]
    TwoCell,
    /// `oneCellAnchor`: top-left on a cell, size in `ext`.
    OneCell,
    /// `absoluteAnchor`: position `pos` from the sheet's top-left, size in `ext`.
    Absolute,
}

/// How an object follows cell edits (the two-cell anchor's `editAs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Placement {
    /// Move and size with cells (`twoCell`).
    #[default]
    MoveAndSize,
    /// Move but don't size with cells (`oneCell`).
    Move,
    /// Don't move or size with cells (`absolute`).
    Fixed,
}

impl Placement {
    pub fn name(self) -> &'static str {
        match self {
            Placement::MoveAndSize => "move_and_size",
            Placement::Move => "move",
            Placement::Fixed => "fixed",
        }
    }

    pub fn from_name(s: &str) -> Option<Placement> {
        Some(match s.to_ascii_lowercase().as_str() {
            "move_and_size" | "twocell" | "two_cell" => Placement::MoveAndSize,
            "move" | "onecell" | "one_cell" => Placement::Move,
            "fixed" | "absolute" => Placement::Fixed,
            _ => return None,
        })
    }
}

/// Where a floating object sits.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct ObjectAnchor {
    pub kind: AnchorKind,
    pub placement: Placement,
    /// Cell corners (`to` only meaningful for `TwoCell`; `from` unused for `Absolute`).
    pub cells: Anchor,
    /// Size in px at 100% zoom (`OneCell` / `Absolute`).
    #[serde(default)]
    pub ext: (f64, f64),
    /// Top-left in px from the sheet's top-left corner (`Absolute`).
    #[serde(default)]
    pub pos: (f64, f64),
}

// ---------------------------------------------------------------------------------------
// Colours

/// One DrawingML colour transform (`lumMod`, `lumOff`, `shade`, `tint`, `alpha`, `satMod`…),
/// value in 1/1000 percent as stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ColorMod {
    pub name: String,
    pub val: i64,
}

/// A DrawingML colour: literal RGB, a theme scheme slot (`accent1`, `tx1`…), or a system
/// colour (`windowText`) with its last known value. Resolve with [`resolve_color`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ObjColor {
    Rgb(Color, Vec<ColorMod>),
    Scheme(String, Vec<ColorMod>),
    System(String, Color, Vec<ColorMod>),
}

impl ObjColor {
    pub fn rgb(c: Color) -> ObjColor {
        ObjColor::Rgb(c, Vec::new())
    }

    pub fn scheme(name: &str) -> ObjColor {
        ObjColor::Scheme(name.to_string(), Vec::new())
    }

    pub fn mods(&self) -> &[ColorMod] {
        match self {
            ObjColor::Rgb(_, m) | ObjColor::Scheme(_, m) | ObjColor::System(_, _, m) => m,
        }
    }
}

/// The default theme's colour scheme (Office 2013+ "Office" theme values, public in every
/// default `theme1.xml`), used until workbook themes are modelled.
pub fn default_scheme_color(name: &str) -> Option<Color> {
    Some(match name {
        "dk1" | "tx1" => Color(0, 0, 0),
        "lt1" | "bg1" => Color(255, 255, 255),
        "dk2" | "tx2" => Color(0x44, 0x54, 0x6A),
        "lt2" | "bg2" => Color(0xE7, 0xE6, 0xE6),
        "accent1" => Color(0x44, 0x72, 0xC4),
        "accent2" => Color(0xED, 0x7D, 0x31),
        "accent3" => Color(0xA5, 0xA5, 0xA5),
        "accent4" => Color(0xFF, 0xC0, 0x00),
        "accent5" => Color(0x5B, 0x9B, 0xD5),
        "accent6" => Color(0x70, 0xAD, 0x47),
        "hlink" => Color(0x05, 0x63, 0xC1),
        "folHlink" => Color(0x95, 0x4F, 0x72),
        _ => return None,
    })
}

/// Theme lookup for scheme colours. The single place theme colour references resolve:
/// point this at the workbook theme once it is modelled.
pub fn theme_color(name: &str) -> Color {
    default_scheme_color(name).unwrap_or(Color::BLACK)
}

fn to_hsl(c: Color) -> (f64, f64, f64) {
    let (r, g, b) = (
        f64::from(c.0) / 255.0,
        f64::from(c.1) / 255.0,
        f64::from(c.2) / 255.0,
    );
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    if (max - min).abs() < 1e-12 {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let h = if (max - r).abs() < 1e-12 {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if (max - g).abs() < 1e-12 {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h / 6.0, s, l)
}

fn from_hsl(h: f64, s: f64, l: f64) -> Color {
    let ch = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    if s <= 0.0 {
        return Color(ch(l), ch(l), ch(l));
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let hue = |mut t: f64| {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
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
    Color(ch(hue(h + 1.0 / 3.0)), ch(hue(h)), ch(hue(h - 1.0 / 3.0)))
}

/// The sRGB value of a colour after its transforms, and its opacity (0..=1).
pub fn resolve_color(c: &ObjColor) -> (Color, f32) {
    let base = match c {
        ObjColor::Rgb(rgb, _) => *rgb,
        ObjColor::Scheme(name, _) => theme_color(name),
        ObjColor::System(_, last, _) => *last,
    };
    let mut out = base;
    let mut alpha = 1.0f32;
    let (mut lum_mod, mut lum_off) = (None, None);
    for m in c.mods() {
        let f = (m.val as f64 / 100_000.0).clamp(-10.0, 10.0);
        match m.name.as_str() {
            "lumMod" => lum_mod = Some(f),
            "lumOff" => lum_off = Some(f),
            "shade" => {
                let s = |v: u8| (f64::from(v) * f.clamp(0.0, 1.0)).round() as u8;
                out = Color(s(out.0), s(out.1), s(out.2));
            }
            "tint" => {
                let t = |v: u8| {
                    let v = f64::from(v);
                    (v + (255.0 - v) * (1.0 - f.clamp(0.0, 1.0))).round() as u8
                };
                out = Color(t(out.0), t(out.1), t(out.2));
            }
            "alpha" => alpha = f.clamp(0.0, 1.0) as f32,
            _ => {}
        }
    }
    if lum_mod.is_some() || lum_off.is_some() {
        let (h, s, l) = to_hsl(out);
        let l2 = l * lum_mod.unwrap_or(1.0) + lum_off.unwrap_or(0.0);
        out = from_hsl(h, s, l2.clamp(0.0, 1.0));
    }
    (out, alpha)
}

// ---------------------------------------------------------------------------------------
// Fill, line, text

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Fill {
    /// Not specified on the shape: the style reference (if any) decides.
    #[default]
    Default,
    /// `noFill`.
    None,
    /// `solidFill`.
    Solid(ObjColor),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct LineFormat {
    /// Line colour (`Default` = from the style reference).
    pub fill: Fill,
    /// Width in EMU (`None` = from the style reference / 0.75 pt).
    pub width: Option<i64>,
    /// Preset dash name (`dash`, `sysDot`…); `None` = solid.
    pub dash: Option<String>,
    /// Arrow head at the start / end (`triangle`, `arrow`, `stealth`, `oval`, `diamond`).
    pub head: Option<String>,
    pub tail: Option<String>,
    /// Anything the `<a:ln>` element carries that is only attributes we keep as written
    /// (`cap`, `cmpd`, `algn`) plus a join child name (`round`, `bevel`, `miter`).
    #[serde(default)]
    pub attrs: Vec<(String, String)>,
    #[serde(default)]
    pub join: Option<String>,
}

impl LineFormat {
    pub fn is_default(&self) -> bool {
        *self == LineFormat::default()
    }
}

/// A shape's theme style references (`xdr:style`): matrix index + colour for line, fill,
/// effect, and font (`major`/`minor`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StyleRefs {
    pub line: (u32, ObjColor),
    pub fill: (u32, ObjColor),
    pub effect: (u32, ObjColor),
    pub font: (String, ObjColor),
}

impl StyleRefs {
    /// What the target app gives a newly inserted shape: accent-1 fill, darker outline,
    /// light text.
    pub fn shape_default() -> StyleRefs {
        StyleRefs {
            line: (
                2,
                ObjColor::Scheme(
                    "accent1".into(),
                    vec![ColorMod {
                        name: "shade".into(),
                        val: 50_000,
                    }],
                ),
            ),
            fill: (1, ObjColor::scheme("accent1")),
            effect: (0, ObjColor::scheme("accent1")),
            font: ("minor".into(), ObjColor::scheme("lt1")),
        }
    }

    /// A connector line's default: accent-1 line, no fill, dark text.
    pub fn line_default() -> StyleRefs {
        StyleRefs {
            line: (1, ObjColor::scheme("accent1")),
            fill: (0, ObjColor::scheme("accent1")),
            effect: (0, ObjColor::scheme("accent1")),
            font: ("minor".into(), ObjColor::scheme("tx1")),
        }
    }
}

/// Character formatting of a text run (`a:rPr`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RunFont {
    /// Size in hundredths of a point (`sz`).
    pub size: Option<u32>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub color: Option<ObjColor>,
    /// Latin typeface (`a:latin typeface`).
    pub latin: Option<String>,
    /// Other `rPr` attributes kept as written (`lang`, `baseline`…).
    #[serde(default)]
    pub attrs: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TextRun {
    pub text: String,
    pub font: RunFont,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Paragraph {
    /// `algn`: `l`, `ctr`, `r`, `just`, `dist`; `None` = left.
    pub align: Option<String>,
    pub runs: Vec<TextRun>,
    /// End-of-paragraph run properties (size of an empty line).
    pub end: Option<RunFont>,
}

impl Paragraph {
    pub fn text(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }
}

/// A shape's text (`xdr:txBody`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TextBody {
    pub paras: Vec<Paragraph>,
    /// `a:bodyPr` attributes as written (`anchor`, `wrap`, insets, overflow…).
    pub body_attrs: Vec<(String, String)>,
    /// Autofit child of `bodyPr`: `None`, `noAutofit`, `spAutoFit`, `normAutofit`.
    pub autofit: Option<String>,
}

impl TextBody {
    /// Plain text, paragraphs joined by newlines.
    pub fn plain(&self) -> String {
        self.paras
            .iter()
            .map(Paragraph::text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Text body for `text`, one paragraph per line, all runs formatted with `font`.
    pub fn from_plain(text: &str, font: &RunFont, align: Option<&str>) -> TextBody {
        let paras = text
            .split('\n')
            .map(|line| Paragraph {
                align: align.map(str::to_string),
                runs: if line.is_empty() {
                    Vec::new()
                } else {
                    vec![TextRun {
                        text: line.to_string(),
                        font: font.clone(),
                    }]
                },
                end: Some(font.clone()),
            })
            .collect();
        TextBody {
            paras,
            body_attrs: Vec::new(),
            autofit: None,
        }
    }

    pub fn body_attr(&self, k: &str) -> Option<&str> {
        self.body_attrs
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.as_str())
    }

    pub fn set_body_attr(&mut self, k: &str, v: &str) {
        match self.body_attrs.iter_mut().find(|(n, _)| n == k) {
            Some(slot) => slot.1 = v.to_string(),
            None => self.body_attrs.push((k.to_string(), v.to_string())),
        }
    }
}

// ---------------------------------------------------------------------------------------
// Images

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ImageFormat {
    #[default]
    Png,
    Jpeg,
    Gif,
    Bmp,
}

impl ImageFormat {
    /// Recognise an image by its signature bytes.
    pub fn sniff(bytes: &[u8]) -> Option<ImageFormat> {
        if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
            Some(ImageFormat::Png)
        } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
            Some(ImageFormat::Jpeg)
        } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            Some(ImageFormat::Gif)
        } else if bytes.starts_with(b"BM") {
            Some(ImageFormat::Bmp)
        } else {
            None
        }
    }

    pub fn ext(self) -> &'static str {
        match self {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg => "jpeg",
            ImageFormat::Gif => "gif",
            ImageFormat::Bmp => "bmp",
        }
    }

    pub fn content_type(self) -> &'static str {
        match self {
            ImageFormat::Png => "image/png",
            ImageFormat::Jpeg => "image/jpeg",
            ImageFormat::Gif => "image/gif",
            ImageFormat::Bmp => "image/bmp",
        }
    }
}

/// An embedded image file, kept as its original bytes (decoded only for drawing).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ImageData {
    pub bytes: Arc<[u8]>,
    pub format: ImageFormat,
    /// Pixel size from the file header (0 when unknown).
    pub width: u32,
    pub height: u32,
}

impl ImageData {
    /// Stable key for caches: the byte buffer's address and length.
    pub fn key(&self) -> (usize, usize) {
        (self.bytes.as_ptr() as usize, self.bytes.len())
    }
}

// ---------------------------------------------------------------------------------------
// Objects

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Picture {
    pub image: ImageData,
    /// Picture border (`spPr/ln`).
    pub line: LineFormat,
    /// `picLocks` attributes as written (`noChangeAspect` …).
    pub locks: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Shape {
    /// DrawingML preset geometry name (`rect`, `roundRect`, `ellipse`, `rightArrow`…).
    pub preset: String,
    /// Adjust values (`a:gd name="adj" fmla="val N"`).
    pub adjust: Vec<(String, i64)>,
    pub fill: Fill,
    pub line: LineFormat,
    pub style: Option<StyleRefs>,
    pub text: Option<TextBody>,
    /// Stored as a text box (`cNvSpPr txBox="1"`).
    pub text_box: bool,
    /// Stored as a connector (`xdr:cxnSp`): lines and arrows.
    pub connector: bool,
    /// Cell whose value the shape shows (`textlink`, formula text without `=`).
    #[serde(default)]
    pub text_link: String,
    /// `spLocks` / `cxnSpLocks` attributes as written.
    #[serde(default)]
    pub locks: Vec<(String, String)>,
}

impl Shape {
    pub fn adj(&self, name: &str) -> Option<i64> {
        self.adjust.iter().find(|(n, _)| n == name).map(|(_, v)| *v)
    }
}

// Objects are few per sheet; boxing the shape would only add indirection.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ObjectKind {
    Picture(Picture),
    Shape(Shape),
}

impl Default for ObjectKind {
    fn default() -> Self {
        ObjectKind::Shape(Shape::default())
    }
}

/// One floating object. Its index in `Sheet::objects` is its z-order (later = on top).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct DrawObject {
    pub name: String,
    /// Alt text (`descr`).
    pub descr: String,
    /// Alt text title (`title`).
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub hidden: bool,
    pub anchor: ObjectAnchor,
    /// Clockwise rotation in degrees.
    pub rotation: f64,
    pub flip_h: bool,
    pub flip_v: bool,
    pub kind: ObjectKind,
    /// Charts drawn below this object (z-order between charts and objects sharing a
    /// drawing part: the object follows the first `after_charts` charts).
    #[serde(default)]
    pub after_charts: usize,
}

impl DrawObject {
    pub fn shape(&self) -> Option<&Shape> {
        match &self.kind {
            ObjectKind::Shape(s) => Some(s),
            ObjectKind::Picture(_) => None,
        }
    }

    pub fn shape_mut(&mut self) -> Option<&mut Shape> {
        match &mut self.kind {
            ObjectKind::Shape(s) => Some(s),
            ObjectKind::Picture(_) => None,
        }
    }

    pub fn picture(&self) -> Option<&Picture> {
        match &self.kind {
            ObjectKind::Picture(p) => Some(p),
            ObjectKind::Shape(_) => None,
        }
    }

    /// `picture`, `text_box`, `line` or `shape`.
    pub fn type_name(&self) -> &'static str {
        match &self.kind {
            ObjectKind::Picture(_) => "picture",
            ObjectKind::Shape(s) if s.text_box => "text_box",
            ObjectKind::Shape(s) if s.connector => "line",
            ObjectKind::Shape(_) => "shape",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniff_formats() {
        assert_eq!(
            ImageFormat::sniff(&[0x89, b'P', b'N', b'G', 13, 10, 26, 10, 0]),
            Some(ImageFormat::Png)
        );
        assert_eq!(
            ImageFormat::sniff(&[0xFF, 0xD8, 0xFF, 0xE0]),
            Some(ImageFormat::Jpeg)
        );
        assert_eq!(ImageFormat::sniff(b"GIF89a.."), Some(ImageFormat::Gif));
        assert_eq!(ImageFormat::sniff(b"BM...."), Some(ImageFormat::Bmp));
        assert_eq!(ImageFormat::sniff(b""), None);
        assert_eq!(ImageFormat::sniff(b"<svg"), None);
    }

    #[test]
    fn colors_resolve_with_mods() {
        let (c, a) = resolve_color(&ObjColor::scheme("accent1"));
        assert_eq!(c, Color(0x44, 0x72, 0xC4));
        assert_eq!(a, 1.0);
        // 50% shade halves each channel.
        let (c, _) = resolve_color(&ObjColor::Rgb(
            Color(200, 100, 50),
            vec![ColorMod {
                name: "shade".into(),
                val: 50_000,
            }],
        ));
        assert_eq!(c, Color(100, 50, 25));
        // lumMod 50% darkens white to mid grey.
        let (c, _) = resolve_color(&ObjColor::Scheme(
            "bg1".into(),
            vec![ColorMod {
                name: "lumMod".into(),
                val: 50_000,
            }],
        ));
        assert_eq!(c, Color(128, 128, 128));
        let (_, a) = resolve_color(&ObjColor::Rgb(
            Color::BLACK,
            vec![ColorMod {
                name: "alpha".into(),
                val: 40_000,
            }],
        ));
        assert!((a - 0.4).abs() < 1e-6);
        // Hostile values clamp.
        let (c, _) = resolve_color(&ObjColor::Rgb(
            Color(10, 10, 10),
            vec![ColorMod {
                name: "lumOff".into(),
                val: i64::MAX,
            }],
        ));
        assert_eq!(c, Color(255, 255, 255));
        assert_eq!(theme_color("nonsense"), Color::BLACK);
    }

    #[test]
    fn hsl_round_trip() {
        for c in [
            Color(0x44, 0x72, 0xC4),
            Color(255, 0, 0),
            Color(0, 128, 0),
            Color(12, 34, 56),
        ] {
            let (h, s, l) = to_hsl(c);
            assert_eq!(from_hsl(h, s, l), c);
        }
    }

    #[test]
    fn text_body_plain() {
        let t = TextBody::from_plain("a\n\nb", &RunFont::default(), Some("ctr"));
        assert_eq!(t.paras.len(), 3);
        assert_eq!(t.plain(), "a\n\nb");
        assert_eq!(t.paras[0].align.as_deref(), Some("ctr"));
        let mut t = t;
        t.set_body_attr("anchor", "ctr");
        t.set_body_attr("anchor", "b");
        assert_eq!(t.body_attr("anchor"), Some("b"));
        assert_eq!(Placement::from_name("fixed"), Some(Placement::Fixed));
        assert_eq!(
            Placement::from_name(Placement::Move.name()),
            Some(Placement::Move)
        );
    }
}
