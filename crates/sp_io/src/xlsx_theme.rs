//! Workbook theme part (`xl/theme/theme1.xml`, DrawingML `a:theme`).
//!
//! - **Load**: the part stays in the workbook pass-through (byte-identical round trip);
//!   [`parse_theme`] reads its colour scheme and major/minor latin fonts into
//!   `Workbook::theme` before styles are read (styles resolve `theme`/`tint` through it).
//! - **Save**: [`prepare_for_save`] compares the model with the captured part. Unchanged:
//!   nothing to do. Changed: the captured part's `clrScheme` element is replaced and its
//!   font typefaces / scheme names are patched, everything else kept as loaded. No part
//!   (a new workbook) and a non-default theme: a complete theme part is generated.

use crate::xlsx_passthrough::{REL_NS, rel_kind};
use crate::xml::{esc, local, parse_dom};
use quick_xml::Reader;
use quick_xml::events::Event;
use sp_model::Workbook;
use sp_model::passthrough::{RawPart, RawRel};
use sp_model::theme::{SLOT_NAMES, Theme};
use sp_model::Color;
use std::borrow::Cow;
use std::sync::Arc;

pub const THEME_CT: &str = "application/vnd.openxmlformats-officedocument.theme+xml";
/// Largest theme part parsed.
const MAX_THEME_BYTES: usize = 8 << 20;

/// Index of the workbook's theme part in `wb.passthrough.parts`.
fn theme_part_index(wb: &Workbook) -> Option<usize> {
    let pt = &wb.passthrough;
    let rel = pt
        .rels
        .iter()
        .find(|r| !r.external && rel_kind(&r.rel_type) == "theme")?;
    pt.parts.iter().position(|p| p.path == rel.target)
}

/// Parse a theme part.
pub fn parse_theme(xml: &str) -> Option<Theme> {
    if xml.len() > MAX_THEME_BYTES {
        return None;
    }
    let root = parse_dom(xml).ok()?;
    if root.name != "theme" {
        return None;
    }
    let el = root.child("themeElements")?;
    let cs = el.child("clrScheme")?;
    let mut t = Theme::default();
    for (i, slot) in SLOT_NAMES.iter().enumerate() {
        let node = cs.child(slot)?;
        let c = node
            .child("srgbClr")
            .and_then(|n| n.attr("val"))
            .or_else(|| node.child("sysClr").and_then(|n| n.attr("lastClr")))
            .and_then(Color::from_hex)?;
        if let Some(dst) = t.colors.get_mut(i) {
            *dst = c;
        }
    }
    t.name = root.attr("name").unwrap_or("").to_string();
    t.colors_name = cs.attr("name").unwrap_or("").to_string();
    let fs = el.child("fontScheme");
    t.fonts_name = fs.and_then(|f| f.attr("name")).unwrap_or("").to_string();
    let face = |k: &str| {
        fs.and_then(|f| f.child(k))
            .and_then(|m| m.child("latin"))
            .and_then(|l| l.attr("typeface"))
            .map(str::to_string)
    };
    t.major_font = face("majorFont").unwrap_or_default();
    t.minor_font = face("minorFont").unwrap_or_default();
    Some(t)
}

/// The theme of a loaded workbook pass-through (the part related from the workbook).
pub fn theme_from_passthrough(pt: &sp_model::passthrough::Passthrough) -> Option<Theme> {
    let rel = pt
        .rels
        .iter()
        .find(|r| !r.external && rel_kind(&r.rel_type) == "theme")?;
    let part = pt.parts.iter().find(|p| p.path == rel.target)?;
    parse_theme(std::str::from_utf8(&part.data).ok()?)
}

