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
use sp_model::objects::ImageData;
use sp_render::objects::{PdfImage, pdf_image};
use sp_render::paginate::{Item, PageScene};
use sp_render::std14::{self, Face};
use std::collections::{BTreeSet, HashMap};
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

/// Most distinct images embedded in one document (later ones are left out).
const MAX_IMAGES: usize = 10_000;

/// Images used by the document, each encoded once (by image identity) and written as an
/// image XObject `/Im<n>`.
#[derive(Default)]
struct Images {
    /// Image key → index in `list` (`None`: could not be encoded; not drawn).
    by_key: HashMap<(usize, usize), Option<usize>>,
    list: Vec<PdfImage>,
    /// Keeps each image's bytes alive so its key stays unique.
    keep: Vec<ImageData>,
}

impl Images {
    fn index(&mut self, img: &ImageData) -> Option<usize> {
        let key = img.key();
        if let Some(i) = self.by_key.get(&key) {
            return *i;
        }
        let i = if self.list.len() < MAX_IMAGES {
            pdf_image(img).map(|p| {
                self.list.push(p);
                self.list.len() - 1
            })
        } else {
            None
        };
        self.by_key.insert(key, i);
        self.keep.push(img.clone());
        i
    }
}

/// Content-stream writer for one page (flips y to PDF's bottom-left origin).
struct Content<'a> {
    out: String,
    height: f32,
    fonts: &'a mut BTreeSet<usize>,
    images: &'a mut Images,
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
            Item::Poly { pts, color } | Item::Area { pts, color } => {
                if pts.len() < 3 {
                    return;
                }
                let _ = writeln!(self.out, "{} rg", rgb(*color));
                self.path(pts);
                self.out.push_str("h f\n");
            }
            Item::Image { image, corners } => {
                let Some(i) = self.images.index(image) else {
                    return;
                };
                // Image space is the unit square, (0, 0) at the picture's bottom-left.
                let [tl, _, br, bl] = *corners;
                let p = |q: (f32, f32)| (q.0, self.height - q.1);
                let (o, r, t) = (p(bl), p(br), p(tl));
                let _ = writeln!(
                    self.out,
                    "q {} {} {} {} {} {} cm /Im{i} Do Q",
                    num(r.0 - o.0),
                    num(r.1 - o.1),
                    num(t.0 - o.0),
                    num(t.1 - o.1),
                    num(o.0),
                    num(o.1)
                );
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
    write_pdf_pages(pages.iter().cloned(), title)
}

