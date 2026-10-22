//! DrawingML pictures, shapes, text boxes and connectors (`xdr:pic`, `xdr:sp`, `xdr:cxnSp`
//! inside a sheet's drawing part, images in `xl/media/*`), the sheet background picture
//! (worksheet `<picture r:id>`), and cell checkboxes (a cell format extension pointing into
//! the workbook's feature property bag part). Element names follow ECMA-376 Part 1 §20
//! (DrawingML) and §18; the checkbox parts follow the public MS-XLSX extension.
//!
//! Adoption rule (as for charts): an object is modelled only when *every* element and
//! attribute in it is something this module reads and writes back; anything else (groups,
//! gradients, effects, custom geometry, hyperlinks, OLE content…) keeps the whole drawing
//! passed through verbatim.

use crate::xlsx_extras::SheetRels;
use crate::xml::{Node, esc, parse_dom};
use sp_core::{CellAddr, MAX_COLS, MAX_ROWS};
use sp_model::chart::{Anchor, EMU_PER_PX};
use sp_model::objects::{
    AnchorKind, ColorMod, DrawObject, Fill, ImageData, LineFormat, ObjColor, ObjectAnchor,
    ObjectKind, Paragraph, Picture, Placement, RunFont, Shape, StyleRefs, TextBody, TextRun,
};
use sp_model::passthrough::{Passthrough, RawPart, RawRel};
use sp_model::{Color, Sheet, Style, Workbook};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::Arc;

const NS_A: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
const NS_R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const NS_XDR: &str = "http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing";
const REL_IMAGE: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image";
/// Blip extension that only records "use local DPI" (safe to drop and regenerate without).
const EXT_LOCAL_DPI: &str = "{28A0092B-C50C-407E-A947-70E740481C1C}";

/// Feature property bag (cell checkboxes).
pub const REL_FPB: &str =
    "http://schemas.microsoft.com/office/2022/11/relationships/FeaturePropertyBag";
pub const CT_FPB: &str = "application/vnd.ms-excel.featurepropertybag+xml";
pub const FPB_PART: &str = "xl/featurePropertyBag/featurePropertyBag.xml";
const NS_FPB: &str = "http://schemas.microsoft.com/office/spreadsheetml/2022/featurepropertybag";
const EXT_XF_COMPLEMENT: &str = "{C7286773-470A-42A8-94C5-96B5CB345126}";
/// The bag types a checkbox-only feature property bag part holds.
const CHECKBOX_BAGS: &[&str] = &["Checkbox", "XFControls", "XFComplement", "XFComplements"];

/// Most objects read from one drawing.
const MAX_READ: usize = sp_model::objects::MAX_OBJECTS;

// ---------------------------------------------------------------------------------------
// Reading

type R<T> = Option<T>;

fn num<T: std::str::FromStr>(n: &Node, k: &str) -> Option<T> {
    n.attr(k).and_then(|v| v.trim().parse::<T>().ok())
}

fn emu_px(v: i64) -> f64 {
    v.clamp(0, 1_000_000_000_000) as f64 / EMU_PER_PX
}

fn bool_attr(n: &Node, k: &str) -> bool {
    matches!(n.attr(k), Some("1" | "true"))
}

/// Attributes kept as written, minus `skip` and namespace declarations (the DOM strips
/// `xmlns:` prefixes, so declarations are recognised by their URI values).
fn kept_attrs(n: &Node, skip: &[&str]) -> Vec<(String, String)> {
    n.attrs
        .iter()
        .filter(|(k, v)| {
            !skip.contains(&k.as_str())
                && k != "xmlns"
                && !v.starts_with("http://")
                && !v.starts_with("urn:")
        })
        .cloned()
        .collect()
}

/// Every attribute is in `allowed` (namespace declarations aside).
fn only_attrs(n: &Node, allowed: &[&str]) -> bool {
    kept_attrs(n, allowed).is_empty()
}

fn marker(n: &Node) -> R<(CellAddr, (f64, f64))> {
    let u = |k: &str| {
        n.child(k)
            .and_then(|x| x.text.trim().parse::<i64>().ok())
            .unwrap_or(0)
    };
    for c in &n.children {
        if !matches!(c.name.as_str(), "col" | "colOff" | "row" | "rowOff") {
            return None;
        }
    }
    let row = u("row").clamp(0, i64::from(MAX_ROWS) - 1) as u32;
    let col = u("col").clamp(0, i64::from(MAX_COLS) - 1) as u32;
    Some((
        CellAddr::new(row, col),
        (emu_px(u("colOff")), emu_px(u("rowOff"))),
    ))
}

fn ext_of(n: &Node) -> (f64, f64) {
    (
        emu_px(num::<i64>(n, "cx").unwrap_or(0)),
        emu_px(num::<i64>(n, "cy").unwrap_or(0)),
    )
}

/// The anchor of an anchor element (`twoCellAnchor` / `oneCellAnchor` / `absoluteAnchor`).
fn anchor_of(n: &Node) -> R<ObjectAnchor> {
    let mut a = ObjectAnchor::default();
    match n.name.as_str() {
        "twoCellAnchor" => {
            if !only_attrs(n, &["editAs"]) {
                return None;
            }
            a.kind = AnchorKind::TwoCell;
            a.placement = match n.attr("editAs") {
                Some("oneCell") => Placement::Move,
                Some("absolute") => Placement::Fixed,
                None | Some("twoCell") => Placement::MoveAndSize,
                Some(_) => return None,
            };
            let (from, from_off) = marker(n.child("from")?)?;
            let (to, to_off) = marker(n.child("to")?)?;
            a.cells = Anchor {
                from,
                from_off,
                to,
                to_off,
            };
        }
        "oneCellAnchor" => {
            a.kind = AnchorKind::OneCell;
            a.placement = Placement::Move;
            let (from, from_off) = marker(n.child("from")?)?;
            a.cells.from = from;
            a.cells.from_off = from_off;
            a.cells.to = from;
            a.ext = ext_of(n.child("ext")?);
        }
        "absoluteAnchor" => {
            a.kind = AnchorKind::Absolute;
            a.placement = Placement::Fixed;
            let p = n.child("pos")?;
            a.pos = (
                emu_px(num::<i64>(p, "x").unwrap_or(0)),
                emu_px(num::<i64>(p, "y").unwrap_or(0)),
            );
            a.ext = ext_of(n.child("ext")?);
        }
        _ => return None,
    }
    Some(a)
}

const COLOR_TAGS: &[&str] = &["srgbClr", "schemeClr", "sysClr"];

fn color_node(n: &Node) -> R<ObjColor> {
    let mut mods = Vec::new();
    for m in &n.children {
        if !only_attrs(m, &["val"]) || !m.children.is_empty() {
            return None;
        }
        mods.push(ColorMod {
            name: m.name.clone(),
            val: num::<i64>(m, "val").unwrap_or(0),
        });
    }
    Some(match n.name.as_str() {
        "srgbClr" if only_attrs(n, &["val"]) => {
            ObjColor::Rgb(Color::from_hex(n.attr("val")?)?, mods)
        }
        "schemeClr" if only_attrs(n, &["val"]) => {
            ObjColor::Scheme(n.attr("val")?.to_string(), mods)
        }
        "sysClr" if only_attrs(n, &["val", "lastClr"]) => ObjColor::System(
            n.attr("val")?.to_string(),
            n.attr("lastClr")
                .and_then(Color::from_hex)
                .unwrap_or(Color::BLACK),
            mods,
        ),
        _ => return None,
    })
}

/// The single colour child of `n` (`solidFill`, `lnRef`…).
fn color_child(n: &Node) -> R<ObjColor> {
    match n.children.as_slice() {
        [c] if COLOR_TAGS.contains(&c.name.as_str()) => color_node(c),
        _ => None,
    }
}

/// A fill element (`noFill` / `solidFill`), or `None` when `n` is not a fill we model.
fn fill_of(n: &Node) -> R<Fill> {
    match n.name.as_str() {
        "noFill" if n.children.is_empty() => Some(Fill::None),
        "solidFill" => Some(Fill::Solid(color_child(n)?)),
        _ => None,
    }
}

