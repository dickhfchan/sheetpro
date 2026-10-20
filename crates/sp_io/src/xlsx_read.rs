//! .xlsx (SpreadsheetML, ECMA-376) reader.

use crate::xlsx_passthrough as pass;
use crate::xml::{Node, local, parse_dom, resolve_ref, unescape_ooxml};
use crate::{IoError, LoadReport};
use quick_xml::Reader;
use quick_xml::events::Event;
use sp_core::{CellAddr, CellRange, ErrorKind, Value};
use sp_formula::shared::FormulaMemo;
use sp_formula::shift::shift_for_copy;
use sp_formula::{parse, to_formula};
use sp_model::{
    Border, BorderStyle, Cell, Color, Content, DefinedName, HAlign, Sheet, SpillArea, Style,
    StyleId, VAlign, Visibility, Workbook,
};
use std::collections::HashMap;
use std::io::{Read, Seek};
use std::sync::Arc;

/// Per-part decompressed size cap (zip-bomb guard).
const MAX_PART_BYTES: u64 = 1 << 30;

fn read_part<R: Read + Seek>(
    zip: &mut zip::ZipArchive<R>,
    name: &str,
) -> Result<Option<String>, IoError> {
    let file = match zip.by_name(name) {
        Ok(f) => f,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(e) => return Err(IoError::Zip(e.to_string())),
    };
    let mut buf = String::new();
    let mut limited = file.take(MAX_PART_BYTES + 1);
    limited
        .read_to_string(&mut buf)
        .map_err(|e| IoError::Format(format!("{name}: {e}")))?;
    if buf.len() as u64 > MAX_PART_BYTES {
        return Err(IoError::Format(format!("{name} exceeds size limit")));
    }
    Ok(Some(buf))
}

/// Built-in number formats by id (ECMA-376 Part 1 §18.8.30, en-US rendering).
pub fn builtin_numfmt(id: u32) -> Option<&'static str> {
    Some(match id {
        0 => "General",
        1 => "0",
        2 => "0.00",
        3 => "#,##0",
        4 => "#,##0.00",
        9 => "0%",
        10 => "0.00%",
        11 => "0.00E+00",
        12 => "# ?/?",
        13 => "# ??/??",
        14 => "m/d/yyyy",
        15 => "d-mmm-yy",
        16 => "d-mmm",
        17 => "mmm-yy",
        18 => "h:mm AM/PM",
        19 => "h:mm:ss AM/PM",
        20 => "h:mm",
        21 => "h:mm:ss",
        22 => "m/d/yyyy h:mm",
        37 => "#,##0 ;(#,##0)",
        38 => "#,##0 ;[Red](#,##0)",
        39 => "#,##0.00;(#,##0.00)",
        40 => "#,##0.00;[Red](#,##0.00)",
        45 => "mm:ss",
        46 => "[h]:mm:ss",
        47 => "mmss.0",
        48 => "##0.0E+0",
        49 => "@",
        _ => return None,
    })
}

fn color_of(n: Option<&Node>) -> Option<Color> {
    n.and_then(|c| c.attr("rgb")).and_then(Color::from_hex)
}

fn border_of(n: Option<&Node>) -> Border {
    let Some(n) = n else { return Border::default() };
    let style = match n.attr("style").unwrap_or("") {
        "thin" | "hair" => BorderStyle::Thin,
        "medium" | "mediumDashed" | "mediumDashDot" | "mediumDashDotDot" | "slantDashDot" => {
            BorderStyle::Medium
        }
        "thick" => BorderStyle::Thick,
        "dashed" | "dashDot" | "dashDotDot" => BorderStyle::Dashed,
        "dotted" => BorderStyle::Dotted,
        "double" => BorderStyle::Double,
        _ => BorderStyle::None,
    };
    Border {
        style,
        color: color_of(n.child("color")),
    }
}