/// Byte range and qualified-name prefix (`a:` or empty) of the first element named
/// `name` (local) inside `within` (byte range) of `xml`.
fn element_range(xml: &str, name: &str, within: (usize, usize)) -> Option<(usize, usize, String)> {
    let slice = xml.get(within.0..within.1)?;
    let mut r = Reader::from_str(slice);
    let mut depth = 0usize;
    let mut start: Option<(usize, String)> = None;
    loop {
        let before = usize::try_from(r.buffer_position()).ok()?;
        match r.read_event().ok()? {
            Event::Start(e) => {
                let q = e.name().into_inner().to_string();
                if start.is_some() {
                    depth += 1;
                } else if local(&q) == name {
                    let prefix = q.strip_suffix(name).unwrap_or("").to_string();
                    start = Some((before, prefix));
                    depth = 0;
                }
            }
            Event::Empty(e) => {
                let q = e.name().into_inner().to_string();
                if start.is_none() && local(&q) == name {
                    let end = usize::try_from(r.buffer_position()).ok()?;
                    let prefix = q.strip_suffix(name).unwrap_or("").to_string();
                    return Some((within.0 + before, within.0 + end, prefix));
                }
            }
            Event::End(_) => {
                if let Some((s, p)) = &start {
                    if depth == 0 {
                        let end = usize::try_from(r.buffer_position()).ok()?;
                        return Some((within.0 + s, within.0 + end, p.clone()));
                    }
                    depth -= 1;
                }
            }
            Event::Eof => return None,
            _ => {}
        }
    }
}

/// Replace the value of attribute `attr` in the start tag beginning at `tag_start`
/// (inserting it when absent).
fn set_attr(xml: &mut String, tag_start: usize, attr: &str, value: &str) {
    let Some(rest) = xml.get(tag_start..) else {
        return;
    };
    let Some(close) = rest.find('>') else { return };
    let tag = rest.get(..close).unwrap_or("");
    let needle = format!(" {attr}=\"");
    if let Some(i) = tag.find(&needle) {
        let vstart = tag_start + i + needle.len();
        if let Some(len) = xml.get(vstart..).and_then(|r| r.find('"')) {
            xml.replace_range(vstart..vstart + len, &esc(value));
        }
    } else {
        let name_end = tag
            .find(|c: char| c.is_whitespace() || c == '/')
            .unwrap_or(tag.len());
        xml.insert_str(tag_start + name_end, &format!(" {attr}=\"{}\"", esc(value)));
    }
}

