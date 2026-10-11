//! .xlsx writer: a minimal, valid SpreadsheetML package.

use crate::IoError;
use crate::xml::esc;
use sp_core::{Value, col_to_letters};
use sp_formula::{Expr, parse, to_formula};
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
        Ok(mut e) => {
            e.walk_mut(&mut |n| {
                if let Expr::Call(name, _) = n
                    && let Some(p) = needs_prefix(name)
                {
                    *name = format!("{p}{name}");
                }
            });
            to_formula(&e)
        }
        // Keep unparseable text untouched so nothing is lost.
        Err(_) => src.to_string(),
    }
}

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
        if let Some(c) = s.font_color {
            let _ = write!(f, r#"<color rgb="{}"/>"#, c.to_argb_hex());
        }
        let _ = write!(
            f,
            r#"<name val="{}"/><family val="2"/></font>"#,
            esc(&s.font_name)
        );
        let font_id = intern(&mut fonts, f);
        let fill_id = match s.fill {
            Some(c) => intern(
                &mut fills,
                format!(
                    r#"<fill><patternFill patternType="solid"><fgColor rgb="{}"/><bgColor indexed="64"/></patternFill></fill>"#,
                    c.to_argb_hex()
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
        if al.is_empty() && !prot {
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

fn fmt_num(n: f64) -> String {
    // Shortest representation that round-trips.
    let s = format!("{n}");
    if s.contains('e') { format!("{n:E}") } else { s }
}

pub fn write_xlsx<W: Write + Seek>(wb: &Workbook, w: W) -> Result<(), IoError> {
    let mut zip = zip::ZipWriter::new(w);
    let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let put = |zip: &mut zip::ZipWriter<W>, name: &str, data: &str| -> Result<(), IoError> {
        zip.start_file(name, opts)
            .map_err(|e| IoError::Zip(e.to_string()))?;
        zip.write_all(data.as_bytes())
            .map_err(|e| IoError::Zip(e.to_string()))
    };

    let mut sst: Vec<String> = Vec::new();
    let mut sst_idx: HashMap<String, usize> = HashMap::new();
    let mut sst_count = 0usize;
    let styles = build_styles(wb);

    let mut ct = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="vml" ContentType="application/vnd.openxmlformats-officedocument.vmlDrawing"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/><Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/><Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/><Override PartName="/docProps/app.xml" ContentType="application/vnd.openxmlformats-officedocument.extended-properties+xml"/>"#,
    );
    let mut wb_rels = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
    );
    let mut sheets_xml = String::new();

    for (i, sheet) in wb.sheets.iter().enumerate() {
        let n = i + 1;
        let _ = write!(
            ct,
            r#"<Override PartName="/xl/worksheets/sheet{n}.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>"#
        );
        let _ = write!(
            wb_rels,
            r#"<Relationship Id="rId{n}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet{n}.xml"/>"#
        );
        let state = match sheet.visibility {
            Visibility::Visible => "",
            Visibility::Hidden => r#" state="hidden""#,
            Visibility::VeryHidden => r#" state="veryHidden""#,
        };
        let _ = write!(
            sheets_xml,
            r#"<sheet name="{}" sheetId="{n}"{state} r:id="rId{n}"/>"#,
            esc(&sheet.name)
        );

        let mut x = String::from(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">"#,
        );
        x.push_str(&crate::xlsx_extras::sheet_pr_xml(sheet));
        if let Some(u) = sheet.used_range() {
            let _ = write!(x, r#"<dimension ref="{}"/>"#, u.to_a1());
        }
        let _ = write!(
            x,
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
        let _ = write!(
            x,
            r#"<sheetFormatPr defaultRowHeight="{}"/>"#,
            sheet.default_row_height
        );
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
            x.push_str("<cols>");
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
        }
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
        for r in rows {
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
                for (c, cell) in cells {
                    let addr = format!("{}{}", col_to_letters(*c), r + 1);
                    let s = styles
                        .xf_index
                        .get(cell.style.0 as usize)
                        .copied()
                        .unwrap_or(0);
                    let s_attr = if s == 0 {
                        String::new()
                    } else {
                        format!(r#" s="{s}""#)
                    };
                    let f_xml = match &cell.content {
                        Content::Formula(f) => format!("<f>{}</f>", esc(&file_formula(f))),
                        Content::Value(_) => String::new(),
                    };
                    let is_formula = !f_xml.is_empty();
                    match &cell.value {
                        Value::Empty if !is_formula => {
                            let _ = write!(x, r#"<c r="{addr}"{s_attr}/>"#);
                        }
                        Value::Empty => {
                            let _ = write!(x, r#"<c r="{addr}"{s_attr}>{f_xml}</c>"#);
                        }
                        Value::Number(n) => {
                            let _ = write!(
                                x,
                                r#"<c r="{addr}"{s_attr}>{f_xml}<v>{}</v></c>"#,
                                fmt_num(*n)
                            );
                        }
                        Value::Bool(b) => {
                            let _ = write!(
                                x,
                                r#"<c r="{addr}"{s_attr} t="b">{f_xml}<v>{}</v></c>"#,
                                u8::from(*b)
                            );
                        }
                        Value::Error(e) => {
                            let _ = write!(
                                x,
                                r#"<c r="{addr}"{s_attr} t="e">{f_xml}<v>{}</v></c>"#,
                                e.as_str()
                            );
                        }
                        Value::Text(t) if is_formula => {
                            let _ = write!(
                                x,
                                r#"<c r="{addr}"{s_attr} t="str">{f_xml}<v>{}</v></c>"#,
                                esc(t)
                            );
                        }
                        Value::Text(t) => {
                            let idx = *sst_idx.entry(t.to_string()).or_insert_with(|| {
                                sst.push(t.to_string());
                                sst.len() - 1
                            });
                            sst_count += 1;
                            let _ = write!(x, r#"<c r="{addr}"{s_attr} t="s"><v>{idx}</v></c>"#);
                        }
                    }
                }
            }
            x.push_str("</row>");
        }
        x.push_str("</sheetData>");
        x.push_str(&crate::xlsx_extras::before_merges_xml(sheet));
        if !sheet.merges.is_empty() {
            let _ = write!(x, r#"<mergeCells count="{}">"#, sheet.merges.len());
            for m in &sheet.merges {
                let _ = write!(x, r#"<mergeCell ref="{}"/>"#, m.to_a1());
            }
            x.push_str("</mergeCells>");
        }
        let mut rels = crate::xlsx_extras::SheetRels::default();
        x.push_str(&crate::xlsx_extras::after_merges_xml(sheet, n, &mut rels));
        x.push_str("</worksheet>");
        put(&mut zip, &format!("xl/worksheets/sheet{n}.xml"), &x)?;
        if !sheet.comments.is_empty() {
            let _ = write!(
                ct,
                r#"<Override PartName="/xl/comments{n}.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.comments+xml"/>"#
            );
            put(&mut zip, &format!("xl/comments{n}.xml"), &crate::xlsx_extras::comments_xml(sheet))?;
            put(
                &mut zip,
                &format!("xl/drawings/vmlDrawing{n}.vml"),
                &crate::xlsx_extras::vml_xml(sheet, n),
            )?;
        }
        if !rels.is_empty() {
            put(&mut zip, &format!("xl/worksheets/_rels/sheet{n}.xml.rels"), &rels.xml())?;
        }
    }
    let ns = wb.sheets.len();
    let _ = write!(
        wb_rels,
        r#"<Relationship Id="rId{}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>"#,
        ns + 1
    );
    let _ = write!(
        wb_rels,
        r#"<Relationship Id="rId{}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/>"#,
        ns + 2
    );
    wb_rels.push_str("</Relationships>");
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
    let workbook = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">{}<bookViews><workbookView activeTab="{}"/></bookViews><sheets>{sheets_xml}</sheets>{names}{calc}</workbook>"#,
        crate::xlsx_extras::workbook_protection_xml(wb),
        wb.active
    );
    let mut sst_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="{sst_count}" uniqueCount="{}">"#,
        sst.len()
    );
    for s in &sst {
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
    let root_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties" Target="docProps/app.xml"/></Relationships>"#;

    put(&mut zip, "[Content_Types].xml", &ct)?;
    put(&mut zip, "_rels/.rels", root_rels)?;
    put(&mut zip, "xl/workbook.xml", &workbook)?;
    put(&mut zip, "xl/_rels/workbook.xml.rels", &wb_rels)?;
    put(&mut zip, "xl/styles.xml", &styles.xml)?;
    put(&mut zip, "xl/sharedStrings.xml", &sst_xml)?;
    put(&mut zip, "docProps/core.xml", &core)?;
    put(&mut zip, "docProps/app.xml", app)?;
    zip.finish().map_err(|e| IoError::Zip(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
