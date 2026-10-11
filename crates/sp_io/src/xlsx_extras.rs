//! .xlsx parts beyond cells: autoFilter, sheet/workbook protection, protected ranges, data
//! validations, hyperlinks, legacy comments (notes) with their VML, page margins/setup,
//! print options, manual page breaks and outline settings. Reader and writer helpers with
//! small call sites in `xlsx_read.rs` / `xlsx_write.rs`.

use crate::IoError;
use crate::xml::{Node, esc, parse_dom};
use quick_xml::Reader;
use quick_xml::events::Event;
use sp_core::{CellAddr, CellRange};
use sp_model::{
    AllowEditRange, AutoFilter, Comment, CommentKind, DataValidation, ErrorStyle, FilterColumn,
    FilterCond, FilterCriteria, Hyperlink, Orientation, PasswordHash, Sheet, SheetProtection,
    ValidationKind, ValidationOp, Workbook, WorkbookProtection,
};
use std::collections::HashMap;
use std::fmt::Write as _;

const REL_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

fn flag(v: Option<&str>) -> Option<bool> {
    v.map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

fn b01(b: bool) -> u8 {
    u8::from(b)
}

fn sqref(text: &str) -> Vec<CellRange> {
    text.split_whitespace()
        .filter_map(|r| CellRange::parse(&r.replace('$', "")).ok())
        .take(10_000)
        .collect()
}

fn sqref_text(rs: &[CellRange]) -> String {
    rs.iter()
        .map(CellRange::to_a1)
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------------------------------
// Passwords

fn read_password(n: &Node, prefix: &str) -> Option<PasswordHash> {
    let k = |s: &str| {
        if prefix.is_empty() {
            s.to_string()
        } else {
            // workbookAlgorithmName, workbookHashValue…
            let mut c = s.chars();
            c.next()
                .map(|f| format!("{prefix}{}{}", f.to_ascii_uppercase(), c.as_str()))
                .unwrap_or_default()
        }
    };
    if let (Some(alg), Some(hash)) = (n.attr(&k("algorithmName")), n.attr(&k("hashValue"))) {
        return Some(PasswordHash::Modern {
            algorithm: alg.to_string(),
            hash: hash.to_string(),
            salt: n.attr(&k("saltValue")).unwrap_or("").to_string(),
            spin: n
                .attr(&k("spinCount"))
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
        });
    }
    let legacy = if prefix.is_empty() {
        n.attr("password")
    } else {
        n.attr(&format!("{prefix}Password"))
    };
    legacy
        .filter(|p| !p.is_empty())
        .map(|p| PasswordHash::Legacy(p.to_string()))
}

fn write_password(out: &mut String, p: Option<&PasswordHash>, prefix: &str) {
    let key = |s: &str| {
        if prefix.is_empty() {
            s.to_string()
        } else {
            let mut c = s.chars();
            c.next()
                .map(|f| format!("{prefix}{}{}", f.to_ascii_uppercase(), c.as_str()))
                .unwrap_or_default()
        }
    };
    match p {
        None => {}
        Some(PasswordHash::Legacy(h)) => {
            let _ = write!(out, r#" {}="{}""#, key("password"), esc(h));
        }
        Some(PasswordHash::Modern {
            algorithm,
            hash,
            salt,
            spin,
        }) => {
            let _ = write!(
                out,
                r#" {}="{}" {}="{}" {}="{}" {}="{spin}""#,
                key("algorithmName"),
                esc(algorithm),
                key("hashValue"),
                esc(hash),
                key("saltValue"),
                esc(salt),
                key("spinCount"),
            );
        }
    }
}

// ---------------------------------------------------------------------------------------
// Writer

/// `<sheetPr>` with tab colour, outline and fit-to-page settings (empty when default).
pub fn sheet_pr_xml(sheet: &Sheet) -> String {
    let mut inner = String::new();
    if let Some(t) = sheet.tab_color {
        let _ = write!(inner, r#"<tabColor rgb="{}"/>"#, t.to_argb_hex());
    }
    if !sheet.outline.summary_below || !sheet.outline.summary_right {
        let _ = write!(
            inner,
            r#"<outlinePr summaryBelow="{}" summaryRight="{}"/>"#,
            b01(sheet.outline.summary_below),
            b01(sheet.outline.summary_right)
        );
    }
    let ps = &sheet.page_setup;
    if ps.fit_width.is_some() || ps.fit_height.is_some() {
        inner.push_str(r#"<pageSetUpPr fitToPage="1"/>"#);
    }
    if inner.is_empty() {
        String::new()
    } else {
        format!("<sheetPr>{inner}</sheetPr>")
    }
}

/// `outlineLevel` attribute for a row/col (empty when 0).
pub fn outline_attr(sheet: &Sheet, index: u32, rows: bool) -> String {
    let lv = if rows {
        sheet.outline.rows.get(&index)
    } else {
        sheet.outline.cols.get(&index)
    };
    match lv {
        Some(l) if *l > 0 => format!(r#" outlineLevel="{l}""#),
        _ => String::new(),
    }
}

/// Elements between `</sheetData>` and `<mergeCells>`: protection, ranges, autoFilter.
pub fn before_merges_xml(sheet: &Sheet) -> String {
    let mut x = String::new();
    if let Some(p) = &sheet.protection {
        x.push_str("<sheetProtection");
        write_password(&mut x, p.password.as_ref(), "");
        let _ = write!(
            x,
            r#" sheet="1" objects="{}" scenarios="{}" formatCells="{}" formatColumns="{}" formatRows="{}" insertColumns="{}" insertRows="{}" insertHyperlinks="{}" deleteColumns="{}" deleteRows="{}" selectLockedCells="{}" sort="{}" autoFilter="{}" pivotTables="{}" selectUnlockedCells="{}"/>"#,
            b01(!p.objects),
            b01(!p.scenarios),
            b01(!p.format_cells),
            b01(!p.format_columns),
            b01(!p.format_rows),
            b01(!p.insert_columns),
            b01(!p.insert_rows),
            b01(!p.insert_hyperlinks),
            b01(!p.delete_columns),
            b01(!p.delete_rows),
            b01(!p.select_locked),
            b01(!p.sort),
            b01(!p.auto_filter),
            b01(!p.pivot_tables),
            b01(!p.select_unlocked),
        );
    }
    if !sheet.allow_edit_ranges.is_empty() {
        x.push_str("<protectedRanges>");
        for r in &sheet.allow_edit_ranges {
            let _ = write!(x, r#"<protectedRange name="{}""#, esc(&r.title));
            write_password(&mut x, r.password.as_ref(), "");
            let _ = write!(x, r#" sqref="{}"/>"#, sqref_text(&r.ranges));
        }
        x.push_str("</protectedRanges>");
    }
    if let Some(af) = &sheet.auto_filter {
        let _ = write!(x, r#"<autoFilter ref="{}""#, af.range.to_a1());
        if af.columns.is_empty() {
            x.push_str("/>");
        } else {
            x.push('>');
            for fc in &af.columns {
                let _ = write!(x, r#"<filterColumn colId="{}">"#, fc.col);
                match &fc.criteria {
                    FilterCriteria::Values { values, blanks } => {
                        x.push_str(if *blanks {
                            r#"<filters blank="1">"#
                        } else {
                            "<filters>"
                        });
                        for v in values {
                            let _ = write!(x, r#"<filter val="{}"/>"#, esc(v));
                        }
                        x.push_str("</filters>");
                    }
                    FilterCriteria::Custom { and, conds } => {
                        x.push_str(if *and {
                            r#"<customFilters and="1">"#
                        } else {
                            "<customFilters>"
                        });
                        for c in conds {
                            if c.op == "equal" {
                                let _ = write!(x, r#"<customFilter val="{}"/>"#, esc(&c.val));
                            } else {
                                let _ = write!(
                                    x,
                                    r#"<customFilter operator="{}" val="{}"/>"#,
                                    esc(&c.op),
                                    esc(&c.val)
                                );
                            }
                        }
                        x.push_str("</customFilters>");
                    }
                    FilterCriteria::Top10 { top, percent, val } => {
                        let _ = write!(
                            x,
                            r#"<top10 top="{}" percent="{}" val="{val}"/>"#,
                            b01(*top),
                            b01(*percent)
                        );
                    }
                }
                x.push_str("</filterColumn>");
            }
            x.push_str("</autoFilter>");
        }
    }
    x
}

/// Relationships a worksheet needs (hyperlink targets, comments, VML, pass-through).
#[derive(Default)]
pub struct SheetRels {
    /// (id, full type URI, target, external).
    pub rels: Vec<(String, String, String, bool)>,
    /// Ids taken by pass-through relationships (never allocated here).
    pub reserved: std::collections::HashSet<String>,
    next: usize,
}

impl SheetRels {
    /// Empty set whose allocated ids avoid `reserved`.
    pub fn with_reserved(reserved: std::collections::HashSet<String>) -> Self {
        SheetRels {
            reserved,
            ..SheetRels::default()
        }
    }

    /// Allocate a fresh id (avoiding reserved pass-through ids) for a new relationship.
    pub fn add(&mut self, kind: &str, target: &str, external: bool) -> String {
        let id = loop {
            self.next += 1;
            let id = format!("rId{}", self.next);
            if !self.reserved.contains(&id) {
                break id;
            }
        };
        self.rels.push((
            id.clone(),
            format!("{REL_NS}/{kind}"),
            target.to_string(),
            external,
        ));
        id
    }

    /// Add a relationship with a fixed id and full type (pass-through).
    pub fn push_raw(&mut self, id: &str, rel_type: &str, target: &str, external: bool) {
        self.rels.push((
            id.to_string(),
            rel_type.to_string(),
            target.to_string(),
            external,
        ));
    }

    pub fn is_empty(&self) -> bool {
        self.rels.is_empty()
    }

    pub fn xml(&self) -> String {
        let mut x = String::from(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
        );
        for (id, kind, target, external) in &self.rels {
            let _ = write!(
                x,
                r#"<Relationship Id="{id}" Type="{}" Target="{}"{}/>"#,
                esc(kind),
                esc(target),
                if *external {
                    r#" TargetMode="External""#
                } else {
                    ""
                }
            );
        }
        x.push_str("</Relationships>");
        x
    }
}

/// Elements after `</mergeCells>`: validations, hyperlinks, print settings, page breaks,
/// legacy drawing (comments). `n` is the 1-based sheet number used for part names.
pub fn after_merges_xml(sheet: &Sheet, n: usize, rels: &mut SheetRels) -> String {
    let mut x = String::new();
    if !sheet.validations.is_empty() {
        let _ = write!(
            x,
            r#"<dataValidations count="{}">"#,
            sheet.validations.len()
        );
        for v in &sheet.validations {
            x.push_str("<dataValidation");
            if v.kind != ValidationKind::Any {
                let _ = write!(x, r#" type="{}""#, v.kind.as_str());
            }
            if v.op != ValidationOp::Between {
                let _ = write!(x, r#" operator="{}""#, v.op.as_str());
            }
            match v.error_style {
                ErrorStyle::Stop => {}
                ErrorStyle::Warning => x.push_str(r#" errorStyle="warning""#),
                ErrorStyle::Information => x.push_str(r#" errorStyle="information""#),
            }
            if v.allow_blank {
                x.push_str(r#" allowBlank="1""#);
            }
            if !v.dropdown {
                // The file flag *suppresses* the dropdown.
                x.push_str(r#" showDropDown="1""#);
            }
            if v.show_prompt {
                x.push_str(r#" showInputMessage="1""#);
            }
            if v.show_error {
                x.push_str(r#" showErrorMessage="1""#);
            }
            for (k, t) in [
                ("errorTitle", &v.error_title),
                ("error", &v.error),
                ("promptTitle", &v.prompt_title),
                ("prompt", &v.prompt),
            ] {
                if !t.is_empty() {
                    let _ = write!(x, r#" {k}="{}""#, esc(t));
                }
            }
            let _ = write!(x, r#" sqref="{}">"#, sqref_text(&v.ranges));
            if !v.formula1.is_empty() {
                let _ = write!(x, "<formula1>{}</formula1>", esc(&v.formula1));
            }
            if !v.formula2.is_empty() {
                let _ = write!(x, "<formula2>{}</formula2>", esc(&v.formula2));
            }
            x.push_str("</dataValidation>");
        }
        x.push_str("</dataValidations>");
    }
    if !sheet.hyperlinks.is_empty() {
        x.push_str("<hyperlinks>");
        for h in &sheet.hyperlinks {
            let _ = write!(x, r#"<hyperlink ref="{}""#, h.range.to_a1());
            if !h.target.is_empty() {
                let id = rels.add("hyperlink", &h.target, true);
                let _ = write!(x, r#" r:id="{id}""#);
            }
            for (k, t) in [
                ("location", &h.location),
                ("tooltip", &h.tooltip),
                ("display", &h.display),
            ] {
                if !t.is_empty() {
                    let _ = write!(x, r#" {k}="{}""#, esc(t));
                }
            }
            x.push_str("/>");
        }
        x.push_str("</hyperlinks>");
    }
    let ps = &sheet.page_setup;
    if ps.print_gridlines || ps.print_headings {
        let _ = write!(
            x,
            r#"<printOptions headings="{}" gridLines="{}"/>"#,
            b01(ps.print_headings),
            b01(ps.print_gridlines)
        );
    }
    let m = ps.margins;
    let _ = write!(
        x,
        r#"<pageMargins left="{}" right="{}" top="{}" bottom="{}" header="{}" footer="{}"/>"#,
        m.left, m.right, m.top, m.bottom, m.header, m.footer
    );
    let default = sp_model::PageSetup::default();
    if ps.paper_size != default.paper_size
        || ps.scale != default.scale
        || ps.orientation != default.orientation
        || ps.fit_width.is_some()
        || ps.fit_height.is_some()
    {
        let _ = write!(
            x,
            r#"<pageSetup paperSize="{}" scale="{}""#,
            ps.paper_size, ps.scale
        );
        if let Some(w) = ps.fit_width {
            let _ = write!(x, r#" fitToWidth="{w}""#);
        }
        if let Some(h) = ps.fit_height {
            let _ = write!(x, r#" fitToHeight="{h}""#);
        }
        if ps.orientation == Orientation::Landscape {
            x.push_str(r#" orientation="landscape""#);
        } else {
            x.push_str(r#" orientation="portrait""#);
        }
        x.push_str("/>");
    }
    for (tag, set, max) in [
        ("rowBreaks", &ps.row_breaks, sp_core::MAX_COLS - 1),
        ("colBreaks", &ps.col_breaks, sp_core::MAX_ROWS - 1),
    ] {
        if !set.is_empty() {
            let _ = write!(
                x,
                r#"<{tag} count="{0}" manualBreakCount="{0}">"#,
                set.len()
            );
            for id in set {
                let _ = write!(x, r#"<brk id="{id}" max="{max}" man="1"/>"#);
            }
            let _ = write!(x, "</{tag}>");
        }
    }
    if !sheet.comments.is_empty() {
        rels.add("comments", &format!("../comments{n}.xml"), false);
        let id = rels.add(
            "vmlDrawing",
            &format!("../drawings/vmlDrawing{n}.vml"),
            false,
        );
        let _ = write!(x, r#"<legacyDrawing r:id="{id}"/>"#);
    }
    x
}

/// Text stored for a comment: threads keep their replies as extra lines.
fn comment_body(c: &Comment) -> String {
    let mut t = c.text.clone();
    for r in &c.replies {
        let _ = write!(t, "\n{}: {}", r.author, r.text);
    }
    t
}

/// `xl/commentsN.xml` for a sheet with comments.
pub fn comments_xml(sheet: &Sheet) -> String {
    let mut authors: Vec<&str> = Vec::new();
    for c in &sheet.comments {
        if !authors.contains(&c.author.as_str()) {
            authors.push(&c.author);
        }
    }
    let mut x = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<comments xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><authors>"#,
    );
    for a in &authors {
        let _ = write!(x, "<author>{}</author>", esc(a));
    }
    x.push_str("</authors><commentList>");
    for c in &sheet.comments {
        let aid = authors.iter().position(|a| *a == c.author).unwrap_or(0);
        let _ = write!(
            x,
            r#"<comment ref="{}" authorId="{aid}"><text><r><t xml:space="preserve">{}</t></r></text></comment>"#,
            c.addr.to_a1(),
            esc(&comment_body(c))
        );
    }
    x.push_str("</commentList></comments>");
    x
}

/// `xl/drawings/vmlDrawingN.vml`: the note boxes for a sheet's comments.
pub fn vml_xml(sheet: &Sheet, n: usize) -> String {
    let mut x = format!(
        r#"<xml xmlns:v="urn:schemas-microsoft-com:vml" xmlns:o="urn:schemas-microsoft-com:office:office" xmlns:x="urn:schemas-microsoft-com:office:excel"><o:shapelayout v:ext="edit"><o:idmap v:ext="edit" data="{n}"/></o:shapelayout><v:shapetype id="_x0000_t202" coordsize="21600,21600" o:spt="202" path="m,l,21600r21600,l21600,xe"><v:stroke joinstyle="miter"/><v:path gradientshapeok="t" o:connecttype="rect"/></v:shapetype>"#
    );
    for (i, c) in sheet.comments.iter().enumerate() {
        let (r, col) = (c.addr.row, c.addr.col);
        let _ = write!(
            x,
            r##"<v:shape id="_x0000_s{}" type="#_x0000_t202" style="position:absolute;margin-left:59.25pt;margin-top:1.5pt;width:108pt;height:59.25pt;z-index:{};visibility:{}" fillcolor="#ffffe1" o:insetmode="auto"><v:fill color2="#ffffe1"/><v:shadow on="t" color="black" obscured="t"/><v:path o:connecttype="none"/><v:textbox style="mso-direction-alt:auto"><div style="text-align:left"></div></v:textbox><x:ClientData ObjectType="Note"><x:MoveWithCells/><x:SizeWithCells/><x:Anchor>{}, 15, {}, 10, {}, 15, {}, 4</x:Anchor><x:AutoFill>False</x:AutoFill><x:Row>{r}</x:Row><x:Column>{col}</x:Column>{}</x:ClientData></v:shape>"##,
            n * 1024 + i + 1,
            i + 1,
            if c.visible { "visible" } else { "hidden" },
            col + 1,
            r.saturating_sub(1),
            col + 3,
            r + 3,
            if c.visible { "<x:Visible/>" } else { "" },
        );
    }
    x.push_str("</xml>");
    x
}

/// `<workbookProtection>` (goes before `<bookViews>`).
pub fn workbook_protection_xml(wb: &Workbook) -> String {
    let Some(p) = &wb.protection else {
        return String::new();
    };
    let mut x = String::from("<workbookProtection");
    write_password(&mut x, p.password.as_ref(), "workbook");
    let _ = write!(
        x,
        r#" lockStructure="{}" lockWindows="{}"/>"#,
        b01(p.structure),
        b01(p.windows)
    );
    x
}

// ---------------------------------------------------------------------------------------
// Reader

/// Sheet relationships: id → (resolved target, external).
pub type Rels = HashMap<String, (String, bool)>;

pub fn parse_rels(xml: &str, base_dir: &str) -> Result<Rels, IoError> {
    let root = parse_dom(xml)?;
    Ok(root
        .children_named("Relationship")
        .filter_map(|r| {
            let id = r.attr("Id")?.to_string();
            let target = r.attr("Target")?;
            let external = r.attr("TargetMode") == Some("External");
            let t = if external {
                target.to_string()
            } else {
                crate::xlsx_read::resolve_target_pub(base_dir, target)
            };
            Some((id, (t, external)))
        })
        .collect())
}

/// Split a worksheet around `sheetData`: (text before its start tag, text after its end tag).
/// Only these small pieces are re-parsed; the cell data is never scanned twice.
pub fn split_at_sheet_data(xml: &str) -> Option<(&str, &str)> {
    let first = xml.find("sheetData")?;
    let start = xml.get(..first)?.rfind('<')?;
    let last = xml.rfind("sheetData")?;
    let close = last + xml.get(last..)?.find('>')?;
    Some((xml.get(..start)?, xml.get(close + 1..)?))
}

/// Read every extra element of a worksheet part into `sheet`.
pub fn read_sheet_extras(xml: &str, sheet: &mut Sheet, rels: &Rels) -> Result<(), IoError> {
    let Some((head, tail)) = split_at_sheet_data(xml) else {
        return Ok(());
    };
    read_head(head, sheet);
    // Drop the closing `</worksheet>` and parse the rest as one fragment.
    let body = match tail.rfind("</") {
        Some(i) => tail.get(..i).unwrap_or(""),
        None => tail,
    };
    if body.trim().is_empty() {
        return Ok(());
    }
    let root = parse_dom(&format!("<root>{body}</root>"))?;
    for n in &root.children {
        match n.name.as_str() {
            "sheetProtection" => read_protection(n, sheet),
            "protectedRanges" => {
                for r in n.children_named("protectedRange") {
                    sheet.allow_edit_ranges.push(AllowEditRange {
                        title: r.attr("name").unwrap_or("Range").to_string(),
                        ranges: sqref(r.attr("sqref").unwrap_or("")),
                        password: read_password(r, ""),
                    });
                }
            }
            "autoFilter" => sheet.auto_filter = read_auto_filter(n),
            "dataValidations" => {
                for v in n.children_named("dataValidation") {
                    sheet.validations.push(read_validation(v));
                }
            }
            "hyperlinks" => {
                for h in n.children_named("hyperlink") {
                    let Some(range) = h.attr("ref").and_then(|r| CellRange::parse(r).ok()) else {
                        continue;
                    };
                    let target = h
                        .attr("id")
                        .and_then(|id| rels.get(id))
                        .map(|(t, _)| t.clone())
                        .unwrap_or_default();
                    let s = |k: &str| h.attr(k).unwrap_or("").to_string();
                    sheet.hyperlinks.push(Hyperlink {
                        range,
                        target,
                        location: s("location"),
                        tooltip: s("tooltip"),
                        display: s("display"),
                    });
                }
            }
            "printOptions" => {
                let ps = &mut sheet.page_setup;
                ps.print_gridlines = flag(n.attr("gridLines")).unwrap_or(false);
                ps.print_headings = flag(n.attr("headings")).unwrap_or(false);
            }
            "pageMargins" => {
                let m = &mut sheet.page_setup.margins;
                for (k, slot) in [
                    ("left", &mut m.left),
                    ("right", &mut m.right),
                    ("top", &mut m.top),
                    ("bottom", &mut m.bottom),
                    ("header", &mut m.header),
                    ("footer", &mut m.footer),
                ] {
                    if let Some(v) = n.attr(k).and_then(|v| v.parse::<f64>().ok())
                        && v.is_finite()
                    {
                        *slot = v;
                    }
                }
            }
            "pageSetup" => {
                let ps = &mut sheet.page_setup;
                let num = |k: &str| n.attr(k).and_then(|v| v.parse::<u32>().ok());
                if let Some(p) = num("paperSize") {
                    ps.paper_size = p;
                }
                if let Some(sc) = num("scale") {
                    ps.scale = sc.clamp(10, 400);
                }
                if n.attr("orientation") == Some("landscape") {
                    ps.orientation = Orientation::Landscape;
                }
                // Only meaningful when sheetPr/pageSetUpPr fitToPage is set (read first).
                if ps.fit_width.is_some() {
                    ps.fit_width = Some(num("fitToWidth").unwrap_or(1));
                    ps.fit_height = Some(num("fitToHeight").unwrap_or(1));
                }
            }
            "rowBreaks" | "colBreaks" => {
                let set = if n.name == "rowBreaks" {
                    &mut sheet.page_setup.row_breaks
                } else {
                    &mut sheet.page_setup.col_breaks
                };
                for b in n.children_named("brk").take(100_000) {
                    if flag(b.attr("man")).unwrap_or(false)
                        && let Some(id) = b.attr("id").and_then(|v| v.parse::<u32>().ok())
                    {
                        set.insert(id);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// `sheetPr` (outlinePr, pageSetUpPr) from the part before `sheetData`.
fn read_head(head: &str, sheet: &mut Sheet) {
    let mut reader = Reader::from_str(head);
    loop {
        let e = match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => e,
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => continue,
        };
        let a = crate::xml::attrs(&e);
        let get = |k: &str| a.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        match crate::xml::local(e.name().as_ref()) {
            "outlinePr" => {
                sheet.outline.summary_below = flag(get("summaryBelow")).unwrap_or(true);
                sheet.outline.summary_right = flag(get("summaryRight")).unwrap_or(true);
            }
            "pageSetUpPr" if flag(get("fitToPage")).unwrap_or(false) => {
                sheet.page_setup.fit_width = Some(1);
                sheet.page_setup.fit_height = Some(1);
            }
            _ => {}
        }
    }
}

fn read_protection(n: &Node, sheet: &mut Sheet) {
    if !flag(n.attr("sheet")).unwrap_or(false) {
        return;
    }
    // File attributes say what is *locked*; absent attributes take the file defaults.
    let locked = |k: &str, default: bool| flag(n.attr(k)).unwrap_or(default);
    sheet.protection = Some(SheetProtection {
        password: read_password(n, ""),
        objects: !locked("objects", false),
        scenarios: !locked("scenarios", false),
        format_cells: !locked("formatCells", true),
        format_columns: !locked("formatColumns", true),
        format_rows: !locked("formatRows", true),
        insert_columns: !locked("insertColumns", true),
        insert_rows: !locked("insertRows", true),
        insert_hyperlinks: !locked("insertHyperlinks", true),
        delete_columns: !locked("deleteColumns", true),
        delete_rows: !locked("deleteRows", true),
        select_locked: !locked("selectLockedCells", false),
        sort: !locked("sort", true),
        auto_filter: !locked("autoFilter", true),
        pivot_tables: !locked("pivotTables", true),
        select_unlocked: !locked("selectUnlockedCells", false),
    });
}

fn read_auto_filter(n: &Node) -> Option<AutoFilter> {
    let range = CellRange::parse(&n.attr("ref")?.replace('$', "")).ok()?;
    let mut columns = Vec::new();
    for fc in n.children_named("filterColumn") {
        let Some(col) = fc.attr("colId").and_then(|v| v.parse::<u32>().ok()) else {
            continue;
        };
        let criteria = if let Some(f) = fc.child("filters") {
            FilterCriteria::Values {
                values: f
                    .children_named("filter")
                    .filter_map(|x| x.attr("val").map(str::to_string))
                    .collect(),
                blanks: flag(f.attr("blank")).unwrap_or(false),
            }
        } else if let Some(cf) = fc.child("customFilters") {
            FilterCriteria::Custom {
                and: flag(cf.attr("and")).unwrap_or(false),
                conds: cf
                    .children_named("customFilter")
                    .take(2)
                    .map(|c| FilterCond {
                        op: c.attr("operator").unwrap_or("equal").to_string(),
                        val: c.attr("val").unwrap_or("").to_string(),
                    })
                    .collect(),
            }
        } else if let Some(t) = fc.child("top10") {
            FilterCriteria::Top10 {
                top: flag(t.attr("top")).unwrap_or(true),
                percent: flag(t.attr("percent")).unwrap_or(false),
                val: t
                    .attr("val")
                    .and_then(|v| v.parse::<f64>().ok())
                    .filter(|v| v.is_finite())
                    .unwrap_or(10.0),
            }
        } else {
            continue;
        };
        columns.push(FilterColumn { col, criteria });
    }
    Some(AutoFilter { range, columns })
}

fn read_validation(v: &Node) -> DataValidation {
    let s = |k: &str| v.attr(k).unwrap_or("").to_string();
    let f = |k: &str| {
        v.child(k)
            .map(|n| n.text.trim().to_string())
            .unwrap_or_default()
    };
    DataValidation {
        ranges: sqref(v.attr("sqref").unwrap_or("")),
        kind: v
            .attr("type")
            .and_then(ValidationKind::parse)
            .unwrap_or_default(),
        op: v
            .attr("operator")
            .and_then(ValidationOp::parse)
            .unwrap_or_default(),
        formula1: f("formula1"),
        formula2: f("formula2"),
        allow_blank: flag(v.attr("allowBlank")).unwrap_or(false),
        dropdown: !flag(v.attr("showDropDown")).unwrap_or(false),
        error_style: match v.attr("errorStyle") {
            Some("warning") => ErrorStyle::Warning,
            Some("information") => ErrorStyle::Information,
            _ => ErrorStyle::Stop,
        },
        show_error: flag(v.attr("showErrorMessage")).unwrap_or(false),
        error_title: s("errorTitle"),
        error: s("error"),
        show_prompt: flag(v.attr("showInputMessage")).unwrap_or(false),
        prompt_title: s("promptTitle"),
        prompt: s("prompt"),
    }
}

/// Legacy comments part → notes (visibility from the VML part when given).
pub fn read_comments(xml: &str, vml: Option<&str>, sheet: &mut Sheet) -> Result<(), IoError> {
    let root = parse_dom(xml)?;
    let authors: Vec<String> = root
        .child("authors")
        .map(|a| a.children_named("author").map(|x| x.all_text()).collect())
        .unwrap_or_default();
    let visible: Vec<CellAddr> = vml.map(vml_visible).unwrap_or_default();
    if let Some(list) = root.child("commentList") {
        for c in list.children_named("comment").take(1_000_000) {
            let Some(addr) = c.attr("ref").and_then(|r| CellAddr::parse(r).ok()) else {
                continue;
            };
            let author = c
                .attr("authorId")
                .and_then(|i| i.parse::<usize>().ok())
                .and_then(|i| authors.get(i))
                .cloned()
                .unwrap_or_default();
            let text = c.child("text").map(text_of).unwrap_or_default();
            if sheet.comment_at(addr).is_some() {
                continue;
            }
            sheet.comments.push(Comment {
                addr,
                kind: CommentKind::Note,
                author,
                text,
                replies: Vec::new(),
                visible: visible.contains(&addr),
                resolved: false,
            });
        }
    }
    sheet.comments.sort_by_key(|c| c.addr);
    Ok(())
}

/// Text of a rich-text `<text>` node: its `<t>` runs, skipping phonetic runs.
fn text_of(n: &Node) -> String {
    let mut s = String::new();
    for c in &n.children {
        match c.name.as_str() {
            "t" => s.push_str(&c.text),
            "r" => s.push_str(
                &c.children_named("t")
                    .map(|t| t.text.as_str())
                    .collect::<String>(),
            ),
            _ => {}
        }
    }
    crate::xml::unescape_ooxml(&s)
}

/// Cells whose VML note shape is marked visible.
fn vml_visible(vml: &str) -> Vec<CellAddr> {
    let Ok(root) = parse_dom(vml) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for shape in root.children_named("shape") {
        let Some(cd) = shape.child("ClientData") else {
            continue;
        };
        let num = |k: &str| cd.child(k).and_then(|n| n.text.trim().parse::<u32>().ok());
        if cd.child("Visible").is_some()
            && let (Some(r), Some(c)) = (num("Row"), num("Column"))
        {
            out.push(CellAddr::new(r, c));
        }
    }
    out
}

pub fn read_workbook_protection(root: &Node, wb: &mut Workbook) {
    let Some(n) = root.child("workbookProtection") else {
        return;
    };
    let structure = flag(n.attr("lockStructure")).unwrap_or(false);
    let windows = flag(n.attr("lockWindows")).unwrap_or(false);
    if structure || windows {
        wb.protection = Some(WorkbookProtection {
            password: read_password(n, "workbook"),
            structure,
            windows,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sp_core::Value;
    use sp_model::{CommentReply, Margins};

    fn sample() -> Workbook {
        let mut wb = Workbook::new();
        let sh = &mut wb.sheets[0];
        sh.set(
            CellAddr::new(0, 0),
            sp_model::Cell::constant(Value::text("h")),
        );
        sh.auto_filter = Some(AutoFilter {
            range: CellRange::parse("A1:C9").unwrap(),
            columns: vec![
                FilterColumn {
                    col: 0,
                    criteria: FilterCriteria::Values {
                        values: vec!["a&b".into()],
                        blanks: true,
                    },
                },
                FilterColumn {
                    col: 1,
                    criteria: FilterCriteria::Custom {
                        and: true,
                        conds: vec![
                            FilterCond {
                                op: "greaterThan".into(),
                                val: "5".into(),
                            },
                            FilterCond {
                                op: "equal".into(),
                                val: "x*".into(),
                            },
                        ],
                    },
                },
                FilterColumn {
                    col: 2,
                    criteria: FilterCriteria::Top10 {
                        top: false,
                        percent: true,
                        val: 20.0,
                    },
                },
            ],
        });
        sh.protection = Some(SheetProtection {
            password: Some(PasswordHash::legacy("pw")),
            sort: true,
            ..SheetProtection::default()
        });
        sh.allow_edit_ranges.push(AllowEditRange {
            title: "Inputs".into(),
            ranges: vec![
                CellRange::parse("B2:B5").unwrap(),
                CellRange::parse("D1").unwrap(),
            ],
            password: None,
        });
        sh.validations.push(DataValidation {
            ranges: vec![CellRange::parse("E1:E9").unwrap()],
            kind: ValidationKind::List,
            formula1: "\"x,y\"".into(),
            allow_blank: true,
            dropdown: true,
            show_error: true,
            error_style: ErrorStyle::Warning,
            error: "pick <one>".into(),
            ..DataValidation::default()
        });
        sh.validations.push(DataValidation {
            ranges: vec![CellRange::parse("F1").unwrap()],
            kind: ValidationKind::Whole,
            op: ValidationOp::GreaterThan,
            formula1: "$A$1*2".into(),
            ..DataValidation::default()
        });
        sh.hyperlinks.push(Hyperlink {
            range: CellRange::parse("G1").unwrap(),
            target: "https://example.com/?a=1&b=2".into(),
            location: String::new(),
            tooltip: "tip".into(),
            display: "Example".into(),
        });
        sh.hyperlinks.push(Hyperlink {
            range: CellRange::parse("G2").unwrap(),
            target: String::new(),
            location: "Sheet1!A1".into(),
            tooltip: String::new(),
            display: String::new(),
        });
        sh.comments.push(Comment {
            addr: CellAddr::new(1, 1),
            kind: CommentKind::Note,
            author: "Ann".into(),
            text: "first & <note>".into(),
            replies: vec![],
            visible: true,
            resolved: false,
        });
        sh.comments.push(Comment {
            addr: CellAddr::new(4, 3),
            kind: CommentKind::Thread,
            author: "Bob".into(),
            text: "q".into(),
            replies: vec![CommentReply {
                author: "Ann".into(),
                text: "a".into(),
            }],
            visible: false,
            resolved: false,
        });
        let ps = &mut sh.page_setup;
        ps.margins = Margins::NARROW;
        ps.orientation = Orientation::Landscape;
        ps.paper_size = 9;
        ps.fit_width = Some(1);
        ps.fit_height = Some(0);
        ps.print_gridlines = true;
        ps.row_breaks.insert(19);
        ps.col_breaks.insert(4);
        sh.outline.rows.insert(2, 1);
        sh.outline.rows.insert(3, 2);
        sh.outline.cols.insert(5, 1);
        sh.outline.summary_below = false;
        wb.protection = Some(WorkbookProtection {
            password: Some(PasswordHash::Modern {
                algorithm: "SHA-512".into(),
                hash: "aGFzaA==".into(),
                salt: "c2FsdA==".into(),
                spin: 100_000,
            }),
            structure: true,
            windows: false,
        });
        wb
    }

    #[test]
    fn extras_round_trip() {
        let wb = sample();
        let mut buf = std::io::Cursor::new(Vec::new());
        crate::xlsx_write::write_xlsx(&wb, &mut buf).unwrap();
        buf.set_position(0);
        let (back, _) = crate::xlsx_read::read_xlsx(buf).unwrap();
        let (a, b) = (&wb.sheets[0], &back.sheets[0]);
        assert_eq!(a.auto_filter, b.auto_filter);
        assert_eq!(a.protection, b.protection);
        assert_eq!(a.allow_edit_ranges, b.allow_edit_ranges);
        assert_eq!(a.validations, b.validations);
        assert_eq!(a.hyperlinks, b.hyperlinks);
        assert_eq!(a.page_setup, b.page_setup);
        assert_eq!(a.outline, b.outline);
        assert_eq!(wb.protection, back.protection);
        // Notes round-trip exactly; threads come back as notes carrying their replies.
        assert_eq!(b.comments.len(), 2);
        assert_eq!(b.comments[0], a.comments[0]);
        assert_eq!(b.comments[1].text, "q\nAnn: a");
        assert_eq!(b.comments[1].author, "Bob");
        assert!(!b.comments[1].visible);
    }

    #[test]
    fn defaults_write_nothing_extra() {
        let wb = Workbook::new();
        let sh = &wb.sheets[0];
        assert_eq!(sheet_pr_xml(sh), "");
        assert_eq!(before_merges_xml(sh), "");
        let mut rels = SheetRels::default();
        let x = after_merges_xml(sh, 1, &mut rels);
        assert!(x.starts_with("<pageMargins"));
        assert!(rels.is_empty());
        assert_eq!(workbook_protection_xml(&wb), "");
    }
}
