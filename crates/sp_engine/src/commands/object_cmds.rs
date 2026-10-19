//! Pictures, shapes, text boxes, equations, cell checkboxes and the sheet background
//! (`insert.picture`, `insert.shapes`, `insert.text_box`, `insert.equation`,
//! `insert.checkbox`, `layout.background`), plus object editing commands outside the
//! catalog: `object.format`, `object.delete`, `object.bring_forward`,
//! `object.send_backward`, `object.bring_to_front`, `object.send_to_back`, `object.list`
//! and `cell.toggle_checkbox`. Objects are addressed by their index on the active sheet
//! (`{"object": N}`; higher = drawn on top). Everything is undoable.

use crate::params::{bool_opt, f64_opt, parse_ref, range, str_opt, u32_opt};
use crate::{CmdError, CmdResult, CommandSpec, Json, Session};
use serde_json::json;
use sp_core::{CellAddr, MAX_CELLS_PER_OP, Value};
use sp_model::objects::{
    AnchorKind, ColorMod, DrawObject, Fill, ImageData, LineFormat, MAX_IMAGE_BYTES, MAX_OBJECTS,
    MAX_TEXT_CHARS, ObjColor, ObjectKind, Picture, Placement, RunFont, Shape, StyleRefs, TextBody,
};
use sp_model::{Cell, Color, Sheet};
use sp_render::objects as geo;

/// Largest object side accepted (px at 100%).
const MAX_SIDE: f64 = 100_000.0;

// ---------------------------------------------------------------------------------------
// Params

fn sheet(s: &Session) -> Result<&Sheet, CmdError> {
    s.wb.sheet(s.sheet_index())
        .ok_or_else(|| CmdError::Failed("no sheet".into()))
}

fn object_index(s: &Session, p: &Json) -> Result<usize, CmdError> {
    let i = u32_opt(p, "object")?
        .ok_or_else(|| CmdError::Param("missing 'object' (index on the active sheet)".into()))?
        as usize;
    if i >= sheet(s)?.objects.len() {
        return Err(CmdError::Param(format!("no object {i} on this sheet")));
    }
    Ok(i)
}

fn read_image(path: &str) -> Result<ImageData, CmdError> {
    let meta = std::fs::metadata(path).map_err(|e| CmdError::Failed(format!("{path}: {e}")))?;
    if meta.len() > MAX_IMAGE_BYTES as u64 {
        return Err(CmdError::Failed("image file is too large".into()));
    }
    let bytes = std::fs::read(path).map_err(|e| CmdError::Failed(format!("{path}: {e}")))?;
    geo::image_data(bytes).map_err(CmdError::Failed)
}

/// `"RRGGBB"`, a theme slot name (`accent1`, `tx1`…), or `"none"`.
fn fill_param(p: &Json, k: &str) -> Result<Option<Fill>, CmdError> {
    let Some(t) = str_opt(p, k) else {
        if p.get(k).is_some_and(|v| !v.is_null()) {
            return Err(CmdError::Param(format!("'{k}' must be text")));
        }
        return Ok(None);
    };
    let t = t.trim();
    if t.eq_ignore_ascii_case("none") {
        return Ok(Some(Fill::None));
    }
    if sp_model::objects::default_scheme_color(t).is_some() {
        return Ok(Some(Fill::Solid(ObjColor::scheme(t))));
    }
    Color::from_hex(t)
        .map(|c| Some(Fill::Solid(ObjColor::rgb(c))))
        .ok_or_else(|| CmdError::Param(format!("'{k}' must be RRGGBB, a theme colour or none")))
}

fn color_param(p: &Json, k: &str) -> Result<Option<ObjColor>, CmdError> {
    Ok(match fill_param(p, k)? {
        Some(Fill::Solid(c)) => Some(c),
        Some(_) => return Err(CmdError::Param(format!("'{k}' must be a colour"))),
        None => None,
    })
}

fn text_param(p: &Json, k: &str) -> Result<Option<String>, CmdError> {
    match p.get(k) {
        None | Some(Json::Null) => Ok(None),
        Some(Json::String(t)) => {
            if t.chars().count() > MAX_TEXT_CHARS {
                return Err(CmdError::Param(format!("'{k}' is too long")));
            }
            Ok(Some(t.replace("\r\n", "\n")))
        }
        Some(_) => Err(CmdError::Param(format!("'{k}' must be text"))),
    }
}

fn short_text(p: &Json, k: &str) -> Result<Option<String>, CmdError> {
    let t = text_param(p, k)?;
    if t.as_ref().is_some_and(|t| t.chars().count() > 4096) {
        return Err(CmdError::Param(format!("'{k}' is too long")));
    }
    Ok(t)
}

fn placement_param(p: &Json) -> Result<Option<Placement>, CmdError> {
    match str_opt(p, "placement") {
        None => Ok(None),
        Some(t) => Placement::from_name(t).map(Some).ok_or_else(|| {
            CmdError::Param("'placement' must be move_and_size, move or fixed".into())
        }),
    }
}

fn kind_param(p: &Json) -> Result<Option<AnchorKind>, CmdError> {
    Ok(match str_opt(p, "anchor_type") {
        None => None,
        Some("two_cell") => Some(AnchorKind::TwoCell),
        Some("one_cell") => Some(AnchorKind::OneCell),
        Some("absolute") => Some(AnchorKind::Absolute),
        Some(_) => {
            return Err(CmdError::Param(
                "'anchor_type' must be two_cell, one_cell or absolute".into(),
            ));
        }
    })
}