/// Parse styles.xml into one `Style` per cellXfs entry.
fn read_styles(xml: &str, theme: &sp_model::theme::Theme) -> Result<Vec<Style>, IoError> {
    let root = parse_dom(xml)?;
    let mut fmts: HashMap<u32, String> = HashMap::new();
    if let Some(nf) = root.child("numFmts") {
        for f in nf.children_named("numFmt") {
            if let (Some(id), Some(code)) = (
                f.attr("numFmtId").and_then(|s| s.parse().ok()),
                f.attr("formatCode"),
            ) {
                fmts.insert(id, code.to_string());
            }
        }
    }
    let fonts: Vec<&Node> = root
        .child("fonts")
        .map(|f| f.children_named("font").collect())
        .unwrap_or_default();
    let fills: Vec<&Node> = root
        .child("fills")
        .map(|f| f.children_named("fill").collect())
        .unwrap_or_default();
    let borders: Vec<&Node> = root
        .child("borders")
        .map(|f| f.children_named("border").collect())
        .unwrap_or_default();
    let idx = |n: &Node, k: &str| n.attr(k).and_then(|s| s.parse::<usize>().ok()).unwrap_or(0);
    let flag = |n: Option<&Node>| {
        n.is_some_and(|x| x.attr("val").is_none_or(|v| v != "0" && v != "false"))
    };
    let mut out = Vec::new();
    if let Some(xfs) = root.child("cellXfs") {
        for xf in xfs.children_named("xf") {
            let mut s = Style::default();
            let fmt_id = idx(xf, "numFmtId") as u32;
            s.num_fmt = fmts
                .get(&fmt_id)
                .cloned()
                .or_else(|| builtin_numfmt(fmt_id).map(str::to_string))
                .unwrap_or_else(|| "General".into());
            if let Some(font) = fonts.get(idx(xf, "fontId")) {
                s.bold = flag(font.child("b"));
                s.italic = flag(font.child("i"));
                s.strike = flag(font.child("strike"));
                s.underline = match font.child("u").map(|u| u.attr("val").unwrap_or("single")) {
                    Some("double") | Some("doubleAccounting") => 2,
                    Some("none") | None => 0,
                    Some(_) => 1,
                };
                if let Some(sz) = font
                    .child("sz")
                    .and_then(|n| n.attr("val"))
                    .and_then(|v| v.parse::<f64>().ok())
                {
                    s.font_size_tenths = (sz * 10.0).round().clamp(10.0, 4090.0) as u32;
                }
                if let Some(name) = font.child("name").and_then(|n| n.attr("val")) {
                    s.font_name = name.to_string();
                }
                (s.font_color, s.font_theme) =
                    crate::xlsx_theme::style_color(font.child("color"), theme, true);
            }
            if let Some(fill) = fills.get(idx(xf, "fillId"))
                && let Some(pf) = fill.child("patternFill")
                && pf.attr("patternType").is_some_and(|p| p != "none")
            {
                (s.fill, s.fill_theme) =
                    crate::xlsx_theme::style_color(pf.child("fgColor"), theme, false);
            }
            if let Some(b) = borders.get(idx(xf, "borderId")) {
                s.border_left = border_of(b.child("left").or_else(|| b.child("start")));
                s.border_right = border_of(b.child("right").or_else(|| b.child("end")));
                s.border_top = border_of(b.child("top"));
                s.border_bottom = border_of(b.child("bottom"));
            }
            if let Some(al) = xf.child("alignment") {
                s.h_align = match al.attr("horizontal").unwrap_or("") {
                    "left" => HAlign::Left,
                    "center" => HAlign::Center,
                    "right" => HAlign::Right,
                    "fill" => HAlign::Fill,
                    "justify" | "distributed" => HAlign::Justify,
                    "centerContinuous" => HAlign::CenterAcross,
                    _ => HAlign::General,
                };
                s.v_align = match al.attr("vertical").unwrap_or("") {
                    "top" => VAlign::Top,
                    "center" => VAlign::Center,
                    _ => VAlign::Bottom,
                };
                s.wrap = al.attr("wrapText").is_some_and(|v| v == "1" || v == "true");
                s.indent = al
                    .attr("indent")
                    .and_then(|v| v.parse::<u8>().ok())
                    .unwrap_or(0);
                s.rotation = al
                    .attr("textRotation")
                    .and_then(|v| v.parse::<i16>().ok())
                    .unwrap_or(0);
            }
            if let Some(p) = xf.child("protection") {
                s.locked = p.attr("locked").is_none_or(|v| v != "0");
                s.hidden_formula = p.attr("hidden").is_some_and(|v| v == "1");
            }
            out.push(s);
        }
    }
    Ok(out)
}

