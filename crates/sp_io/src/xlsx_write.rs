//! .xlsx writer: a minimal, valid SpreadsheetML package.

use crate::IoError;
use crate::xlsx_passthrough as pass;
use crate::xml::esc;
use sp_core::Value;
use sp_formula::shared::FormulaMemo;
use sp_formula::{Expr, parse, to_formula};
use sp_model::passthrough::RawRel;
use sp_model::{Border, BorderStyle, Content, HAlign, Style, VAlign, Visibility, Workbook};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{Seek, Write};
use zip::write::SimpleFileOptions;

/// Functions added after the original file format; stored with an `_xlfn.` prefix so older
/// readers show `#NAME?` instead of misinterpreting them.
const FUTURE_FUNCTIONS: &[&str] = &[
    "ACOT",
    "ACOTH",
    "AGGREGATE",
    "ARABIC",
    "ARRAYTOTEXT",
    "BASE",
    "BITAND",
    "BITLSHIFT",
    "BITOR",
    "BITRSHIFT",
    "BITXOR",
    "BYCOL",
    "BYROW",
    "CEILING.MATH",
    "CEILING.PRECISE",
    "CHOOSECOLS",
    "CHOOSEROWS",
    "COMBINA",
    "CONCAT",
    "COT",
    "COTH",
    "CSC",
    "CSCH",
    "DAYS",
    "DECIMAL",
    "DROP",
    "ENCODEURL",
    "EXPAND",
    "FILTERXML",
    "FLOOR.MATH",
    "FLOOR.PRECISE",
    "FORMULATEXT",
    "GAMMA",
    "GAUSS",
    "HSTACK",
    "IFNA",
    "IFS",
    "IMAGE",
    "ISFORMULA",
    "ISOMITTED",
    "ISOWEEKNUM",
    "LAMBDA",
    "LET",
    "MAKEARRAY",
    "MAP",
    "MAXIFS",
    "MINIFS",
    "MUNIT",
    "NUMBERVALUE",
    "PDURATION",
    "PERMUTATIONA",
    "PHI",
    "RANDARRAY",
    "REDUCE",
    "RRI",
    "SCAN",
    "SEC",
    "SECH",
    "SEQUENCE",
    "SHEET",
    "SHEETS",
    "SINGLE",
    "SWITCH",
    "TAKE",
    "TEXTAFTER",
    "TEXTBEFORE",
    "TEXTJOIN",
    "TEXTSPLIT",
    "TOCOL",
    "TOROW",
    "UNICHAR",
    "UNICODE",
    "UNIQUE",
    "VALUETOTEXT",
    "VSTACK",
    "WEBSERVICE",
    "WRAPCOLS",
    "WRAPROWS",
    "XLOOKUP",
    "XMATCH",
    "XOR",
    "SORTBY",
    "STOCKHISTORY",
    "GROUPBY",
    "PIVOTBY",
    "REGEXTEST",
    "REGEXEXTRACT",
    "REGEXREPLACE",
    "TRIMRANGE",
    "IMCOSH",
    "IMCOT",
    "IMCSC",
    "IMCSCH",
    "IMSEC",
    "IMSECH",
    "IMSINH",
    "IMTAN",
];
const WORKSHEET_FUNCTIONS: &[&str] = &["FILTER", "SORT"];

fn needs_prefix(name: &str) -> Option<&'static str> {
    if WORKSHEET_FUNCTIONS.contains(&name) {
        return Some("_xlfn._xlws.");
    }
    // Dotted statistical names (STDEV.S, NORM.DIST…) are 2010+ functions.
    let dotted_2010 = name.contains('.') && !matches!(name, "ERROR.TYPE");
    (FUTURE_FUNCTIONS.contains(&name) || dotted_2010).then_some("_xlfn.")
}

/// Formula text as stored in the file (prefixes added; no leading `=`).
pub fn file_formula(src: &str) -> String {
    match parse(src) {
        Ok(e) => file_expr(e, None),
        // Keep unparseable text untouched so nothing is lost.
        Err(_) => src.to_string(),
    }
}

/// How a formula cell is saved: plain text (legacy semantics), or as an array formula over
/// `ref` (dynamic when `dynamic`).
struct FileFormula {
    text: String,
    array: Option<(String, bool)>,
}

/// [`formula_for_cell`], converted once per relative form (`memo`) for formulas whose file
/// text does not depend on where they are (no array area, outside tables).
fn formula_for_cell_memo(
    memo: &mut FormulaMemo,
    src: &str,
    sheet: &sp_model::Sheet,
    at: sp_core::CellAddr,
) -> FileFormula {
    if sheet.spill_at_anchor(at).is_some() || sheet.table_at(at).is_some() {
        return formula_for_cell(src, sheet, at);
    }
    let mut array = None;
    let text = memo.rewrite(src, at, at, "save", |s| {
        let f = formula_for_cell(s, sheet, at);
        array = f.array.clone();
        // A dynamic formula is saved as an array over its own cell: not position-free.
        f.array.is_none().then_some(f.text)
    });
    match text {
        Some(text) if array.is_none() => FileFormula { text, array: None },
        _ => formula_for_cell(src, sheet, at),
    }
}

fn formula_for_cell(src: &str, sheet: &sp_model::Sheet, at: sp_core::CellAddr) -> FileFormula {
    let area = sheet.spill_at_anchor(at);
    let table = sheet.table_at(at).map(|t| t.name.as_str());
    let Ok(e) = parse(src) else {
        return FileFormula {
            text: src.to_string(),
            array: None,
        };
    };
    if let Some(a) = area {
        let r = if a.blocked {
            sp_core::CellRange::single(at)
        } else {
            a.range
        };
        return FileFormula {
            text: file_expr(e, table),
            array: Some((r.to_a1(), !a.fixed)),
        };
    }
    match e.clone().from_legacy() {
        Some(plain) => FileFormula {
            text: file_expr(plain, table),
            array: None,
        },
        None => FileFormula {
            text: file_expr(e, table),
            array: Some((at.to_a1(), true)),
        },
    }
}