fn size_param(p: &Json, k: &str) -> Result<Option<f64>, CmdError> {
    match f64_opt(p, k)? {
        Some(v) if !(1.0..=MAX_SIDE).contains(&v) => {
            Err(CmdError::Param(format!("'{k}' must be 1..{MAX_SIDE} px")))
        }
        v => Ok(v),
    }
}

/// The rectangle (px from A1 at 100%) a new or edited object should cover: `"range"`
/// (cell area), `"rect"` ([x, y, w, h]), or `"cell"` (top-left, default the active cell)
/// with `width`/`height` (default `size`).
fn rect_param(
    s: &Session,
    p: &Json,
    size: (f64, f64),
) -> Result<Option<(f64, f64, f64, f64)>, CmdError> {
    let sh = sheet(s)?;
    if let Some(t) = str_opt(p, "range") {
        let (_, r) = parse_ref(s, t)?;
        let x = f64::from(geo_col(sh, r.start.col));
        let y = f64::from(geo_row(sh, r.start.row));
        let x1 = f64::from(geo_col(sh, r.end.col.saturating_add(1)));
        let y1 = f64::from(geo_row(sh, r.end.row.saturating_add(1)));
        return Ok(Some((x, y, (x1 - x).max(1.0), (y1 - y).max(1.0))));
    }
    if let Some(v) = p.get("rect") {
        let a: Vec<f64> = v
            .as_array()
            .map(|a| a.iter().filter_map(Json::as_f64).collect())
            .unwrap_or_default();
        let [x, y, w, h] = a.as_slice() else {
            return Err(CmdError::Param(
                "'rect' must be [x, y, width, height]".into(),
            ));
        };
        let ok = |v: f64, lo: f64| v.is_finite() && (lo..=1e8).contains(&v);
        if !(ok(*x, 0.0) && ok(*y, 0.0) && ok(*w, 1.0) && ok(*h, 1.0))
            || *w > MAX_SIDE
            || *h > MAX_SIDE
        {
            return Err(CmdError::Param("'rect' out of range".into()));
        }
        return Ok(Some((*x, *y, *w, *h)));
    }
    let w = size_param(p, "width")?;
    let h = size_param(p, "height")?;
    let cell = str_opt(p, "cell");
    if cell.is_none() && w.is_none() && h.is_none() {
        return Ok(None);
    }
    let at = match cell {
        Some(t) => parse_ref(s, t)?.1.start,
        None => s.sel.active,
    };
    Ok(Some((
        f64::from(geo_col(sh, at.col)),
        f64::from(geo_row(sh, at.row)),
        w.unwrap_or(size.0),
        h.unwrap_or(size.1),
    )))
}

fn geo_col(sh: &Sheet, col: u32) -> f32 {
    sp_render::chart::col_offset_px(sh, 0, col)
}

fn geo_row(sh: &Sheet, row: u32) -> f32 {
    sp_render::chart::row_offset_px(sh, 0, row)
}

fn default_rect(s: &Session, size: (f64, f64)) -> Result<(f64, f64, f64, f64), CmdError> {
    let sh = sheet(s)?;
    let a = s.sel.active;
    Ok((
        f64::from(geo_col(sh, a.col)),
        f64::from(geo_row(sh, a.row)),
        size.0,
        size.1,
    ))
}

fn object_json(sh: &Sheet, i: usize, o: &DrawObject) -> Json {
    let (x, y, w, h) = geo::object_rect_abs(sh, &o.anchor);
    let mut j = json!({
        "object": i,
        "type": o.type_name(),
        "name": o.name,
        "alt": o.descr,
        "rect": [x, y, w, h],
        "rotation": o.rotation,
        "placement": o.anchor.placement.name(),
    });
    if let Some(m) = j.as_object_mut() {
        if let Some(s) = o.shape() {
            m.insert("shape".into(), json!(s.preset));
            if let Some(t) = &s.text {
                m.insert("text".into(), json!(t.plain()));
            }
        }
        if let Some(pic) = o.picture() {
            m.insert("pixels".into(), json!([pic.image.width, pic.image.height]));
        }
    }
    j
}

/// Next free "<base> N" name on the sheet.
fn next_name(sh: &Sheet, base: &str) -> String {
    let mut n = sh.objects.len() + 1;
    loop {
        let cand = format!("{base} {n}");
        if !sh.objects.iter().any(|o| o.name == cand) || n > MAX_OBJECTS * 2 {
            return cand;
        }
        n += 1;
    }
}

fn add_object(s: &mut Session, label: &str, mut o: DrawObject) -> CmdResult {
    let si = s.sheet_index();
    let sh = sheet(s)?;
    if sh.objects.len() >= MAX_OBJECTS {
        return Err(CmdError::Failed("too many objects on this sheet".into()));
    }
    o.after_charts = sh.drawings.charts.len();
    let idx = sh.objects.len();
    let out = object_json(sh, idx, &o);
    s.transact(label, |tx, _| {
        tx.touch_sheet(si);
        if let Some(sh) = tx.wb.sheet_mut(si) {
            sh.objects.push(o);
        }
        Ok(())
    })?;
    Ok(out)
}

fn font_param(p: &Json, base: RunFont) -> Result<RunFont, CmdError> {
    let mut f = base;
    if let Some(sz) = f64_opt(p, "size")? {
        if !(1.0..=4000.0).contains(&sz) {
            return Err(CmdError::Param("'size' must be 1..4000 pt".into()));
        }
        f.size = Some((sz * 100.0).round() as u32);
    }
    if let Some(b) = bool_opt(p, "bold") {
        f.bold = b;
    }
    if let Some(i) = bool_opt(p, "italic") {
        f.italic = i;
    }
    if let Some(u) = bool_opt(p, "underline") {
        f.underline = u;
    }
    if let Some(c) = color_param(p, "color")? {
        f.color = Some(c);
    }
    if let Some(name) = short_text(p, "font")? {
        f.latin = Some(name).filter(|n| !n.is_empty());
    }
    Ok(f)
}