fn line_of(n: &Node) -> R<LineFormat> {
    let mut l = LineFormat {
        width: num::<i64>(n, "w"),
        attrs: kept_attrs(n, &["w"]),
        ..LineFormat::default()
    };
    if l.attrs
        .iter()
        .any(|(k, _)| !matches!(k.as_str(), "cap" | "cmpd" | "algn"))
    {
        return None;
    }
    for c in &n.children {
        match c.name.as_str() {
            "noFill" | "solidFill" => l.fill = fill_of(c)?,
            "prstDash" if only_attrs(c, &["val"]) => l.dash = c.attr("val").map(str::to_string),
            "round" | "bevel" | "miter" => l.join = Some(c.name.clone()),
            "headEnd" | "tailEnd" => {
                if !only_attrs(c, &["type", "w", "len"]) {
                    return None;
                }
                let t = c.attr("type").map(str::to_string).filter(|t| t != "none");
                // Non-default sizes are not modelled.
                if c.attr("w").is_some_and(|v| v != "med")
                    || c.attr("len").is_some_and(|v| v != "med")
                {
                    return None;
                }
                if c.name == "headEnd" {
                    l.head = t;
                } else {
                    l.tail = t;
                }
            }
            _ => return None,
        }
    }
    Some(l)
}

struct SpPr {
    rot: f64,
    flip_h: bool,
    flip_v: bool,
    preset: String,
    adjust: Vec<(String, i64)>,
    fill: Fill,
    line: LineFormat,
}

fn sp_pr(n: &Node) -> R<SpPr> {
    if !only_attrs(n, &["bwMode"]) {
        return None;
    }
    let mut out = SpPr {
        rot: 0.0,
        flip_h: false,
        flip_v: false,
        preset: "rect".into(),
        adjust: Vec::new(),
        fill: Fill::Default,
        line: LineFormat::default(),
    };
    for c in &n.children {
        match c.name.as_str() {
            "xfrm" => {
                if !only_attrs(c, &["rot", "flipH", "flipV"])
                    || c.children
                        .iter()
                        .any(|x| !matches!(x.name.as_str(), "off" | "ext"))
                {
                    return None;
                }
                out.rot = num::<f64>(c, "rot")
                    .filter(|v| v.is_finite())
                    .map_or(0.0, |v| (v / 60_000.0) % 360.0);
                out.flip_h = bool_attr(c, "flipH");
                out.flip_v = bool_attr(c, "flipV");
            }
            "prstGeom" => {
                out.preset = c.attr("prst")?.to_string();
                for av in &c.children {
                    if av.name != "avLst" {
                        return None;
                    }
                    for gd in &av.children {
                        let f = gd.attr("fmla")?;
                        let v = f.strip_prefix("val ")?.trim().parse::<i64>().ok()?;
                        out.adjust.push((gd.attr("name")?.to_string(), v));
                    }
                }
            }
            "noFill" | "solidFill" => out.fill = fill_of(c)?,
            "ln" => out.line = line_of(c)?,
            "effectLst" if c.children.is_empty() => {}
            _ => return None,
        }
    }
    Some(out)
}

fn style_of(n: &Node) -> R<StyleRefs> {
    let r = |name: &str| -> R<(u32, ObjColor)> {
        let c = n.child(name)?;
        Some((num::<u32>(c, "idx").unwrap_or(0), color_child(c)?))
    };
    let font = n.child("fontRef")?;
    if n.children.len() != 4 {
        return None;
    }
    Some(StyleRefs {
        line: r("lnRef")?,
        fill: r("fillRef")?,
        effect: r("effectRef")?,
        font: (
            font.attr("idx").unwrap_or("minor").to_string(),
            color_child(font)?,
        ),
    })
}

fn run_font(n: &Node) -> R<RunFont> {
    let mut f = RunFont {
        size: num::<u32>(n, "sz"),
        bold: bool_attr(n, "b"),
        italic: bool_attr(n, "i"),
        underline: match n.attr("u") {
            None | Some("none") => false,
            Some("sng") => true,
            Some(_) => return None,
        },
        attrs: kept_attrs(n, &["sz", "b", "i", "u"]),
        ..RunFont::default()
    };
    for c in &n.children {
        match c.name.as_str() {
            "solidFill" => f.color = Some(color_child(c)?),
            "latin" => f.latin = c.attr("typeface").map(str::to_string),
            "ea" | "cs" => {
                if let Some(t) = c.attr("typeface") {
                    f.attrs.push((format!("_{}", c.name), t.to_string()));
                }
            }
            _ => return None,
        }
    }
    Some(f)
}

fn text_body(n: &Node) -> R<TextBody> {
    let mut t = TextBody::default();
    for c in &n.children {
        match c.name.as_str() {
            "bodyPr" => {
                t.body_attrs = kept_attrs(c, &[]);
                for k in &c.children {
                    match k.name.as_str() {
                        "noAutofit" | "spAutoFit" | "normAutofit" => {
                            t.autofit = Some(k.name.clone());
                        }
                        _ => return None,
                    }
                }
            }
            "lstStyle" if c.children.is_empty() => {}
            "p" => {
                let mut p = Paragraph::default();
                for k in &c.children {
                    match k.name.as_str() {
                        "pPr" => {
                            if !only_attrs(k, &["algn", "rtl"]) || !k.children.is_empty() {
                                return None;
                            }
                            p.align = k.attr("algn").map(str::to_string);
                        }
                        "r" => {
                            let mut run = TextRun::default();
                            for x in &k.children {
                                match x.name.as_str() {
                                    "rPr" => run.font = run_font(x)?,
                                    "t" => run.text.push_str(&x.text),
                                    _ => return None,
                                }
                            }
                            p.runs.push(run);
                        }
                        "br" => {
                            let font = match k.child("rPr") {
                                Some(x) => run_font(x)?,
                                None => RunFont::default(),
                            };
                            p.runs.push(TextRun {
                                text: "\n".into(),
                                font,
                            });
                        }
                        "endParaRPr" => p.end = Some(run_font(k)?),
                        _ => return None,
                    }
                }
                t.paras.push(p);
            }
            _ => return None,
        }
    }
    Some(t)
}

/// `cNvPr`: (name, descr, title, hidden).
fn c_nv_pr(n: &Node) -> R<(String, String, String, bool)> {
    if !only_attrs(n, &["id", "name", "descr", "title", "hidden"]) {
        return None;
    }
    // Only the creation-id extension list may appear (no hyperlinks).
    for c in &n.children {
        if c.name != "extLst" {
            return None;
        }
    }
    Some((
        n.attr("name").unwrap_or("").to_string(),
        n.attr("descr").unwrap_or("").to_string(),
        n.attr("title").unwrap_or("").to_string(),
        bool_attr(n, "hidden"),
    ))
}

/// Attributes of the single locks child (`spLocks`, `picLocks`, `cxnSpLocks`) of `n`.
fn locks(n: &Node, tag: &str) -> R<Vec<(String, String)>> {
    let mut out = Vec::new();
    for c in &n.children {
        if c.name != tag || !c.children.is_empty() {
            return None;
        }
        out = kept_attrs(c, &[]);
    }
    Some(out)
}

/// What a picture needs from the package: its image bytes by relationship id.
pub struct MediaLookup<'a> {
    pub rels: &'a [RawRel],
    pub parts: &'a [&'a RawPart],
}

impl MediaLookup<'_> {
    fn image(&self, rid: &str) -> R<(ImageData, String)> {
        let rel = self.rels.iter().find(|r| r.id == rid && !r.external)?;
        let part = self.parts.iter().find(|p| p.path == rel.target)?;
        let img = sp_render::objects::image_from_file(part.data.clone())?;
        Some((img, part.path.clone()))
    }
}