/// `table`: the table containing the formula's cell, which qualifies `[@Col]`-style
/// references (files always name the table and spell `@` as `[#This Row]`).
fn file_expr(mut e: Expr, table: Option<&str>) -> String {
    e.walk_mut(&mut |n| {
        if let Expr::Table(t) = n {
            if t.table.is_none() {
                t.table = table.map(str::to_string);
            }
            if t.table.is_some() {
                // A name node prints verbatim.
                *n = Expr::Name(sp_formula::structref::print_file(t));
            }
        }
    });
    // LET / LAMBDA parameter names carry the `_xlpm.` prefix in files.
    let mut params: Vec<String> = Vec::new();
    e.walk(&mut |n| {
        if let Expr::Call(name, args) = n {
            let names: Vec<&Expr> = match name.as_str() {
                "LAMBDA" => args.iter().rev().skip(1).collect(),
                "LET" => args.iter().step_by(2).take(args.len() / 2).collect(),
                _ => Vec::new(),
            };
            for a in names {
                if let Expr::Name(p) = a {
                    params.push(p.to_ascii_lowercase());
                }
            }
        }
    });
    e.walk_mut(&mut |n| match n {
        Expr::Call(name, _) => {
            if let Some(p) = needs_prefix(name) {
                *name = format!("{p}{name}");
            }
        }
        Expr::Name(x) if params.contains(&x.to_ascii_lowercase()) => {
            *x = format!("_xlpm.{x}");
        }
        _ => {}
    });
    // `@x` and `x#` are spelled as functions in files.
    e.walk_mut(&mut |n| {
        let wrap = match n {
            Expr::Implicit(a) => Some(("_xlfn.SINGLE", std::mem::replace(&mut **a, Expr::Missing))),
            Expr::Spill(a) => Some((
                "_xlfn.ANCHORARRAY",
                std::mem::replace(&mut **a, Expr::Missing),
            )),
            _ => None,
        };
        if let Some((f, inner)) = wrap {
            *n = Expr::Call(f.to_string(), vec![inner]);
        }
    });
    to_formula(&e)
}

/// Cell metadata marking `cm="1"` formulas as dynamic arrays.
const DYNAMIC_ARRAY_METADATA: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<metadata xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:xda="http://schemas.microsoft.com/office/spreadsheetml/2017/dynamicarray"><metadataTypes count="1"><metadataType name="XLDAPR" minSupportedVersion="120000" copy="1" pasteAll="1" pasteValues="1" merge="1" splitFirst="1" rowColShift="1" clearFormats="1" clearComments="1" assign="1" coerce="1" cellMeta="1"/></metadataTypes><futureMetadata name="XLDAPR" count="1"><bk><extLst><ext uri="{bdbb8cdc-fa1e-496e-a857-3c3f30c029c3}"><xda:dynamicArrayProperties fDynamic="1" fCollapsed="0"/></ext></extLst></bk></futureMetadata><cellMetadata count="1"><bk><rc t="1" v="0"/></bk></cellMetadata></metadata>"#;

struct StylesXml {
    xml: String,
    /// StyleId index → cellXfs index.
    xf_index: Vec<usize>,
}