fn clr_scheme_xml(p: &str, t: &Theme) -> String {
    let mut x = format!(r#"<{p}clrScheme name="{}">"#, esc(&t.colors_name));
    for (slot, c) in SLOT_NAMES.iter().zip(t.colors.iter()) {
        x.push_str(&format!(
            r#"<{p}{slot}><{p}srgbClr val="{:02X}{:02X}{:02X}"/></{p}{slot}>"#,
            c.0, c.1, c.2
        ));
    }
    x.push_str(&format!("</{p}clrScheme>"));
    x
}

/// The captured theme part rewritten for `t` (see module docs); `None` when the part's
/// structure is not one we can patch.
pub fn patch_theme(xml: &str, t: &Theme) -> Option<String> {
    let mut out = xml.to_string();
    let (cs, ce, p) = element_range(&out, "clrScheme", (0, out.len()))?;
    out.replace_range(cs..ce, &clr_scheme_xml(&p, t));
    for (font, face) in [("majorFont", &t.major_font), ("minorFont", &t.minor_font)] {
        let Some((fs, fe, _)) = element_range(&out, font, (0, out.len())) else {
            continue;
        };
        if let Some((ls, _, _)) = element_range(&out, "latin", (fs, fe)) {
            set_attr(&mut out, ls, "typeface", face);
        }
    }
    if let Some((fs, _, _)) = element_range(&out, "fontScheme", (0, out.len())) {
        set_attr(&mut out, fs, "name", &t.fonts_name);
    }
    if let Some((ts, _, _)) = element_range(&out, "theme", (0, out.len())) {
        set_attr(&mut out, ts, "name", &t.name);
    }
    // The result must still parse to the requested theme.
    (parse_theme(&out).as_ref() == Some(t)).then_some(out)
}

/// A complete theme part for `t`.
pub fn theme_xml(t: &Theme) -> String {
    let fill = |inner: &str| format!("<a:solidFill><a:schemeClr val=\"phClr\">{inner}</a:schemeClr></a:solidFill>");
    let line = |w: u32| {
        format!(
            r#"<a:ln w="{w}" cap="flat" cmpd="sng" algn="ctr"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:prstDash val="solid"/></a:ln>"#
        )
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" name="{name}"><a:themeElements>{clr}<a:fontScheme name="{fname}"><a:majorFont><a:latin typeface="{major}"/><a:ea typeface=""/><a:cs typeface=""/></a:majorFont><a:minorFont><a:latin typeface="{minor}"/><a:ea typeface=""/><a:cs typeface=""/></a:minorFont></a:fontScheme><a:fmtScheme name="{name}"><a:fillStyleLst>{f1}{f2}{f3}</a:fillStyleLst><a:lnStyleLst>{l1}{l2}{l3}</a:lnStyleLst><a:effectStyleLst><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle></a:effectStyleLst><a:bgFillStyleLst>{f1}{f2}{f3}</a:bgFillStyleLst></a:fmtScheme></a:themeElements><a:objectDefaults/><a:extraClrSchemeLst/></a:theme>"#,
        name = esc(&t.name),
        clr = clr_scheme_xml("a:", t),
        fname = esc(&t.fonts_name),
        major = esc(&t.major_font),
        minor = esc(&t.minor_font),
        f1 = fill(""),
        f2 = fill(r#"<a:tint val="60000"/>"#),
        f3 = fill(r#"<a:shade val="80000"/>"#),
        l1 = line(6350),
        l2 = line(12700),
        l3 = line(19050),
    )
}

/// The workbook to write: unchanged when the theme matches the captured part (or is the
/// default with no part), else with the theme part patched or added.
pub fn prepare_for_save(wb: &Workbook) -> Cow<'_, Workbook> {
    match theme_part_index(wb) {
        Some(i) => {
            let Some(part) = wb.passthrough.parts.get(i) else {
                return Cow::Borrowed(wb);
            };
            let Ok(text) = std::str::from_utf8(&part.data) else {
                return Cow::Borrowed(wb);
            };
            match parse_theme(text) {
                Some(t) if t == wb.theme => return Cow::Borrowed(wb),
                // A part we could not read stays as it is unless the theme was changed.
                None if wb.theme == Theme::default() => return Cow::Borrowed(wb),
                _ => {}
            }
            let new = patch_theme(text, &wb.theme).unwrap_or_else(|| theme_xml(&wb.theme));
            let mut out = wb.clone();
            if let Some(p) = out.passthrough.parts.get_mut(i) {
                p.data = Arc::from(new.into_bytes());
            }
            Cow::Owned(out)
        }
        None => {
            if wb.theme == Theme::default() {
                return Cow::Borrowed(wb);
            }
            let mut out = wb.clone();
            let taken = |p: &str| {
                wb.passthrough.parts.iter().any(|x| x.path.eq_ignore_ascii_case(p))
                    || wb.sheets.iter().any(|s| s.passthrough.parts.iter().any(|x| x.path.eq_ignore_ascii_case(p)))
            };
            let path = (1..1000)
                .map(|k| format!("xl/theme/theme{k}.xml"))
                .find(|p| !taken(p))
                .unwrap_or_else(|| "xl/theme/themeSp.xml".into());
            let id = (1..1000)
                .map(|k| format!("rIdTheme{k}"))
                .find(|id| !wb.passthrough.rels.iter().any(|r| &r.id == id))
                .unwrap_or_else(|| "rIdThemeSp".into());
            out.passthrough.rels.push(RawRel {
                id,
                rel_type: format!("{REL_NS}/theme"),
                target: path.clone(),
                external: false,
            });
            out.passthrough.parts.push(RawPart {
                path,
                content_type: THEME_CT.into(),
                data: Arc::from(theme_xml(&wb.theme).into_bytes()),
                rels: Vec::new(),
            });
            Cow::Owned(out)
        }
    }
}

/// `<color …/>` for a style colour: the theme reference when it still applies, else sRGB.
pub fn color_xml(tag: &str, rgb: Option<Color>, theme: Option<sp_model::theme::ThemeColor>) -> Option<String> {
    let c = rgb?;
    Some(match theme {
        Some(t) if t.applies_to(rgb) => {
            if t.tint == 0 {
                format!(r#"<{tag} theme="{}"/>"#, t.idx)
            } else {
                format!(r#"<{tag} theme="{}" tint="{}"/>"#, t.idx, t.tint_f64())
            }
        }
        _ => format!(r#"<{tag} rgb="{}"/>"#, c.to_argb_hex()),
    })
}

/// A `<color>`-like node of styles.xml → (sRGB, theme reference). Theme colours resolve
/// through `theme`; the default text colour (`theme="1"`, no tint) on a font counts as
/// automatic (no colour).
pub fn style_color(
    n: Option<&crate::xml::Node>,
    theme: &Theme,
    font: bool,
) -> (Option<Color>, Option<sp_model::theme::ThemeColor>) {
    let Some(n) = n else { return (None, None) };
    if let Some(c) = n.attr("rgb").and_then(Color::from_hex) {
        return (Some(c), None);
    }
    let Some(idx) = n.attr("theme").and_then(|v| v.parse::<u32>().ok()) else {
        return (None, None);
    };
    let tint = n
        .attr("tint")
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|t| t.is_finite())
        .unwrap_or(0.0)
        .clamp(-1.0, 1.0);
    if idx > 11 {
        return (None, None);
    }
    let tint_m = (tint * 1_000_000.0).round() as i32;
    if font && idx == 1 && tint_m == 0 {
        return (None, None);
    }
    let rgb = sp_model::theme::resolve_theme_color(theme, idx, tint);
    (
        Some(rgb),
        Some(sp_model::theme::ThemeColor {
            idx: idx as u8,
            tint: tint_m,
            rgb,
        }),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use sp_model::theme::builtin_themes;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" name="Sample"><a:themeElements><a:clrScheme name="Sample"><a:dk1><a:sysClr val="windowText" lastClr="000000"/></a:dk1><a:lt1><a:sysClr val="window" lastClr="FFFFFF"/></a:lt1><a:dk2><a:srgbClr val="112233"/></a:dk2><a:lt2><a:srgbClr val="EEEEEE"/></a:lt2><a:accent1><a:srgbClr val="AA0000"/></a:accent1><a:accent2><a:srgbClr val="00AA00"/></a:accent2><a:accent3><a:srgbClr val="0000AA"/></a:accent3><a:accent4><a:srgbClr val="AAAA00"/></a:accent4><a:accent5><a:srgbClr val="00AAAA"/></a:accent5><a:accent6><a:srgbClr val="AA00AA"/></a:accent6><a:hlink><a:srgbClr val="0000FF"/></a:hlink><a:folHlink><a:srgbClr val="800080"/></a:folHlink></a:clrScheme><a:fontScheme name="SampleFonts"><a:majorFont><a:latin typeface="Head Face" panose="020F0302020204030204"/><a:ea typeface=""/></a:majorFont><a:minorFont><a:latin typeface="Body Face"/></a:minorFont></a:fontScheme><a:fmtScheme name="x"><a:unknown keep="me"/></a:fmtScheme></a:themeElements><a:objectDefaults/></a:theme>"#;

    #[test]
    fn parse_patch_and_generate() {
        let t = parse_theme(SAMPLE).unwrap();
        assert_eq!(t.name, "Sample");
        assert_eq!(t.colors[0], Color(0, 0, 0));
        assert_eq!(t.colors[4], Color(0xAA, 0, 0));
        assert_eq!(t.major_font, "Head Face");
        assert_eq!(t.minor_font, "Body Face");
        let mut n = builtin_themes()[1].clone();
        n.name = "Mine & Co".into();
        let patched = patch_theme(SAMPLE, &n).unwrap();
        assert_eq!(parse_theme(&patched).unwrap(), n);
        // Unknown content is kept.
        assert!(patched.contains(r#"<a:unknown keep="me"/>"#));
        assert!(patched.contains(r#"panose="020F0302020204030204""#));
        let generated = theme_xml(&n);
        assert_eq!(parse_theme(&generated).unwrap(), n);
        assert!(parse_theme("<nope/>").is_none());
        assert!(parse_theme("garbage <<").is_none());
    }

    #[test]
    fn colors_resolve_and_write_back() {
        let t = parse_theme(SAMPLE).unwrap();
        let node = parse_dom(r#"<color theme="4" tint="0.5"/>"#).unwrap();
        let (rgb, r) = style_color(Some(&node), &t, false);
        let r = r.unwrap();
        assert_eq!(r.idx, 4);
        assert_eq!(r.tint, 500_000);
        assert_eq!(rgb, Some(sp_model::theme::apply_tint(Color(0xAA, 0, 0), 0.5)));
        assert_eq!(
            color_xml("fgColor", rgb, Some(r)).unwrap(),
            r#"<fgColor theme="4" tint="0.5"/>"#
        );
        // A changed colour drops the reference.
        assert_eq!(
            color_xml("color", Some(Color(1, 2, 3)), Some(r)).unwrap(),
            r#"<color rgb="FF010203"/>"#
        );
        let auto = parse_dom(r#"<color theme="1"/>"#).unwrap();
        assert_eq!(style_color(Some(&auto), &t, true), (None, None));
        let bad = parse_dom(r#"<color theme="99"/>"#).unwrap();
        assert_eq!(style_color(Some(&bad), &t, false), (None, None));
    }

    #[test]
    fn workbook_theme_and_theme_colours_round_trip() {
        use sp_core::{CellAddr, Value};
        let mut wb = Workbook::new();
        let save = |wb: &Workbook| {
            let mut buf = std::io::Cursor::new(Vec::new());
            crate::xlsx_write::write_xlsx(wb, &mut buf).unwrap();
            buf.set_position(0);
            crate::xlsx_read::read_xlsx(buf).unwrap().0
        };
        // Default theme, no part: nothing written, default back.
        assert_eq!(save(&wb).theme, Theme::default());
        wb.theme = builtin_themes()[3].clone();
        let rgb = sp_model::theme::resolve_theme_color(&wb.theme, 5, -0.25);
        let id = wb.styles.derive(sp_model::StyleId::DEFAULT, |s| {
            s.fill = Some(rgb);
            s.fill_theme = Some(sp_model::theme::ThemeColor { idx: 5, tint: -250_000, rgb });
        });
        wb.sheets[0].set(
            CellAddr::new(0, 0),
            sp_model::Cell { style: id, ..sp_model::Cell::constant(Value::Number(1.0)) },
        );
        let back = save(&wb);
        assert_eq!(back.theme, wb.theme);
        let st = back.styles.get(back.sheets[0].get(CellAddr::new(0, 0)).unwrap().style);
        assert_eq!(st.fill, Some(rgb));
        assert_eq!(st.fill_theme.map(|t| (t.idx, t.tint)), Some((5, -250_000)));
        // Change the theme of the loaded book: the captured part is patched in place.
        let mut b2 = back.clone();
        b2.theme = builtin_themes()[5].clone();
        let again = save(&b2);
        assert_eq!(again.theme, b2.theme);
        let st = again.styles.get(again.sheets[0].get(CellAddr::new(0, 0)).unwrap().style);
        assert_eq!(st.fill, Some(sp_model::theme::resolve_theme_color(&b2.theme, 5, -0.25)));
    }
}