fn picture(n: &Node, media: &MediaLookup) -> R<(DrawObject, String)> {
    if !only_attrs(n, &["macro", "fPublished"])
        || n.attr("macro").is_some_and(|m| !m.is_empty())
        || bool_attr(n, "fPublished")
    {
        return None;
    }
    let mut o = DrawObject::default();
    let mut pic = Picture::default();
    let mut path = None;
    for c in &n.children {
        match c.name.as_str() {
            "nvPicPr" => {
                for k in &c.children {
                    match k.name.as_str() {
                        "cNvPr" => (o.name, o.descr, o.title, o.hidden) = c_nv_pr(k)?,
                        "cNvPicPr" if only_attrs(k, &["preferRelativeResize"]) => {
                            pic.locks = locks(k, "picLocks")?;
                        }
                        _ => return None,
                    }
                }
            }
            "blipFill" => {
                if !only_attrs(c, &["rotWithShape", "dpi"]) {
                    return None;
                }
                for k in &c.children {
                    match k.name.as_str() {
                        "blip" => {
                            if !only_attrs(k, &["embed", "cstate"]) {
                                return None;
                            }
                            for e in &k.children {
                                let ok = e.name == "extLst"
                                    && e.children.iter().all(|x| {
                                        x.name == "ext"
                                            && x.attr("uri").is_some_and(|u| {
                                                u.eq_ignore_ascii_case(EXT_LOCAL_DPI)
                                            })
                                    });
                                if !ok {
                                    return None;
                                }
                            }
                            let (img, p) = media.image(k.attr("embed")?)?;
                            pic.image = img;
                            path = Some(p);
                        }
                        "srcRect" => {
                            if k.attrs.iter().any(|(_, v)| v.trim() != "0") {
                                return None; // cropping is not modelled
                            }
                        }
                        "stretch" => {
                            if k.children
                                .iter()
                                .any(|f| f.name != "fillRect" || !f.attrs.is_empty())
                            {
                                return None;
                            }
                        }
                        _ => return None,
                    }
                }
            }
            "spPr" => {
                let sp = sp_pr(c)?;
                if sp.preset != "rect"
                    || !sp.adjust.is_empty()
                    || sp.fill != Fill::Default && sp.fill != Fill::None
                {
                    return None;
                }
                o.rotation = sp.rot;
                o.flip_h = sp.flip_h;
                o.flip_v = sp.flip_v;
                pic.line = sp.line;
            }
            _ => return None,
        }
    }
    o.kind = ObjectKind::Picture(pic);
    Some((o, path?))
}

fn shape(n: &Node, connector: bool) -> R<DrawObject> {
    if !only_attrs(n, &["macro", "textlink", "fLocksText", "fPublished"])
        || n.attr("macro").is_some_and(|m| !m.is_empty())
        || bool_attr(n, "fPublished")
        || n.attr("fLocksText") == Some("0")
    {
        return None;
    }
    let mut o = DrawObject::default();
    let mut s = Shape {
        connector,
        text_link: n.attr("textlink").unwrap_or("").to_string(),
        ..Shape::default()
    };
    let (nv, cnv, lock_tag) = if connector {
        ("nvCxnSpPr", "cNvCxnSpPr", "cxnSpLocks")
    } else {
        ("nvSpPr", "cNvSpPr", "spLocks")
    };
    let mut seen_pr = false;
    for c in &n.children {
        match c.name.as_str() {
            x if x == nv => {
                for k in &c.children {
                    match k.name.as_str() {
                        "cNvPr" => (o.name, o.descr, o.title, o.hidden) = c_nv_pr(k)?,
                        x if x == cnv && only_attrs(k, &["txBox"]) => {
                            s.text_box = bool_attr(k, "txBox");
                            s.locks = locks(k, lock_tag)?;
                        }
                        _ => return None,
                    }
                }
            }
            "spPr" => {
                let sp = sp_pr(c)?;
                o.rotation = sp.rot;
                o.flip_h = sp.flip_h;
                o.flip_v = sp.flip_v;
                s.preset = sp.preset;
                s.adjust = sp.adjust;
                s.fill = sp.fill;
                s.line = sp.line;
                seen_pr = true;
            }
            "style" => s.style = Some(style_of(c)?),
            "txBody" if !connector => s.text = Some(text_body(c)?),
            _ => return None,
        }
    }
    if !seen_pr {
        return None;
    }
    o.kind = ObjectKind::Shape(s);
    Some(o)
}

/// The modelled object in one drawing anchor (with the media part it uses), or `None`
/// when anything in it is unmodelled. `charts_before` charts precede it in the drawing.
pub fn parse_object(
    anchor: &Node,
    media: &MediaLookup,
    charts_before: usize,
) -> R<(DrawObject, Option<String>)> {
    let a = anchor_of(anchor)?;
    let mut found: Option<(DrawObject, Option<String>)> = None;
    for c in &anchor.children {
        match c.name.as_str() {
            "from" | "to" | "ext" | "pos" => {}
            "clientData" => {
                if c.attrs.iter().any(|(_, v)| v == "0" || v == "false") || !c.children.is_empty() {
                    return None;
                }
            }
            "pic" if found.is_none() => {
                let (o, p) = picture(c, media)?;
                found = Some((o, Some(p)));
            }
            "sp" if found.is_none() => found = Some((shape(c, false)?, None)),
            "cxnSp" if found.is_none() => found = Some((shape(c, true)?, None)),
            _ => return None,
        }
    }
    let (mut o, path) = found?;
    o.anchor = a;
    o.after_charts = charts_before;
    Some((o, path))
}

/// Whether an anchor holds a picture, shape or connector (rather than a chart frame).
pub fn is_object_anchor(anchor: &Node) -> bool {
    anchor.children.iter().any(|c| {
        matches!(
            c.name.as_str(),
            "pic" | "sp" | "cxnSp" | "grpSp" | "contentPart"
        )
    })
}

/// Most objects one drawing may hold for adoption.
pub fn max_objects() -> usize {
    MAX_READ
}

/// Keep only the media paths nothing else in the package still points at (the drawing
/// being adopted aside), so shared images stay with their other users.
pub fn droppable_media(
    pt: &Passthrough,
    wb_parts: &[RawPart],
    drawing: &str,
    media: Vec<String>,
) -> Vec<String> {
    let mut used: HashSet<&str> = pt
        .rels
        .iter()
        .filter(|r| !r.external)
        .map(|r| r.target.as_str())
        .collect();
    for p in pt.parts.iter().chain(wb_parts.iter()) {
        if p.path == drawing {
            continue;
        }
        used.extend(
            p.rels
                .iter()
                .filter(|r| !r.external)
                .map(|r| r.target.as_str()),
        );
    }
    media
        .into_iter()
        .filter(|m| !used.contains(m.as_str()))
        .collect()
}

// ---------------------------------------------------------------------------------------
// Sheet background

/// Adopt each sheet's captured `<picture r:id>` (sheet background) when it points at an
/// image we draw; the image part is dropped from pass-through unless something else uses it.
pub fn adopt_backgrounds(wb: &mut Workbook) {
    let wb_parts = wb.passthrough.parts.clone();
    for sheet in &mut wb.sheets {
        let pt = &mut sheet.passthrough;
        let Some(ei) = pt
            .elements
            .iter()
            .position(|e| crate::xml::local(&e.name) == "picture")
        else {
            continue;
        };
        let Some(rid) = pt
            .elements
            .get(ei)
            .and_then(|e| parse_dom(&e.xml).ok())
            .and_then(|n| n.attr("id").map(str::to_string))
        else {
            continue;
        };
        let Some(rel) = pt.rels.iter().find(|r| r.id == rid && !r.external).cloned() else {
            continue;
        };
        let Some(part) = pt
            .parts
            .iter()
            .chain(wb_parts.iter())
            .find(|p| p.path == rel.target)
        else {
            continue;
        };
        let Some(img) = sp_render::objects::image_from_file(part.data.clone()) else {
            continue;
        };
        sheet.background = Some(img);
        pt.elements.remove(ei);
        pt.rels.retain(|r| r.id != rid);
        let still_used = pt.rels.iter().any(|r| r.target == rel.target)
            || pt
                .parts
                .iter()
                .chain(wb_parts.iter())
                .any(|p| p.rels.iter().any(|r| r.target == rel.target));
        if !still_used {
            pt.parts.retain(|p| p.path != rel.target);
        }
    }
}

// ---------------------------------------------------------------------------------------
// Writing

