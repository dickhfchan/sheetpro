//! .xlsx conditional formatting (`conditionalFormatting`, differential formats in
//! `styles.xml` `<dxfs>`, a minimal `x14` data bar / icon set extension) and table parts
//! (`xl/tables/tableN.xml` + sheet relationships). Reader and writer, called from small hooks
//! in `xlsx_read.rs` / `xlsx_write.rs`.

use crate::LoadReport;
use crate::xml::{Node, esc, parse_dom};
use sp_core::CellRange;
use sp_model::{
    Border, BorderStyle, CfKind, CfOp, CfRule, Cfvo, CfvoKind, Color, CondFormat, DiffStyle, Sheet,
    Table, TableColumn, TextOp, TimePeriod,
};
use std::fmt::Write as _;
use std::io::{Read, Seek};

const MAX_PART_BYTES: u64 = 256 << 20;
/// Rules read per sheet (hostile-file guard).
const MAX_RULES: usize = 100_000;
const DEFAULT_BAR: Color = Color(96, 140, 200);
const X14_NS: &str = "http://schemas.microsoft.com/office/spreadsheetml/2009/9/main";
const XM_NS: &str = "http://schemas.microsoft.com/office/excel/2006/main";
const DATABAR_EXT: &str = "{B025F937-C7B1-47D3-B67F-A62EFF666E3E}";
const CF_EXT: &str = "{78C0D931-6437-407d-A8EE-F0AAD7539E65}";

fn read_part<R: Read + Seek>(zip: &mut zip::ZipArchive<R>, name: &str) -> Option<String> {
    let file = zip.by_name(name).ok()?;
    let mut buf = String::new();
    file.take(MAX_PART_BYTES + 1)
        .read_to_string(&mut buf)
        .ok()?;
    (buf.len() as u64 <= MAX_PART_BYTES).then_some(buf)
}

// ── reading ────────────────────────────────────────────────────────────────────────────────

fn rgb(n: Option<&Node>) -> Option<Color> {
    n.and_then(|c| c.attr("rgb")).and_then(Color::from_hex)
}

fn border_style(s: &str) -> BorderStyle {
    match s {
        "medium" | "mediumDashed" | "mediumDashDot" | "mediumDashDotDot" | "slantDashDot" => {
            BorderStyle::Medium
        }
        "thick" => BorderStyle::Thick,
        "dashed" | "dashDot" | "dashDotDot" => BorderStyle::Dashed,
        "dotted" | "hair" => BorderStyle::Dotted,
        "double" => BorderStyle::Double,
        "none" => BorderStyle::None,
        _ => BorderStyle::Thin,
    }
}

fn flag(n: &Node, name: &str) -> Option<bool> {
    n.child(name)
        .map(|b| !matches!(b.attr("val"), Some("0" | "false")))
}

fn dxf_of(n: &Node) -> DiffStyle {
    let mut d = DiffStyle::default();
    if let Some(f) = n.child("font") {
        d.bold = flag(f, "b");
        d.italic = flag(f, "i");
        d.strike = flag(f, "strike");
        d.underline = f.child("u").map(|u| u.attr("val") != Some("none"));
        d.font_color = rgb(f.child("color"));
    }
    if let Some(nf) = n.child("numFmt") {
        d.num_fmt = nf.attr("formatCode").map(str::to_string);
    }
    if let Some(pf) = n.child("fill").and_then(|f| f.child("patternFill")) {
        d.fill = rgb(pf.child("bgColor")).or_else(|| rgb(pf.child("fgColor")));
    }
    if let Some(b) = n.child("border") {
        for side in ["left", "right", "top", "bottom"] {
            if let Some(s) = b.child(side)
                && let Some(st) = s.attr("style")
            {
                d.border = Some(Border {
                    style: border_style(st),
                    color: rgb(s.child("color")),
                });
                break;
            }
        }
    }
    d
}

/// Differential formats of `xl/styles.xml`, by `dxfId`.
pub fn read_dxfs<R: Read + Seek>(zip: &mut zip::ZipArchive<R>) -> Vec<DiffStyle> {
    let Some(x) = read_part(zip, "xl/styles.xml") else {
        return Vec::new();
    };
    // Only the <dxfs> element is needed; avoid building a DOM of the whole part.
    let (Some(a), Some(b)) = (x.find("<dxfs"), x.find("</dxfs>")) else {
        return Vec::new();
    };
    let Some(part) = x.get(a..b + "</dxfs>".len()) else {
        return Vec::new();
    };
    parse_dom(part)
        .ok()
        .filter(|d| d.name == "dxfs")
        .map(|d| d.children_named("dxf").map(dxf_of).collect())
        .unwrap_or_default()
}

fn sqref(s: &str) -> Vec<CellRange> {
    s.split_whitespace()
        .filter_map(|r| CellRange::parse(&r.replace('$', "")).ok())
        .collect()
}

fn cfvo_of(n: &Node) -> Option<Cfvo> {
    let kind = CfvoKind::from_name(n.attr("type")?)?;
    let value = n
        .attr("val")
        .map(str::to_string)
        .or_else(|| n.child("f").map(|f| f.text.trim().to_string()))
        .unwrap_or_default();
    Some(Cfvo {
        kind,
        value,
        gte: n.attr("gte") != Some("0"),
    })
}

fn colors(n: &Node) -> Vec<Color> {
    n.children_named("color")
        .map(|c| rgb(Some(c)).unwrap_or(DEFAULT_BAR))
        .collect()
}

fn on(n: &Node, k: &str, default: bool) -> bool {
    match n.attr(k) {
        Some("1" | "true") => true,
        Some("0" | "false") => false,
        _ => default,
    }
}