/// Stream sharedStrings.xml: each `<si>` concatenates its `<t>` runs (phonetic runs skipped).
fn read_shared_strings(xml: &str) -> Result<Vec<String>, IoError> {
    let mut reader = Reader::from_str(xml);
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_t = false;
    let mut in_rph = false;
    loop {
        match reader
            .read_event()
            .map_err(|e| IoError::Xml(e.to_string()))?
        {
            Event::Start(e) => match local(e.name().as_ref()) {
                "si" => cur.clear(),
                "t" => in_t = true,
                "rPh" => in_rph = true,
                _ => {}
            },
            Event::Empty(e) if local(e.name().as_ref()) == "si" => out.push(String::new()),
            Event::End(e) => match local(e.name().as_ref()) {
                "si" => out.push(unescape_ooxml(&std::mem::take(&mut cur))),
                "t" => in_t = false,
                "rPh" => in_rph = false,
                _ => {}
            },
            Event::Text(t) if in_t && !in_rph => cur.push_str(&t.xml10_content()),
            Event::GeneralRef(r) if in_t && !in_rph => cur.push_str(&resolve_ref(&r)),
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

struct SharedFormula {
    anchor: CellAddr,
    /// The master formula's text and tree, parsed once (`None`: unparseable).
    src: String,
    expr: Option<sp_formula::Expr>,
}

/// Formula text for a follower of a shared formula: the master shifted by the offset.
fn expand_shared(master: &SharedFormula, at: CellAddr) -> Option<String> {
    let mut e = master.expr.clone()?;
    shift_for_copy(
        &mut e,
        i64::from(at.row) - i64::from(master.anchor.row),
        i64::from(at.col) - i64::from(master.anchor.col),
    );
    Some(to_formula(&e))
}

#[derive(Default)]
struct CellBuf {
    addr: Option<CellAddr>,
    t: String,
    s: usize,
    v: String,
    f: Option<String>,
    f_shared_si: Option<String>,
    f_ref: Option<String>,
    /// `<f t="array">`: an array formula over `f_ref`.
    f_array: bool,
    /// `cm` cell metadata: a dynamic-array formula.
    cm: bool,
    /// `vm` value metadata (1-based): a rich error value.
    vm: Option<usize>,
    is_text: String,
    has_v: bool,
}

impl CellBuf {
    /// Clear for the next cell, keeping string capacity.
    fn reset(&mut self) {
        self.addr = None;
        self.t.clear();
        self.s = 0;
        self.v.clear();
        self.f = None;
        self.f_shared_si = None;
        self.f_ref = None;
        self.f_array = false;
        self.cm = false;
        self.vm = None;
        self.is_text.clear();
        self.has_v = false;
    }
}

/// Per-sheet state shared by the cell parser.
struct SheetCtx<'a> {
    /// Rich error kinds by value-metadata index (0-based).
    rich: &'a [Option<ErrorKind>],
    /// Array-formula areas: (range, fixed legacy array, blocked spill).
    areas: Vec<(CellRange, bool, bool)>,
    /// `<f t="dataTable">` attributes (see `xlsx_pivots::apply_data_tables`).
    data_tables: Vec<Vec<(String, String)>>,
    /// Formula rewrites (shared-formula expansion, normalization) done once per relative
    /// form and instantiated for the rest of a filled-down range.
    memo: FormulaMemo,
}

fn read_sheet(
    xml: &str,
    sheet: &mut Sheet,
    sst: &[Arc<str>],
    xf_map: &[StyleId],
    report: &mut LoadReport,
    rich: &[Option<ErrorKind>],
) -> Result<(), IoError> {
    let mut ctx = SheetCtx {
        rich,
        areas: Vec::new(),
        data_tables: Vec::new(),
        memo: FormulaMemo::new(),
    };
    read_sheet_cells(xml, sheet, sst, xf_map, report, &mut ctx)?;
    crate::xlsx_pivots::apply_data_tables(sheet, &ctx.data_tables);
    // Cells inside an array area hold spilled values, not constants of their own.
    for (range, fixed, blocked) in ctx.areas {
        if !blocked {
            let inside: Vec<CellAddr> = sheet
                .cells_in(range)
                .filter(|(a, c)| *a != range.start && c.formula_src().is_none())
                .map(|(a, _)| a)
                .collect();
            for a in inside {
                if let Some(c) = sheet.get_mut(a) {
                    c.content = Content::Value(Value::Empty);
                    if c.value.is_empty() {
                        c.value = Value::Number(0.0);
                    }
                }
            }
        }
        if sheet.spill_at_anchor(range.start).is_none() {
            sheet.spills.push(SpillArea {
                range,
                fixed,
                blocked,
            });
        }
    }
    sheet
        .spills
        .sort_by_key(|x| (x.range.start.row, x.range.start.col));
    Ok(())
}

fn read_sheet_cells(
    xml: &str,
    sheet: &mut Sheet,
    sst: &[Arc<str>],
    xf_map: &[StyleId],
    report: &mut LoadReport,
    ctx: &mut SheetCtx<'_>,
) -> Result<(), IoError> {
    let mut reader = Reader::from_str(xml);
    let mut shared: HashMap<String, SharedFormula> = HashMap::new();
    let mut c = CellBuf::default();
    let mut in_c = false;
    let mut field: Option<&'static str> = None;
    let mut next_row: u32 = 0;
    let mut next_col: u32 = 0;
    loop {
        let ev = reader
            .read_event()
            .map_err(|e| IoError::Xml(e.to_string()))?;
        let (is_empty, start) = match &ev {
            Event::Start(e) => (false, Some(e.clone())),
            Event::Empty(e) => (true, Some(e.clone())),
            _ => (false, None),
        };
        if let Some(e) = start {
            let ln = e.local_name();
            let name = ln.as_ref();
            // Only elements we read attributes from pay for attribute decoding.
            let wanted = matches!(
                name,
                "row"
                    | "c"
                    | "col"
                    | "mergeCell"
                    | "pane"
                    | "sheetView"
                    | "sheetFormatPr"
                    | "tabColor"
            );
            let mut a: Vec<(&str, std::borrow::Cow<'_, str>)> = Vec::new();
            if wanted {
                for at in e.attributes().with_checks(false).flatten() {
                    let v = if at.value.contains('&') {
                        at.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                            .unwrap_or_default()
                    } else {
                        at.value
                    };
                    a.push((at.key.local_name().into_inner(), v));
                }
            }
            let get = |k: &str| a.iter().find(|(n, _)| *n == k).map(|(_, v)| v.as_ref());
            match name {
                "row" => {
                    if let Some(r) = get("r")
                        .and_then(|r| r.parse::<u32>().ok())
                        .filter(|r| *r >= 1)
                    {
                        next_row = r - 1;
                    }
                    next_col = 0;
                    if let Some(ht) = get("ht").and_then(|v| v.parse::<f64>().ok())
                        && get("customHeight").is_some_and(|v| v == "1" || v == "true")
                    {
                        sheet.row_heights.insert(next_row, ht);
                    }
                    if get("hidden").is_some_and(|v| v == "1" || v == "true") {
                        sheet.hidden_rows.insert(next_row);
                    }
                    if let Some(l) = get("outlineLevel").and_then(|v| v.parse::<u8>().ok())
                        && l > 0
                    {
                        sheet.outline.rows.insert(next_row, l.min(7));
                    }
                    if is_empty {
                        next_row += 1;
                    }
                }
                "c" => {
                    c.reset();
                    c.addr = match get("r") {
                        Some(r) => CellAddr::parse(r).ok(),
                        None => Some(CellAddr::new(next_row, next_col)),
                    };
                    if let Some(a) = c.addr {
                        next_col = a.col + 1;
                    }
                    c.t.push_str(get("t").unwrap_or("n"));
                    c.s = get("s").and_then(|v| v.parse().ok()).unwrap_or(0);
                    c.cm = get("cm").is_some();
                    c.vm = get("vm").and_then(|v| v.parse().ok());
                    in_c = !is_empty;
                    if is_empty {
                        finish_cell(sheet, &mut c, &mut shared, sst, xf_map, report, ctx);
                    }
                }
                "v" if in_c => field = Some("v"),
                "f" if in_c => {
                    field = Some("f");
                    c.f = Some(String::new());
                    let mut t = None;
                    let mut fa: Vec<(String, String)> = Vec::new();
                    for at in e.attributes().with_checks(false).flatten() {
                        let v = at
                            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                            .unwrap_or_default()
                            .into_owned();
                        let key = at.key.local_name();
                        if matches!(key.as_ref(), "dt2D" | "dtr" | "r1" | "r2") {
                            fa.push((key.into_inner().to_string(), v.clone()));
                        }
                        match at.key.local_name().as_ref() {
                            "t" => t = Some(v),
                            "si" => c.f_shared_si = Some(v),
                            "ref" => c.f_ref = Some(v),
                            _ => {}
                        }
                    }
                    match t.as_deref() {
                        Some("shared") => {}
                        Some("array") => {
                            c.f_array = true;
                            c.f_shared_si = None;
                        }
                        Some("dataTable") => {
                            if let Some(r) = c.f_ref.take() {
                                fa.push(("ref".to_string(), r));
                            }
                            ctx.data_tables.push(fa);
                            c.f = None;
                            c.f_shared_si = None;
                            c.f_ref = None;
                        }
                        _ => {
                            c.f_shared_si = None;
                            c.f_ref = None;
                        }
                    }
                    if is_empty {
                        field = None;
                    }
                }
                "t" if in_c => field = Some("is"),
                "col" => {
                    let min = get("min").and_then(|v| v.parse::<u32>().ok()).unwrap_or(1);
                    let max = get("max")
                        .and_then(|v| v.parse::<u32>().ok())
                        .unwrap_or(min)
                        .min(sp_core::MAX_COLS);
                    let width = get("width").and_then(|v| v.parse::<f64>().ok());
                    let hidden = get("hidden").is_some_and(|v| v == "1" || v == "true");
                    // Ignore "rest of sheet" column records to keep storage sparse.
                    if max.saturating_sub(min) < 1024 {
                        for col in min.saturating_sub(1)..max {
                            if let Some(w) = width
                                && get("customWidth").is_some_and(|v| v == "1" || v == "true")
                            {
                                sheet.col_widths.insert(col, w);
                            }
                            if hidden {
                                sheet.hidden_cols.insert(col);
                            }
                            if let Some(l) = get("outlineLevel").and_then(|v| v.parse::<u8>().ok())
                                && l > 0
                            {
                                sheet.outline.cols.insert(col, l.min(7));
                            }
                        }
                    }
                }
                "mergeCell" => {
                    if let Some(r) = get("ref").and_then(|r| CellRange::parse(r).ok()) {
                        sheet.merges.push(r);
                    }
                }
                "pane" => {
                    let frozen = get("state").is_some_and(|s| s == "frozen" || s == "frozenSplit");
                    if frozen {
                        let xs = get("xSplit")
                            .and_then(|v| v.parse::<f64>().ok())
                            .unwrap_or(0.0) as u32;
                        let ys = get("ySplit")
                            .and_then(|v| v.parse::<f64>().ok())
                            .unwrap_or(0.0) as u32;
                        sheet.freeze = Some(CellAddr::new(ys, xs));
                    }
                }
                "sheetView" => {
                    if get("showGridLines").is_some_and(|v| v == "0" || v == "false") {
                        sheet.show_gridlines = false;
                    }
                }
                "sheetFormatPr" => {
                    if let Some(h) = get("defaultRowHeight").and_then(|v| v.parse::<f64>().ok()) {
                        sheet.default_row_height = h;
                    }
                    if let Some(w) = get("defaultColWidth").and_then(|v| v.parse::<f64>().ok()) {
                        sheet.default_col_width = w;
                    }
                }
                "tabColor" => sheet.tab_color = get("rgb").and_then(Color::from_hex),
                _ => {}
            }
            continue;
        }
        match ev {
            Event::Text(t) => {
                if let Some(f) = field {
                    let txt = t.xml10_content();
                    push_field(&mut c, f, &txt);
                }
            }
            Event::GeneralRef(r) => {
                if let Some(f) = field {
                    push_field(&mut c, f, &resolve_ref(&r));
                }
            }
            Event::End(e) => match e.local_name().as_ref() {
                "c" => {
                    in_c = false;
                    finish_cell(sheet, &mut c, &mut shared, sst, xf_map, report, ctx);
                }
                "v" | "f" | "t" => field = None,
                "row" => next_row += 1,
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(())
}

fn push_field(c: &mut CellBuf, field: &str, txt: &str) {
    match field {
        "v" => {
            c.v.push_str(txt);
            c.has_v = true;
        }
        "f" => {
            if let Some(f) = c.f.as_mut() {
                f.push_str(txt);
            }
        }
        _ => c.is_text.push_str(txt),
    }
}

fn finish_cell(
    sheet: &mut Sheet,
    c: &mut CellBuf,
    shared: &mut HashMap<String, SharedFormula>,
    sst: &[Arc<str>],
    xf_map: &[StyleId],
    report: &mut LoadReport,
    ctx: &mut SheetCtx<'_>,
) {
    let Some(addr) = c.addr else { return };
    let style = xf_map.get(c.s).copied().unwrap_or(StyleId::DEFAULT);
    let value = match c.t.as_str() {
        "s" => {
            c.v.trim()
                .parse::<usize>()
                .ok()
                .and_then(|i| sst.get(i))
                .map_or(Value::Empty, |s| Value::Text(s.clone()))
        }
        "str" => Value::text(unescape_ooxml(&c.v)),
        "inlineStr" => Value::text(unescape_ooxml(&c.is_text)),
        "b" => Value::Bool(c.v.trim() == "1"),
        "e" => Value::Error(
            c.vm.and_then(|i| ctx.rich.get(i.checked_sub(1)?).copied().flatten())
                .or_else(|| ErrorKind::parse(c.v.trim()))
                .unwrap_or(ErrorKind::Value),
        ),
        _ if c.has_v => {
            c.v.trim()
                .parse::<f64>()
                .map_or(Value::Empty, Value::Number)
        }
        _ => Value::Empty,
    };
    let formula = match (c.f.take(), c.f_shared_si.take()) {
        (Some(src), Some(si)) if !src.trim().is_empty() => {
            let master = src.trim().trim_start_matches('=');
            shared.insert(
                si,
                SharedFormula {
                    anchor: addr,
                    src: master.to_string(),
                    expr: parse(master).ok(),
                },
            );
            Some(src)
        }
        (Some(_), Some(si)) => match shared.get(&si) {
            Some(master) => {
                let f = if master.expr.is_some() {
                    ctx.memo
                        .rewrite(&master.src, master.anchor, addr, "share", |_| {
                            expand_shared(master, addr)
                        })
                } else {
                    None
                };
                if f.is_none() {
                    report.warnings.push(format!(
                        "{}!{}: could not expand shared formula",
                        sheet.name, addr
                    ));
                }
                f
            }
            None => None,
        },
        (Some(src), None) if !src.trim().is_empty() => Some(src),
        _ => None,
    };
    let area = if c.f_array && formula.is_some() {
        c.f_ref
            .as_deref()
            .and_then(|r| CellRange::parse(r).ok())
            .filter(|r| r.start == addr)
    } else {
        None
    };
    if let Some(r) = area {
        let dynamic = c.cm;
        let blocked = dynamic && value == Value::Error(ErrorKind::Spill);
        if !dynamic || !r.is_single() || blocked {
            ctx.areas.push((r, !dynamic, blocked));
        }
    }
    // Formulas saved without dynamic-array metadata use implicit intersection.
    let legacy = !c.f_array && !c.cm;
    c.f_ref = None;
    c.f_array = false;
    let cell = match formula {
        Some(src) => {
            report.formulas += 1;
            Cell {
                content: Content::Formula(normalize_formula_at(&mut ctx.memo, &src, addr, legacy)),
                value,
                style,
            }
        }
        None => Cell {
            content: Content::Value(value.clone()),
            value,
            style,
        },
    };
    if !cell.is_blank() {
        report.cells += 1;
    }
    sheet.set(addr, cell);
}

/// [`normalize_formula`] of the formula at `at`, done once per relative form (`memo`).
fn normalize_formula_at(memo: &mut FormulaMemo, src: &str, at: CellAddr, legacy: bool) -> String {
    let src = src.trim().trim_start_matches('=');
    if is_simple_canonical(src) {
        return src.to_string();
    }
    let tag = if legacy { "legacy" } else { "dynamic" };
    memo.rewrite(src, at, at, tag, |s| Some(normalize_formula(s, legacy)))
        .unwrap_or_else(|| normalize_formula(src, legacy))
}

/// Canonical in-memory formula text: `_xlfn.`-style prefixes removed. Text we cannot parse
/// yet is kept verbatim (minus prefixes) so saving does not lose it.
fn normalize_formula(src: &str, legacy: bool) -> String {
    let src = src.trim().trim_start_matches('=');
    if is_simple_canonical(src) {
        return src.to_string();
    }
    match parse(src) {
        Ok(e) if legacy => to_formula(&e.into_legacy()),
        Ok(e) => to_formula(&e),
        Err(_) => src
            .replace("_xlfn._xlws.", "")
            .replace("_xlfn.", "")
            .replace("_xlws.", ""),
    }
}

/// Formulas made only of single-cell references, non-negative integers, binary operators
/// and parentheses (e.g. `A1*2+B$3`) are already in canonical form and cannot spill, so
/// they skip the parse/print round trip of `normalize_formula` (the bulk of load time).
fn is_simple_canonical(src: &str) -> bool {
    let b = src.as_bytes();
    if b.is_empty() || b.len() > 256 {
        return false;
    }
    let mut i = 0;
    let mut depth = 0usize;
    // Expect an operand (vs. a binary operator) next.
    let mut want_operand = true;
    while let Some(&ch) = b.get(i) {
        if want_operand {
            match ch {
                b'(' => {
                    depth += 1;
                    i += 1;
                }
                b'0'..=b'9' if ch != b'0' || !b.get(i + 1).is_some_and(u8::is_ascii_digit) => {
                    let start = i;
                    while b.get(i).is_some_and(u8::is_ascii_digit) {
                        i += 1;
                    }
                    // Integers only; a following letter/`.`/`$` means something else (1E5…).
                    if i - start > 15
                        || b.get(i)
                            .is_some_and(|c| c.is_ascii_alphabetic() || matches!(c, b'.' | b'$'))
                    {
                        return false;
                    }
                    want_operand = false;
                }
                b'$' | b'A'..=b'Z' => {
                    let start = i;
                    if b.get(i) == Some(&b'$') {
                        i += 1;
                    }
                    let l0 = i;
                    while b.get(i).is_some_and(u8::is_ascii_uppercase) {
                        i += 1;
                    }
                    if i == l0 || i - l0 > 3 {
                        return false;
                    }
                    if b.get(i) == Some(&b'$') {
                        i += 1;
                    }
                    let d0 = i;
                    while b.get(i).is_some_and(u8::is_ascii_digit) {
                        i += 1;
                    }
                    if i == d0 || i - d0 > 7 || b.get(d0) == Some(&b'0') {
                        return false;
                    }
                    // A letter/`(`/`.`/`!`/`:`/`_` after it: a name, call, sheet or range.
                    if b.get(i).is_some_and(|c| {
                        c.is_ascii_alphanumeric()
                            || matches!(c, b'(' | b'.' | b'!' | b':' | b'_' | b'$')
                    }) {
                        return false;
                    }
                    let Some(tok) = src.get(start..i) else {
                        return false;
                    };
                    if CellAddr::parse(&tok.replace('$', "")).is_err() {
                        return false;
                    }
                    want_operand = false;
                }
                _ => return false,
            }
        } else {
            match ch {
                b')' if depth > 0 => {
                    depth -= 1;
                    i += 1;
                }
                b'+' | b'-' | b'*' | b'/' | b'^' | b'&' | b'=' => {
                    i += 1;
                    want_operand = true;
                }
                b'<' | b'>' => {
                    i += 1;
                    if matches!(b.get(i), Some(b'=')) || (ch == b'<' && b.get(i) == Some(&b'>')) {
                        i += 1;
                    }
                    want_operand = true;
                }
                _ => return false,
            }
        }
    }
    !want_operand && depth == 0
}

pub fn resolve_target_pub(base_dir: &str, target: &str) -> String {
    resolve_target(base_dir, target)
}

fn resolve_target(base_dir: &str, target: &str) -> String {
    if let Some(abs) = target.strip_prefix('/') {
        return abs.to_string();
    }
    let mut parts: Vec<&str> = base_dir.split('/').filter(|p| !p.is_empty()).collect();
    for seg in target.split('/') {
        match seg {
            ".." => {
                parts.pop();
            }
            "." | "" => {}
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// Sheet parts beyond cells (filters, protection, validations, links, notes, page setup).
/// Problems are reported as warnings: the cells already loaded are kept.
fn read_extras<R: Read + Seek>(
    zip: &mut zip::ZipArchive<R>,
    path: &str,
    xml: &str,
    sheet: &mut Sheet,
    report: &mut LoadReport,
) -> Result<(), IoError> {
    let (dir, file) = path.rsplit_once('/').unwrap_or(("", path));
    let rels = match read_part(zip, &format!("{dir}/_rels/{file}.rels"))? {
        Some(x) => crate::xlsx_extras::parse_rels(&x, dir).unwrap_or_default(),
        None => Default::default(),
    };
    if let Err(e) = crate::xlsx_charts::read_sheet_sparklines(xml, sheet) {
        report
            .warnings
            .push(format!("sheet '{}': sparklines skipped ({e})", sheet.name));
    }
    if let Err(e) = crate::xlsx_extras::read_sheet_extras(xml, sheet, &rels) {
        report
            .warnings
            .push(format!("sheet '{}': extras skipped ({e})", sheet.name));
    }
    let find = |kind: &str| {
        rels.values()
            .find(|(t, ext)| !ext && t.contains(kind))
            .map(|(t, _)| t.clone())
    };
    if let Some(cpath) = find("comments") {
        let vml = match find("vmlDrawing") {
            Some(v) => read_part(zip, &v)?,
            None => None,
        };
        if let Some(cx) = read_part(zip, &cpath)?
            && let Err(e) = crate::xlsx_extras::read_comments(&cx, vml.as_deref(), sheet)
        {
            report
                .warnings
                .push(format!("sheet '{}': comments skipped ({e})", sheet.name));
        }
    }
    Ok(())
}

/// Error kind encoded by a rich-value `errorType`.
fn rich_error(code: i64) -> ErrorKind {
    match code {
        0..=7 => ErrorKind::ALL
            .get(code as usize)
            .copied()
            .unwrap_or(ErrorKind::Value),
        8 => ErrorKind::Spill,
        13 => ErrorKind::Calc,
        _ => ErrorKind::Value,
    }
}

/// Newer error values (`#SPILL!`, `#CALC!`) are saved as `#VALUE!` plus value metadata
/// pointing at a rich value; resolve each value-metadata index to its error kind.
fn read_rich_errors<R: Read + Seek>(
    zip: &mut zip::ZipArchive<R>,
) -> Result<Vec<Option<ErrorKind>>, IoError> {
    let (Some(meta), Some(rv)) = (
        read_part(zip, "xl/metadata.xml")?,
        read_part(zip, "xl/richData/rdrichvalue.xml")?,
    ) else {
        return Ok(Vec::new());
    };
    let structs = read_part(zip, "xl/richData/rdrichvaluestructure.xml")?.unwrap_or_default();
    // Position of `errorType` among each structure's keys (None: not an error).
    let error_pos: Vec<Option<usize>> = if structs.is_empty() {
        Vec::new()
    } else {
        parse_dom(&structs)?
            .children_named("s")
            .map(|s| {
                (s.attr("t") == Some("_error")).then(|| {
                    s.children_named("k")
                        .position(|k| k.attr("n") == Some("errorType"))
                })?
            })
            .collect()
    };
    let values: Vec<Option<ErrorKind>> = parse_dom(&rv)?
        .children_named("rv")
        .map(|r| {
            let s: usize = r.attr("s")?.parse().ok()?;
            let pos = (*error_pos.get(s)?)?;
            let code: i64 = r.children_named("v").nth(pos)?.text.trim().parse().ok()?;
            Some(rich_error(code))
        })
        .collect();
    let m = parse_dom(&meta)?;
    let mut future: Vec<Option<usize>> = Vec::new();
    for fm in m.children_named("futureMetadata") {
        if fm.attr("name") == Some("XLRICHVALUE") {
            future = fm
                .children_named("bk")
                .map(|bk| {
                    let rvb = bk.child("extLst")?.child("ext")?.child("rvb")?;
                    rvb.attr("i")?.parse().ok()
                })
                .collect();
        }
    }
    let Some(vmeta) = m.child("valueMetadata") else {
        return Ok(Vec::new());
    };
    Ok(vmeta
        .children_named("bk")
        .map(|bk| {
            let v: usize = bk.child("rc")?.attr("v")?.parse().ok()?;
            let i = (*future.get(v)?)?;
            *values.get(i)?
        })
        .collect())
}

pub fn read_xlsx<R: Read + Seek>(reader: R) -> Result<(Workbook, LoadReport), IoError> {
    let mut zip = zip::ZipArchive::new(reader).map_err(|e| IoError::Zip(e.to_string()))?;
    let mut report = LoadReport::default();
    let wb_xml = read_part(&mut zip, "xl/workbook.xml")?
        .ok_or_else(|| IoError::Format("missing xl/workbook.xml".into()))?;
    let rels_xml = read_part(&mut zip, "xl/_rels/workbook.xml.rels")?.unwrap_or_default();
    let raw_rels = if rels_xml.is_empty() {
        Vec::new()
    } else {
        pass::parse_raw_rels(&rels_xml, "xl")?
    };
    let rels: HashMap<String, (String, String)> = raw_rels
        .iter()
        .map(|r| (r.id.clone(), (r.target.clone(), r.rel_type.clone())))
        .collect();
    let ct = match read_part(&mut zip, "[Content_Types].xml")? {
        Some(x) => pass::ContentTypes::parse(&x).unwrap_or_default(),
        None => pass::ContentTypes::default(),
    };
    let mut capture = pass::Capture::new(ct);
    let wb_pass = match capture.workbook(&mut zip, &wb_xml, &raw_rels) {
        Ok(p) => p,
        Err(e) => {
            report
                .warnings
                .push(format!("workbook: unmodelled content dropped ({e})"));
            Default::default()
        }
    };
    let sst: Vec<Arc<str>> = match read_part(&mut zip, "xl/sharedStrings.xml")? {
        Some(x) => read_shared_strings(&x)?
            .into_iter()
            .map(Arc::from)
            .collect(),
        None => Vec::new(),
    };
    let rich = read_rich_errors(&mut zip)?;
    let mut wb = Workbook::new();
    wb.sheets.clear();
    if let Some(r) = raw_rels
        .iter()
        .find(|r| !r.external && pass::rel_kind(&r.rel_type) == "theme")
        && let Some(x) = read_part(&mut zip, &r.target)?
        && let Some(t) = crate::xlsx_theme::parse_theme(&x)
    {
        wb.theme = t;
    }
    let mut fpb_adopted = false;
    let xf_styles = match read_part(&mut zip, "xl/styles.xml")? {
        Some(x) => {
            let mut v = read_styles(&x, &wb.theme)?;
            if let Some(t) = crate::xlsx_drawings::checkbox_bag_target(&wb_pass)
                && let Some(bag) = read_part(&mut zip, &t)?
            {
                fpb_adopted = crate::xlsx_drawings::read_checkboxes(&x, &bag, &mut v);
            }
            v
        }
        None => Vec::new(),
    };
    let xf_map: Vec<StyleId> = xf_styles.into_iter().map(|s| wb.styles.intern(s)).collect();

    let dxfs = crate::xlsx_cf::read_dxfs(&mut zip);
    let wbdom = parse_dom(&wb_xml)?;
    let sheets_node = wbdom
        .child("sheets")
        .ok_or_else(|| IoError::Format("workbook has no <sheets>".into()))?;
    for (i, sn) in sheets_node.children_named("sheet").enumerate() {
        let name = sn.attr("name").unwrap_or("Sheet").to_string();
        let mut sheet = Sheet::new(name.clone());
        sheet.visibility = match sn.attr("state") {
            Some("hidden") => Visibility::Hidden,
            Some("veryHidden") => Visibility::VeryHidden,
            _ => Visibility::Visible,
        };
        let (path, rel_type) = sn
            .attr("id")
            .and_then(|id| rels.get(id))
            .cloned()
            .unwrap_or_else(|| (format!("xl/worksheets/sheet{}.xml", i + 1), String::new()));
        if pass::is_opaque_sheet_rel(&rel_type) {
            match capture.opaque_sheet(&mut zip, &path, &rel_type) {
                Ok(Some(p)) => sheet.passthrough = p,
                Ok(None) => {
                    capture.skip_sheet();
                    report
                        .warnings
                        .push(format!("sheet '{name}': part {path} not found"));
                }
                Err(e) => {
                    capture.skip_sheet();
                    report
                        .warnings
                        .push(format!("sheet '{name}': dropped ({e})"));
                }
            }
        } else {
            match read_part(&mut zip, &path)? {
                Some(x) => {
                    read_sheet(&x, &mut sheet, &sst, &xf_map, &mut report, &rich)?;
                    read_extras(&mut zip, &path, &x, &mut sheet, &mut report)?;
                    crate::xlsx_cf::read_sheet(&mut zip, &path, &x, &mut sheet, &dxfs, &mut report);
                    let (head, tail) =
                        crate::xlsx_extras::split_at_sheet_data(&x).unwrap_or((x.as_str(), ""));
                    match capture.worksheet(&mut zip, &path, head, tail) {
                        Ok(p) => sheet.passthrough = p,
                        Err(e) => {
                            capture.skip_sheet();
                            report
                                .warnings
                                .push(format!("sheet '{name}': unmodelled content dropped ({e})"));
                        }
                    }
                }
                None => {
                    capture.skip_sheet();
                    report
                        .warnings
                        .push(format!("sheet '{name}': part {path} not found"));
                }
            }
        }
        sheet.passthrough.sheet_id = sn.attr("sheetId").and_then(|v| v.parse().ok());
        wb.sheets.push(sheet);
    }
    if wb.sheets.is_empty() {
        return Err(IoError::Format("workbook contains no sheets".into()));
    }
    wb.passthrough = wb_pass;
    capture.finish(&mut wb);
    if fpb_adopted {
        crate::xlsx_drawings::drop_checkbox_bag(&mut wb);
    }
    crate::xlsx_drawings::adopt_backgrounds(&mut wb);
    crate::xlsx_charts::adopt_captured_charts(&mut wb);
    crate::xlsx_pivots::adopt_captured_pivots(&mut wb);
    if let Some(dn) = wbdom.child("definedNames") {
        for d in dn.children_named("definedName") {
            let Some(name) = d.attr("name") else { continue };
            let scope = d
                .attr("localSheetId")
                .and_then(|v| v.parse::<usize>().ok())
                .filter(|i| *i < wb.sheets.len());
            wb.names.push(DefinedName {
                name: name.to_string(),
                formula: d.text.trim().to_string(),
                scope,
            });
        }
    }
    crate::xlsx_extras::read_workbook_protection(&wbdom, &mut wb);
    if let Some(v) = wbdom
        .child("bookViews")
        .and_then(|b| b.child("workbookView"))
    {
        wb.active = v
            .attr("activeTab")
            .and_then(|t| t.parse::<usize>().ok())
            .filter(|i| *i < wb.sheets.len())
            .unwrap_or(0);
    }
    if let Some(cp) = wbdom.child("calcPr")
        && cp.attr("calcMode") == Some("manual")
    {
        wb.calc_mode = sp_model::CalcMode::Manual;
    }
    if let Some(core) = read_part(&mut zip, "docProps/core.xml")? {
        let d = parse_dom(&core)?;
        wb.props.title = d.child("title").map(|n| n.text.clone()).unwrap_or_default();
        wb.props.author = d
            .child("creator")
            .map(|n| n.text.clone())
            .unwrap_or_default();
        wb.props.created = d
            .child("created")
            .map(|n| n.text.clone())
            .unwrap_or_default();
        wb.props.modified = d
            .child("modified")
            .map(|n| n.text.clone())
            .unwrap_or_default();
    }
    Ok((wb, report))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Normalization and shared-formula expansion through the memo give what doing each
    /// formula on its own gives.
    #[test]
    fn memoized_normalization_matches_per_formula() {
        let fills: [fn(u32) -> String; 9] = [
            |r| format!("A{r}*2+1"),
            |r| format!("SUM($A$1:$A{r})+A{r}"),
            |r| format!("IF(A{r}>5,\"x\",B{r})"),
            |r| format!("_xlfn.CONCAT(A{r},\"A{r}\")"),
            |r| format!("STDEV.S(A$1:A{r})"),
            |r| format!("A{r}:A{}", r + 2),
            |r| format!("SUM(A:A)+Sheet1!A{r}"),
            |r| format!("INDEX(A{r}:B{r},1)"),
            |r| format!("A{r}+B{}", r + 1),
        ];
        let mut memo = FormulaMemo::new();
        for legacy in [true, false] {
            for (c, f) in fills.iter().enumerate() {
                let master = f(1);
                let shared = SharedFormula {
                    anchor: CellAddr::new(0, c as u32 + 2),
                    src: master.clone(),
                    expr: parse(&master).ok(),
                };
                for r in 1..40u32 {
                    let at = CellAddr::new(r - 1, c as u32 + 2);
                    let src = f(r);
                    assert_eq!(
                        normalize_formula_at(&mut memo, &src, at, legacy),
                        normalize_formula(&src, legacy),
                        "{src}"
                    );
                    let expanded = memo.rewrite(&shared.src, shared.anchor, at, "share", |_| {
                        expand_shared(&shared, at)
                    });
                    assert_eq!(expanded, expand_shared(&shared, at), "{master} at {at:?}");
                }
            }
        }
    }

    #[test]
    fn targets() {
        assert_eq!(
            resolve_target("xl", "worksheets/sheet1.xml"),
            "xl/worksheets/sheet1.xml"
        );
        assert_eq!(
            resolve_target("xl", "/xl/worksheets/sheet1.xml"),
            "xl/worksheets/sheet1.xml"
        );
        assert_eq!(
            resolve_target("xl", "../customXml/item1.xml"),
            "customXml/item1.xml"
        );
    }

    #[test]
    fn shared_strings_with_runs() {
        let x = r#"<sst><si><t>plain</t></si><si><r><t>a</t></r><r><t xml:space="preserve"> b&amp;c</t></r></si><si/></sst>"#;
        assert_eq!(read_shared_strings(x).unwrap(), vec!["plain", "a b&c", ""]);
    }

    #[test]
    fn simple_formula_fast_path_matches_parser() {
        for f in [
            "A1*2+1",
            "D1+C2",
            "$A$1-B$2/(C3^2)",
            "A1<>B1",
            "A1<=10",
            "(A1+B1)*((C1))",
            "XFD1048576&A1",
            "A1>=B1",
            "10-3-2",
            "2^3^2",
            "A1=B1",
        ] {
            assert!(is_simple_canonical(f), "{f}");
            for legacy in [true, false] {
                let slow = match parse(f) {
                    Ok(e) if legacy => to_formula(&e.into_legacy()),
                    Ok(e) => to_formula(&e),
                    Err(_) => panic!("{f}"),
                };
                assert_eq!(slow, f, "fast path would differ for {f}");
            }
        }
        for f in [
            "",
            "-A1",
            "+A1",
            "A1+",
            "SUM(A1)",
            "A1:B2",
            "a1",
            "A1 + 1",
            "1.5",
            "1E5",
            "007",
            "XFE1",
            "A0",
            "Sheet1!A1",
            "A1%",
            "(A1",
            "A1)",
            "AB",
            "A1#",
            "TRUE",
        ] {
            assert!(!is_simple_canonical(f), "{f}");
        }
    }

    #[test]
    fn shared_formula_expansion() {
        let m = SharedFormula {
            anchor: CellAddr::parse("F3").unwrap(),
            src: "FV(A3,B3,C3,D3,E3)".into(),
            expr: parse("FV(A3,B3,C3,D3,E3)").ok(),
        };
        assert_eq!(
            expand_shared(&m, CellAddr::parse("F5").unwrap()).unwrap(),
            "FV(A5,B5,C5,D5,E5)"
        );
    }
}