fn align_param(p: &Json) -> Result<Option<&'static str>, CmdError> {
    Ok(match str_opt(p, "align") {
        None => None,
        Some("left") => Some("l"),
        Some("center") => Some("ctr"),
        Some("right") => Some("r"),
        Some("justify") => Some("just"),
        Some(_) => {
            return Err(CmdError::Param(
                "'align' must be left, center, right or justify".into(),
            ));
        }
    })
}

fn valign_param(p: &Json) -> Result<Option<&'static str>, CmdError> {
    Ok(match str_opt(p, "valign") {
        None => None,
        Some("top") => Some("t"),
        Some("middle") => Some("ctr"),
        Some("bottom") => Some("b"),
        Some(_) => {
            return Err(CmdError::Param(
                "'valign' must be top, middle or bottom".into(),
            ));
        }
    })
}

/// Apply the formatting params shared by insert and `object.format`.
fn apply_format(s: &Session, o: &mut DrawObject, p: &Json) -> Result<(), CmdError> {
    if let Some(n) = short_text(p, "name")? {
        o.name = n;
    }
    if let Some(a) = short_text(p, "alt")? {
        o.descr = a;
    }
    if let Some(r) = f64_opt(p, "rotation")? {
        o.rotation = r.rem_euclid(360.0);
    }
    if let Some(f) = bool_opt(p, "flip_h") {
        o.flip_h = f;
    }
    if let Some(f) = bool_opt(p, "flip_v") {
        o.flip_v = f;
    }
    if let Some(h) = bool_opt(p, "hidden") {
        o.hidden = h;
    }
    let placement = placement_param(p)?;
    let kind = kind_param(p)?;
    let sh = sheet(s)?;
    let cur = geo::object_rect_abs(sh, &o.anchor);
    // A bare "cell" moves the object; it keeps its current size.
    let rect = rect_param(s, p, (cur.2, cur.3))?;
    if rect.is_some() || placement.is_some() || kind.is_some() {
        let r = rect.unwrap_or(cur);
        o.anchor = geo::anchor_from_rect(
            sh,
            kind.unwrap_or(o.anchor.kind),
            placement.unwrap_or(o.anchor.placement),
            r,
        );
    }
    let line_fill = fill_param(p, "line")?;
    let line_w = f64_opt(p, "line_width")?;
    if line_w.is_some_and(|w| !(0.0..=1584.0).contains(&w)) {
        return Err(CmdError::Param("'line_width' must be 0..1584 pt".into()));
    }
    let dash = str_opt(p, "dash").map(str::to_string);
    let apply_line = |l: &mut LineFormat| {
        if let Some(f) = line_fill.clone() {
            l.fill = f;
        }
        if let Some(w) = line_w {
            l.width = Some((w * 12_700.0).round() as i64);
        }
        if let Some(d) = &dash {
            l.dash = Some(d.clone()).filter(|d| d != "solid");
        }
    };
    match &mut o.kind {
        ObjectKind::Picture(pic) => {
            apply_line(&mut pic.line);
            if let Some(path) = str_opt(p, "path") {
                pic.image = read_image(path)?;
            }
        }
        ObjectKind::Shape(sh) => {
            if let Some(f) = fill_param(p, "fill")? {
                sh.fill = f;
            }
            apply_line(&mut sh.line);
            if let Some(h) = str_opt(p, "arrow_end") {
                sh.line.tail = Some(h.to_string()).filter(|h| h != "none");
            }
            if let Some(h) = str_opt(p, "arrow_start") {
                sh.line.head = Some(h.to_string()).filter(|h| h != "none");
            }
            if let Some(pr) = str_opt(p, "shape") {
                if !geo::known_preset(pr) {
                    return Err(CmdError::Param(format!("unknown shape '{pr}'")));
                }
                sh.preset = pr.to_string();
                sh.adjust.clear();
            }
            let text = text_param(p, "text")?;
            let font_keys = ["size", "bold", "italic", "underline", "color", "font"];
            let restyle = font_keys.iter().any(|k| p.get(*k).is_some());
            let align = align_param(p)?;
            if text.is_some() || restyle || align.is_some() {
                let old = sh.text.take();
                let base_font = old
                    .as_ref()
                    .and_then(|t| t.paras.first())
                    .and_then(|pp| pp.runs.first().map(|r| r.font.clone()).or(pp.end.clone()))
                    .unwrap_or(RunFont {
                        size: Some(1100),
                        ..RunFont::default()
                    });
                let font = font_param(p, base_font)?;
                let old_align = old
                    .as_ref()
                    .and_then(|t| t.paras.first())
                    .and_then(|pp| pp.align.clone());
                let plain =
                    text.unwrap_or_else(|| old.as_ref().map(TextBody::plain).unwrap_or_default());
                let mut body = TextBody::from_plain(&plain, &font, align.or(old_align.as_deref()));
                if let Some(o) = old {
                    body.body_attrs = o.body_attrs;
                    body.autofit = o.autofit;
                }
                sh.text = Some(body);
            }
            if let Some(v) = valign_param(p)? {
                sh.text
                    .get_or_insert_with(TextBody::default)
                    .set_body_attr("anchor", v);
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Insert

fn insert_picture(s: &mut Session, p: &Json) -> CmdResult {
    let path = str_opt(p, "path")
        .ok_or_else(|| CmdError::Param("missing string 'path' (PNG, JPEG, GIF or BMP)".into()))?;
    let image = read_image(path)?;
    let size = geo::fit_size(&image, 600.0);
    let rect = match rect_param(s, p, size)? {
        Some(r) => r,
        None => default_rect(s, size)?,
    };
    let sh = sheet(s)?;
    let name = next_name(sh, "Picture");
    let mut o = DrawObject {
        name,
        anchor: geo::anchor_from_rect(sh, AnchorKind::TwoCell, Placement::Move, rect),
        kind: ObjectKind::Picture(Picture {
            image,
            line: LineFormat::default(),
            locks: vec![("noChangeAspect".into(), "1".into())],
        }),
        ..DrawObject::default()
    };
    let without_path = {
        let mut q = p.clone();
        if let Some(m) = q.as_object_mut() {
            m.remove("path");
        }
        q
    };
    apply_format(s, &mut o, &without_path)?;
    add_object(s, "Insert Picture", o)
}

fn new_shape(s: &Session, p: &Json, preset: &str) -> Result<DrawObject, CmdError> {
    if !geo::known_preset(preset) {
        let names: Vec<&str> = geo::PRESETS.iter().map(|(n, _)| *n).collect();
        return Err(CmdError::Param(format!(
            "unknown shape '{preset}' (one of {})",
            names.join(", ")
        )));
    }
    let line = geo::is_line_preset(preset);
    let size = if line { (150.0, 0.0) } else { (144.0, 96.0) };
    let rect = match rect_param(s, p, size)? {
        Some(r) => r,
        None => default_rect(s, size)?,
    };
    let sh = sheet(s)?;
    let shape = Shape {
        preset: preset.to_string(),
        connector: line,
        style: Some(if line {
            StyleRefs::line_default()
        } else {
            StyleRefs::shape_default()
        }),
        line: LineFormat {
            tail: (preset == "straightConnector1").then(|| "triangle".to_string()),
            ..LineFormat::default()
        },
        ..Shape::default()
    };
    let label = if line {
        "Straight Connector"
    } else {
        geo::preset_label(preset)
    };
    Ok(DrawObject {
        name: next_name(sh, label),
        anchor: geo::anchor_from_rect(sh, AnchorKind::TwoCell, Placement::MoveAndSize, rect),
        kind: ObjectKind::Shape(shape),
        ..DrawObject::default()
    })
}

fn insert_shape(s: &mut Session, p: &Json) -> CmdResult {
    let preset = str_opt(p, "shape").unwrap_or("rect").to_string();
    let mut o = new_shape(s, p, &preset)?;
    // Text in a shape is centred both ways unless told otherwise.
    if let Some(sh) = o.shape_mut()
        && !sh.connector
        && p.get("text").is_some()
    {
        let mut t = TextBody::from_plain("", &RunFont::default(), Some("ctr"));
        t.body_attrs = vec![
            ("rtlCol".into(), "0".into()),
            ("anchor".into(), "ctr".into()),
        ];
        sh.text = Some(t);
    }
    let mut q = p.clone();
    if let Some(m) = q.as_object_mut() {
        m.remove("shape");
    }
    if o.shape().is_some_and(|sh| sh.connector) && p.get("text").is_some() {
        return Err(CmdError::Param("lines can't hold text".into()));
    }
    apply_format(s, &mut o, &q)?;
    add_object(s, "Insert Shape", o)
}

/// A text box as the target app inserts one: white fill, grey outline, wrapped text,
/// no autofit.
fn text_box(s: &Session, p: &Json, base: &str, font: RunFont) -> Result<DrawObject, CmdError> {
    let size = (192.0, 72.0);
    let rect = match rect_param(s, p, size)? {
        Some(r) => r,
        None => default_rect(s, size)?,
    };
    let sh = sheet(s)?;
    let mut body = TextBody::from_plain("", &font, None);
    body.body_attrs = vec![
        ("vertOverflow".into(), "clip".into()),
        ("horzOverflow".into(), "clip".into()),
        ("wrap".into(), "square".into()),
        ("rtlCol".into(), "0".into()),
        ("anchor".into(), "t".into()),
    ];
    let shape = Shape {
        preset: "rect".into(),
        text_box: true,
        fill: Fill::Solid(ObjColor::scheme("lt1")),
        line: LineFormat {
            fill: Fill::Solid(ObjColor::Scheme(
                "lt1".into(),
                vec![ColorMod {
                    name: "shade".into(),
                    val: 50_000,
                }],
            )),
            width: Some(9525),
            attrs: vec![("cmpd".into(), "sng".into())],
            ..LineFormat::default()
        },
        style: Some(StyleRefs {
            line: (0, ObjColor::scheme("accent1")),
            fill: (0, ObjColor::scheme("accent1")),
            effect: (0, ObjColor::scheme("accent1")),
            font: ("minor".into(), ObjColor::scheme("dk1")),
        }),
        text: Some(body),
        ..Shape::default()
    };
    Ok(DrawObject {
        name: next_name(sh, base),
        anchor: geo::anchor_from_rect(sh, AnchorKind::TwoCell, Placement::MoveAndSize, rect),
        kind: ObjectKind::Shape(shape),
        ..DrawObject::default()
    })
}

fn insert_text_box(s: &mut Session, p: &Json) -> CmdResult {
    let font = RunFont {
        size: Some(1100),
        ..RunFont::default()
    };
    let mut o = text_box(s, p, "TextBox", font)?;
    let mut q = p.clone();
    if let Some(m) = q.as_object_mut() {
        m.entry("text").or_insert(json!(""));
    }
    apply_format(s, &mut o, &q)?;
    add_object(s, "Insert Text Box", o)
}

/// Linear-format math shown as plain text in a centred text box (no OMML is generated).
fn insert_equation(s: &mut Session, p: &Json) -> CmdResult {
    let font = RunFont {
        size: Some(1100),
        italic: true,
        latin: Some("Cambria Math".into()),
        ..RunFont::default()
    };
    let mut o = text_box(s, p, "Equation", font)?;
    let mut q = p.clone();
    if let Some(m) = q.as_object_mut() {
        m.entry("text").or_insert(json!("Type equation here."));
        m.entry("align").or_insert(json!("center"));
    }
    if let Some(sh) = o.shape_mut() {
        sh.fill = Fill::None;
        sh.line.fill = Fill::None;
    }
    apply_format(s, &mut o, &q)?;
    add_object(s, "Insert Equation", o)
}

// ---------------------------------------------------------------------------------------
// Edit / arrange

fn format_object(s: &mut Session, p: &Json) -> CmdResult {
    let i = object_index(s, p)?;
    let si = s.sheet_index();
    let mut o = sheet(s)?
        .objects
        .get(i)
        .cloned()
        .ok_or_else(|| CmdError::Param(format!("no object {i}")))?;
    apply_format(s, &mut o, p)?;
    let out = object_json(sheet(s)?, i, &o);
    s.transact("Format Object", |tx, _| {
        tx.touch_sheet(si);
        if let Some(slot) = tx.wb.sheet_mut(si).and_then(|sh| sh.objects.get_mut(i)) {
            *slot = o;
        }
        Ok(())
    })?;
    Ok(out)
}

fn delete_object(s: &mut Session, p: &Json) -> CmdResult {
    let i = object_index(s, p)?;
    let si = s.sheet_index();
    s.transact("Delete Object", |tx, _| {
        tx.touch_sheet(si);
        if let Some(sh) = tx.wb.sheet_mut(si)
            && i < sh.objects.len()
        {
            sh.objects.remove(i);
        }
        Ok(())
    })?;
    Ok(json!({"deleted": i}))
}

#[derive(Clone, Copy)]
enum Arrange {
    Forward,
    Backward,
    Front,
    Back,
}

fn arrange(s: &mut Session, p: &Json, how: Arrange) -> CmdResult {
    let i = object_index(s, p)?;
    let si = s.sheet_index();
    let n = sheet(s)?.objects.len();
    let to = match how {
        Arrange::Forward => (i + 1).min(n - 1),
        Arrange::Backward => i.saturating_sub(1),
        Arrange::Front => n - 1,
        Arrange::Back => 0,
    };
    if to != i {
        s.transact("Arrange Object", |tx, _| {
            tx.touch_sheet(si);
            if let Some(sh) = tx.wb.sheet_mut(si)
                && i < sh.objects.len()
            {
                let o = sh.objects.remove(i);
                let charts = sh.drawings.charts.len();
                let to = to.min(sh.objects.len());
                sh.objects.insert(to, o);
                // Keep the z-order relative to charts monotonic (file order follows it).
                let mut floor = 0;
                for (k, o) in sh.objects.iter_mut().enumerate() {
                    if k == to {
                        o.after_charts = match how {
                            Arrange::Front => charts,
                            Arrange::Back => 0,
                            _ => o.after_charts,
                        };
                    }
                    o.after_charts = o.after_charts.max(floor).min(charts);
                    floor = o.after_charts;
                }
            }
            Ok(())
        })?;
    }
    Ok(json!({"object": to}))
}

fn list_objects(s: &mut Session, _: &Json) -> CmdResult {
    let sh = sheet(s)?;
    Ok(json!({
        "objects": sh.objects.iter().enumerate().map(|(i, o)| object_json(sh, i, o)).collect::<Vec<_>>(),
        "background": sh.background.as_ref().map(|b| json!([b.width, b.height])),
    }))
}

// ---------------------------------------------------------------------------------------
// Checkboxes and background

fn insert_checkbox(s: &mut Session, p: &Json) -> CmdResult {
    let (si, r) = range(s, p, "range")?;
    if r.cell_count() > MAX_CELLS_PER_OP {
        return Err(CmdError::Failed("selection is too large".into()));
    }
    let remove = bool_opt(p, "remove") == Some(true);
    let label = if remove {
        "Remove Checkbox"
    } else {
        "Insert Checkbox"
    };
    let n = r.cell_count();
    s.transact(label, |tx, _| {
        for a in r.iter() {
            tx.touch_cell(si, a);
            let style = tx.wb.sheet(si).and_then(|sh| sh.get(a)).map(|c| c.style);
            let id = tx
                .wb
                .styles
                .derive(style.unwrap_or_default(), |st| st.checkbox = !remove);
            let Some(sh) = tx.wb.sheet_mut(si) else {
                continue;
            };
            let c = sh.entry(a);
            c.style = id;
            if !remove && c.value.is_empty() && c.formula_src().is_none() {
                *c = Cell {
                    style: id,
                    ..Cell::constant(Value::Bool(false))
                };
            }
            if sh.get(a).is_some_and(Cell::is_blank) {
                sh.remove(a);
            }
        }
        Ok(())
    })?;
    Ok(json!({"cells": n, "checkbox": !remove}))
}

/// Toggle checkbox cells in the range (default: selection): all become checked unless all
/// already are. Cells holding formulas are left alone.
fn toggle_checkbox(s: &mut Session, p: &Json) -> CmdResult {
    let (si, r) = range(s, p, "range")?;
    if r.cell_count() > MAX_CELLS_PER_OP {
        return Err(CmdError::Failed("selection is too large".into()));
    }
    let sh =
        s.wb.sheet(si)
            .ok_or_else(|| CmdError::Failed("no sheet".into()))?;
    let boxes: Vec<(CellAddr, bool)> = sh
        .cells_in(r)
        .filter(|(_, c)| c.formula_src().is_none())
        .filter_map(|(a, _)| geo::checkbox_state(&s.wb, sh, a).map(|b| (a, b)))
        .collect();
    if boxes.is_empty() {
        return Err(CmdError::Failed("no checkbox in the selection".into()));
    }
    let target = !boxes.iter().all(|(_, b)| *b);
    s.transact("Toggle Checkbox", |tx, _| {
        for (a, _) in &boxes {
            tx.touch_cell(si, *a);
            if let Some(c) = tx.wb.sheet_mut(si).and_then(|sh| sh.get_mut(*a)) {
                let style = c.style;
                *c = Cell {
                    style,
                    ..Cell::constant(Value::Bool(target))
                };
            }
        }
        Ok(())
    })?;
    Ok(json!({"cells": boxes.len(), "checked": target}))
}

fn background(s: &mut Session, p: &Json) -> CmdResult {
    let si = s.sheet_index();
    let img = if bool_opt(p, "delete") == Some(true) {
        None
    } else {
        let path = str_opt(p, "path").ok_or_else(|| {
            CmdError::Param("missing string 'path' (or {\"delete\": true})".into())
        })?;
        Some(read_image(path)?)
    };
    let out = json!({"background": img.as_ref().map(|i| json!([i.width, i.height]))});
    let label = if img.is_some() {
        "Background"
    } else {
        "Delete Background"
    };
    s.transact(label, |tx, _| {
        tx.touch_sheet(si);
        if let Some(sh) = tx.wb.sheet_mut(si) {
            sh.background = img;
        }
        Ok(())
    })?;
    Ok(out)
}

fn has_objects(s: &Session) -> bool {
    s.wb.sheet(s.sheet_index())
        .is_some_and(|sh| !sh.objects.is_empty())
}

pub fn specs() -> Vec<CommandSpec> {
    const SHAPE: &str = r#"{"shape"?: "rect|roundRect|ellipse|triangle|rtTriangle|diamond|pentagon|hexagon|octagon|parallelogram|trapezoid|plus|homePlate|chevron|rightArrow|leftArrow|upArrow|downArrow|leftRightArrow|wedgeRectCallout|star4|star5|line|straightConnector1" (default rect), "fill"?: "RRGGBB|accent1…|none", "line"?: "RRGGBB|accent1…|none", "line_width"?: pt, "dash"?: "solid|dash|sysDot…", "arrow_start"?, "arrow_end"?: "triangle|arrow|stealth|oval|diamond|none", "text"?, "size"?, "bold"?, "italic"?, "underline"?, "color"?, "font"?, "align"?: "left|center|right", "valign"?: "top|middle|bottom", ...placement params: "cell"?, "width"?, "height"?, "range"?, "rect"?, "placement"?, "anchor_type"?, "name"?, "alt"?, "rotation"?, "flip_h"?, "flip_v"?}"#;
    const TEXT: &str = r#"{"text"?: "…", "font"?: "Calibri", "size"?: pt, "bold"?, "italic"?, "underline"?, "color"?: "RRGGBB", "align"?: "left|center|right|justify", "valign"?: "top|middle|bottom", "fill"?, "line"?, ...placement params: "cell"?, "width"?, "height"?, "range"?, "rect"?, "placement"?, "anchor_type"?, "name"?, "alt"?, "rotation"?}"#;
    const PIC: &str = r#"{"path": "/path/image.png" (PNG, JPEG, GIF, BMP), "alt"?: alt text, "line"?: "RRGGBB|none", "line_width"?: pt, "cell"?: "B2", "width"?, "height"?: px (default: image size, at most 600), "range"?, "rect"?, "placement"?: "move_and_size|move|fixed" (default move), "anchor_type"?: "two_cell|one_cell|absolute", "name"?, "rotation"?}"#;
    const OBJ: &str = r#"{"object": N}"#;
    vec![
        CommandSpec::new("insert.picture", PIC, insert_picture),
        CommandSpec::new("insert.shapes", SHAPE, insert_shape),
        CommandSpec::new("insert.text_box", TEXT, insert_text_box),
        CommandSpec::new("insert.equation", TEXT, insert_equation),
        CommandSpec::new(
            "insert.checkbox",
            r#"{"range"?: "A1:A9" (default selection; empty cells become FALSE), "remove"?: bool}"#,
            insert_checkbox,
        ),
        CommandSpec::new(
            "layout.background",
            r#"{"path": "/path/image.png" (tiled behind the cells, not printed) | "delete": true}"#,
            background,
        ),
        CommandSpec::new(
            "object.format",
            r#"{"object": N, "path"? (picture: replace image), "shape"?, "fill"?, "line"?, "line_width"?, "dash"?, "arrow_start"?, "arrow_end"?, "text"?, "size"?, "bold"?, "italic"?, "underline"?, "color"?, "font"?, "align"?, "valign"?, "cell"?, "width"?, "height"?, "range"?, "rect"?: [x, y, w, h], "placement"?, "anchor_type"?, "name"?, "alt"?, "rotation"?, "flip_h"?, "flip_v"?, "hidden"?}"#,
            format_object,
        )
        .when(has_objects),
        CommandSpec::new("object.delete", OBJ, delete_object).when(has_objects),
        CommandSpec::new("object.bring_forward", OBJ, |s, p| {
            arrange(s, p, Arrange::Forward)
        })
        .when(has_objects),
        CommandSpec::new("object.send_backward", OBJ, |s, p| {
            arrange(s, p, Arrange::Backward)
        })
        .when(has_objects),
        CommandSpec::new("object.bring_to_front", OBJ, |s, p| {
            arrange(s, p, Arrange::Front)
        })
        .when(has_objects),
        CommandSpec::new("object.send_to_back", OBJ, |s, p| {
            arrange(s, p, Arrange::Back)
        })
        .when(has_objects),
        CommandSpec::new("object.list", "{}", list_objects),
        CommandSpec::new(
            "cell.toggle_checkbox",
            r#"{"range"?: "A1:A9" (default selection)}"#,
            toggle_checkbox,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_file(name: &str, w: u32, h: u32) -> String {
        let dir = std::env::temp_dir().join(format!("sp-objects-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let px: Vec<u8> = (0..w * h)
            .flat_map(|i| [(i * 7) as u8, 90, 160, 255])
            .collect();
        std::fs::write(&path, geo::encode_png(w, h, px).unwrap()).unwrap();
        path.to_string_lossy().into_owned()
    }

    fn objs(s: &Session) -> &Vec<DrawObject> {
        &s.wb.sheets[0].objects
    }

    #[test]
    fn insert_picture_sizes_and_undo() {
        let mut s = Session::new();
        let path = png_file("p.png", 40, 20);
        let out = s
            .execute(
                "insert.picture",
                &json!({"path": path, "cell": "C3", "alt": "logo"}),
            )
            .unwrap();
        assert_eq!(out["type"], "picture");
        let o = &objs(&s)[0];
        assert_eq!(o.descr, "logo");
        assert_eq!(o.anchor.placement, Placement::Move);
        assert_eq!(o.anchor.cells.from, CellAddr::new(2, 2));
        let r = &out["rect"];
        assert!((r[2].as_f64().unwrap() - 40.0).abs() < 0.01);
        assert!((r[3].as_f64().unwrap() - 20.0).abs() < 0.01);
        s.execute("edit.undo", &json!({})).unwrap();
        assert!(objs(&s).is_empty());
        // Errors, not panics.
        assert!(s.execute("insert.picture", &json!({})).is_err());
        assert!(
            s.execute("insert.picture", &json!({"path": "/no/such/file.png"}))
                .is_err()
        );
        let bad = png_file("bad.png", 2, 2).replace("bad.png", "trunc.png");
        std::fs::write(&bad, b"\x89PNG\r\n\x1a\n\0\0").unwrap();
        assert!(s.execute("insert.picture", &json!({"path": bad})).is_err());
        assert!(
            s.execute("insert.picture", &json!({"path": path, "width": -5}))
                .is_err()
        );
    }

    #[test]
    fn format_cell_moves_without_resizing() {
        // Regression: a bare "cell" used to collapse the object to 0×0.
        let mut s = Session::new();
        s.execute(
            "insert.shapes",
            &json!({"shape": "rect", "width": 80, "height": 30}),
        )
        .unwrap();
        let out = s
            .execute("object.format", &json!({"object": 0, "cell": "E10"}))
            .unwrap();
        let r = &out["rect"];
        assert!((r[2].as_f64().unwrap() - 80.0).abs() < 0.01, "{r}");
        assert!((r[3].as_f64().unwrap() - 30.0).abs() < 0.01, "{r}");
        assert_eq!(objs(&s)[0].anchor.cells.from, CellAddr::new(9, 4));
    }

    #[test]
    fn shapes_text_boxes_and_format() {
        let mut s = Session::new();
        for shape in [
            "rect",
            "ellipse",
            "rightArrow",
            "line",
            "straightConnector1",
            "star5",
            "wedgeRectCallout",
        ] {
            s.execute("insert.shapes", &json!({"shape": shape}))
                .unwrap();
        }
        assert_eq!(objs(&s).len(), 7);
        assert_eq!(objs(&s)[3].type_name(), "line");
        assert_eq!(
            objs(&s)[4].shape().unwrap().line.tail.as_deref(),
            Some("triangle")
        );
        assert!(
            s.execute("insert.shapes", &json!({"shape": "nope"}))
                .is_err()
        );
        assert!(
            s.execute("insert.shapes", &json!({"shape": "line", "text": "x"}))
                .is_err()
        );
        s.execute(
            "insert.shapes",
            &json!({"shape": "roundRect", "text": "Hello", "fill": "FF0000", "line": "none", "rotation": 30, "range": "B2:D6"}),
        )
        .unwrap();
        let o = objs(&s).last().unwrap();
        let sh = o.shape().unwrap();
        assert_eq!(sh.fill, Fill::Solid(ObjColor::rgb(Color(255, 0, 0))));
        assert_eq!(sh.line.fill, Fill::None);
        assert_eq!(sh.text.as_ref().unwrap().plain(), "Hello");
        assert_eq!(o.rotation, 30.0);
        let out = s
            .execute(
                "insert.text_box",
                &json!({"text": "Note\nline 2", "bold": true, "size": 14, "align": "center"}),
            )
            .unwrap();
        assert_eq!(out["type"], "text_box");
        let i = out["object"].as_u64().unwrap();
        let tb = objs(&s)[i as usize].shape().unwrap().text.clone().unwrap();
        assert_eq!(tb.paras.len(), 2);
        assert!(tb.autofit.is_none(), "autofit off");
        assert_eq!(tb.paras[0].runs[0].font.size, Some(1400));
        s.execute(
            "object.format",
            &json!({"object": i, "text": "Changed", "rect": [10, 20, 100, 50], "valign": "middle"}),
        )
        .unwrap();
        let o = &objs(&s)[i as usize];
        let tb = o.shape().unwrap().text.as_ref().unwrap();
        assert_eq!(tb.plain(), "Changed");
        assert!(tb.paras[0].runs[0].font.bold, "font kept");
        assert_eq!(tb.body_attr("anchor"), Some("ctr"));
        let r = geo::object_rect_abs(&s.wb.sheets[0], &o.anchor);
        assert!((r.0 - 10.0).abs() < 0.01 && (r.2 - 100.0).abs() < 0.01);
        assert!(s.execute("object.format", &json!({"object": 999})).is_err());
        assert!(
            s.execute("object.format", &json!({"object": 0, "fill": "zz"}))
                .is_err()
        );
        let eq = s
            .execute("insert.equation", &json!({"text": "x^2+y^2=r^2"}))
            .unwrap();
        assert_eq!(eq["text"], "x^2+y^2=r^2");
    }

    #[test]
    fn arrange_and_delete() {
        let mut s = Session::new();
        for n in ["a", "b", "c"] {
            s.execute("insert.shapes", &json!({"name": n})).unwrap();
        }
        let names = |s: &Session| objs(s).iter().map(|o| o.name.clone()).collect::<Vec<_>>();
        s.execute("object.bring_to_front", &json!({"object": 0}))
            .unwrap();
        assert_eq!(names(&s), ["b", "c", "a"]);
        s.execute("object.send_backward", &json!({"object": 2}))
            .unwrap();
        assert_eq!(names(&s), ["b", "a", "c"]);
        s.execute("object.send_to_back", &json!({"object": 2}))
            .unwrap();
        assert_eq!(names(&s), ["c", "b", "a"]);
        s.execute("object.bring_forward", &json!({"object": 0}))
            .unwrap();
        assert_eq!(names(&s), ["b", "c", "a"]);
        s.execute("object.delete", &json!({"object": 1})).unwrap();
        assert_eq!(names(&s), ["b", "a"]);
        let l = s.execute("object.list", &json!({})).unwrap();
        assert_eq!(l["objects"].as_array().unwrap().len(), 2);
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(names(&s).len(), 3);
    }

    #[test]
    fn checkboxes_insert_toggle_and_save() {
        let mut s = Session::new();
        s.execute("cell.set", &json!({"cell": "A2", "input": "TRUE"}))
            .unwrap();
        s.execute("cell.set", &json!({"cell": "A3", "input": "hello"}))
            .unwrap();
        s.execute("insert.checkbox", &json!({"range": "A1:A3"}))
            .unwrap();
        let sh = &s.wb.sheets[0];
        assert_eq!(sh.value(CellAddr::new(0, 0)), &Value::Bool(false));
        assert_eq!(
            geo::checkbox_state(&s.wb, sh, CellAddr::new(1, 0)),
            Some(true)
        );
        assert_eq!(geo::checkbox_state(&s.wb, sh, CellAddr::new(2, 0)), None);
        s.execute("cell.toggle_checkbox", &json!({"range": "A1"}))
            .unwrap();
        assert_eq!(
            s.wb.sheets[0].value(CellAddr::new(0, 0)),
            &Value::Bool(true)
        );
        s.execute("cell.toggle_checkbox", &json!({"range": "A1:A2"}))
            .unwrap();
        assert_eq!(
            s.wb.sheets[0].value(CellAddr::new(1, 0)),
            &Value::Bool(false)
        );
        assert!(
            s.execute("cell.toggle_checkbox", &json!({"range": "C1"}))
                .is_err()
        );
        // The display hides TRUE/FALSE.
        let d = sp_render::layout(
            &s.wb,
            &s.wb.sheets[0],
            CellAddr::default(),
            200.0,
            100.0,
            1.0,
            false,
        );
        assert!(
            d.cells
                .iter()
                .filter(|c| c.addr.col == 0 && c.addr.row < 2)
                .all(|c| c.text.is_empty())
        );
        s.execute("insert.checkbox", &json!({"range": "A1", "remove": true}))
            .unwrap();
        assert_eq!(
            geo::checkbox_state(&s.wb, &s.wb.sheets[0], CellAddr::new(0, 0)),
            None
        );
        assert!(
            s.execute("insert.checkbox", &json!({"range": "A:XFD"}))
                .is_err()
        );
    }

    #[test]
    fn background_set_and_delete() {
        let mut s = Session::new();
        let path = png_file("bg.png", 8, 8);
        s.execute("layout.background", &json!({"path": path}))
            .unwrap();
        assert_eq!(s.wb.sheets[0].background.as_ref().map(|b| b.width), Some(8));
        s.execute("layout.background", &json!({"delete": true}))
            .unwrap();
        assert!(s.wb.sheets[0].background.is_none());
        assert!(s.execute("layout.background", &json!({})).is_err());
    }

    #[test]
    fn insert_rows_moves_objects_through_commands() {
        let mut s = Session::new();
        s.execute("insert.shapes", &json!({"range": "C5:E8"}))
            .unwrap();
        s.select(CellAddr::new(0, 0));
        s.execute("home.cells.insert_rows", &json!({})).unwrap();
        assert_eq!(objs(&s)[0].anchor.cells.from.row, 5);
    }
}