fn rule_of(r: &Node, dxfs: &[DiffStyle]) -> Option<CfRule> {
    let ty = r.attr("type")?;
    let formulas: Vec<String> = r
        .children_named("formula")
        .map(|f| f.text.trim().to_string())
        .collect();
    let f = |i: usize| formulas.get(i).cloned().unwrap_or_default();
    let kind = match ty {
        "cellIs" => CfKind::CellIs {
            op: CfOp::from_name(r.attr("operator").unwrap_or("equal"))?,
            f1: f(0),
            f2: f(1),
        },
        "expression" => CfKind::Expression(f(0)),
        "containsText" | "notContainsText" | "beginsWith" | "endsWith" => CfKind::Text {
            op: TextOp::from_name(ty)?,
            text: r.attr("text").unwrap_or_default().to_string(),
        },
        "timePeriod" => CfKind::TimePeriod(TimePeriod::from_name(r.attr("timePeriod")?)?),
        "duplicateValues" => CfKind::Duplicates,
        "uniqueValues" => CfKind::Unique,
        "containsBlanks" => CfKind::Blanks,
        "notContainsBlanks" => CfKind::NoBlanks,
        "containsErrors" => CfKind::Errors,
        "notContainsErrors" => CfKind::NoErrors,
        "top10" => CfKind::Top {
            bottom: on(r, "bottom", false),
            percent: on(r, "percent", false),
            rank: r
                .attr("rank")
                .and_then(|v| v.parse().ok())
                .unwrap_or(10)
                .clamp(1, 1000),
        },
        "aboveAverage" => CfKind::Average {
            below: !on(r, "aboveAverage", true),
            equal: on(r, "equalAverage", false),
            std_dev: r
                .attr("stdDev")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0)
                .min(3),
        },
        "dataBar" => {
            let db = r.child("dataBar")?;
            let vos: Vec<Cfvo> = db.children_named("cfvo").filter_map(cfvo_of).collect();
            CfKind::DataBar {
                min: vos.first().cloned()?,
                max: vos.get(1).cloned()?,
                color: colors(db).first().copied().unwrap_or(DEFAULT_BAR),
                gradient: true,
                show_value: on(db, "showValue", true),
            }
        }
        "colorScale" => {
            let cs = r.child("colorScale")?;
            let vos: Vec<Cfvo> = cs.children_named("cfvo").filter_map(cfvo_of).collect();
            let cols = colors(cs);
            if vos.len() < 2 || vos.len() != cols.len() {
                return None;
            }
            CfKind::ColorScale {
                points: vos.into_iter().zip(cols).take(3).collect(),
            }
        }
        "iconSet" => {
            let is = r.child("iconSet")?;
            CfKind::IconSet {
                set: is.attr("iconSet").unwrap_or("3TrafficLights1").to_string(),
                points: is
                    .children_named("cfvo")
                    .filter_map(cfvo_of)
                    .take(5)
                    .collect(),
                reverse: on(is, "reverse", false),
                show_value: on(is, "showValue", true),
            }
        }
        _ => return None,
    };
    let style = r
        .attr("dxfId")
        .and_then(|i| i.parse::<usize>().ok())
        .and_then(|i| dxfs.get(i).cloned())
        .unwrap_or_default();
    Some(CfRule {
        kind,
        style,
        priority: r.attr("priority").and_then(|p| p.parse().ok()).unwrap_or(1),
        stop_if_true: on(r, "stopIfTrue", false),
    })
}

/// Elements after `<sheetData>` as a DOM (cheap: the cells are skipped).
fn tail_dom(xml: &str) -> Option<Node> {
    let end = xml
        .rfind("</sheetData>")
        .map(|p| p + "</sheetData>".len())
        .or_else(|| xml.rfind("<sheetData/>").map(|p| p + "<sheetData/>".len()))?;
    let tail = xml.get(end..)?;
    let close = tail.rfind("</")?;
    let body = tail.get(..close)?;
    // A single top element is returned as the root itself.
    parse_dom(&format!("<tail>{body}</tail>"))
        .ok()
        .filter(|n| n.name == "tail")
}

