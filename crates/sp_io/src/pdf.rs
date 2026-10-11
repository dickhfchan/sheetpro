//! PDF writer for printed pages (written from ISO 32000-1, PDF 1.7).
//!
//! Input is a list of [`PageScene`]s from `sp_render::paginate` (points, top-left origin);
//! output is a classic uncompressed PDF: catalog, page tree, one page object and one content
//! stream per page, a shared resource dictionary, a cross-reference table and trailer.
//!
//! Text uses the standard-14 Type 1 faces (Helvetica, Times, Courier and their bold/italic
//! variants) with `/WinAnsiEncoding`, so no font program is embedded: every conforming
//! viewer supplies them. Characters outside WinAnsi are written as `?` (see
//! `sp_render::std14`), the documented fallback until font embedding exists.
//! [`validate`] re-reads a file and checks the structure (header, xref offsets, stream
//! lengths, page count); tests use it on every export.

use sp_model::Color;
use sp_render::paginate::{Item, PageScene};
use sp_render::std14::{self, Face};
use std::collections::BTreeSet;
use std::fmt::Write as _;

/// Number in a content stream: at most 3 decimals, no exponent, non-finite → 0.
fn num(v: f32) -> String {
    if !v.is_finite() {
        return "0".into();
    }
    let v = v.clamp(-1.0e7, 1.0e7);
    let s = format!("{v:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" || s.is_empty() {
        "0".into()
    } else {
        s.to_string()
    }
}

fn rgb(c: Color) -> String {
    format!(
        "{} {} {}",
        num(f32::from(c.0) / 255.0),
        num(f32::from(c.1) / 255.0),
        num(f32::from(c.2) / 255.0)
    )
}

/// A literal string `( … )` of WinAnsi bytes with `\`, parentheses and non-ASCII escaped.
fn literal(text: &str) -> String {
    let mut s = String::from("(");
    for b in std14::to_winansi(text) {
        match b {
            b'(' | b')' | b'\\' => {
                s.push('\\');
                s.push(char::from(b));
            }
            0x20..=0x7E => s.push(char::from(b)),
            _ => {
                let _ = write!(s, "\\{b:03o}");
            }
        }
    }
    s.push(')');
    s
}

/// A text string for the document information dictionary: UTF-16BE with a byte-order mark.
fn text_string(text: &str) -> String {
    let mut s = String::from("<FEFF");
    for u in text.chars().take(1_000).collect::<String>().encode_utf16() {
        let _ = write!(s, "{u:04X}");
    }
    s.push('>');
    s
}

/// Content-stream writer for one page (flips y to PDF's bottom-left origin).
struct Content<'a> {
    out: String,
    height: f32,
    fonts: &'a mut BTreeSet<usize>,
}