/// Media part names for every distinct image the writer emits (object pictures and sheet
/// backgrounds), chosen around captured part names.
#[derive(Default)]
pub struct MediaPlan {
    by_key: HashMap<(usize, usize), String>,
    files: Vec<(String, Arc<[u8]>, &'static str)>,
}

impl MediaPlan {
    /// Plan media for `sheets` (the sheets whose drawings / backgrounds the writer
    /// generates), taking names not in `taken`.
    pub fn new(wb: &Workbook, sheets: &[usize], taken: &mut HashSet<String>) -> MediaPlan {
        let mut plan = MediaPlan::default();
        let mut n = 0usize;
        let images = wb.sheets.iter().enumerate().flat_map(|(i, s)| {
            let own = sheets.contains(&i);
            s.objects
                .iter()
                .filter(move |_| own)
                .filter_map(|o| o.picture().map(|p| &p.image))
                .chain(s.background.iter())
        });
        for img in images {
            if plan.by_key.contains_key(&img.key()) {
                continue;
            }
            let path = loop {
                n += 1;
                let cand = format!("xl/media/image{n}.{}", img.format.ext());
                if taken.insert(cand.to_ascii_lowercase()) || n > 10_000_000 {
                    break cand;
                }
            };
            plan.by_key.insert(img.key(), path.clone());
            plan.files
                .push((path, img.bytes.clone(), img.format.content_type()));
        }
        plan
    }

    pub fn path_of(&self, img: &ImageData) -> Option<&str> {
        self.by_key.get(&img.key()).map(String::as_str)
    }

    pub fn part_names(&self) -> Vec<String> {
        self.files.iter().map(|f| f.0.clone()).collect()
    }