/// Like [`write_pdf`], but pages are produced one at a time (each scene is dropped once its
/// content stream is written, so long jobs don't hold every scene in memory).
pub fn write_pdf_pages(pages: impl IntoIterator<Item = PageScene>, title: &str) -> Vec<u8> {
    let mut fonts = BTreeSet::new();
    let mut images = Images::default();
    let mut streams: Vec<(f32, f32, String)> = Vec::new();
    for p in pages {
        let mut c = Content {
            out: String::new(),
            height: p.height,
            fonts: &mut fonts,
            images: &mut images,
        };
        c.items(&p.items, 0);
        streams.push((p.width, p.height, c.out));
    }
    if streams.is_empty() {
        streams.push((612.0, 792.0, String::new()));
    }
    // Object numbers: 1 catalog, 2 page tree, 3 info, 4 resources, then fonts, then images
    // (each followed by its soft mask, if any), then a (page, content) pair per page.
    let faces: Vec<Face> = Face::all()
        .into_iter()
        .filter(|f| fonts.contains(&f.index()))
        .collect();
    let font_obj = |i: usize| 5 + i;
    let mut image_objs = Vec::with_capacity(images.list.len());
    let mut next = 5 + faces.len();
    for im in &images.list {
        image_objs.push(next);
        next += if im.alpha.is_some() { 2 } else { 1 };
    }
    let first_page = next;
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
    let xobjects = if image_objs.is_empty() {
        String::new()
    } else {
        let refs: Vec<String> = image_objs
            .iter()
            .enumerate()
            .map(|(i, n)| format!("/Im{i} {n} 0 R"))
            .collect();
        format!(" /XObject << {} >>", refs.join(" "))
    };
    objs.push(
        format!(
            "<< /Font << {} >>{xobjects} /ProcSet [/PDF /Text /ImageB /ImageC] >>",
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
    for (im, n) in images.list.iter().zip(&image_objs) {
        let smask = im
            .alpha
            .as_ref()
            .map(|_| format!(" /SMask {} 0 R", n + 1))
            .unwrap_or_default();
        let space = if im.colors == 1 {
            "/DeviceGray"
        } else {
            "/DeviceRGB"
        };
        let filter = if im.dct {
            "/Filter /DCTDecode".to_string()
        } else {
            predictor(im.width, im.colors)
        };
        objs.push(image_object(
            &format!(
                "/Width {} /Height {} /ColorSpace {space} /BitsPerComponent 8 {filter}{smask}",
                im.width, im.height
            ),
            &im.data,
        ));
        if let Some(a) = &im.alpha {
            objs.push(image_object(
                &format!(
                    "/Width {} /Height {} /ColorSpace /DeviceGray /BitsPerComponent 8 {}",
                    im.width,
                    im.height,
                    predictor(im.width, 1)
                ),
                a,
            ));
        }
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
        let _ = writeln!(x, "{o:010} 00000 n ");
    }
    let _ = write!(
        x,
        "trailer\n<< /Size {} /Root 1 0 R /Info 3 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        objs.len() + 1
    );
    out.extend_from_slice(x.as_bytes());
    out
}

/// `/FlateDecode` with PNG predictors for rows of `width` pixels of `colors` components.
fn predictor(width: u32, colors: u8) -> String {
    format!(
        "/Filter /FlateDecode /DecodeParms << /Predictor 15 /Colors {colors} /BitsPerComponent 8 /Columns {width} >>"
    )
}

/// An image XObject stream with dictionary entries `dict` and binary `data`.
fn image_object(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut s = format!(
        "<< /Type /XObject /Subtype /Image {dict} /Length {} >>\nstream\n",
        data.len()
    )
    .into_bytes();
    s.extend_from_slice(data);
    s.extend_from_slice(b"\nendstream");
    s
}

/// What [`validate`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdfSummary {
    pub objects: usize,
    pub pages: usize,
    /// Image XObjects (soft masks included).
    pub images: usize,
}

/// `/Im<n>` names in `hay` followed by `suffix` (e.g. `" Do"` or `" "`).
fn image_names(hay: &[u8], suffix: &[u8]) -> BTreeSet<usize> {
    let mut out = BTreeSet::new();
    let mut from = 0;
    while let Some(at) = find(hay, b"/Im", from) {
        let digits = hay
            .get(at + 3..)
            .map_or(0, |t| t.iter().take_while(|b| b.is_ascii_digit()).count());
        let end = at + 3 + digits;
        if digits > 0
            && hay.get(end..).is_some_and(|t| t.starts_with(suffix))
            && let Some(n) = hay.get(at + 3..end).and_then(ascii_num)
        {
            out.insert(n);
        }
        from = end.max(at + 1);
    }
    out
}

/// Balanced dictionaries/arrays (outside literal strings) and in-range `n 0 R` references.
fn check_syntax(obj: &[u8], count: usize) -> Result<(), String> {
    let mut stack: Vec<u8> = Vec::new();
    let mut paren = 0usize;
    let mut i = 0usize;
    while let Some(&c) = obj.get(i) {
        if paren > 0 {
            match c {
                b'\\' => i += 1,
                b'(' => paren += 1,
                b')' => paren -= 1,
                _ => {}
            }
        } else {
            match c {
                b'(' => paren = 1,
                b'[' => stack.push(b']'),
                b'<' if obj.get(i + 1) == Some(&b'<') => {
                    stack.push(b'>');
                    i += 1;
                }
                b'>' if obj.get(i + 1) == Some(&b'>') => {
                    if stack.pop() != Some(b'>') {
                        return Err("unbalanced >>".into());
                    }
                    i += 1;
                }
                b']' => {
                    if stack.pop() != Some(b']') {
                        return Err("unbalanced ]".into());
                    }
                }
                b'R' if i >= 4 && obj.get(i - 3..i) == Some(&b" 0 "[..]) => {
                    let digits_end = i - 3;
                    let start = obj
                        .get(..digits_end)
                        .and_then(|h| h.iter().rposition(|b| !b.is_ascii_digit()))
                        .map_or(0, |p| p + 1);
                    let n = obj.get(start..digits_end).and_then(ascii_num);
                    if !n.is_some_and(|n| n >= 1 && n < count) {
                        return Err(format!("reference {n:?} 0 R out of range"));
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    if paren != 0 || !stack.is_empty() {
        return Err("unterminated string, dictionary or array".into());
    }
    Ok(())
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
/// in-use xref entry pointing at `N 0 obj`, every object parsing (balanced dictionaries,
/// arrays and strings; `n 0 R` references in range), each stream's `/Length` matching, and
/// the page tree `/Count` equal to the number of page objects.
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
    let mut images = 0usize;
    let mut declared = None;
    let mut xobjects: BTreeSet<usize> = BTreeSet::new();
    let mut used: BTreeSet<usize> = BTreeSet::new();
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
        // The object (or a stream's dictionary) must parse: balanced `<< >>` / `[ ]` outside
        // strings, and every `n 0 R` reference must name an object in the table.
        let dict = find(body, b"\nstream\n", 0).map_or(body, |sp| body.get(..sp).unwrap_or(body));
        check_syntax(dict, count).map_err(|e| format!("object {i}: {e}"))?;
        if body.starts_with(b"<< /Type /Page ") {
            pages += 1;
        }
        if body.starts_with(b"<< /Font ") {
            xobjects.extend(image_names(dict, b" "));
        }
        if dict.starts_with(b"<< /Type /XObject /Subtype /Image ") {
            images += 1;
            for key in [
                &b"/Width "[..],
                b"/Height ",
                b"/ColorSpace ",
                b"/BitsPerComponent 8",
                b"/Filter ",
            ] {
                if find(dict, key, 0).is_none() {
                    return Err(format!(
                        "image object {i} lacks {}",
                        String::from_utf8_lossy(key).trim()
                    ));
                }
            }
        } else if let Some(sp) = find(body, b"\nstream\n", 0) {
            used.extend(image_names(body.get(sp..).unwrap_or_default(), b" Do"));
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
                return Err(format!(
                    "object {i}: /Length {len} does not match its stream"
                ));
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
    if let Some(n) = used.difference(&xobjects).next() {
        return Err(format!(
            "content draws /Im{n}, which the resources don't define"
        ));
    }
    Ok(PdfSummary {
        objects: count.saturating_sub(1),
        pages,
        images,
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

    fn png_with_alpha() -> sp_model::objects::ImageData {
        let px = vec![
            200, 30, 30, 255, 0, 0, 0, 0, 30, 200, 30, 128, 30, 30, 200, 255,
        ];
        let png = sp_render::objects::encode_png(2, 2, px).unwrap();
        sp_render::objects::image_data(png).unwrap()
    }

    fn jpeg() -> sp_model::objects::ImageData {
        let rgb: Vec<u8> = (0..8 * 4 * 3).map(|i| (i * 7) as u8).collect();
        let jpg = sp_render::objects::encode_jpeg(8, 4, rgb).unwrap();
        sp_render::objects::image_data(jpg).unwrap()
    }

    fn image_scene(
        png: &sp_model::objects::ImageData,
        jpg: &sp_model::objects::ImageData,
    ) -> PageScene {
        let corners = [(10.0, 10.0), (110.0, 10.0), (110.0, 60.0), (10.0, 60.0)];
        PageScene {
            width: 612.0,
            height: 792.0,
            items: vec![
                Item::Area {
                    pts: vec![
                        (0.0, 0.0),
                        (40.0, 0.0),
                        (20.0, 10.0),
                        (40.0, 30.0),
                        (0.0, 30.0),
                    ],
                    color: Color(255, 192, 0),
                },
                Item::Image {
                    image: png.clone(),
                    corners,
                },
                Item::Image {
                    image: jpg.clone(),
                    corners,
                },
                // The same picture again: embedded once.
                Item::Image {
                    image: png.clone(),
                    corners,
                },
                // Undecodable bytes: skipped, not referenced.
                Item::Image {
                    image: sp_model::objects::ImageData {
                        bytes: std::sync::Arc::from(&b"\x89PNG\r\n\x1a\nbroken"[..]),
                        ..Default::default()
                    },
                    corners,
                },
            ],
        }
    }

    #[test]
    fn pictures_embed_as_image_xobjects() {
        let (png, j) = (png_with_alpha(), jpeg());
        let pdf = write_pdf(&[image_scene(&png, &j), image_scene(&png, &j)], "pics");
        let s = validate(&pdf).unwrap_or_else(|e| panic!("{e}"));
        // PNG (RGB + soft mask) and JPEG, shared by both pages.
        assert_eq!((s.pages, s.images), (2, 3));
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.contains("/Filter /DCTDecode"));
        assert!(text.contains("/Predictor 15 /Colors 3"));
        assert!(text.contains("/SMask "));
        assert!(text.contains("/XObject << /Im0 "));
        assert_eq!(text.matches("/Im0 Do").count(), 4);
        assert_eq!(text.matches("/Im1 Do").count(), 2);
        assert!(!text.contains("/Im2"));
        // Unit square → the 100 × 50 pt box, y flipped (bottom-left at 792 - 60).
        assert!(text.contains("q 100 0 0 50 10 732 cm /Im0 Do Q"), "{text}");
        // Concave area: one filled path.
        assert!(text.contains("1 0.753 0 rg"));
        // The JPEG passes through byte for byte.
        assert!(pdf.windows(j.bytes.len()).any(|w| w == &j.bytes[..]));
    }

    #[test]
    fn validator_checks_images() {
        let pdf = write_pdf(&[image_scene(&png_with_alpha(), &jpeg())], "pics");
        assert!(validate(&pdf).is_ok());
        let swap = |from: &[u8], to: &[u8]| {
            let mut b = pdf.clone();
            let at = find(&b, from, 0).unwrap();
            b.splice(at..at + from.len(), to.iter().copied());
            b
        };
        // Drawing an image the resources don't name.
        let bad = swap(b"/Im1 Do", b"/Im7 Do");
        assert!(validate(&bad).unwrap_err().contains("/Im7"));
        // An image without a size.
        let bad = swap(b"/Width ", b"/Wxdth ");
        assert!(validate(&bad).unwrap_err().contains("lacks /Width"));
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
        // Same-length edits that break object syntax or references.
        let good = write_pdf(&[scene()], "t");
        let swap = |from: &[u8], to: &[u8]| {
            let mut b = good.clone();
            let at = find(&b, from, 0).unwrap();
            b.splice(at..at + from.len(), to.iter().copied());
            b
        };
        let bad = swap(b"/Pages 2 0 R", b"/Pages 9 0 R");
        assert!(validate(&bad).unwrap_err().contains("out of range"));
        let bad = swap(b"/Pages 2 0 R >>", b"/Pages 2 0 R ]>");
        assert!(validate(&bad).is_err());
    }

    #[test]
    fn syntax_checker() {
        assert!(check_syntax(b"<< /A [1 2 (x]) ] /B << >> /C 3 0 R >>", 4).is_ok());
        assert!(check_syntax(b"<< /A (unterminated >>", 4).is_err());
        assert!(check_syntax(b"<< /A [ >>", 4).is_err());
        assert!(check_syntax(b"<< /A 4 0 R >>", 4).is_err());
        assert!(check_syntax(b"<< /A (esc \\) still) >>", 4).is_ok());
        assert!(check_syntax(b"", 1).is_ok());
    }

    /// If `qpdf` or `pdfinfo` is installed, it must accept our output; skipped otherwise.
    #[test]
    fn external_checkers_accept_output() {
        let pages: Vec<PageScene> = (0..3).map(|_| scene()).collect();
        let pdf = write_pdf(&pages, "Report (draft) \u{e9}");
        let dir = std::env::temp_dir().join(format!("sp_pdf_ext_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("check.pdf");
        std::fs::write(&path, &pdf).unwrap();
        let run = |tool: &str, args: &[&str]| {
            std::process::Command::new(tool)
                .args(args)
                .arg(&path)
                .output()
                .ok()
        };
        if let Some(out) = run("qpdf", &["--check"]) {
            let log = String::from_utf8_lossy(&out.stdout);
            assert!(out.status.success(), "qpdf --check failed: {log}");
            assert!(!log.contains("WARNING"), "qpdf warnings: {log}");
        } else {
            eprintln!("qpdf not installed; skipped");
        }
        if let Some(out) = run("pdfinfo", &[]) {
            let log = String::from_utf8_lossy(&out.stdout);
            assert!(out.status.success(), "pdfinfo failed: {log}");
            assert!(
                log.lines()
                    .any(|l| l.starts_with("Pages:") && l.trim_end().ends_with('3'))
            );
        } else {
            eprintln!("pdfinfo not installed; skipped");
        }
        let _ = std::fs::remove_dir_all(&dir);
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