/// Read a sheet's conditional formats and tables. Problems become warnings.
pub fn read_sheet<R: Read + Seek>(
    zip: &mut zip::ZipArchive<R>,
    path: &str,
    xml: &str,
    sheet: &mut Sheet,
    dxfs: &[DiffStyle],
    report: &mut LoadReport,
) {
    if !xml.contains("conditionalFormatting") && !xml.contains("tablePart") {
        return;
    }
    let Some(tail) = tail_dom(xml) else {
        report.warnings.push(format!(
            "sheet '{}': conditional formats skipped",
            sheet.name
        ));
        return;
    };
    let mut count = 0usize;
    for cf in tail.children_named("conditionalFormatting") {
        let ranges = sqref(cf.attr("sqref").unwrap_or_default());
        let rules: Vec<CfRule> = cf
            .children_named("cfRule")
            .filter_map(|r| rule_of(r, dxfs))
            .collect();
        count += rules.len();
        if !ranges.is_empty() && !rules.is_empty() && count <= MAX_RULES {
            sheet.cond_formats.push(CondFormat { ranges, rules });
        }
    }
    // x14-only rules (icon sets the base schema lacks, e.g. 3Stars).
    for ext in tail
        .child("extLst")
        .into_iter()
        .flat_map(|e| e.children_named("ext"))
    {
        for cf in ext
            .children_named("conditionalFormattings")
            .flat_map(|c| c.children_named("conditionalFormatting"))
        {
            let ranges = sqref(cf.child("sqref").map_or("", |s| s.text.as_str()));
            for r in cf.children_named("cfRule") {
                if r.attr("type") != Some("iconSet") || ranges.is_empty() {
                    continue;
                }
                let Some(is) = r.child("iconSet") else {
                    continue;
                };
                let rule = CfRule {
                    kind: CfKind::IconSet {
                        set: is.attr("iconSet").unwrap_or("3Stars").to_string(),
                        points: is
                            .children_named("cfvo")
                            .filter_map(cfvo_of)
                            .take(5)
                            .collect(),
                        reverse: on(is, "reverse", false),
                        show_value: on(is, "showValue", true),
                    },
                    style: DiffStyle::default(),
                    priority: r.attr("priority").and_then(|p| p.parse().ok()).unwrap_or(1),
                    stop_if_true: false,
                };
                sheet.cond_formats.push(CondFormat {
                    ranges: ranges.clone(),
                    rules: vec![rule],
                });
            }
        }
    }
    // Tables.
    let parts: Vec<String> = tail
        .child("tableParts")
        .map(|t| {
            t.children_named("tablePart")
                .filter_map(|p| p.attr("id").map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if parts.is_empty() {
        return;
    }
    let (dir, file) = path.rsplit_once('/').unwrap_or(("", path));
    let rels = read_part(zip, &format!("{dir}/_rels/{file}.rels"))
        .and_then(|x| crate::xlsx_extras::parse_rels(&x, dir).ok())
        .unwrap_or_default();
    for id in parts {
        let Some((target, _)) = rels.get(&id) else {
            continue;
        };
        let Some(tx) = read_part(zip, target) else {
            continue;
        };
        match parse_dom(&tx).map(|d| read_table(&d)) {
            Ok(Some(t)) => sheet.tables.push(t),
            _ => report.warnings.push(format!(
                "sheet '{}': table part {target} skipped",
                sheet.name
            )),
        }
    }
    // A filtered table is the sheet's filter in the app (filter buttons, Clear, Reapply);
    // saving puts the criteria back on the table.
    if sheet.auto_filter.is_none()
        && let Some(t) = sheet
            .tables
            .iter_mut()
            .find(|t| !t.filter_columns.is_empty() && t.filter_range().is_some())
        && let Some(range) = t.filter_range()
    {
        sheet.auto_filter = Some(sp_model::AutoFilter {
            range,
            columns: std::mem::take(&mut t.filter_columns),
        });
    }
}

fn read_table(t: &Node) -> Option<Table> {
    if t.name != "table" {
        return None;
    }
    let range = CellRange::parse(t.attr("ref")?).ok()?;
    let name = t
        .attr("displayName")
        .or_else(|| t.attr("name"))?
        .to_string();
    let columns: Vec<TableColumn> = t
        .child("tableColumns")
        .map(|c| {
            c.children_named("tableColumn")
                .map(|col| TableColumn {
                    name: col.attr("name").unwrap_or_default().to_string(),
                    totals_function: col.attr("totalsRowFunction").map(str::to_string),
                    totals_label: col.attr("totalsRowLabel").map(str::to_string),
                    totals_formula: col
                        .child("totalsRowFormula")
                        .map(|f| f.text.trim().to_string()),
                    calc_formula: col
                        .child("calculatedColumnFormula")
                        .map(|f| f.text.trim().to_string()),
                })
                .collect()
        })
        .unwrap_or_default();
    if columns.len() != range.cols() as usize {
        return None;
    }
    let mut out = Table::new(
        t.attr("id").and_then(|v| v.parse().ok()).unwrap_or(1),
        name,
        range,
        Vec::new(),
    );
    out.columns = columns;
    out.header_row = t.attr("headerRowCount") != Some("0");
    out.totals_row = t
        .attr("totalsRowCount")
        .and_then(|v| v.parse::<u32>().ok())
        .is_some_and(|n| n > 0);
    out.autofilter = t.child("autoFilter").is_some();
    out.filter_columns = t
        .child("autoFilter")
        .and_then(crate::xlsx_extras::read_auto_filter)
        .map(|af| af.columns)
        .unwrap_or_default();
    match t.child("tableStyleInfo") {
        Some(si) => {
            out.style = si.attr("name").unwrap_or_default().to_string();
            out.first_col = on(si, "showFirstColumn", false);
            out.last_col = on(si, "showLastColumn", false);
            out.banded_rows = on(si, "showRowStripes", false);
            out.banded_cols = on(si, "showColumnStripes", false);
        }
        None => out.style.clear(),
    }
    Some(out)
}

// ── writing ────────────────────────────────────────────────────────────────────────────────

fn color_xml(tag: &str, c: Color) -> String {
    format!(r#"<{tag} rgb="{}"/>"#, c.to_argb_hex())
}

fn border_name(b: BorderStyle) -> &'static str {
    match b {
        BorderStyle::None => "none",
        BorderStyle::Thin => "thin",
        BorderStyle::Medium => "medium",
        BorderStyle::Thick => "thick",
        BorderStyle::Dashed => "dashed",
        BorderStyle::Dotted => "dotted",
        BorderStyle::Double => "double",
    }
}

fn dxf_xml(d: &DiffStyle, numfmt_id: usize) -> String {
    let mut x = String::from("<dxf>");
    let b = |v: Option<bool>, tag: &str| match v {
        Some(true) => format!("<{tag}/>"),
        Some(false) => format!(r#"<{tag} val="0"/>"#),
        None => String::new(),
    };
    let font = format!(
        "{}{}{}{}{}",
        b(d.bold, "b"),
        b(d.italic, "i"),
        b(d.strike, "strike"),
        match d.underline {
            Some(true) => "<u/>".to_string(),
            Some(false) => r#"<u val="none"/>"#.to_string(),
            None => String::new(),
        },
        d.font_color
            .map(|c| color_xml("color", c))
            .unwrap_or_default()
    );
    if !font.is_empty() {
        let _ = write!(x, "<font>{font}</font>");
    }
    if let Some(f) = &d.num_fmt {
        let _ = write!(
            x,
            r#"<numFmt numFmtId="{numfmt_id}" formatCode="{}"/>"#,
            esc(f)
        );
    }
    if let Some(c) = d.fill {
        let _ = write!(
            x,
            "<fill><patternFill>{}</patternFill></fill>",
            color_xml("bgColor", c)
        );
    }
    if let Some(bd) = d.border {
        let side = |tag: &str| {
            format!(
                r#"<{tag} style="{}">{}</{tag}>"#,
                border_name(bd.style),
                bd.color.map(|c| color_xml("color", c)).unwrap_or_default()
            )
        };
        let _ = write!(
            x,
            "<border>{}{}{}{}</border>",
            side("left"),
            side("right"),
            side("top"),
            side("bottom")
        );
    }
    x.push_str("</dxf>");
    x
}

fn cfvo_xml(v: &Cfvo, icon: bool) -> String {
    let val = if v.value.is_empty() {
        String::new()
    } else {
        format!(r#" val="{}""#, esc(&v.value))
    };
    let gte = if icon && !v.gte { r#" gte="0""# } else { "" };
    format!(r#"<cfvo type="{}"{val}{gte}/>"#, v.kind.name())
}

fn x14_cfvo(v: &Cfvo, icon: bool) -> String {
    let gte = if icon && !v.gte { r#" gte="0""# } else { "" };
    if v.value.is_empty() || matches!(v.kind, CfvoKind::Min | CfvoKind::Max) {
        let kind = match v.kind {
            CfvoKind::Min => "autoMin",
            CfvoKind::Max => "autoMax",
            k => k.name(),
        };
        format!(r#"<x14:cfvo type="{kind}"{gte}/>"#)
    } else {
        format!(
            r#"<x14:cfvo type="{}"{gte}><xm:f>{}</xm:f></x14:cfvo>"#,
            v.kind.name(),
            esc(&v.value)
        )
    }
}

/// Icon sets only the 2009 extension knows.
fn x14_only_set(set: &str) -> bool {
    matches!(set, "3Stars" | "3Triangles" | "5Boxes")
}

/// Formula text as stored in the file (future functions prefixed).
fn ff(src: &str) -> String {
    esc(&crate::xlsx_write::file_formula(src))
}

/// Writer state shared across sheets: differential formats and table parts.
#[derive(Default)]
pub struct CfWriter {
    dxfs: Vec<DiffStyle>,
    tables: Vec<(String, String)>,
    ext_id: u32,
    /// x14 extension of the sheet being written (from `sheet_cf_xml`).
    pending_ext: String,
    /// Lower-cased table names written so far.
    table_names: std::collections::HashSet<String>,
}

impl CfWriter {
    pub fn new() -> Self {
        CfWriter::default()
    }

    fn dxf_id(&mut self, d: &DiffStyle) -> usize {
        if let Some(i) = self.dxfs.iter().position(|x| x == d) {
            return i;
        }
        self.dxfs.push(d.clone());
        self.dxfs.len() - 1
    }

    fn guid(&mut self) -> String {
        self.ext_id += 1;
        format!("{{5A1B0C2D-0000-4000-8000-{:012X}}}", self.ext_id)
    }

    /// `<conditionalFormatting>` elements (placed after `<mergeCells>`), plus the sheet's
    /// x14 extension list (placed last, see [`CfWriter::sheet_tail_xml`]).
    fn sheet_cf(&mut self, sheet: &Sheet) -> (String, String) {
        let mut x = String::new();
        let mut ext = String::new();
        for cf in &sheet.cond_formats {
            let sq: Vec<String> = cf.ranges.iter().map(CellRange::to_a1).collect();
            let sq = sq.join(" ");
            let mut rules = String::new();
            for r in &cf.rules {
                let stop = if r.stop_if_true {
                    r#" stopIfTrue="1""#
                } else {
                    ""
                };
                let pr = r.priority;
                let dxf = |w: &mut Self| format!(r#" dxfId="{}""#, w.dxf_id(&r.style));
                match &r.kind {
                    CfKind::CellIs { op, f1, f2 } => {
                        let d = dxf(self);
                        let _ = write!(
                            rules,
                            r#"<cfRule type="cellIs"{d} priority="{pr}"{stop} operator="{}"><formula>{}</formula>{}</cfRule>"#,
                            op.name(),
                            ff(f1),
                            if f2.is_empty() {
                                String::new()
                            } else {
                                format!("<formula>{}</formula>", ff(f2))
                            }
                        );
                    }
                    CfKind::Expression(f) => {
                        let d = dxf(self);
                        let _ = write!(
                            rules,
                            r#"<cfRule type="expression"{d} priority="{pr}"{stop}><formula>{}</formula></cfRule>"#,
                            ff(f)
                        );
                    }
                    CfKind::Text { op, text } => {
                        let d = dxf(self);
                        let a = cf.anchor().to_a1();
                        let q = text.replace('"', "\"\"");
                        let f = match op {
                            TextOp::Contains => format!(r#"NOT(ISERROR(SEARCH("{q}",{a})))"#),
                            TextOp::NotContains => format!(r#"ISERROR(SEARCH("{q}",{a}))"#),
                            TextOp::BeginsWith => {
                                format!(r#"LEFT({a},{})="{q}""#, text.chars().count())
                            }
                            TextOp::EndsWith => {
                                format!(r#"RIGHT({a},{})="{q}""#, text.chars().count())
                            }
                        };
                        let opn = match op {
                            TextOp::Contains => "containsText",
                            TextOp::NotContains => "notContains",
                            TextOp::BeginsWith => "beginsWith",
                            TextOp::EndsWith => "endsWith",
                        };
                        let _ = write!(
                            rules,
                            r#"<cfRule type="{}"{d} priority="{pr}"{stop} operator="{opn}" text="{}"><formula>{}</formula></cfRule>"#,
                            op.name(),
                            esc(text),
                            esc(&f)
                        );
                    }
                    CfKind::TimePeriod(p) => {
                        let d = dxf(self);
                        let _ = write!(
                            rules,
                            r#"<cfRule type="timePeriod"{d} priority="{pr}"{stop} timePeriod="{}"/>"#,
                            p.name()
                        );
                    }
                    CfKind::Duplicates
                    | CfKind::Unique
                    | CfKind::Blanks
                    | CfKind::NoBlanks
                    | CfKind::Errors
                    | CfKind::NoErrors => {
                        let d = dxf(self);
                        let a = cf.anchor().to_a1();
                        let (ty, f) = match &r.kind {
                            CfKind::Duplicates => ("duplicateValues", String::new()),
                            CfKind::Unique => ("uniqueValues", String::new()),
                            CfKind::Blanks => ("containsBlanks", format!("LEN(TRIM({a}))=0")),
                            CfKind::NoBlanks => ("notContainsBlanks", format!("LEN(TRIM({a}))>0")),
                            CfKind::Errors => ("containsErrors", format!("ISERROR({a})")),
                            _ => ("notContainsErrors", format!("NOT(ISERROR({a}))")),
                        };
                        let body = if f.is_empty() {
                            String::new()
                        } else {
                            format!("<formula>{f}</formula>")
                        };
                        let _ = write!(
                            rules,
                            r#"<cfRule type="{ty}"{d} priority="{pr}"{stop}>{body}</cfRule>"#
                        );
                    }
                    CfKind::Top {
                        bottom,
                        percent,
                        rank,
                    } => {
                        let d = dxf(self);
                        let _ = write!(
                            rules,
                            r#"<cfRule type="top10"{d} priority="{pr}"{stop} rank="{rank}"{}{}/>"#,
                            if *percent { r#" percent="1""# } else { "" },
                            if *bottom { r#" bottom="1""# } else { "" }
                        );
                    }
                    CfKind::Average {
                        below,
                        equal,
                        std_dev,
                    } => {
                        let d = dxf(self);
                        let _ = write!(
                            rules,
                            r#"<cfRule type="aboveAverage"{d} priority="{pr}"{stop}{}{}{}/>"#,
                            if *below { r#" aboveAverage="0""# } else { "" },
                            if *equal { r#" equalAverage="1""# } else { "" },
                            if *std_dev > 0 {
                                format!(r#" stdDev="{std_dev}""#)
                            } else {
                                String::new()
                            }
                        );
                    }
                    CfKind::DataBar {
                        min,
                        max,
                        color,
                        gradient,
                        show_value,
                    } => {
                        let id = self.guid();
                        let _ = write!(
                            rules,
                            r#"<cfRule type="dataBar" priority="{pr}"{stop}><dataBar{}>{}{}{}</dataBar><extLst><ext uri="{DATABAR_EXT}" xmlns:x14="{X14_NS}"><x14:id>{id}</x14:id></ext></extLst></cfRule>"#,
                            if *show_value { "" } else { r#" showValue="0""# },
                            cfvo_xml(min, false),
                            cfvo_xml(max, false),
                            color_xml("color", *color)
                        );
                        let _ = write!(
                            ext,
                            r#"<x14:conditionalFormatting xmlns:xm="{XM_NS}"><x14:cfRule type="dataBar" id="{id}"><x14:dataBar minLength="0" maxLength="100"{}>{}{}{}<x14:negativeFillColor rgb="FFFF0000"/><x14:axisColor rgb="FF000000"/></x14:dataBar></x14:cfRule><xm:sqref>{sq}</xm:sqref></x14:conditionalFormatting>"#,
                            if *gradient { "" } else { r#" gradient="0""# },
                            x14_cfvo(min, false),
                            x14_cfvo(max, false),
                            if *gradient {
                                String::new()
                            } else {
                                color_xml("x14:fillColor", *color)
                            }
                        );
                    }
                    CfKind::ColorScale { points } => {
                        let vos: String = points.iter().map(|(v, _)| cfvo_xml(v, false)).collect();
                        let cols: String =
                            points.iter().map(|(_, c)| color_xml("color", *c)).collect();
                        let _ = write!(
                            rules,
                            r#"<cfRule type="colorScale" priority="{pr}"{stop}><colorScale>{vos}{cols}</colorScale></cfRule>"#
                        );
                    }
                    CfKind::IconSet {
                        set,
                        points,
                        reverse,
                        show_value,
                    } => {
                        let attrs = format!(
                            r#" iconSet="{}"{}{}"#,
                            esc(set),
                            if *show_value { "" } else { r#" showValue="0""# },
                            if *reverse { r#" reverse="1""# } else { "" }
                        );
                        if x14_only_set(set) {
                            let id = self.guid();
                            let vos: String = points.iter().map(|v| x14_cfvo(v, true)).collect();
                            let _ = write!(
                                ext,
                                r#"<x14:conditionalFormatting xmlns:xm="{XM_NS}"><x14:cfRule type="iconSet" priority="{pr}" id="{id}"><x14:iconSet{attrs}>{vos}</x14:iconSet></x14:cfRule><xm:sqref>{sq}</xm:sqref></x14:conditionalFormatting>"#
                            );
                        } else {
                            let vos: String = points.iter().map(|v| cfvo_xml(v, true)).collect();
                            let _ = write!(
                                rules,
                                r#"<cfRule type="iconSet" priority="{pr}"{stop}><iconSet{attrs}>{vos}</iconSet></cfRule>"#
                            );
                        }
                    }
                }
            }
            if !rules.is_empty() {
                let _ = write!(
                    x,
                    r#"<conditionalFormatting sqref="{sq}">{rules}</conditionalFormatting>"#
                );
            }
        }
        (x, ext)
    }

    /// Conditional formats of a sheet: write right after `</mergeCells>`.
    pub fn sheet_cf_xml(&mut self, sheet: &Sheet) -> String {
        let (x, ext) = self.sheet_cf(sheet);
        self.pending_ext = ext;
        x
    }

    /// `<tableParts>` and the sheet's `extLst` as (local name, XML) elements for the
    /// worksheet merge. A captured `extLst` keeps its other `<ext>` children; its
    /// conditional-formatting ext is replaced by ours (the rules it held were read into the
    /// model), so the URI never appears twice.
    pub fn sheet_tail(
        &mut self,
        sheet: &Sheet,
        rels: &mut crate::xlsx_extras::SheetRels,
        pt: &sp_model::passthrough::Passthrough,
    ) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut ids = Vec::new();
        for t in &sheet.tables {
            if !self.table_names.insert(t.name.to_ascii_lowercase()) {
                continue; // table names are workbook-unique; a duplicate is not written
            }
            let n = self.tables.len() + 1;
            let rid = rels.add("table", &format!("../tables/table{n}.xml"), false);
            self.tables
                .push((format!("xl/tables/table{n}.xml"), table_xml(t, n, sheet.auto_filter.as_ref())));
            ids.push(rid);
        }
        if !ids.is_empty() {
            let mut x = format!(r#"<tableParts count="{}">"#, ids.len());
            for id in ids {
                let _ = write!(x, r#"<tablePart r:id="{id}"/>"#);
            }
            x.push_str("</tableParts>");
            out.push(("tableParts".to_string(), x));
        }
        let ext = std::mem::take(&mut self.pending_ext);
        let captured = pt
            .elements
            .iter()
            .find(|e| crate::xml::local(&e.name) == "extLst");
        let mut kept: Vec<&str> = Vec::new();
        let mut had_cf = false;
        if let Some(c) = captured
            && let Ok((_, children)) = crate::xlsx_passthrough::scan_children(&c.xml, true)
        {
            for ch in children {
                if ch
                    .xml
                    .to_ascii_uppercase()
                    .contains(&CF_EXT.to_ascii_uppercase())
                {
                    had_cf = true;
                } else {
                    kept.push(ch.xml);
                }
            }
        }
        if ext.is_empty() && !had_cf {
            return out; // the captured extLst (if any) passes through unchanged
        }
        let mut x = String::new();
        if !ext.is_empty() || !kept.is_empty() {
            x.push_str("<extLst>");
            if !ext.is_empty() {
                let _ = write!(
                    x,
                    r#"<ext uri="{CF_EXT}" xmlns:x14="{X14_NS}"><x14:conditionalFormattings>{ext}</x14:conditionalFormattings></ext>"#
                );
            }
            for k in kept {
                x.push_str(k);
            }
            x.push_str("</extLst>");
        }
        // Even when empty, naming `extLst` as generated suppresses the captured one.
        out.push(("extLst".to_string(), x));
        out
    }

    /// `styles.xml` with the collected `<dxfs>` inserted (replacing an empty one).
    pub fn styles_xml(&self, styles: &str) -> String {
        if self.dxfs.is_empty() {
            return styles.to_string();
        }
        let mut d = format!(r#"<dxfs count="{}">"#, self.dxfs.len());
        for (i, x) in self.dxfs.iter().enumerate() {
            d.push_str(&dxf_xml(x, 300 + i));
        }
        d.push_str("</dxfs>");
        let mut s = styles.to_string();
        if let Some(a) = s.find("<dxfs") {
            let end = s
                .get(a..)
                .and_then(|t| t.find("</dxfs>").map(|e| a + e + "</dxfs>".len()))
                .or_else(|| s.get(a..).and_then(|t| t.find("/>").map(|e| a + e + 2)));
            if let Some(e) = end {
                s.replace_range(a..e, &d);
                return s;
            }
        }
        for anchor in ["<tableStyles", "<colors", "<extLst", "</styleSheet>"] {
            if let Some(p) = s.find(anchor) {
                s.insert_str(p, &d);
                return s;
            }
        }
        s
    }

    /// Table parts to write (path, XML); content type `TABLE_CONTENT_TYPE`.
    pub fn finish(self) -> Vec<(String, String)> {
        self.tables
    }
}

/// Every table part name the writer may produce (reserved before pass-through placement).
pub fn table_part_names(wb: &sp_model::Workbook) -> Vec<String> {
    let n: usize = wb.sheets.iter().map(|s| s.tables.len()).sum();
    (1..=n).map(|k| format!("xl/tables/table{k}.xml")).collect()
}

/// `sheet_filter`: the sheet's filter, written here (not on the sheet) when it is this
/// table's filter: a sheet-level AutoFilter over a table is invalid in the file format.
fn table_xml(t: &Table, n: usize, sheet_filter: Option<&sp_model::AutoFilter>) -> String {
    let mut x = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<table xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" id="{}" name="{}" displayName="{}" ref="{}"{}{}>"#,
        if t.id == 0 { n as u32 } else { t.id },
        esc(&t.name),
        esc(&t.name),
        t.range.to_a1(),
        if t.header_row {
            ""
        } else {
            r#" headerRowCount="0""#
        },
        if t.totals_row {
            r#" totalsRowCount="1""#
        } else {
            r#" totalsRowShown="0""#
        }
    );
    if t.autofilter && t.header_row {
        let end_row = if t.totals_row {
            t.range.end.row.saturating_sub(1).max(t.range.start.row)
        } else {
            t.range.end.row
        };
        let af = CellRange::new(
            t.range.start,
            sp_core::CellAddr::new(end_row, t.range.end.col),
        );
        let columns = match sheet_filter {
            Some(sf) if t.filter_columns.is_empty() && sf.range == af => &sf.columns,
            _ => &t.filter_columns,
        };
        if columns.is_empty() {
            let _ = write!(x, r#"<autoFilter ref="{}"/>"#, af.to_a1());
        } else {
            let _ = write!(x, r#"<autoFilter ref="{}">"#, af.to_a1());
            crate::xlsx_extras::write_filter_columns(&mut x, columns);
            x.push_str("</autoFilter>");
        }
    }
    let _ = write!(x, r#"<tableColumns count="{}">"#, t.columns.len());
    for (i, c) in t.columns.iter().enumerate() {
        let mut attrs = format!(r#" id="{}" name="{}""#, i + 1, esc(&c.name));
        if let Some(f) = &c.totals_function {
            let _ = write!(attrs, r#" totalsRowFunction="{}""#, esc(f));
        }
        if let Some(l) = &c.totals_label {
            let _ = write!(attrs, r#" totalsRowLabel="{}""#, esc(l));
        }
        let mut inner = String::new();
        if let Some(f) = &c.calc_formula {
            let _ = write!(
                inner,
                "<calculatedColumnFormula>{}</calculatedColumnFormula>",
                ff(f)
            );
        }
        if let Some(f) = &c.totals_formula {
            let _ = write!(inner, "<totalsRowFormula>{}</totalsRowFormula>", ff(f));
        }
        if inner.is_empty() {
            let _ = write!(x, "<tableColumn{attrs}/>");
        } else {
            let _ = write!(x, "<tableColumn{attrs}>{inner}</tableColumn>");
        }
    }
    x.push_str("</tableColumns>");
    if !t.style.is_empty() {
        let _ = write!(
            x,
            r#"<tableStyleInfo name="{}" showFirstColumn="{}" showLastColumn="{}" showRowStripes="{}" showColumnStripes="{}"/>"#,
            esc(&t.style),
            u8::from(t.first_col),
            u8::from(t.last_col),
            u8::from(t.banded_rows),
            u8::from(t.banded_cols)
        );
    }
    x.push_str("</table>");
    x
}

#[cfg(test)]
mod tests {
    use super::*;
    use sp_core::{CellAddr, Value};
    use sp_model::{Cell, Workbook};
    use std::io::Cursor;

    fn rule(kind: CfKind, priority: u32, style: DiffStyle) -> CfRule {
        CfRule {
            kind,
            style,
            priority,
            stop_if_true: priority == 2,
        }
    }

    fn book() -> Workbook {
        let mut wb = Workbook::new();
        let s = &mut wb.sheets[0];
        for (a, v) in [("A1", "Item"), ("B1", "Qty"), ("C1", "Unit Price")] {
            s.set(CellAddr::parse(a).unwrap(), Cell::constant(Value::text(v)));
        }
        for r in 1..4u32 {
            s.set(CellAddr::new(r, 0), Cell::constant(Value::text("x")));
            s.set(
                CellAddr::new(r, 1),
                Cell::constant(Value::Number(f64::from(r))),
            );
            s.set(CellAddr::new(r, 2), Cell::formula("[@Qty]*2"));
        }
        s.set(CellAddr::new(4, 0), Cell::constant(Value::text("Total")));
        s.set(
            CellAddr::new(4, 2),
            Cell::formula("SUBTOTAL(109,Sales[Unit Price])"),
        );
        let mut t = Table::new(
            3,
            "Sales",
            CellRange::parse("A1:C5").unwrap(),
            vec!["Item".into(), "Qty".into(), "Unit Price".into()],
        );
        t.totals_row = true;
        t.style = "TableStyleDark3".into();
        t.banded_cols = true;
        t.first_col = true;
        t.columns[0].totals_label = Some("Total".into());
        t.columns[2].totals_function = Some("sum".into());
        t.columns[2].calc_formula = Some("[@Qty]*2".into());
        s.tables.push(t);
        let full = DiffStyle {
            font_color: Some(Color(1, 2, 3)),
            bold: Some(true),
            italic: Some(false),
            underline: Some(true),
            strike: Some(true),
            fill: Some(Color(200, 100, 50)),
            num_fmt: Some("0.0%".into()),
            border: Some(Border {
                style: BorderStyle::Dashed,
                color: Some(Color(9, 9, 9)),
            }),
        };
        let red = DiffStyle::preset("red");
        let rules = vec![
            rule(
                CfKind::CellIs {
                    op: CfOp::Between,
                    f1: "1".into(),
                    f2: "$B$2*2".into(),
                },
                1,
                full.clone(),
            ),
            rule(CfKind::Expression("MOD(ROW(),2)=0".into()), 2, red.clone()),
            rule(
                CfKind::Text {
                    op: TextOp::BeginsWith,
                    text: "a\"b".into(),
                },
                3,
                red.clone(),
            ),
            rule(CfKind::TimePeriod(TimePeriod::LastMonth), 4, red.clone()),
            rule(CfKind::Duplicates, 5, red.clone()),
            rule(CfKind::Unique, 6, red.clone()),
            rule(CfKind::Blanks, 7, red.clone()),
            rule(CfKind::NoErrors, 8, red.clone()),
            rule(
                CfKind::Top {
                    bottom: true,
                    percent: true,
                    rank: 15,
                },
                9,
                red.clone(),
            ),
            rule(
                CfKind::Average {
                    below: true,
                    equal: true,
                    std_dev: 2,
                },
                10,
                red.clone(),
            ),
            rule(
                CfKind::DataBar {
                    min: Cfvo::new(CfvoKind::Num, "0"),
                    max: Cfvo::new(CfvoKind::Percentile, "90"),
                    color: Color(10, 20, 30),
                    gradient: true,
                    show_value: false,
                },
                11,
                DiffStyle::default(),
            ),
            rule(
                CfKind::ColorScale {
                    points: vec![
                        (Cfvo::new(CfvoKind::Min, ""), Color(255, 0, 0)),
                        (Cfvo::new(CfvoKind::Percentile, "50"), Color(255, 255, 0)),
                        (Cfvo::new(CfvoKind::Max, ""), Color(0, 255, 0)),
                    ],
                },
                12,
                DiffStyle::default(),
            ),
            rule(
                CfKind::IconSet {
                    set: "4Rating".into(),
                    points: vec![
                        Cfvo::new(CfvoKind::Percent, "0"),
                        Cfvo::new(CfvoKind::Percent, "25"),
                        Cfvo {
                            gte: false,
                            ..Cfvo::new(CfvoKind::Num, "50")
                        },
                        Cfvo::new(CfvoKind::Formula, "$B$2"),
                    ],
                    reverse: true,
                    show_value: false,
                },
                13,
                DiffStyle::default(),
            ),
        ];
        s.cond_formats.push(CondFormat {
            ranges: vec![
                CellRange::parse("B2:B4").unwrap(),
                CellRange::parse("E1:E9").unwrap(),
            ],
            rules,
        });
        s.cond_formats.push(CondFormat {
            ranges: vec![CellRange::parse("H1:H5").unwrap()],
            rules: vec![rule(
                CfKind::IconSet {
                    set: "3Stars".into(),
                    points: vec![
                        Cfvo::new(CfvoKind::Percent, "0"),
                        Cfvo::new(CfvoKind::Percent, "33"),
                        Cfvo::new(CfvoKind::Percent, "67"),
                    ],
                    reverse: false,
                    show_value: true,
                },
                14,
                DiffStyle::default(),
            )],
        });
        wb
    }

    #[test]
    fn round_trip_cf_and_tables() {
        let wb = book();
        let mut buf = Cursor::new(Vec::new());
        crate::xlsx_write::write_xlsx(&wb, &mut buf).unwrap();
        let (back, report) = crate::xlsx_read::read_xlsx(Cursor::new(buf.into_inner())).unwrap();
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        let (a, b) = (&wb.sheets[0], &back.sheets[0]);
        assert_eq!(a.tables, b.tables);
        assert_eq!(a.cond_formats.len(), b.cond_formats.len());
        for (x, y) in a.cond_formats.iter().zip(&b.cond_formats) {
            assert_eq!(x.ranges, y.ranges);
            for (rx, ry) in x.rules.iter().zip(&y.rules) {
                assert_eq!(rx, ry);
            }
            assert_eq!(x.rules.len(), y.rules.len());
        }
        // Structured references survive and evaluate.
        assert_eq!(
            b.get(CellAddr::new(4, 2)).and_then(|c| c.formula_src()),
            Some("SUBTOTAL(109,Sales[Unit Price])")
        );
        // `[@Qty]` is saved qualified (`Sales[[#This Row],[Qty]]`) and reads back as such.
        assert_eq!(
            b.get(CellAddr::new(1, 2)).and_then(|c| c.formula_src()),
            Some("Sales[@Qty]*2")
        );
        // Write again: identical package content for the CF/table parts.
        let mut buf2 = Cursor::new(Vec::new());
        crate::xlsx_write::write_xlsx(&back, &mut buf2).unwrap();
        let (again, _) = crate::xlsx_read::read_xlsx(Cursor::new(buf2.into_inner())).unwrap();
        assert_eq!(again.sheets[0].cond_formats, back.sheets[0].cond_formats);
        assert_eq!(again.sheets[0].tables, back.sheets[0].tables);
    }

    #[test]
    fn styles_injection() {
        let mut w = CfWriter::new();
        w.dxf_id(&DiffStyle::preset("red"));
        let s =
            w.styles_xml("<styleSheet><cellXfs/><dxfs count=\"0\"/><tableStyles/></styleSheet>");
        assert!(s.contains("<dxfs count=\"1\"><dxf>"));
        assert_eq!(s.matches("<dxfs").count(), 1);
        let s = w.styles_xml("<styleSheet><cellXfs/></styleSheet>");
        assert!(s.ends_with("</dxfs></styleSheet>"));
    }

    #[test]
    fn hostile_input_is_skipped() {
        let mut sheet = Sheet::new("S");
        let xml = r#"<worksheet><sheetData/><conditionalFormatting sqref="ZZZZZZ1 A1"><cfRule type="cellIs" operator="bogus"/><cfRule type="colorScale"><colorScale><cfvo type="min"/></colorScale></cfRule><cfRule type="top10" rank="99999999999" dxfId="999"/></conditionalFormatting></worksheet>"#;
        let mut zip = zip::ZipArchive::new(Cursor::new(empty_zip())).unwrap();
        let mut rep = LoadReport::default();
        read_sheet(
            &mut zip,
            "xl/worksheets/sheet1.xml",
            xml,
            &mut sheet,
            &[],
            &mut rep,
        );
        assert_eq!(sheet.cond_formats.len(), 1);
        assert_eq!(sheet.cond_formats[0].rules.len(), 1);
    }

    fn empty_zip() -> Vec<u8> {
        let mut buf = Cursor::new(Vec::new());
        let w = zip::ZipWriter::new(&mut buf);
        w.finish().unwrap();
        buf.into_inner()
    }
}

#[cfg(test)]
mod ext_tests {
    use super::*;
    use sp_model::Workbook;
    use sp_model::passthrough::RawElement;
    use std::io::{Cursor, Read};

    #[test]
    fn captured_extlst_merges_without_duplicate_cf_ext() {
        let mut wb = Workbook::new();
        let s = &mut wb.sheets[0];
        s.passthrough.root_attrs.push((
            "xmlns:x14".into(),
            "http://schemas.microsoft.com/office/spreadsheetml/2009/9/main".into(),
        ));
        s.passthrough.elements.push(RawElement {
            name: "extLst".into(),
            after: "pageMargins".into(),
            xml: format!(
                r#"<extLst><ext uri="{CF_EXT}"><x14:conditionalFormattings/></ext><ext uri="{{OTHER}}"><x14:keep/></ext></extLst>"#
            ),
        });
        let with_bar = |s: &mut Sheet| {
            s.cond_formats.push(CondFormat {
                ranges: vec![CellRange::parse("A1:A3").unwrap()],
                rules: vec![CfRule {
                    kind: CfKind::DataBar {
                        min: Cfvo::new(CfvoKind::Min, ""),
                        max: Cfvo::new(CfvoKind::Max, ""),
                        color: DEFAULT_BAR,
                        gradient: false,
                        show_value: true,
                    },
                    style: DiffStyle::default(),
                    priority: 1,
                    stop_if_true: false,
                }],
            })
        };
        let sheet_xml = |wb: &Workbook| {
            let mut buf = Cursor::new(Vec::new());
            crate::xlsx_write::write_xlsx(wb, &mut buf).unwrap();
            let bytes = buf.into_inner();
            assert!(crate::xlsx_passthrough::validate_package(&bytes).is_empty());
            let mut z = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
            let mut x = String::new();
            z.by_name("xl/worksheets/sheet1.xml")
                .unwrap()
                .read_to_string(&mut x)
                .unwrap();
            x
        };
        // No rules: the stale CF ext is dropped, the other ext kept.
        let x = sheet_xml(&wb);
        assert_eq!(x.matches(CF_EXT).count(), 0, "{x}");
        assert_eq!(x.matches("<extLst>").count(), 1);
        assert!(x.contains("x14:keep"));
        with_bar(&mut wb.sheets[0]);
        let x = sheet_xml(&wb);
        assert_eq!(x.matches(&format!(r#"uri="{CF_EXT}""#)).count(), 1, "{x}");
        // One extLst on the data bar rule, one on the sheet.
        assert_eq!(x.matches("<extLst>").count(), 2);
        assert_eq!(x.matches("x14:keep").count(), 1);
        assert!(x.contains("x14:keep") && x.contains("x14:dataBar"));
        // No captured extLst at all.
        let mut plain = Workbook::new();
        with_bar(&mut plain.sheets[0]);
        let x = sheet_xml(&plain);
        assert_eq!(x.matches("<extLst>").count(), 2);
    }
}