    /// (part name, bytes, content type) of every planned image.
    pub fn files(&self) -> &[(String, Arc<[u8]>, &'static str)] {
        &self.files
    }
}

/// The worksheet `<picture r:id>` for the sheet background (relationship added to `rels`).
pub fn background_element(
    sheet: &Sheet,
    media: &MediaPlan,
    rels: &mut SheetRels,
) -> Option<String> {
    let img = sheet.background.as_ref()?;
    let path = media.path_of(img)?;
    let file = path.rsplit('/').next().unwrap_or(path);
    let id = rels.add("image", &format!("../media/{file}"), false);
    Some(format!("<picture r:id=\"{id}\"/>"))
}

fn emu(px: f64) -> i64 {
    if px.is_finite() {
        (px.max(0.0) * EMU_PER_PX).round().min(1e13) as i64
    } else {
        0
    }
}

fn marker_xml(tag: &str, a: CellAddr, off: (f64, f64)) -> String {
    format!(
        "<xdr:{tag}><xdr:col>{}</xdr:col><xdr:colOff>{}</xdr:colOff><xdr:row>{}</xdr:row><xdr:rowOff>{}</xdr:rowOff></xdr:{tag}>",
        a.col,
        emu(off.0),
        a.row,
        emu(off.1)
    )
}

fn attrs_xml(attrs: &[(String, String)]) -> String {
    let mut s = String::new();
    for (k, v) in attrs {
        if k.starts_with('_') || k.is_empty() {
            continue;
        }
        let _ = write!(s, " {k}=\"{}\"", esc(v));
    }
    s
}

fn color_xml(c: &ObjColor) -> String {
    let mods = |m: &[ColorMod]| {
        m.iter()
            .map(|x| format!("<a:{} val=\"{}\"/>", esc(&x.name), x.val))
            .collect::<String>()
    };
    match c {
        ObjColor::Rgb(rgb, m) => format!(
            "<a:srgbClr val=\"{:02X}{:02X}{:02X}\">{}</a:srgbClr>",
            rgb.0,
            rgb.1,
            rgb.2,
            mods(m)
        ),
        ObjColor::Scheme(n, m) => {
            format!("<a:schemeClr val=\"{}\">{}</a:schemeClr>", esc(n), mods(m))
        }
        ObjColor::System(n, last, m) => format!(
            "<a:sysClr val=\"{}\" lastClr=\"{:02X}{:02X}{:02X}\">{}</a:sysClr>",
            esc(n),
            last.0,
            last.1,
            last.2,
            mods(m)
        ),
    }
}

fn fill_xml(f: &Fill) -> String {
    match f {
        Fill::Default => String::new(),
        Fill::None => "<a:noFill/>".into(),
        Fill::Solid(c) => format!("<a:solidFill>{}</a:solidFill>", color_xml(c)),
    }
}

fn line_xml(l: &LineFormat) -> String {
    if l.is_default() {
        return String::new();
    }
    let mut x = String::from("<a:ln");
    if let Some(w) = l.width {
        let _ = write!(x, " w=\"{}\"", w.clamp(0, 20_116_800));
    }
    x.push_str(&attrs_xml(&l.attrs));
    x.push('>');
    x.push_str(&fill_xml(&l.fill));
    if let Some(d) = &l.dash {
        let _ = write!(x, "<a:prstDash val=\"{}\"/>", esc(d));
    }
    if let Some(j) = &l.join {
        let _ = write!(x, "<a:{}/>", esc(j));
    }
    if let Some(h) = &l.head {
        let _ = write!(x, "<a:headEnd type=\"{}\"/>", esc(h));
    }
    if let Some(t) = &l.tail {
        let _ = write!(x, "<a:tailEnd type=\"{}\"/>", esc(t));
    }
    x.push_str("</a:ln>");
    x
}

fn xfrm_xml(o: &DrawObject, rect: (f64, f64, f64, f64)) -> String {
    let mut x = String::from("<a:xfrm");
    let rot = (o.rotation.rem_euclid(360.0) * 60_000.0).round() as i64;
    if rot != 0 {
        let _ = write!(x, " rot=\"{rot}\"");
    }
    if o.flip_h {
        x.push_str(" flipH=\"1\"");
    }
    if o.flip_v {
        x.push_str(" flipV=\"1\"");
    }
    let _ = write!(
        x,
        "><a:off x=\"{}\" y=\"{}\"/><a:ext cx=\"{}\" cy=\"{}\"/></a:xfrm>",
        emu(rect.0),
        emu(rect.1),
        emu(rect.2),
        emu(rect.3)
    );
    x
}

fn geom_xml(preset: &str, adjust: &[(String, i64)]) -> String {
    let p = if preset.is_empty() { "rect" } else { preset };
    if adjust.is_empty() {
        return format!("<a:prstGeom prst=\"{}\"><a:avLst/></a:prstGeom>", esc(p));
    }
    let mut x = format!("<a:prstGeom prst=\"{}\"><a:avLst>", esc(p));
    for (n, v) in adjust {
        let _ = write!(x, "<a:gd name=\"{}\" fmla=\"val {v}\"/>", esc(n));
    }
    x.push_str("</a:avLst></a:prstGeom>");
    x
}

fn rpr_xml(tag: &str, f: &RunFont) -> String {
    let mut x = format!("<a:{tag}");
    x.push_str(&attrs_xml(&f.attrs));
    if let Some(sz) = f.size {
        let _ = write!(x, " sz=\"{}\"", sz.clamp(100, 400_000));
    }
    if f.bold {
        x.push_str(" b=\"1\"");
    }
    if f.italic {
        x.push_str(" i=\"1\"");
    }
    if f.underline {
        x.push_str(" u=\"sng\"");
    }
    let mut kids = String::new();
    if let Some(c) = &f.color {
        let _ = write!(kids, "<a:solidFill>{}</a:solidFill>", color_xml(c));
    }
    if let Some(l) = &f.latin {
        let _ = write!(kids, "<a:latin typeface=\"{}\"/>", esc(l));
    }
    for (k, v) in &f.attrs {
        if let Some(t) = k.strip_prefix('_') {
            let _ = write!(kids, "<a:{t} typeface=\"{}\"/>", esc(v));
        }
    }
    if kids.is_empty() {
        x.push_str("/>");
    } else {
        let _ = write!(x, ">{kids}</a:{tag}>");
    }
    x
}

fn txbody_xml(t: &TextBody) -> String {
    let mut x = format!("<xdr:txBody><a:bodyPr{}", attrs_xml(&t.body_attrs));
    match &t.autofit {
        Some(a) => {
            let _ = write!(x, "><a:{}/></a:bodyPr>", esc(a));
        }
        None => x.push_str("/>"),
    }
    x.push_str("<a:lstStyle/>");
    if t.paras.is_empty() {
        x.push_str("<a:p/>");
    }
    for p in &t.paras {
        x.push_str("<a:p>");
        if let Some(a) = &p.align {
            let _ = write!(x, "<a:pPr algn=\"{}\"/>", esc(a));
        }
        for r in &p.runs {
            if r.text == "\n" {
                let _ = write!(x, "<a:br>{}</a:br>", rpr_xml("rPr", &r.font));
                continue;
            }
            let _ = write!(
                x,
                "<a:r>{}<a:t>{}</a:t></a:r>",
                rpr_xml("rPr", &r.font),
                esc(&r.text)
            );
        }
        if let Some(e) = &p.end {
            x.push_str(&rpr_xml("endParaRPr", e));
        }
        x.push_str("</a:p>");
    }
    x.push_str("</xdr:txBody>");
    x
}

fn style_xml(s: &StyleRefs) -> String {
    format!(
        "<xdr:style><a:lnRef idx=\"{}\">{}</a:lnRef><a:fillRef idx=\"{}\">{}</a:fillRef><a:effectRef idx=\"{}\">{}</a:effectRef><a:fontRef idx=\"{}\">{}</a:fontRef></xdr:style>",
        s.line.0,
        color_xml(&s.line.1),
        s.fill.0,
        color_xml(&s.fill.1),
        s.effect.0,
        color_xml(&s.effect.1),
        esc(&s.font.0),
        color_xml(&s.font.1)
    )
}

fn nv_pr(o: &DrawObject, id: usize, fallback: &str) -> String {
    let name = if o.name.is_empty() {
        format!("{fallback} {id}")
    } else {
        o.name.clone()
    };
    let mut x = format!("<xdr:cNvPr id=\"{id}\" name=\"{}\"", esc(&name));
    if !o.descr.is_empty() {
        let _ = write!(x, " descr=\"{}\"", esc(&o.descr));
    }
    if !o.title.is_empty() {
        let _ = write!(x, " title=\"{}\"", esc(&o.title));
    }
    if o.hidden {
        x.push_str(" hidden=\"1\"");
    }
    x.push_str("/>");
    x
}

fn locks_xml(tag: &str, l: &[(String, String)]) -> String {
    if l.is_empty() {
        String::new()
    } else {
        format!("<a:{tag}{}/>", attrs_xml(l))
    }
}

/// One anchor element holding object `o` (cNvPr `id`; `image_rid` = the relationship of
/// a picture's image). Declares its namespaces so it can also go into a drawing part
/// written by another application. `rect` = the object box from A1 in px (for `xfrm`).
pub fn object_anchor_xml(
    o: &DrawObject,
    id: usize,
    image_rid: Option<&str>,
    rect: (f64, f64, f64, f64),
) -> Option<String> {
    let ns = format!("xmlns:xdr=\"{NS_XDR}\" xmlns:a=\"{NS_A}\" xmlns:r=\"{NS_R}\"");
    let a = &o.anchor;
    let (open, close) = match a.kind {
        AnchorKind::TwoCell => {
            let edit = match a.placement {
                Placement::MoveAndSize => "",
                Placement::Move => " editAs=\"oneCell\"",
                Placement::Fixed => " editAs=\"absolute\"",
            };
            (
                format!(
                    "<xdr:twoCellAnchor{edit} {ns}>{}{}",
                    marker_xml("from", a.cells.from, a.cells.from_off),
                    marker_xml("to", a.cells.to, a.cells.to_off)
                ),
                "</xdr:twoCellAnchor>",
            )
        }
        AnchorKind::OneCell => (
            format!(
                "<xdr:oneCellAnchor {ns}>{}<xdr:ext cx=\"{}\" cy=\"{}\"/>",
                marker_xml("from", a.cells.from, a.cells.from_off),
                emu(a.ext.0),
                emu(a.ext.1)
            ),
            "</xdr:oneCellAnchor>",
        ),
        AnchorKind::Absolute => (
            format!(
                "<xdr:absoluteAnchor {ns}><xdr:pos x=\"{}\" y=\"{}\"/><xdr:ext cx=\"{}\" cy=\"{}\"/>",
                emu(a.pos.0),
                emu(a.pos.1),
                emu(a.ext.0),
                emu(a.ext.1)
            ),
            "</xdr:absoluteAnchor>",
        ),
    };
    let body = match &o.kind {
        ObjectKind::Picture(p) => {
            let rid = image_rid?;
            format!(
                "<xdr:pic macro=\"\"><xdr:nvPicPr>{}<xdr:cNvPicPr>{}</xdr:cNvPicPr></xdr:nvPicPr><xdr:blipFill><a:blip r:embed=\"{}\"/><a:stretch><a:fillRect/></a:stretch></xdr:blipFill><xdr:spPr>{}{}{}</xdr:spPr></xdr:pic>",
                nv_pr(o, id, "Picture"),
                locks_xml("picLocks", &p.locks),
                esc(rid),
                xfrm_xml(o, rect),
                geom_xml("rect", &[]),
                line_xml(&p.line)
            )
        }
        ObjectKind::Shape(s) => {
            let sp_pr = format!(
                "<xdr:spPr>{}{}{}{}</xdr:spPr>",
                xfrm_xml(o, rect),
                geom_xml(&s.preset, &s.adjust),
                fill_xml(&s.fill),
                line_xml(&s.line)
            );
            let style = s.style.as_ref().map(style_xml).unwrap_or_default();
            if s.connector && s.text.is_none() && s.text_link.is_empty() {
                format!(
                    "<xdr:cxnSp macro=\"\"><xdr:nvCxnSpPr>{}<xdr:cNvCxnSpPr>{}</xdr:cNvCxnSpPr></xdr:nvCxnSpPr>{sp_pr}{style}</xdr:cxnSp>",
                    nv_pr(o, id, "Straight Connector"),
                    locks_xml("cxnSpLocks", &s.locks)
                )
            } else {
                let tb = if s.text_box { " txBox=\"1\"" } else { "" };
                format!(
                    "<xdr:sp macro=\"\" textlink=\"{}\"><xdr:nvSpPr>{}<xdr:cNvSpPr{tb}>{}</xdr:cNvSpPr></xdr:nvSpPr>{sp_pr}{style}{}</xdr:sp>",
                    esc(&s.text_link),
                    nv_pr(o, id, if s.text_box { "TextBox" } else { "Shape" }),
                    locks_xml("spLocks", &s.locks),
                    s.text.as_ref().map(txbody_xml).unwrap_or_default()
                )
            }
        }
    };
    Some(format!("{open}{body}<xdr:clientData/>{close}"))
}

/// Draw order of a drawing part holding `n_charts` charts and `objects`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Chart(usize),
    Object(usize),
}

pub fn draw_order(n_charts: usize, objects: &[DrawObject]) -> Vec<Slot> {
    let mut out = Vec::with_capacity(n_charts + objects.len());
    for k in 0..=n_charts {
        for (i, o) in objects.iter().enumerate() {
            let at = o.after_charts.min(n_charts);
            if at == k {
                out.push(Slot::Object(i));
            }
        }
        if k < n_charts {
            out.push(Slot::Chart(k));
        }
    }
    out
}

/// Writes the anchors for one drawing part: allocates image relationship ids.
pub struct ObjectWriter<'a> {
    pub wb: &'a Workbook,
    pub sheet: usize,
    pub media: &'a MediaPlan,
    next_rid: usize,
}

impl<'a> ObjectWriter<'a> {
    pub fn new(wb: &'a Workbook, sheet: usize, media: &'a MediaPlan) -> Self {
        ObjectWriter {
            wb,
            sheet,
            media,
            next_rid: 0,
        }
    }