impl Content<'_> {
    fn y(&self, y: f32) -> String {
        num(self.height - y)
    }

    fn path(&mut self, pts: &[(f32, f32)]) {
        for (i, (x, y)) in pts.iter().enumerate() {
            let op = if i == 0 { "m" } else { "l" };
            let _ = writeln!(self.out, "{} {} {op}", num(*x), self.y(*y));
        }
    }

    fn items(&mut self, items: &[Item], depth: usize) {
        for it in items {
            self.item(it, depth);
        }
    }

    fn item(&mut self, it: &Item, depth: usize) {
        match it {
            Item::Fill { x, y, w, h, color } => {
                if *w <= 0.0 || *h <= 0.0 {
                    return;
                }
                let _ = writeln!(
                    self.out,
                    "{} rg {} {} {} {} re f",
                    rgb(*color),
                    num(*x),
                    self.y(y + h),
                    num(*w),
                    num(*h)
                );
            }
            Item::Line {
                pts,
                width,
                color,
                dash,
                closed,
            } => {
                if pts.len() < 2 {
                    return;
                }
                let _ = writeln!(self.out, "{} RG {} w", rgb(*color), num(width.max(0.0)));
                match dash {
                    Some((on, off)) => {
                        let _ = writeln!(self.out, "[{} {}] 0 d", num(*on), num(*off));
                    }
                    None => self.out.push_str("[] 0 d\n"),
                }
                self.path(pts);
                self.out.push_str(if *closed { "s\n" } else { "S\n" });
            }
            Item::Poly { pts, color } => {
                if pts.len() < 3 {
                    return;
                }
                let _ = writeln!(self.out, "{} rg", rgb(*color));
                self.path(pts);
                self.out.push_str("h f\n");
            }
            Item::Circle { x, y, r, color } => {
                if *r <= 0.0 {
                    return;
                }
                // Four cubic Béziers; 0.5523 ≈ 4/3·(√2 − 1) puts the midpoints on the circle.
                let k = 0.5523 * r;
                let (cx, cy) = (*x, self.height - y);
                let _ = writeln!(self.out, "{} rg", rgb(*color));
                let p = |a: f32, b: f32| format!("{} {}", num(a), num(b));
                let _ = writeln!(self.out, "{} m", p(cx + r, cy));
                for (c1, c2, e) in [
                    ((cx + r, cy + k), (cx + k, cy + r), (cx, cy + r)),
                    ((cx - k, cy + r), (cx - r, cy + k), (cx - r, cy)),
                    ((cx - r, cy - k), (cx - k, cy - r), (cx, cy - r)),
                    ((cx + k, cy - r), (cx + r, cy - k), (cx + r, cy)),
                ] {
                    let _ = writeln!(
                        self.out,
                        "{} {} {} c",
                        p(c1.0, c1.1),
                        p(c2.0, c2.1),
                        p(e.0, e.1)
                    );
                }
                self.out.push_str("f\n");
            }
            Item::Text {
                x,
                y,
                text,
                face,
                size,
                color,
                angle,
            } => {
                if text.is_empty() {
                    return;
                }
                self.fonts.insert(face.index());
                let a = angle.to_radians();
                let (c, s) = (a.cos(), a.sin());
                let _ = writeln!(
                    self.out,
                    "BT /F{} {} Tf {} rg {} {} {} {} {} {} Tm {} Tj ET",
                    face.index(),
                    num(size.max(0.1)),
                    rgb(*color),
                    num(c),
                    num(s),
                    num(-s),
                    num(c),
                    num(*x),
                    self.y(*y),
                    literal(text)
                );
            }
            Item::Clip { x, y, w, h, items } => {
                if depth > 32 {
                    return;
                }
                let _ = writeln!(
                    self.out,
                    "q {} {} {} {} re W n",
                    num(*x),
                    self.y(y + h),
                    num(w.max(0.0)),
                    num(h.max(0.0))
                );
                self.items(items, depth + 1);
                self.out.push_str("Q\n");
            }
        }
    }
}

/// Serialize `pages` as a PDF document titled `title`. With no pages a single blank Letter
/// page is written so the file still opens.
pub fn write_pdf(pages: &[PageScene], title: &str) -> Vec<u8> {
    let blank = [PageScene {
        width: 612.0,
        height: 792.0,
        items: Vec::new(),
    }];
    let pages = if pages.is_empty() { &blank[..] } else { pages };
    let mut fonts = BTreeSet::new();
    let streams: Vec<(f32, f32, String)> = pages
        .iter()
        .map(|p| {
            let mut c = Content {
                out: String::new(),
                height: p.height,
                fonts: &mut fonts,
            };
            c.items(&p.items, 0);
            (p.width, p.height, c.out)
        })
        .collect();
    // Object numbers: 1 catalog, 2 page tree, 3 info, 4 resources, then fonts, then a
    // (page, content) pair per page.
    let faces: Vec<Face> = Face::all()
        .into_iter()
        .filter(|f| fonts.contains(&f.index()))
        .collect();
    let font_obj = |i: usize| 5 + i;
    let first_page = 5 + faces.len();
    let page_obj = |i: usize| first_page + 2 * i;
    let mut objs: Vec<Vec<u8>> = Vec::new();
    objs.push(b"<< /Type /Catalog /Pages 2 0 R >>".to_vec());
    let kids: Vec<String> = (0..streams.len())
        .map(|i| format!("{} 0 R", page_obj(i)))
        .collect();
    objs.push(
        format!(
            "<< /Type /Pages /Kids [{}] /Count {} >>",
            kids.join(" "),
            streams.len()
        )
        .into_bytes(),
    );
    objs.push(
        format!(
            "<< /Producer {} /Title {} >>",
            text_string("SheetPro"),
            text_string(title)
        )
        .into_bytes(),
    );
    let font_refs: Vec<String> = faces
        .iter()
        .enumerate()
        .map(|(i, f)| format!("/F{} {} 0 R", f.index(), font_obj(i)))
        .collect();
    objs.push(
        format!(
            "<< /Font << {} >> /ProcSet [/PDF /Text] >>",
            font_refs.join(" ")
        )
        .into_bytes(),
    );
    for f in &faces {
        objs.push(
            format!(
                "<< /Type /Font /Subtype /Type1 /BaseFont /{} /Encoding /WinAnsiEncoding >>",
                f.base_font()
            )
            .into_bytes(),
        );
    }
    for (i, (w, h, content)) in streams.iter().enumerate() {
        objs.push(
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Resources 4 0 R /Contents {} 0 R >>",
                num(*w),
                num(*h),
                page_obj(i) + 1
            )
            .into_bytes(),
        );
        let mut s = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
        s.extend_from_slice(content.as_bytes());
        s.extend_from_slice(b"\nendstream");
        objs.push(s);
    }
    // Header with a binary comment so transfer tools treat the file as binary.
    let mut out: Vec<u8> = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = Vec::with_capacity(objs.len());
    for (i, body) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    let mut x = format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1);
    for o in &offsets {
        let _ = write!(x, "{o:010} 00000 n \n");
    }
    let _ = write!(
        x,
        "trailer\n<< /Size {} /Root 1 0 R /Info 3 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        objs.len() + 1
    );
    out.extend_from_slice(x.as_bytes());
    out
}