fn border_xml(tag: &str, b: &Border) -> String {
    let style = match b.style {
        BorderStyle::None => return format!("<{tag}/>"),
        BorderStyle::Thin => "thin",
        BorderStyle::Medium => "medium",
        BorderStyle::Thick => "thick",
        BorderStyle::Dashed => "dashed",
        BorderStyle::Dotted => "dotted",
        BorderStyle::Double => "double",
    };
    match b.color {
        Some(c) => format!(
            r#"<{tag} style="{style}"><color rgb="{}"/></{tag}>"#,
            c.to_argb_hex()
        ),
        None => format!(r#"<{tag} style="{style}"><color auto="1"/></{tag}>"#),
    }
}

fn build_styles(wb: &Workbook) -> StylesXml {
    let mut fonts: Vec<String> = Vec::new();
    let mut fills: Vec<String> = vec![
        r#"<fill><patternFill patternType="none"/></fill>"#.into(),
        r#"<fill><patternFill patternType="gray125"/></fill>"#.into(),
    ];
    let mut borders: Vec<String> = Vec::new();
    let mut numfmts: Vec<(u32, String)> = Vec::new();
    let mut xfs: Vec<String> = Vec::new();
    let mut xf_index = Vec::new();
    let intern = |list: &mut Vec<String>, item: String| -> usize {
        match list.iter().position(|x| *x == item) {
            Some(i) => i,
            None => {
                list.push(item);
                list.len() - 1
            }
        }
    };
    for (_, s) in wb.styles.iter() {
        let s: &Style = s;
        let mut f = String::from("<font>");
        if s.bold {
            f.push_str("<b/>");
        }
        if s.italic {
            f.push_str("<i/>");
        }
        if s.strike {
            f.push_str("<strike/>");
        }
        match s.underline {
            0 => {}
            2 => f.push_str(r#"<u val="double"/>"#),
            _ => f.push_str("<u/>"),
        }
        let _ = write!(f, r#"<sz val="{}"/>"#, s.font_size());
        if let Some(c) = crate::xlsx_theme::color_xml("color", s.font_color, s.font_theme) {
            f.push_str(&c);
        }
        let _ = write!(
            f,
            r#"<name val="{}"/><family val="2"/></font>"#,
            esc(&s.font_name)
        );
        let font_id = intern(&mut fonts, f);
        let fill_id = match crate::xlsx_theme::color_xml("fgColor", s.fill, s.fill_theme) {
            Some(fg) => intern(
                &mut fills,
                format!(
                    r#"<fill><patternFill patternType="solid">{fg}<bgColor indexed="64"/></patternFill></fill>"#
                ),
            ),
            None => 0,
        };
        let b = format!(
            "<border>{}{}{}{}<diagonal/></border>",
            border_xml("left", &s.border_left),
            border_xml("right", &s.border_right),
            border_xml("top", &s.border_top),
            border_xml("bottom", &s.border_bottom)
        );
        let border_id = intern(&mut borders, b);
        let fmt_id = (0..=49u32)
            .find(|id| crate::xlsx_read::builtin_numfmt(*id) == Some(s.num_fmt.as_str()))
            .unwrap_or_else(|| match numfmts.iter().find(|(_, c)| *c == s.num_fmt) {
                Some((id, _)) => *id,
                None => {
                    let id = 164 + numfmts.len() as u32;
                    numfmts.push((id, s.num_fmt.clone()));
                    id
                }
            });
        let mut xf = format!(
            r#"<xf numFmtId="{fmt_id}" fontId="{font_id}" fillId="{fill_id}" borderId="{border_id}" xfId="0""#
        );
        if fmt_id != 0 {
            xf.push_str(r#" applyNumberFormat="1""#);
        }
        if font_id != 0 {
            xf.push_str(r#" applyFont="1""#);
        }
        if fill_id != 0 {
            xf.push_str(r#" applyFill="1""#);
        }
        if border_id != 0 {
            xf.push_str(r#" applyBorder="1""#);
        }
        let mut al = String::new();
        let h = match s.h_align {
            HAlign::General => None,
            HAlign::Left => Some("left"),
            HAlign::Center => Some("center"),
            HAlign::Right => Some("right"),
            HAlign::Fill => Some("fill"),
            HAlign::Justify => Some("justify"),
            HAlign::CenterAcross => Some("centerContinuous"),
        };
        if let Some(h) = h {
            let _ = write!(al, r#" horizontal="{h}""#);
        }
        match s.v_align {
            VAlign::Top => al.push_str(r#" vertical="top""#),
            VAlign::Center => al.push_str(r#" vertical="center""#),
            VAlign::Bottom => {}
        }
        if s.wrap {
            al.push_str(r#" wrapText="1""#);
        }
        if s.indent > 0 {
            let _ = write!(al, r#" indent="{}""#, s.indent);
        }
        if s.rotation != 0 {
            let _ = write!(al, r#" textRotation="{}""#, s.rotation);
        }
        let prot = !s.locked || s.hidden_formula;
        let ext = crate::xlsx_drawings::checkbox_xf_ext(s);
        if al.is_empty() && !prot && ext.is_empty() {
            xf.push_str("/>");
        } else {
            xf.push_str(r#" applyAlignment="1">"#);
            if !al.is_empty() {
                let _ = write!(xf, "<alignment{al}/>");
            }
            if prot {
                let _ = write!(
                    xf,
                    r#"<protection locked="{}" hidden="{}"/>"#,
                    u8::from(s.locked),
                    u8::from(s.hidden_formula)
                );
            }
            xf.push_str(ext);
            xf.push_str("</xf>");
        }
        xf_index.push(xfs.len());
        xfs.push(xf);
    }
    let mut x = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">"#,
    );
    if !numfmts.is_empty() {
        let _ = write!(x, r#"<numFmts count="{}">"#, numfmts.len());
        for (id, code) in &numfmts {
            let _ = write!(x, r#"<numFmt numFmtId="{id}" formatCode="{}"/>"#, esc(code));
        }
        x.push_str("</numFmts>");
    }
    let _ = write!(
        x,
        r#"<fonts count="{}">{}</fonts>"#,
        fonts.len(),
        fonts.concat()
    );
    let _ = write!(
        x,
        r#"<fills count="{}">{}</fills>"#,
        fills.len(),
        fills.concat()
    );
    let _ = write!(
        x,
        r#"<borders count="{}">{}</borders>"#,
        borders.len(),
        borders.concat()
    );
    x.push_str(r#"<cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs>"#);
    let _ = write!(
        x,
        r#"<cellXfs count="{}">{}</cellXfs>"#,
        xfs.len(),
        xfs.concat()
    );
    x.push_str(r#"<cellStyles count="1"><cellStyle name="Normal" xfId="0" builtinId="0"/></cellStyles></styleSheet>"#);
    StylesXml { xml: x, xf_index }
}

/// Append the shortest round-tripping text of `n` (exponent form as `1E+20`).
fn push_num(x: &mut String, n: f64) {
    let start = x.len();
    let _ = write!(x, "{n}");
    if x.get(start..).is_some_and(|t| t.contains('e')) {
        x.truncate(start);
        let _ = write!(x, "{n:E}");
    }
}

/// Shared-string table being built while writing cells.
#[derive(Default)]
struct Sst {
    list: Vec<String>,
    idx: HashMap<String, usize>,
    count: usize,
}

/// Rows per block of parallel formula conversion in [`write_sheet_data`].
const FORMULA_BLOCK_ROWS: usize = 16_384;

/// How each formula cell of `rows` (row-major) is saved: `None` for a data-table cell,
/// which [`write_cell`] spells itself. Converted on all cores for large blocks.
fn file_formulas(sheet: &sp_model::Sheet, rows: &[u32]) -> Vec<Option<FileFormula>> {
    let cells: Vec<(sp_core::CellAddr, &str)> = rows
        .iter()
        .filter_map(|r| sheet.cells.get(r).map(|row| (*r, row)))
        .flat_map(|(r, row)| {
            row.iter().filter_map(move |(c, cell)| {
                Some((sp_core::CellAddr::new(r, *c), cell.formula_src()?))
            })
        })
        .collect();
    // Each worker converts a filled-down range once per relative form (`FormulaMemo`).
    let convert = |chunk: &[(sp_core::CellAddr, &str)]| -> Vec<Option<FileFormula>> {
        let mut memo = FormulaMemo::new();
        chunk
            .iter()
            .map(|(at, src)| {
                crate::xlsx_pivots::data_table_f(sheet, *at, src)
                    .is_none()
                    .then(|| formula_for_cell_memo(&mut memo, src, sheet, *at))
            })
            .collect()
    };
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get().min(16));
    if threads < 2 || cells.len() < 4_096 {
        return convert(&cells);
    }
    let per = cells.len().div_ceil(threads);
    let convert = &convert;
    std::thread::scope(|scope| {
        let handles: Vec<_> = cells
            .chunks(per)
            .map(|chunk| scope.spawn(move || convert(chunk)))
            .collect();
        let mut out = Vec::with_capacity(cells.len());
        for (h, chunk) in handles.into_iter().zip(cells.chunks(per)) {
            match h.join() {
                Ok(v) => out.extend(v),
                // A worker never panics on valid input; convert its chunk here if it did.
                Err(_) => out.extend(convert(chunk)),
            }
        }
        out
    })
}

/// Append `A1`-style address of (`row`, `col`) without allocating.
fn push_addr(x: &mut String, row: u32, col: u32) {
    let mut letters = [0u8; 4];
    let mut n = u64::from(col) + 1;
    let mut i = letters.len();
    while n > 0 && i > 0 {
        i -= 1;
        if let Some(b) = letters.get_mut(i) {
            *b = b'A' + ((n - 1) % 26) as u8;
        }
        n = (n - 1) / 26;
    }
    for b in letters.iter().skip(i) {
        x.push(char::from(*b));
    }
    let _ = write!(x, "{}", u64::from(row) + 1);
}

/// One `<c>` element. `ff` is the cell's converted formula (see [`file_formulas`]).
/// Returns whether it is a dynamic array formula.
#[allow(clippy::too_many_arguments)]
fn write_cell(
    x: &mut String,
    sheet: &sp_model::Sheet,
    styles: &StylesXml,
    sst: &mut Sst,
    r: u32,
    c: u32,
    cell: &sp_model::Cell,
    ff: Option<FileFormula>,
) -> bool {
    let at = sp_core::CellAddr::new(r, c);
    let s = styles
        .xf_index
        .get(cell.style.0 as usize)
        .copied()
        .unwrap_or(0);
    let mut dynamic = false;
    let f_xml = match &cell.content {
        Content::Formula(f) => match ff {
            Some(ff) => match ff.array {
                Some((rf, dy)) => {
                    dynamic = dy;
                    format!(r#"<f t="array" ref="{rf}">{}</f>"#, esc(&ff.text))
                }
                None => format!("<f>{}</f>", esc(&ff.text)),
            },
            None => crate::xlsx_pivots::data_table_f(sheet, at, f)
                .unwrap_or_else(|| format!("<f>{}</f>", esc(&file_formula(f)))),
        },
        Content::Value(_) => String::new(),
    };
    let is_formula = !f_xml.is_empty();
    let open = |x: &mut String, t: &str| {
        x.push_str(r#"<c r=""#);
        push_addr(x, r, c);
        x.push('"');
        if s != 0 {
            let _ = write!(x, r#" s="{s}""#);
        }
        if dynamic {
            x.push_str(r#" cm="1""#);
        }
        x.push_str(t);
    };
    match &cell.value {
        Value::Empty if !is_formula => {
            open(x, "");
            x.push_str("/>");
        }
        Value::Empty => {
            open(x, ">");
            x.push_str(&f_xml);
            x.push_str("</c>");
        }
        Value::Number(n) => {
            open(x, ">");
            x.push_str(&f_xml);
            x.push_str("<v>");
            push_num(x, *n);
            x.push_str("</v></c>");
        }
        Value::Bool(b) => {
            open(x, r#" t="b">"#);
            let _ = write!(x, "{f_xml}<v>{}</v></c>", u8::from(*b));
        }
        Value::Error(e) => {
            open(x, r#" t="e">"#);
            // Newer error kinds need rich-value metadata; a formula's cached result is saved
            // as the classic equivalent.
            let v = match e {
                sp_core::ErrorKind::Spill
                | sp_core::ErrorKind::Calc
                | sp_core::ErrorKind::Circ
                | sp_core::ErrorKind::GettingData
                    if is_formula =>
                {
                    "#VALUE!"
                }
                other => other.as_str(),
            };
            let _ = write!(x, "{f_xml}<v>{v}</v></c>");
        }
        Value::Text(t) if is_formula => {
            open(x, r#" t="str">"#);
            let _ = write!(x, "{f_xml}<v>{}</v></c>", esc(t));
        }
        Value::Text(t) => {
            let idx = match sst.idx.get(&**t) {
                Some(i) => *i,
                None => {
                    sst.list.push(t.to_string());
                    let i = sst.list.len() - 1;
                    sst.idx.insert(t.to_string(), i);
                    i
                }
            };
            sst.count += 1;
            open(x, r#" t="s">"#);
            let _ = write!(x, "<v>{idx}</v></c>");
        }
    }
    dynamic
}

/// `<sheetData>…</sheetData>` for one sheet; returns whether a dynamic array was written.
fn write_sheet_data(
    x: &mut String,
    sheet: &sp_model::Sheet,
    styles: &StylesXml,
    sst: &mut Sst,
) -> bool {
    let mut uses_dynamic = false;
    x.push_str("<sheetData>");
    let mut rows: Vec<u32> = sheet
        .cells
        .keys()
        .chain(sheet.row_heights.keys())
        .chain(sheet.hidden_rows.iter())
        .chain(sheet.outline.rows.keys())
        .copied()
        .collect();
    rows.sort_unstable();
    rows.dedup();
    // Formula text conversion (parse + file spelling) dominates large sheets: it runs in
    // parallel, one block of rows at a time (bounded memory), then cells are written in order.
    for block in rows.chunks(FORMULA_BLOCK_ROWS) {
        let formulas = file_formulas(sheet, block);
        let mut formulas = formulas.into_iter();
        for &r in block {
            let _ = write!(x, r#"<row r="{}""#, r + 1);
            if let Some(h) = sheet.row_heights.get(&r) {
                let _ = write!(x, r#" ht="{h}" customHeight="1""#);
            }
            if sheet.hidden_rows.contains(&r) {
                x.push_str(r#" hidden="1""#);
            }
            x.push_str(&crate::xlsx_extras::outline_attr(sheet, r, true));
            x.push('>');
            if let Some(cells) = sheet.cells.get(&r) {
                for (c, cell) in cells.iter() {
                    let ff = match &cell.content {
                        Content::Formula(_) => formulas.next().flatten(),
                        Content::Value(_) => None,
                    };
                    uses_dynamic |= write_cell(x, sheet, styles, sst, r, *c, cell, ff);
                }
            }
            x.push_str("</row>");
        }
    }
    x.push_str("</sheetData>");

    uses_dynamic
}

/// Generated head elements of a worksheet (before `sheetData`), as (name, XML).
fn sheet_head(wb: &Workbook, i: usize, sheet: &sp_model::Sheet) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let pr = crate::xlsx_extras::sheet_pr_xml(sheet);
    if !pr.is_empty() {
        out.push(("sheetPr".into(), pr));
    }
    if let Some(u) = sheet.used_range() {
        out.push((
            "dimension".into(),
            format!(r#"<dimension ref="{}"/>"#, u.to_a1()),
        ));
    }
    let mut x = format!(
        r#"<sheetViews><sheetView workbookViewId="0"{}{}>"#,
        if i == wb.active {
            r#" tabSelected="1""#
        } else {
            ""
        },
        if sheet.show_gridlines {
            ""
        } else {
            r#" showGridLines="0""#
        }
    );
    if let Some(f) = sheet.freeze.filter(|f| f.row > 0 || f.col > 0) {
        let mut p = String::from("<pane");
        if f.col > 0 {
            let _ = write!(p, r#" xSplit="{}""#, f.col);
        }
        if f.row > 0 {
            let _ = write!(p, r#" ySplit="{}""#, f.row);
        }
        let pane = match (f.row > 0, f.col > 0) {
            (true, true) => "bottomRight",
            (true, false) => "bottomLeft",
            _ => "topRight",
        };
        let _ = write!(
            p,
            r#" topLeftCell="{}" activePane="{pane}" state="frozen"/>"#,
            f.to_a1()
        );
        x.push_str(&p);
    }
    x.push_str("</sheetView></sheetViews>");
    out.push(("sheetViews".into(), x));
    out.push((
        "sheetFormatPr".into(),
        // The standard column width is kept only when it was changed (Default Width…).
        if (sheet.default_col_width - sp_model::DEFAULT_COL_WIDTH).abs() > 1e-9 {
            format!(
                r#"<sheetFormatPr defaultColWidth="{}" defaultRowHeight="{}"/>"#,
                sheet.default_col_width, sheet.default_row_height
            )
        } else {
            format!(
                r#"<sheetFormatPr defaultRowHeight="{}"/>"#,
                sheet.default_row_height
            )
        },
    ));
    let mut cols: Vec<u32> = sheet
        .col_widths
        .keys()
        .chain(sheet.hidden_cols.iter())
        .chain(sheet.outline.cols.keys())
        .copied()
        .collect();
    cols.sort_unstable();
    cols.dedup();
    if !cols.is_empty() {
        let mut x = String::from("<cols>");
        for c in cols {
            let w = sheet
                .col_widths
                .get(&c)
                .copied()
                .unwrap_or(sheet.default_col_width);
            let _ = write!(
                x,
                r#"<col min="{0}" max="{0}" width="{w}" customWidth="1"{1}{2}/>"#,
                c + 1,
                if sheet.hidden_cols.contains(&c) {
                    r#" hidden="1""#
                } else {
                    ""
                },
                crate::xlsx_extras::outline_attr(sheet, c, false)
            );
        }
        x.push_str("</cols>");
        out.push(("cols".into(), x));
    }
    out
}

/// Sheet ids: the original ones where still unique, fresh ones otherwise.
fn sheet_ids(wb: &Workbook) -> Vec<u32> {
    let mut used: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut ids: Vec<Option<u32>> = wb
        .sheets
        .iter()
        .map(|s| {
            s.passthrough
                .sheet_id
                .filter(|id| *id > 0 && used.insert(*id))
        })
        .collect();
    let mut next = 1u32;
    for id in &mut ids {
        if id.is_none() {
            while used.contains(&next) {
                next = next.saturating_add(1);
            }
            used.insert(next);
            *id = Some(next);
        }
    }
    ids.into_iter().map(|i| i.unwrap_or(1)).collect()
}

/// Sequential `rIdN` ids that skip ids taken by pass-through relationships.
struct IdAlloc<'a> {
    next: usize,
    reserved: &'a std::collections::HashSet<String>,
}

impl IdAlloc<'_> {
    fn take(&mut self) -> String {
        loop {
            self.next += 1;
            let id = format!("rId{}", self.next);
            if !self.reserved.contains(&id) {
                return id;
            }
        }
    }
}

/// Deflate level for saved parts (1 = fastest, 9 = smallest).
const SAVE_DEFLATE_LEVEL: i64 = 2;

pub fn write_xlsx<W: Write + Seek>(wb: &Workbook, w: W) -> Result<(), IoError> {
    let themed = crate::xlsx_theme::prepare_for_save(wb);
    let prepared = crate::xlsx_charts::prepare_for_save(&themed);
    let wb: &Workbook = &prepared;
    let mut zip = zip::ZipWriter::new(w);
    // Fast deflate: a 1M-row save takes ~2.5 s instead of ~4.4 s for a ~17% larger file
    // (chosen for speed, 2026-10-20). Any deflate level reads back in every app.
    let opts = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .compression_level(Some(SAVE_DEFLATE_LEVEL));
    let put = |zip: &mut zip::ZipWriter<W>, name: &str, data: &[u8]| -> Result<(), IoError> {
        zip.start_file(name, opts)
            .map_err(|e| IoError::Zip(e.to_string()))?;
        zip.write_all(data).map_err(|e| IoError::Zip(e.to_string()))
    };

    let mut sst = Sst::default();
    let styles = build_styles(wb);

    // Part names we generate, reserved before pass-through parts are placed.
    let mut reserved: Vec<String> = [
        "[Content_Types].xml",
        "_rels/.rels",
        "xl/workbook.xml",
        "xl/_rels/workbook.xml.rels",
        "xl/styles.xml",
        "xl/sharedStrings.xml",
        "docProps/core.xml",
        "docProps/app.xml",
        "xl/metadata.xml",
        crate::xlsx_drawings::FPB_PART,
        crate::xlsx_extras::PERSON_PART,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let mut generated: Vec<String> = Vec::new();
    for (i, sheet) in wb.sheets.iter().enumerate() {
        let n = i + 1;
        let names = [
            format!("xl/worksheets/sheet{n}.xml"),
            format!("xl/worksheets/_rels/sheet{n}.xml.rels"),
            format!("xl/comments{n}.xml"),
            format!("xl/drawings/vmlDrawing{n}.vml"),
            crate::xlsx_extras::threaded_part(n),
        ];
        if sheet.passthrough.sheet_part.is_none() {
            generated.push(names[0].clone());
        }
        reserved.extend(names);
    }
    generated.extend(
        ["xl/workbook.xml", "xl/styles.xml", "xl/sharedStrings.xml"]
            .iter()
            .map(|s| s.to_string()),
    );
    reserved.extend(crate::xlsx_cf::table_part_names(wb));
    let chart_plan = crate::xlsx_charts::ChartPlan::new(wb);
    reserved.extend(chart_plan.part_names());
    generated.extend(chart_plan.part_names());
    let pivot_plan = crate::xlsx_pivots::PivotPlan::new(wb);
    reserved.extend(pivot_plan.part_names());
    generated.extend(pivot_plan.part_names());
    let emit = pass::Emit::plan(wb, &reserved, &generated);

    let mut ct = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="vml" ContentType="application/vnd.openxmlformats-officedocument.vmlDrawing"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/><Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/><Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/><Override PartName="/docProps/app.xml" ContentType="application/vnd.openxmlformats-officedocument.extended-properties+xml"/>"#,
    );
    // A workbook carrying a VBA project must declare the macro-enabled main content type.
    if wb
        .passthrough
        .rels
        .iter()
        .any(|r| pass::rel_kind(&r.rel_type) == "vbaProject")
    {
        ct = ct.replace(
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml",
            "application/vnd.ms-excel.sheet.macroEnabled.main+xml",
        );
    }
    let override_ct = |ct: &mut String, path: &str, t: &str| {
        let _ = write!(
            ct,
            r#"<Override PartName="/{}" ContentType="{}"/>"#,
            esc(path),
            esc(t)
        );
    };

    // Workbook relationships: pass-through ones keep their ids (workbook.xml references
    // them); ours are allocated around them.
    let wb_pt = &wb.passthrough;
    let wb_reserved: std::collections::HashSet<String> =
        wb_pt.rels.iter().map(|r| r.id.clone()).collect();
    let mut wb_ids = IdAlloc {
        next: 0,
        reserved: &wb_reserved,
    };
    let mut wb_rels: Vec<RawRel> = Vec::new();
    let mut sheets_xml = String::new();
    let mut uses_dynamic = false;
    let mut cfw = crate::xlsx_cf::CfWriter::new();
    let ids = sheet_ids(wb);

    for (i, sheet) in wb.sheets.iter().enumerate() {
        let n = i + 1;
        let state = match sheet.visibility {
            Visibility::Visible => "",
            Visibility::Hidden => r#" state="hidden""#,
            Visibility::VeryHidden => r#" state="veryHidden""#,
        };
        let rid = wb_ids.take();
        let sheet_id = ids.get(i).copied().unwrap_or(1);
        let _ = write!(
            sheets_xml,
            r#"<sheet name="{}" sheetId="{sheet_id}"{state} r:id="{rid}"/>"#,
            esc(&sheet.name)
        );
        let pt = &sheet.passthrough;

        // A chart sheet (or other non-worksheet sheet) is written back verbatim.
        if let (Some(os), Some(path)) = (&pt.sheet_part, emit.opaque_paths.get(&i)) {
            wb_rels.push(RawRel {
                id: rid,
                rel_type: os.rel_type.clone(),
                target: format!("/{path}"),
                external: false,
            });
            override_ct(&mut ct, path, &os.part.content_type);
            put(&mut zip, path, &os.part.data)?;
            if !os.part.rels.is_empty() {
                let rels = emit.rels_for(Some(i), &os.part.rels);
                put(
                    &mut zip,
                    &pass::rels_path(path),
                    pass::rels_xml(&rels).as_bytes(),
                )?;
            }
            continue;
        }

        wb_rels.push(RawRel {
            id: rid,
            rel_type: format!("{}/worksheet", pass::REL_NS),
            target: format!("worksheets/sheet{n}.xml"),
            external: false,
        });
        override_ct(
            &mut ct,
            &format!("xl/worksheets/sheet{n}.xml"),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml",
        );

        // Pass-through relationships whose targets are written keep their ids.
        let pt_rels = emit.rels_for(Some(i), &pt.rels);
        let valid: std::collections::HashSet<String> =
            pt_rels.iter().map(|r| r.id.clone()).collect();
        let mut rels = crate::xlsx_extras::SheetRels::with_reserved(
            pt.rels.iter().map(|r| r.id.clone()).collect(),
        );
        let mut known = sheet_head(wb, i, sheet);
        known.extend(pass::split_known(&crate::xlsx_extras::before_merges_xml(
            sheet,
        )));
        if !sheet.merges.is_empty() {
            let mut m = format!(r#"<mergeCells count="{}">"#, sheet.merges.len());
            for r in &sheet.merges {
                let _ = write!(m, r#"<mergeCell ref="{}"/>"#, r.to_a1());
            }
            m.push_str("</mergeCells>");
            known.push(("mergeCells".into(), m));
        }
        known.extend(pass::split_known(&cfw.sheet_cf_xml(sheet)));
        known.extend(pass::split_known(&crate::xlsx_extras::after_merges_xml(
            sheet, n, &mut rels,
        )));
        known.extend(cfw.sheet_tail(sheet, &mut rels, pt));
        crate::xlsx_charts::merge_sparkline_ext(&mut known, sheet, pt);
        let drawn = crate::xlsx_charts::write_sheet_drawings(wb, i, &chart_plan, &mut rels);
        if let Some(el) = drawn.element {
            known.push(("drawing".to_string(), el));
        }
        if let Some(el) =
            crate::xlsx_drawings::background_element(sheet, &chart_plan.media, &mut rels)
        {
            known.push(("picture".to_string(), el));
        }
        for (part, body, t) in pivot_plan.write_sheet(i, &mut rels) {
            if !t.is_empty() {
                override_ct(&mut ct, &part, t);
            }
            put(&mut zip, &part, &body)?;
        }
        let (items, used_ids) =
            pass::merge_elements(pass::WORKSHEET_ORDER, &known, pt, &|_| false, &valid);
        let all_ids = pass::referenced_ids(pt);
        for r in &pt_rels {
            if used_ids.contains(&r.id) || !all_ids.contains(&r.id) {
                rels.push_raw(&r.id, &r.rel_type, &r.target, r.external);
            }
        }

        let mut x = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"{}>"#,
            pass::root_attrs_xml(pt)
        );
        let data_rank = 2 * pass::WORKSHEET_ORDER
            .iter()
            .position(|e| *e == "sheetData")
            .unwrap_or(5);
        for (_, el) in items.iter().filter(|(r, _)| *r < data_rank) {
            x.push_str(el);
        }
        uses_dynamic |= write_sheet_data(&mut x, sheet, &styles, &mut sst);
        for (_, el) in items.iter().filter(|(r, _)| *r >= data_rank) {
            x.push_str(el);
        }
        x.push_str("</worksheet>");
        for (part, body, t) in &drawn.parts {
            if !t.is_empty() {
                override_ct(&mut ct, part, t);
            }
            put(&mut zip, part, body.as_bytes())?;
        }
        put(
            &mut zip,
            &format!("xl/worksheets/sheet{n}.xml"),
            x.as_bytes(),
        )?;
        if !sheet.comments.is_empty() {
            override_ct(
                &mut ct,
                &format!("xl/comments{n}.xml"),
                "application/vnd.openxmlformats-officedocument.spreadsheetml.comments+xml",
            );
            put(
                &mut zip,
                &format!("xl/comments{n}.xml"),
                crate::xlsx_extras::comments_xml(sheet, n).as_bytes(),
            )?;
            if crate::xlsx_extras::has_threads(sheet) {
                let part = crate::xlsx_extras::threaded_part(n);
                override_ct(&mut ct, &part, crate::xlsx_extras::THREADED_CT);
                put(
                    &mut zip,
                    &part,
                    crate::xlsx_extras::threaded_comments_xml(sheet, n).as_bytes(),
                )?;
            }
            put(
                &mut zip,
                &format!("xl/drawings/vmlDrawing{n}.vml"),
                crate::xlsx_extras::vml_xml(sheet, n).as_bytes(),
            )?;
        }
        if !rels.is_empty() {
            put(
                &mut zip,
                &format!("xl/worksheets/_rels/sheet{n}.xml.rels"),
                rels.xml().as_bytes(),
            )?;
        }
    }
    for (rel_type, target) in [
        ("styles", "styles.xml"),
        ("sharedStrings", "sharedStrings.xml"),
    ] {
        wb_rels.push(RawRel {
            id: wb_ids.take(),
            rel_type: format!("{}/{rel_type}", pass::REL_NS),
            target: target.into(),
            external: false,
        });
    }
    let people = crate::xlsx_extras::thread_authors(wb);
    if !people.is_empty() {
        let part = crate::xlsx_extras::PERSON_PART;
        wb_rels.push(RawRel {
            id: wb_ids.take(),
            rel_type: crate::xlsx_extras::PERSON_REL.to_string(),
            target: part.trim_start_matches("xl/").into(),
            external: false,
        });
        override_ct(&mut ct, part, crate::xlsx_extras::PERSON_CT);
        put(
            &mut zip,
            part,
            crate::xlsx_extras::persons_xml(&people).as_bytes(),
        )?;
    }
    let styles_out = cfw.styles_xml(&styles.xml);
    for (path, data) in cfw.finish() {
        override_ct(&mut ct, &path, pass::TABLE_CONTENT_TYPE);
        put(&mut zip, &path, data.as_bytes())?;
    }
    if uses_dynamic {
        wb_rels.push(RawRel {
            id: wb_ids.take(),
            rel_type: format!("{}/sheetMetadata", pass::REL_NS),
            target: "metadata.xml".into(),
            external: false,
        });
        ct.push_str(r#"<Override PartName="/xl/metadata.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheetMetadata+xml"/>"#);
        put(
            &mut zip,
            "xl/metadata.xml",
            DYNAMIC_ARRAY_METADATA.as_bytes(),
        )?;
    }

    // Images of pictures and sheet backgrounds; the checkbox feature property bag.
    for (path, data, t) in chart_plan.media.files() {
        override_ct(&mut ct, path, t);
        put(&mut zip, path, data)?;
    }
    if crate::xlsx_drawings::uses_checkboxes(wb) {
        wb_rels.push(RawRel {
            id: wb_ids.take(),
            rel_type: crate::xlsx_drawings::REL_FPB.to_string(),
            target: crate::xlsx_drawings::FPB_PART
                .trim_start_matches("xl/")
                .to_string(),
            external: false,
        });
        override_ct(
            &mut ct,
            crate::xlsx_drawings::FPB_PART,
            crate::xlsx_drawings::CT_FPB,
        );
        put(
            &mut zip,
            crate::xlsx_drawings::FPB_PART,
            crate::xlsx_drawings::checkbox_bag_xml().as_bytes(),
        )?;
    }

    // Unmodelled parts, verbatim.
    for (path, data, t) in emit.files() {
        if !t.is_empty() {
            override_ct(&mut ct, &path, &t);
        }
        put(&mut zip, &path, &data)?;
    }
    ct.push_str("</Types>");

    let mut names = String::new();
    if !wb.names.is_empty() {
        names.push_str("<definedNames>");
        for n in &wb.names {
            let scope = n
                .scope
                .map(|s| format!(r#" localSheetId="{s}""#))
                .unwrap_or_default();
            let _ = write!(
                names,
                r#"<definedName name="{}"{scope}>{}</definedName>"#,
                esc(&n.name),
                esc(&n.formula)
            );
        }
        names.push_str("</definedNames>");
    }
    let calc = if wb.calc_mode == sp_model::CalcMode::Manual {
        r#"<calcPr calcMode="manual"/>"#
    } else {
        r#"<calcPr calcId="191029"/>"#
    };
    let mut known: Vec<(String, String)> = Vec::new();
    let prot = crate::xlsx_extras::workbook_protection_xml(wb);
    if !prot.is_empty() {
        known.push(("workbookProtection".into(), prot));
    }
    known.push((
        "bookViews".into(),
        format!(
            r#"<bookViews><workbookView activeTab="{}"/></bookViews>"#,
            wb.active
        ),
    ));
    known.push(("sheets".into(), format!("<sheets>{sheets_xml}</sheets>")));
    if !names.is_empty() {
        known.push(("definedNames".into(), names));
    }
    known.push(("calcPr".into(), calc.to_string()));
    let pt_rels = emit.rels_for(None, &wb_pt.rels);
    let valid: std::collections::HashSet<String> = pt_rels.iter().map(|r| r.id.clone()).collect();
    let mut keep_ids = std::collections::HashSet::new();
    if let Some((el, keep)) =
        pivot_plan.workbook_element(&valid, &mut || wb_ids.take(), &mut wb_rels)
    {
        known.push(("pivotCaches".into(), el));
        keep_ids = keep;
    }
    let (items, used_ids) =
        pass::merge_elements(pass::WORKBOOK_ORDER, &known, wb_pt, &|_| false, &valid);
    let all_ids = pass::referenced_ids(wb_pt);
    for r in pt_rels {
        if used_ids.contains(&r.id) || keep_ids.contains(&r.id) || !all_ids.contains(&r.id) {
            wb_rels.push(r);
        }
    }
    let mut workbook = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"{}>"#,
        pass::root_attrs_xml(wb_pt)
    );
    for (_, el) in &items {
        workbook.push_str(el);
    }
    workbook.push_str("</workbook>");

    let mut sst_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="{}" uniqueCount="{}">"#,
        sst.count,
        sst.list.len()
    );
    for s in &sst.list {
        let sp = if s.starts_with(' ') || s.ends_with(' ') || s.contains('\n') {
            r#" xml:space="preserve""#
        } else {
            ""
        };
        let _ = write!(sst_xml, "<si><t{sp}>{}</t></si>", esc(s));
    }
    sst_xml.push_str("</sst>");
    let core = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><dc:title>{}</dc:title><dc:creator>{}</dc:creator></cp:coreProperties>"#,
        esc(&wb.props.title),
        esc(&wb.props.author)
    );
    let app = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties"><Application>SheetPro</Application></Properties>"#;
    let mut root_rels: Vec<RawRel> = [
        (
            "officeDocument",
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument",
            "xl/workbook.xml",
        ),
        (
            "core",
            "http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties",
            "docProps/core.xml",
        ),
        (
            "app",
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties",
            "docProps/app.xml",
        ),
    ]
    .iter()
    .enumerate()
    .map(|(k, (_, t, target))| RawRel {
        id: format!("rId{}", k + 1),
        rel_type: t.to_string(),
        target: target.to_string(),
        external: false,
    })
    .collect();
    for r in emit.rels_for(None, &wb_pt.package_rels) {
        let id = format!("rId{}", root_rels.len() + 1);
        root_rels.push(RawRel { id, ..r });
    }

    put(&mut zip, "[Content_Types].xml", ct.as_bytes())?;
    put(
        &mut zip,
        "_rels/.rels",
        pass::rels_xml(&root_rels).as_bytes(),
    )?;
    put(&mut zip, "xl/workbook.xml", workbook.as_bytes())?;
    put(
        &mut zip,
        "xl/_rels/workbook.xml.rels",
        pass::rels_xml(&wb_rels).as_bytes(),
    )?;
    put(&mut zip, "xl/styles.xml", styles_out.as_bytes())?;
    put(&mut zip, "xl/sharedStrings.xml", sst_xml.as_bytes())?;
    put(&mut zip, "docProps/core.xml", core.as_bytes())?;
    put(&mut zip, "docProps/app.xml", app.as_bytes())?;
    zip.finish().map_err(|e| IoError::Zip(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A changed standard column width (Default Width…) is saved and read back; before, it
    /// was dropped on save.
    #[test]
    fn standard_column_width_round_trips() {
        let mut wb = Workbook::new();
        wb.sheets[0].default_col_width = 12.0;
        let mut buf = std::io::Cursor::new(Vec::new());
        write_xlsx(&wb, &mut buf).unwrap();
        let (back, _) =
            crate::xlsx_read::read_xlsx(std::io::Cursor::new(buf.into_inner())).unwrap();
        assert_eq!(back.sheets[0].default_col_width, 12.0);
        // The default stays implicit.
        let mut buf = std::io::Cursor::new(Vec::new());
        write_xlsx(&Workbook::new(), &mut buf).unwrap();
        let (back, _) =
            crate::xlsx_read::read_xlsx(std::io::Cursor::new(buf.into_inner())).unwrap();
        assert_eq!(
            back.sheets[0].default_col_width,
            sp_model::DEFAULT_COL_WIDTH
        );
    }

    /// Conversion through the memo gives what converting each formula on its own gives,
    /// including dynamic arrays and array areas.
    #[test]
    fn memoized_file_formulas_match_per_formula() {
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
        let mut sheet = sp_model::Sheet::new("Sheet1");
        for r in 1..=40u32 {
            sheet.set(
                sp_core::CellAddr::new(r - 1, 0),
                sp_model::Cell::constant(Value::Number(f64::from(r))),
            );
            for (c, f) in fills.iter().enumerate() {
                sheet.set(
                    sp_core::CellAddr::new(r - 1, c as u32 + 2),
                    sp_model::Cell::formula(f(r)),
                );
            }
        }
        sheet.spills.push(sp_model::SpillArea {
            range: sp_core::CellRange::parse("H5:H7").unwrap(),
            fixed: false,
            blocked: false,
        });
        let mut memo = FormulaMemo::new();
        for (at, c) in sheet.iter() {
            let Some(src) = c.formula_src() else { continue };
            let a = formula_for_cell_memo(&mut memo, src, &sheet, at);
            let b = formula_for_cell(src, &sheet, at);
            assert_eq!((a.text, a.array), (b.text, b.array), "{src}");
        }
    }

    #[test]
    fn addresses_without_allocating() {
        for (row, col, want) in [
            (0, 0, "A1"),
            (9, 25, "Z10"),
            (99, 26, "AA100"),
            (1_048_575, 16_383, "XFD1048576"),
        ] {
            let mut x = String::new();
            push_addr(&mut x, row, col);
            assert_eq!(x, want);
        }
        let mut x = String::new();
        push_num(&mut x, 0.1);
        push_num(&mut x, -3.0);
        assert_eq!(x, "0.1-3");
    }

    /// Enough formulas for the parallel conversion path; every kind of cached value and
    /// formula spelling must come back exactly.
    #[test]
    fn large_sheet_round_trips_through_parallel_formula_conversion() {
        use sp_core::{CellAddr, ErrorKind};
        use sp_model::Cell;
        let mut wb = Workbook::new();
        let s = &mut wb.sheets[0];
        let n = 6_000u32;
        for r in 0..n {
            let row = r + 1;
            s.set(
                CellAddr::new(r, 0),
                Cell::constant(Value::Number(f64::from(r) + 0.5)),
            );
            s.set(
                CellAddr::new(r, 1),
                Cell::constant(Value::text(format!("t{}", r % 7))),
            );
            let mut f = Cell::formula(format!("A{row}*2"));
            f.value = Value::Number(f64::from(r) * 2.0 + 1.0);
            s.set(CellAddr::new(r, 2), f);
            let mut f = Cell::formula(format!("IF(A{row}>10,\"big\",\"small\")"));
            f.value = Value::text(if r > 9 { "big" } else { "small" });
            s.set(CellAddr::new(r, 3), f);
            let mut f = Cell::formula(format!("CONCAT(B{row},\"x\")"));
            f.value = Value::Bool(r % 2 == 0);
            s.set(CellAddr::new(r, 4), f);
            let mut f = Cell::formula(format!("1/(A{row}-A{row})"));
            f.value = Value::Error(ErrorKind::Div0);
            s.set(CellAddr::new(r, 5), f);
        }
        let mut buf = std::io::Cursor::new(Vec::new());
        write_xlsx(&wb, &mut buf).unwrap();
        buf.set_position(0);
        let (back, _) = crate::xlsx_read::read_xlsx(buf).unwrap();
        let (a, b) = (&wb.sheets[0], &back.sheets[0]);
        assert_eq!(a.cell_count(), b.cell_count());
        for (addr, c) in a.iter() {
            let d = b.get(addr).unwrap();
            assert_eq!(c.content, d.content, "{}", addr.to_a1());
            assert_eq!(c.value, d.value, "{}", addr.to_a1());
        }
    }

    #[test]
    fn dynamic_arrays_round_trip() {
        use sp_core::CellAddr;
        use sp_model::Cell;
        let mut wb = Workbook::new();
        let s = &mut wb.sheets[0];
        let at = |a: &str| CellAddr::parse(a).unwrap();
        s.set(at("A1"), Cell::formula("SEQUENCE(3)"));
        s.set(at("B2"), Cell::formula("@A1:A3*2"));
        s.set(at("C1"), Cell::formula("SUM(A1#)"));
        s.set(at("D1"), Cell::formula("LET(x,2,LAMBDA(y,x*y)(3))"));
        s.set(at("E1"), Cell::formula("SEQUENCE(2)"));
        s.set(at("E2"), Cell::constant(Value::Number(1.0)));
        sp_formula::recalc(
            &mut wb,
            &mut sp_formula::AstCache::default(),
            Default::default(),
        );
        assert_eq!(wb.sheets[0].value(at("B2")), &Value::Number(4.0));
        let mut buf = std::io::Cursor::new(Vec::new());
        write_xlsx(&wb, &mut buf).unwrap();
        buf.set_position(0);
        let mut zip = zip::ZipArchive::new(buf.clone()).unwrap();
        let mut x = String::new();
        std::io::Read::read_to_string(
            &mut zip.by_name("xl/worksheets/sheet1.xml").unwrap(),
            &mut x,
        )
        .unwrap();
        assert!(
            x.contains(r#"<c r="A1" cm="1"><f t="array" ref="A1:A3">_xlfn.SEQUENCE(3)</f>"#),
            "{x}"
        );
        assert!(x.contains("<f>A1:A3*2</f>"), "{x}");
        assert!(x.contains("_xlfn.ANCHORARRAY(A1)"), "{x}");
        assert!(x.contains("_xlpm.x"), "{x}");
        assert!(zip.by_name("xl/metadata.xml").is_ok());
        let (back, _) = crate::xlsx_read::read_xlsx(buf).unwrap();
        let b = &back.sheets[0];
        assert_eq!(b.get(at("B2")).unwrap().formula_src(), Some("@A1:A3*2"));
        assert_eq!(
            b.get(at("D1")).unwrap().formula_src(),
            Some("LET(x,2,LAMBDA(y,x*y)(3))")
        );
        assert!(b.get(at("A2")).unwrap().is_spilled());
        assert_eq!(b.spills.first(), wb.sheets[0].spills.first());
        // A blocked spill's #SPILL! is saved as the classic #VALUE! (recalc restores it).
        assert_eq!(b.value(at("E1")), &Value::Error(sp_core::ErrorKind::Value));
    }

    #[test]
    fn prefixes() {
        assert_eq!(
            file_formula("CONCAT(A1,STDEV.S(B1:B2))+SUM(1)"),
            "_xlfn.CONCAT(A1,_xlfn.STDEV.S(B1:B2))+SUM(1)"
        );
        assert_eq!(
            file_formula("FILTER(A1:A3,B1:B3)"),
            "_xlfn._xlws.FILTER(A1:A3,B1:B3)"
        );
        assert_eq!(file_formula("ERROR.TYPE(A1)"), "ERROR.TYPE(A1)");
    }
}