    /// The anchor XML of object `o` (cNvPr `id`), adding its image relationship to `drels`
    /// (a drawing part's `Relationships` XML being built).
    pub fn anchor(&mut self, o: &DrawObject, id: usize, drels: &mut String) -> String {
        let rect = self
            .wb
            .sheet(self.sheet)
            .map(|s| sp_render::objects::object_rect_abs(s, &o.anchor))
            .unwrap_or_default();
        let mut rid = None;
        if let Some(p) = o.picture()
            && let Some(path) = self.media.path_of(&p.image)
        {
            self.next_rid += 1;
            let id = format!("rIdImg{}", self.next_rid);
            let file = path.rsplit('/').next().unwrap_or(path);
            let _ = write!(
                drels,
                "<Relationship Id=\"{id}\" Type=\"{REL_IMAGE}\" Target=\"../media/{file}\"/>"
            );
            rid = Some(id);
        }
        object_anchor_xml(o, id, rid.as_deref(), rect).unwrap_or_default()
    }
}

/// Objects added in SheetPro to a sheet whose drawing part is passed through (it holds
/// something we don't model) are appended to that part as extra anchors, their images as
/// new pass-through media parts. Called on the save copy by `xlsx_charts::prepare_for_save`.
pub fn append_to_captured(out: &mut Workbook, taken: &mut HashSet<String>) {
    let mut m = 0usize;
    for si in 0..out.sheets.len() {
        let rects: Vec<(f64, f64, f64, f64)> = match out.sheets.get(si) {
            Some(s) => s
                .objects
                .iter()
                .map(|o| sp_render::objects::object_rect_abs(s, &o.anchor))
                .collect(),
            None => continue,
        };
        let Some(sheet) = out.sheets.get_mut(si) else {
            continue;
        };
        if sheet.objects.is_empty() {
            continue;
        }
        let Some((_, _, target)) = crate::xlsx_charts::captured_drawing(&sheet.passthrough) else {
            continue;
        };
        let pt = &mut sheet.passthrough;
        let Some(di) = pt.parts.iter().position(|p| p.path == target) else {
            continue; // drawing shared with other sheets: left as it was
        };
        let Some(mut text) = pt
            .parts
            .get(di)
            .and_then(|p| std::str::from_utf8(&p.data).ok())
            .map(str::to_string)
        else {
            continue;
        };
        let Some(close) = text.rfind("</") else {
            continue;
        };
        let objects = std::mem::take(&mut sheet.objects);
        let pt = &mut sheet.passthrough;
        let existing: HashSet<String> = pt
            .parts
            .get(di)
            .map(|p| p.rels.iter().map(|r| r.id.clone()).collect())
            .unwrap_or_default();
        let mut anchors = String::new();
        let mut new_rels = Vec::new();
        let mut new_parts = Vec::new();
        let mut by_key: HashMap<(usize, usize), String> = HashMap::new();
        let mut j = 0usize;
        for (k, o) in objects.iter().enumerate() {
            let mut rid = None;
            if let Some(p) = o.picture() {
                let path = match by_key.get(&p.image.key()) {
                    Some(path) => path.clone(),
                    None => {
                        let path = loop {
                            m += 1;
                            let cand = format!("xl/media/spimage{m}.{}", p.image.format.ext());
                            if taken.insert(cand.to_ascii_lowercase()) {
                                break cand;
                            }
                        };
                        new_parts.push(RawPart {
                            path: path.clone(),
                            content_type: p.image.format.content_type().to_string(),
                            data: p.image.bytes.clone(),
                            rels: Vec::new(),
                        });
                        by_key.insert(p.image.key(), path.clone());
                        path
                    }
                };
                let id = loop {
                    j += 1;
                    let id = format!("rIdSpImg{j}");
                    if !existing.contains(&id) {
                        break id;
                    }
                };
                new_rels.push(RawRel {
                    id: id.clone(),
                    rel_type: REL_IMAGE.to_string(),
                    target: path,
                    external: false,
                });
                rid = Some(id);
            }
            let rect = rects.get(k).copied().unwrap_or_default();
            if let Some(x) = object_anchor_xml(o, 6000 + k, rid.as_deref(), rect) {
                anchors.push_str(&x);
            }
        }
        text.insert_str(close, &anchors);
        if let Some(d) = pt.parts.get_mut(di) {
            d.data = Arc::from(text.into_bytes());
            d.rels.extend(new_rels);
        }
        pt.parts.extend(new_parts);
    }
}

// ---------------------------------------------------------------------------------------
// Cell checkboxes

/// The `<extLst>` a checkbox cell format carries inside its `<xf>` (empty otherwise).
pub fn checkbox_xf_ext(s: &Style) -> &'static str {
    if s.checkbox {
        concat!(
            "<extLst><ext uri=\"{C7286773-470A-42A8-94C5-96B5CB345126}\" ",
            "xmlns:xfpb=\"http://schemas.microsoft.com/office/spreadsheetml/2022/featurepropertybag\">",
            "<xfpb:xfComplement i=\"0\"/></ext></extLst>"
        )
    } else {
        ""
    }
}

/// Whether the workbook needs the checkbox feature property bag part.
pub fn uses_checkboxes(wb: &Workbook) -> bool {
    wb.styles.iter().any(|(_, s)| s.checkbox)
        && !wb
            .passthrough
            .rels
            .iter()
            .any(|r| r.rel_type.eq_ignore_ascii_case(REL_FPB))
}

/// The feature property bag part declaring one checkbox cell control (complement 0).
pub fn checkbox_bag_xml() -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<FeaturePropertyBags xmlns=\"{NS_FPB}\"><bag type=\"Checkbox\"/><bag type=\"XFControls\"><bagId k=\"CellControl\">0</bagId></bag><bag type=\"XFComplement\"><bagId k=\"XFControls\">1</bagId></bag><bag type=\"XFComplements\" extRef=\"XFComplementsMapperExtRef\"><a k=\"MappedFeaturePropertyBags\"><bagId>2</bagId></a></bag></FeaturePropertyBags>"
    )
}

/// The `xfComplement` index of each `cellXfs/xf` (None when it has none).
fn xf_complements(styles_xml: &str) -> Vec<Option<usize>> {
    let Ok(root) = parse_dom(styles_xml) else {
        return Vec::new();
    };
    let Some(xfs) = root.child("cellXfs") else {
        return Vec::new();
    };
    xfs.children_named("xf")
        .map(|xf| {
            xf.child("extLst")?
                .children_named("ext")
                .filter(|e| {
                    e.attr("uri")
                        .is_some_and(|u| u.eq_ignore_ascii_case(EXT_XF_COMPLEMENT))
                })
                .find_map(|e| e.child("xfComplement"))
                .and_then(|c| num::<usize>(c, "i"))
        })
        .collect()
}

/// Bag types and the complement indices that resolve to a checkbox.
fn checkbox_complements(bag_xml: &str) -> Option<(Vec<String>, HashSet<usize>)> {
    let root = parse_dom(bag_xml).ok()?;
    let bags: Vec<&Node> = root.children_named("bag").collect();
    let types: Vec<String> = bags
        .iter()
        .map(|b| b.attr("type").unwrap_or("").to_string())
        .collect();
    let id_of = |n: &Node| n.text.trim().parse::<usize>().ok();
    let is_checkbox = |start: usize| {
        // Follow bagId links (XFComplement → XFControls → Checkbox), at most a few hops.
        let mut cur = start;
        for _ in 0..8 {
            let b = bags.get(cur)?;
            if b.attr("type") == Some("Checkbox") {
                return Some(true);
            }
            cur = b.children_named("bagId").find_map(id_of)?;
        }
        Some(false)
    };
    let mut set = HashSet::new();
    if let Some(map) = bags
        .iter()
        .find(|b| b.attr("type") == Some("XFComplements"))
        .and_then(|b| {
            b.children_named("a")
                .find(|a| a.attr("k") == Some("MappedFeaturePropertyBags"))
        })
    {
        for (i, id) in map.children_named("bagId").enumerate() {
            if id_of(id).and_then(is_checkbox) == Some(true) {
                set.insert(i);
            }
        }
    }
    Some((types, set))
}

/// The feature property bag part the workbook points at, if any.
pub fn checkbox_bag_target(wb_pass: &Passthrough) -> Option<String> {
    wb_pass
        .rels
        .iter()
        .find(|r| r.rel_type.eq_ignore_ascii_case(REL_FPB) && !r.external)
        .map(|r| r.target.clone())
}