/// What [`validate`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdfSummary {
    pub objects: usize,
    pub pages: usize,
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    hay.get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

fn ascii_num(bytes: &[u8]) -> Option<usize> {
    std::str::from_utf8(bytes).ok()?.trim().parse().ok()
}

/// Structural check of a PDF this module wrote: header, `startxref` → `xref`, every
/// in-use xref entry pointing at `N 0 obj`, each stream's `/Length` matching, and the page
/// tree `/Count` equal to the number of page objects.
pub fn validate(bytes: &[u8]) -> Result<PdfSummary, String> {
    if !bytes.starts_with(b"%PDF-1.") {
        return Err("missing %PDF header".into());
    }
    let tail_from = bytes.len().saturating_sub(64);
    let sx = bytes
        .get(tail_from..)
        .and_then(|t| find(t, b"startxref\n", 0))
        .map(|p| p + tail_from)
        .ok_or("no startxref")?;
    let num_start = sx + b"startxref\n".len();
    let num_end = find(bytes, b"\n", num_start).ok_or("bad startxref")?;
    let xref = bytes
        .get(num_start..num_end)
        .and_then(ascii_num)
        .ok_or("bad startxref offset")?;
    if !bytes
        .get(xref..)
        .is_some_and(|b| b.starts_with(b"xref\n0 "))
    {
        return Err(format!("startxref {xref} does not point at xref"));
    }
    let line_end = find(bytes, b"\n", xref + 5).ok_or("bad xref")?;
    let count = bytes
        .get(xref + 7..line_end)
        .and_then(ascii_num)
        .ok_or("bad xref count")?;
    let mut pos = line_end + 1;
    let mut pages = 0usize;
    let mut declared = None;
    for i in 0..count {
        let entry = bytes.get(pos..pos + 20).ok_or("truncated xref")?;
        pos += 20;
        if entry.len() != 20 || !entry.ends_with(b" \n") {
            return Err(format!("xref entry {i} is not 20 bytes"));
        }
        if i == 0 {
            continue;
        }
        if entry.get(17) != Some(&b'n') {
            return Err(format!("object {i} not in use"));
        }
        let off = entry.get(..10).and_then(ascii_num).ok_or("bad offset")?;
        let head = format!("{i} 0 obj\n");
        if !bytes
            .get(off..)
            .is_some_and(|b| b.starts_with(head.as_bytes()))
        {
            return Err(format!("xref offset {off} of object {i} is wrong"));
        }
        let body_start = off + head.len();
        let end = find(bytes, b"\nendobj\n", body_start).ok_or("unterminated object")?;
        let body = bytes.get(body_start..end).ok_or("bad object")?;
        if body.starts_with(b"<< /Type /Page ") {
            pages += 1;
        }
        if body.starts_with(b"<< /Type /Pages ") {
            let c = find(body, b"/Count ", 0).ok_or("no /Count")? + 7;
            let ce = find(body, b" ", c).ok_or("bad /Count")?;
            declared = body.get(c..ce).and_then(ascii_num);
        }
        if let Some(sp) = find(body, b">>\nstream\n", 0) {
            let l = find(body, b"/Length ", 0).ok_or("stream without /Length")? + 8;
            let le = find(body, b" ", l).ok_or("bad /Length")?;
            let len = body.get(l..le).and_then(ascii_num).ok_or("bad /Length")?;
            let data = sp + b">>\nstream\n".len();
            if body.get(data + len..) != Some(&b"\nendstream"[..]) {
                return Err(format!("object {i}: /Length {len} does not match its stream"));
            }
        }
    }
    if bytes
        .get(pos..)
        .is_none_or(|b| !b.starts_with(b"trailer\n<< /Size "))
    {
        return Err("no trailer after xref".into());
    }
    if declared != Some(pages) {
        return Err(format!("/Count {declared:?} but {pages} page objects"));
    }
    Ok(PdfSummary {
        objects: count.saturating_sub(1),
        pages,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scene() -> PageScene {
        PageScene {
            width: 612.0,
            height: 792.0,
            items: vec![
                Item::Fill {
                    x: 10.0,
                    y: 10.0,
                    w: 100.0,
                    h: 20.0,
                    color: Color(255, 255, 0),
                },
                Item::Clip {
                    x: 0.0,
                    y: 0.0,
                    w: 300.0,
                    h: 300.0,
                    items: vec![Item::Text {
                        x: 12.0,
                        y: 25.0,
                        text: "Total (net) \\ 5€ 中".into(),
                        face: Face::for_font("Arial", true, false),
                        size: 11.0,
                        color: Color::BLACK,
                        angle: 0.0,
                    }],
                },
                Item::Line {
                    pts: vec![(0.0, 0.0), (10.0, 10.0)],
                    width: 0.5,
                    color: Color(0, 0, 255),
                    dash: Some((2.0, 1.0)),
                    closed: false,
                },
                Item::Poly {
                    pts: vec![(0.0, 0.0), (10.0, 0.0), (5.0, 5.0)],
                    color: Color(1, 2, 3),
                },
                Item::Circle {
                    x: 50.0,
                    y: 50.0,
                    r: 3.0,
                    color: Color(9, 9, 9),
                },
            ],
        }
    }

    #[test]
    fn structure_is_valid() {
        let pdf = write_pdf(&[scene(), scene(), scene()], "Book (1)");
        let s = validate(&pdf).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.pages, 3);
        // catalog, pages, info, resources, 1 font, 3 × (page + content)
        assert_eq!(s.objects, 4 + 1 + 6);
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.contains("/BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding"));
        // Escaped parentheses/backslash, € as octal 200, CJK fallback '?'.
        assert!(text.contains(r"(Total \(net\) \\ 5\200 ?) Tj"), "{text}");
        assert!(text.contains("re f"));
        assert!(text.contains("[2 1] 0 d"));
        assert!(text.contains("612 792"));
    }

    #[test]
    fn empty_document_gets_a_blank_page() {
        let pdf = write_pdf(&[], "x");
        assert_eq!(validate(&pdf).map(|s| s.pages), Ok(1));
    }

    #[test]
    fn validator_catches_corruption() {
        let mut pdf = write_pdf(&[scene()], "t");
        assert!(validate(&pdf).is_ok());
        // Shift every object by one byte: the xref offsets are now wrong.
        pdf.insert(16, b' ');
        assert!(validate(&pdf).is_err());
        assert!(validate(b"not a pdf").is_err());
        assert!(validate(b"%PDF-1.7\nstartxref\n999999\n%%EOF\n").is_err());
    }

    #[test]
    fn numbers_are_plain() {
        assert_eq!(num(1.0), "1");
        assert_eq!(num(0.5), "0.5");
        assert_eq!(num(-0.0001), "0");
        assert_eq!(num(f32::NAN), "0");
        assert_eq!(num(1e30), "10000000");
    }
}