/// Mark checkbox cell formats in `styles` (indexed like `cellXfs`) from the feature
/// property bag `bag_xml`. Returns whether the bag holds only checkbox bags (then the
/// writer regenerates it: drop it from pass-through with [`drop_checkbox_bag`]); a bag
/// that other features use stays passed through and nothing is modelled.
pub fn read_checkboxes(styles_xml: &str, bag_xml: &str, styles: &mut [Style]) -> bool {
    let Some((types, set)) = checkbox_complements(bag_xml) else {
        return false;
    };
    if !types.iter().all(|t| CHECKBOX_BAGS.contains(&t.as_str())) {
        return false;
    }
    for (i, c) in xf_complements(styles_xml).into_iter().enumerate() {
        if let (Some(c), Some(s)) = (c, styles.get_mut(i))
            && set.contains(&c)
        {
            s.checkbox = true;
        }
    }
    true
}

/// Remove the (regenerated) checkbox feature property bag from workbook pass-through.
pub fn drop_checkbox_bag(wb: &mut Workbook) {
    let pt = &mut wb.passthrough;
    let Some(target) = checkbox_bag_target(pt) else {
        return;
    };
    pt.rels
        .retain(|r| !(r.rel_type.eq_ignore_ascii_case(REL_FPB) && r.target == target));
    pt.parts.retain(|p| p.path != target);
}

#[cfg(test)]
mod tests {
    use super::*;
    use sp_model::objects::{ImageFormat, StyleRefs};

    pub(crate) fn png() -> Vec<u8> {
        sp_render::objects::encode_png(1, 1, vec![0, 0, 255, 255]).unwrap()
    }

    fn rect_obj() -> DrawObject {
        DrawObject {
            name: "Rectangle 1".into(),
            descr: "alt <text>".into(),
            anchor: ObjectAnchor {
                kind: AnchorKind::TwoCell,
                placement: Placement::Move,
                cells: Anchor {
                    from: CellAddr::new(1, 1),
                    from_off: (3.0, 4.0),
                    to: CellAddr::new(5, 4),
                    to_off: (0.0, 0.0),
                },
                ..ObjectAnchor::default()
            },
            rotation: 30.0,
            flip_h: true,
            kind: ObjectKind::Shape(Shape {
                preset: "roundRect".into(),
                adjust: vec![("adj".into(), 25_000)],
                fill: Fill::Solid(ObjColor::Rgb(
                    Color(1, 2, 3),
                    vec![ColorMod {
                        name: "alpha".into(),
                        val: 50_000,
                    }],
                )),
                line: LineFormat {
                    fill: Fill::Solid(ObjColor::scheme("accent2")),
                    width: Some(19_050),
                    dash: Some("dash".into()),
                    tail: Some("triangle".into()),
                    ..LineFormat::default()
                },
                style: Some(StyleRefs::shape_default()),
                text: Some(TextBody {
                    paras: vec![Paragraph {
                        align: Some("ctr".into()),
                        runs: vec![
                            TextRun {
                                text: "Hi & bye".into(),
                                font: RunFont {
                                    size: Some(1400),
                                    bold: true,
                                    color: Some(ObjColor::rgb(Color(9, 9, 9))),
                                    latin: Some("Arial".into()),
                                    attrs: vec![("lang".into(), "en-US".into())],
                                    ..RunFont::default()
                                },
                            },
                            TextRun {
                                text: "\n".into(),
                                font: RunFont::default(),
                            },
                        ],
                        end: Some(RunFont {
                            size: Some(1100),
                            ..RunFont::default()
                        }),
                    }],
                    body_attrs: vec![("anchor".into(), "ctr".into())],
                    autofit: Some("spAutoFit".into()),
                }),
                text_link: "$A$1".into(),
                ..Shape::default()
            }),
            ..DrawObject::default()
        }
    }

    fn reparse(o: &DrawObject, rid: Option<&str>, media: &MediaLookup) -> Option<DrawObject> {
        let x = object_anchor_xml(o, 2, rid, (0.0, 0.0, 10.0, 10.0))?;
        let n = parse_dom(&x).ok()?;
        parse_object(&n, media, o.after_charts).map(|(o, _)| o)
    }

    #[test]
    fn shape_xml_round_trips() {
        let empty = MediaLookup {
            rels: &[],
            parts: &[],
        };
        let o = rect_obj();
        let back = reparse(&o, None, &empty).unwrap();
        assert_eq!(back.kind, o.kind);
        assert_eq!(back.anchor, o.anchor);
        assert_eq!(
            (back.name.as_str(), back.descr.as_str()),
            ("Rectangle 1", "alt <text>")
        );
        assert!((back.rotation - 30.0).abs() < 1e-9 && back.flip_h);
        // Connector and the other anchor kinds.
        let mut c = o.clone();
        if let ObjectKind::Shape(s) = &mut c.kind {
            s.connector = true;
            s.text = None;
            s.text_link.clear();
            s.preset = "straightConnector1".into();
        }
        c.anchor.kind = AnchorKind::OneCell;
        c.anchor.placement = Placement::Move;
        c.anchor.ext = (120.0, 40.0);
        let back = reparse(&c, None, &empty).unwrap();
        assert_eq!(back.kind, c.kind);
        assert_eq!(back.anchor.ext, (120.0, 40.0));
        c.anchor.kind = AnchorKind::Absolute;
        c.anchor.placement = Placement::Fixed;
        c.anchor.pos = (15.0, 25.0);
        let back = reparse(&c, None, &empty).unwrap();
        assert_eq!(back.anchor.pos, (15.0, 25.0));
    }

    #[test]
    fn picture_xml_round_trips() {
        let part = RawPart {
            path: "xl/media/image1.png".into(),
            content_type: "image/png".into(),
            data: Arc::from(png()),
            rels: Vec::new(),
        };
        let rels = vec![RawRel {
            id: "rIdImg1".into(),
            rel_type: REL_IMAGE.into(),
            target: part.path.clone(),
            external: false,
        }];
        let parts = [&part];
        let media = MediaLookup {
            rels: &rels,
            parts: &parts,
        };
        let o = DrawObject {
            name: "Picture 1".into(),
            descr: "a dot".into(),
            kind: ObjectKind::Picture(Picture {
                image: sp_render::objects::image_from_file(Arc::from(png())).unwrap(),
                locks: vec![("noChangeAspect".into(), "1".into())],
                ..Picture::default()
            }),
            ..DrawObject::default()
        };
        let back = reparse(&o, Some("rIdImg1"), &media).unwrap();
        let p = back.picture().unwrap();
        assert_eq!(p.image.format, ImageFormat::Png);
        assert_eq!((p.image.width, p.image.height), (1, 1));
        assert_eq!(p.locks, o.picture().unwrap().locks);
        assert_eq!(back.descr, "a dot");
        // A missing image or an unknown rel keeps the drawing unmodelled.
        assert!(reparse(&o, Some("nope"), &media).is_none());
    }

    #[test]
    fn unmodelled_content_is_refused() {
        let empty = MediaLookup {
            rels: &[],
            parts: &[],
        };
        let base = object_anchor_xml(&rect_obj(), 2, None, (0.0, 0.0, 1.0, 1.0)).unwrap();
        for (from, to) in [
            (
                "<a:solidFill><a:srgbClr val=\"010203\">",
                "<a:gradFill><a:gsLst/></a:gradFill><a:solidFill><a:srgbClr val=\"010203\">",
            ),
            ("<a:avLst>", "<a:avLst><a:gd name=\"x\" fmla=\"*/ w 1 2\"/>"),
            (
                "<xdr:cNvSpPr>",
                "<xdr:cNvSpPr><a:hlinkClick r:id=\"rId9\"/>",
            ),
            ("<a:lstStyle/>", "<a:lstStyle><a:lvl1pPr/></a:lstStyle>"),
            (
                "<xdr:clientData/>",
                "<xdr:clientData fPrintsWithSheet=\"0\"/>",
            ),
            ("macro=\"\"", "macro=\"Module1.Run\""),
            ("<a:prstDash val=\"dash\"/>", "<a:custDash/>"),
        ] {
            assert!(base.contains(from), "{from}");
            let x = base.replacen(from, to, 1);
            let n = parse_dom(&x).unwrap();
            assert!(parse_object(&n, &empty, 0).is_none(), "{to}");
        }
        let group = format!(
            "<xdr:twoCellAnchor xmlns:xdr=\"{NS_XDR}\"><xdr:from/><xdr:to/><xdr:grpSp/><xdr:clientData/></xdr:twoCellAnchor>"
        );
        assert!(parse_object(&parse_dom(&group).unwrap(), &empty, 0).is_none());
    }

    #[test]
    fn draw_order_interleaves() {
        let mk = |k| DrawObject {
            after_charts: k,
            ..DrawObject::default()
        };
        let objs = vec![mk(0), mk(2), mk(1), mk(9)];
        assert_eq!(
            draw_order(2, &objs),
            vec![
                Slot::Object(0),
                Slot::Chart(0),
                Slot::Object(2),
                Slot::Chart(1),
                Slot::Object(1),
                Slot::Object(3)
            ]
        );
    }

    #[test]
    fn checkbox_bag_resolves() {
        let (types, set) = checkbox_complements(&checkbox_bag_xml()).unwrap();
        assert_eq!(types.len(), 4);
        assert!(set.contains(&0));
        let styles = r#"<styleSheet><cellXfs count="2"><xf numFmtId="0"/><xf numFmtId="0"><extLst><ext uri="{C7286773-470A-42A8-94C5-96B5CB345126}" xmlns:xfpb="x"><xfpb:xfComplement i="0"/></ext></extLst></xf></cellXfs></styleSheet>"#;
        assert_eq!(xf_complements(styles), vec![None, Some(0)]);
        let mut st = vec![Style::default(), Style::default()];
        assert!(read_checkboxes(styles, &checkbox_bag_xml(), &mut st));
        assert!(!st[0].checkbox && st[1].checkbox);
        let other = r#"<FeaturePropertyBags><bag type="Checkbox"/><bag type="SomethingElse"/></FeaturePropertyBags>"#;
        assert!(!read_checkboxes(styles, other, &mut st));
        // Hostile: cyclic bag links terminate.
        let cyc = r#"<FeaturePropertyBags><bag type="XFComplement"><bagId>0</bagId></bag><bag type="XFComplements"><a k="MappedFeaturePropertyBags"><bagId>0</bagId></a></bag></FeaturePropertyBags>"#;
        let (_, set) = checkbox_complements(cyc).unwrap();
        assert!(set.is_empty());
    }

    fn save_load(wb: &Workbook) -> (Workbook, Vec<u8>) {
        let mut buf = std::io::Cursor::new(Vec::new());
        crate::xlsx_write::write_xlsx(wb, &mut buf).unwrap();
        let bytes = buf.into_inner();
        let problems = crate::xlsx_passthrough::validate_package(&bytes);
        assert!(problems.is_empty(), "{problems:?}");
        let (back, _) = crate::xlsx_read::read_xlsx(std::io::Cursor::new(bytes.clone())).unwrap();
        (back, bytes)
    }

    fn part_names(bytes: &[u8]) -> Vec<String> {
        let z = zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec())).unwrap();
        z.file_names().map(str::to_string).collect()
    }

    #[test]
    fn workbook_round_trip_with_chart_objects_background_checkbox() {
        use sp_model::chart::{Chart, Series};
        let mut wb = Workbook::new();
        let img = sp_render::objects::image_from_file(Arc::from(png())).unwrap();
        let cb = wb
            .styles
            .derive(sp_model::StyleId::DEFAULT, |s| s.checkbox = true);
        let s = &mut wb.sheets[0];
        s.set(
            CellAddr::new(0, 0),
            sp_model::Cell::constant(sp_core::Value::Number(1.0)),
        );
        let mut c = sp_model::Cell::constant(sp_core::Value::Bool(true));
        c.style = cb;
        s.set(CellAddr::new(2, 0), c);
        s.drawings.charts.push(Chart {
            series: vec![Series {
                values: "Sheet1!$A$1:$A$1".into(),
                ..Series::default()
            }],
            anchor: Anchor {
                from: CellAddr::new(5, 5),
                to: CellAddr::new(15, 10),
                ..Anchor::default()
            },
            ..Chart::default()
        });
        let mut shape = rect_obj();
        shape.after_charts = 1;
        let pic = DrawObject {
            name: "Picture 2".into(),
            kind: ObjectKind::Picture(Picture {
                image: img.clone(),
                ..Picture::default()
            }),
            ..DrawObject::default()
        };
        // The same image twice is stored once.
        let mut top = pic.clone();
        top.after_charts = 1;
        s.objects = vec![pic, shape.clone(), top];
        s.background = Some(img);
        let (back, bytes) = save_load(&wb);
        let b = &back.sheets[0];
        assert_eq!(b.drawings.charts.len(), 1);
        assert_eq!(b.objects.len(), 3);
        assert_eq!(b.objects[0].after_charts, 0);
        assert_eq!(b.objects[1].kind, shape.kind);
        assert_eq!(b.objects[1].after_charts, 1);
        assert!(b.background.is_some());
        assert!(b.passthrough.is_empty() || b.passthrough.elements.is_empty());
        assert!(
            back.styles
                .get(b.get(CellAddr::new(2, 0)).unwrap().style)
                .checkbox
        );
        let names = part_names(&bytes);
        assert_eq!(
            names.iter().filter(|n| n.starts_with("xl/media/")).count(),
            1,
            "{names:?}"
        );
        assert!(names.iter().any(|n| n == FPB_PART));
        // A second cycle is stable.
        let (again, _) = save_load(&back);
        assert_eq!(again.sheets[0].objects, back.sheets[0].objects);
        assert_eq!(again.sheets[0].background, back.sheets[0].background);
    }

    #[test]
    fn objects_added_to_a_passed_through_drawing_are_appended() {
        let mut wb = Workbook::new();
        // Build a drawing we don't model (a group), via a file.
        wb.sheets[0].objects.push(rect_obj());
        let (back, bytes) = save_load(&wb);
        assert_eq!(back.sheets[0].objects.len(), 1);
        // Turn the stored shape into a group so the drawing passes through.
        let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut out = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for i in 0..z.len() {
            let mut f = z.by_index(i).unwrap();
            let name = f.name().to_string();
            let mut data = Vec::new();
            std::io::Read::read_to_end(&mut f, &mut data).unwrap();
            if name.starts_with("xl/drawings/drawing") && name.ends_with(".xml") {
                let t = String::from_utf8(data).unwrap();
                let t = t.replacen("<xdr:sp ", "<xdr:grpSp><xdr:nvGrpSpPr><xdr:cNvPr id=\"9\" name=\"G\"/><xdr:cNvGrpSpPr/></xdr:nvGrpSpPr><xdr:grpSpPr/><xdr:sp ", 1);
                let t = t.replacen("</xdr:sp>", "</xdr:sp></xdr:grpSp>", 1);
                data = t.into_bytes();
            }
            out.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            std::io::Write::write_all(&mut out, &data).unwrap();
        }
        let grouped = out.finish().unwrap().into_inner();
        let (mut wb2, _) = crate::xlsx_read::read_xlsx(std::io::Cursor::new(grouped)).unwrap();
        assert!(
            wb2.sheets[0].objects.is_empty(),
            "group stays passed through"
        );
        let img = sp_render::objects::image_from_file(Arc::from(png())).unwrap();
        wb2.sheets[0].objects.push(DrawObject {
            kind: ObjectKind::Picture(Picture {
                image: img,
                ..Picture::default()
            }),
            ..DrawObject::default()
        });
        let (back, bytes) = save_load(&wb2);
        let names = part_names(&bytes);
        assert!(
            names.iter().any(|n| n.starts_with("xl/media/")),
            "{names:?}"
        );
        assert!(
            back.sheets[0].objects.is_empty(),
            "still one unmodelled drawing"
        );
        let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut d = String::new();
        let dn = names
            .iter()
            .find(|n| n.starts_with("xl/drawings/drawing") && n.ends_with(".xml"))
            .unwrap();
        std::io::Read::read_to_string(&mut z.by_name(dn).unwrap(), &mut d).unwrap();
        assert!(d.contains("grpSp") && d.contains("xdr:pic"), "{d}");
    }
}
