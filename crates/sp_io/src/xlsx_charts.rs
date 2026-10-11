//! DrawingML charts (`xl/drawings/drawingN.xml` → `xl/charts/chartN.xml`, histogram as a
//! chartex part) and sparklines (worksheet `extLst`), read and written for the chart kinds
//! in `sp_model::chart`. Element names follow ECMA-376 / MS-XLSX (interoperability facts).

use crate::IoError;
use crate::xlsx_extras::SheetRels;
use crate::xml::{Node, esc, parse_dom};
use sp_core::addr::split_sheet_prefix;
use sp_core::{CellAddr, CellRange, MAX_COLS, MAX_ROWS, Value};
use sp_model::chart::{
    Anchor, Chart, ChartKind, EMU_PER_PX, Grouping, LegendPos, Series, SeriesKind, Sparkline,
    SparklineGroup, SparklineKind,
};
use sp_model::passthrough::Passthrough;
use sp_model::{Color, Sheet, Workbook};
use std::fmt::Write as _;

const NS_C: &str = "http://schemas.openxmlformats.org/drawingml/2006/chart";
const NS_A: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
const NS_R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const NS_XDR: &str = "http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing";
const NS_CX: &str = "http://schemas.microsoft.com/office/drawing/2014/chartex";
const NS_CX1: &str = "http://schemas.microsoft.com/office/drawing/2015/9/8/chartex";
const NS_MC: &str = "http://schemas.openxmlformats.org/markup-compatibility/2006";
const NS_X14: &str = "http://schemas.microsoft.com/office/spreadsheetml/2009/9/main";
const NS_XM: &str = "http://schemas.microsoft.com/office/excel/2006/main";
const REL_CHART: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart";
const REL_CHARTEX: &str = "http://schemas.microsoft.com/office/2014/relationships/chartEx";
const CT_DRAWING: &str = "application/vnd.openxmlformats-officedocument.drawing+xml";
const CT_CHART: &str = "application/vnd.openxmlformats-officedocument.drawingml.chart+xml";
const CT_CHARTEX: &str = "application/vnd.ms-office.chartex+xml";
/// Extension URI of the worksheet sparkline-groups extension.
const SPARKLINE_EXT: &str = "{05C60535-1F16-4fd2-B633-F4F36F0B64E0}";
/// Most cached points written per reference.
const MAX_CACHE: u64 = 10_000;
/// Most charts read from one drawing part.
const MAX_CHARTS: usize = 1000;

// ---------------------------------------------------------------------------------------
// Writer

/// What one sheet's charts add to the package.
#[derive(Default)]
pub struct DrawingParts {
    /// The worksheet's `<drawing r:id="…"/>` element.
    pub element: Option<String>,
    /// (part name, content, content type).
    pub parts: Vec<(String, String, String)>,
}

/// Resolve reference text against `sheet` to (sheet index, range).
fn resolve(wb: &Workbook, sheet: usize, text: &str) -> Option<(usize, CellRange)> {
    let t = text.trim().trim_start_matches('=');
    let (sn, body) = split_sheet_prefix(t);
    let idx = match sn {
        Some(n) => wb.sheet_index(&n)?,
        None => sheet,
    };
    CellRange::parse(body).ok().map(|r| (idx, r))
}

fn cached_values(wb: &Workbook, sheet: usize, text: &str) -> Vec<Value> {
    let Some((si, r)) = resolve(wb, sheet, text) else {
        return Vec::new();
    };
    let Some(sh) = wb.sheet(si) else {
        return Vec::new();
    };
    let Some(u) = sh.used_range() else {
        return Vec::new();
    };
    let r = CellRange::new(
        r.start,
        CellAddr::new(
            r.end.row.min(u.end.row.max(r.start.row)),
            r.end.col.min(u.end.col.max(r.start.col)),
        ),
    );
    r.iter()
        .take(MAX_CACHE as usize)
        .map(|a| sh.value(a).clone())
        .collect()
}

fn num_cache(vals: &[Value]) -> String {
    let mut x = format!(
        "<c:numCache><c:formatCode>General</c:formatCode><c:ptCount val=\"{}\"/>",
        vals.len()
    );
    for (i, v) in vals.iter().enumerate() {
        if let Value::Number(n) = v
            && n.is_finite()
        {
            let _ = write!(x, "<c:pt idx=\"{i}\"><c:v>{n}</c:v></c:pt>");
        }
    }
    x.push_str("</c:numCache>");
    x
}

fn str_cache(vals: &[Value]) -> String {
    let mut x = format!("<c:strCache><c:ptCount val=\"{}\"/>", vals.len());
    for (i, v) in vals.iter().enumerate() {
        if !v.is_empty() {
            let _ = write!(
                x,
                "<c:pt idx=\"{i}\"><c:v>{}</c:v></c:pt>",
                esc(&v.to_display())
            );
        }
    }
    x.push_str("</c:strCache>");
    x
}

/// Literal series name (`"Sales"`) → its text.
fn literal(name: &str) -> Option<String> {
    name.trim()
        .strip_prefix('"')
        .and_then(|x| x.strip_suffix('"'))
        .map(|x| x.replace("\"\"", "\""))
}

fn tx_xml(wb: &Workbook, sheet: usize, name: &str) -> String {
    if name.trim().is_empty() {
        return String::new();
    }
    match literal(name) {
        Some(t) => format!("<c:tx><c:v>{}</c:v></c:tx>", esc(&t)),
        None => format!(
            "<c:tx><c:strRef><c:f>{}</c:f>{}</c:strRef></c:tx>",
            esc(name),
            str_cache(&cached_values(wb, sheet, name))
        ),
    }
}

/// `<c:cat>`/`<c:val>`-style data source; numeric when every non-blank cell is a number.
fn data_ref(wb: &Workbook, sheet: usize, tag: &str, f: &str, force_num: bool) -> String {
    if f.trim().is_empty() {
        return String::new();
    }
    let vals = cached_values(wb, sheet, f);
    let numeric = force_num
        || vals
            .iter()
            .all(|v| matches!(v, Value::Number(_) | Value::Empty));
    if numeric {
        format!(
            "<c:{tag}><c:numRef><c:f>{}</c:f>{}</c:numRef></c:{tag}>",
            esc(f),
            num_cache(&vals)
        )
    } else {
        format!(
            "<c:{tag}><c:strRef><c:f>{}</c:f>{}</c:strRef></c:{tag}>",
            esc(f),
            str_cache(&vals)
        )
    }
}

fn rich(tag: &str, text: &str) -> String {
    format!(
        "<{tag}><c:tx><c:rich><a:bodyPr/><a:lstStyle/><a:p><a:r><a:t>{}</a:t></a:r></a:p></c:rich></c:tx><c:overlay val=\"0\"/></{tag}>",
        esc(text)
    )
}

fn hex(c: Color) -> String {
    format!("{:02X}{:02X}{:02X}", c.0, c.1, c.2)
}

fn sp_pr(color: Option<Color>, line: bool, no_line: bool) -> String {
    match (color, line) {
        (_, true) if no_line => "<c:spPr><a:ln><a:noFill/></a:ln></c:spPr>".into(),
        (Some(c), true) => format!(
            "<c:spPr><a:ln w=\"28575\"><a:solidFill><a:srgbClr val=\"{}\"/></a:solidFill></a:ln></c:spPr>",
            hex(c)
        ),
        (Some(c), false) => format!(
            "<c:spPr><a:solidFill><a:srgbClr val=\"{}\"/></a:solidFill></c:spPr>",
            hex(c)
        ),
        (None, _) => String::new(),
    }
}

fn dlbls(show_val: bool, show_pct: bool) -> String {
    let b = |x: bool| u8::from(x);
    format!(
        "<c:dLbls><c:showLegendKey val=\"0\"/><c:showVal val=\"{}\"/><c:showCatName val=\"0\"/><c:showSerName val=\"0\"/><c:showPercent val=\"{}\"/><c:showBubbleSize val=\"0\"/></c:dLbls>",
        b(show_val),
        b(show_pct)
    )
}

fn grouping_attr(g: Grouping, line: bool) -> &'static str {
    match g {
        Grouping::Clustered if line => "standard",
        Grouping::Clustered => "clustered",
        Grouping::Stacked => "stacked",
        Grouping::PercentStacked => "percentStacked",
    }
}

/// One `<c:ser>` element for drawing style `style`.
fn ser_xml(wb: &Workbook, sheet: usize, c: &Chart, i: usize, s: &Series, style: &str) -> String {
    let mut x = format!("<c:ser><c:idx val=\"{i}\"/><c:order val=\"{i}\"/>");
    x.push_str(&tx_xml(wb, sheet, &s.name));
    match style {
        "line" => {
            x.push_str(&sp_pr(s.color, true, false));
            if !c.markers {
                x.push_str("<c:marker><c:symbol val=\"none\"/></c:marker>");
            }
        }
        "scatter" => {
            x.push_str(&sp_pr(s.color, true, true));
            x.push_str("<c:marker><c:symbol val=\"circle\"/><c:size val=\"5\"/></c:marker>");
        }
        "bar" => {
            x.push_str(&sp_pr(s.color, false, false));
            x.push_str("<c:invertIfNegative val=\"0\"/>");
        }
        _ => x.push_str(&sp_pr(s.color, false, false)),
    }
    if style == "scatter" {
        x.push_str(&data_ref(wb, sheet, "xVal", &s.categories, false));
        x.push_str(&data_ref(wb, sheet, "yVal", &s.values, true));
        x.push_str("<c:smooth val=\"0\"/>");
    } else {
        x.push_str(&data_ref(wb, sheet, "cat", &s.categories, false));
        x.push_str(&data_ref(wb, sheet, "val", &s.values, true));
        if style == "line" {
            x.push_str("<c:smooth val=\"0\"/>");
        }
    }
    x.push_str("</c:ser>");
    x
}

fn group_xml(
    wb: &Workbook,
    sheet: usize,
    c: &Chart,
    style: &str,
    series: &[(usize, &Series)],
    ax: (u32, u32),
) -> String {
    let mut x = String::new();
    let sers: String = series
        .iter()
        .map(|(i, s)| ser_xml(wb, sheet, c, *i, s, style))
        .collect();
    let axids = format!("<c:axId val=\"{}\"/><c:axId val=\"{}\"/>", ax.0, ax.1);
    let lbl = dlbls(c.data_labels, false);
    match style {
        "bar" => {
            let dir = if c.kind == ChartKind::Bar {
                "bar"
            } else {
                "col"
            };
            let stacked = c.grouping != Grouping::Clustered && c.kind != ChartKind::Combo;
            let g = if c.kind == ChartKind::Combo {
                Grouping::Clustered
            } else {
                c.grouping
            };
            let _ = write!(
                x,
                "<c:barChart><c:barDir val=\"{dir}\"/><c:grouping val=\"{}\"/><c:varyColors val=\"0\"/>{sers}{lbl}<c:gapWidth val=\"150\"/>{}{axids}</c:barChart>",
                grouping_attr(g, false),
                if stacked {
                    "<c:overlap val=\"100\"/>"
                } else {
                    ""
                }
            );
        }
        "line" => {
            let g = if c.kind == ChartKind::Combo {
                Grouping::Clustered
            } else {
                c.grouping
            };
            let _ = write!(
                x,
                "<c:lineChart><c:grouping val=\"{}\"/><c:varyColors val=\"0\"/>{sers}{lbl}<c:marker val=\"1\"/>{axids}</c:lineChart>",
                grouping_attr(g, true)
            );
        }
        "area" => {
            let g = if c.kind == ChartKind::Combo {
                Grouping::Clustered
            } else {
                c.grouping
            };
            let _ = write!(
                x,
                "<c:areaChart><c:grouping val=\"{}\"/><c:varyColors val=\"0\"/>{sers}{lbl}{axids}</c:areaChart>",
                grouping_attr(g, true)
            );
        }
        "scatter" => {
            let _ = write!(
                x,
                "<c:scatterChart><c:scatterStyle val=\"lineMarker\"/><c:varyColors val=\"0\"/>{sers}{lbl}{axids}</c:scatterChart>"
            );
        }
        "pie" => {
            let _ = write!(
                x,
                "<c:pieChart><c:varyColors val=\"1\"/>{sers}{}<c:firstSliceAng val=\"0\"/></c:pieChart>",
                dlbls(false, c.data_labels)
            );
        }
        "doughnut" => {
            let _ = write!(
                x,
                "<c:doughnutChart><c:varyColors val=\"1\"/>{sers}{}<c:firstSliceAng val=\"0\"/><c:holeSize val=\"50\"/></c:doughnutChart>",
                dlbls(false, c.data_labels)
            );
        }
        _ => {}
    }
    x
}

#[allow(clippy::too_many_arguments)]
fn axis_xml(
    tag: &str,
    id: u32,
    cross: u32,
    pos: &str,
    deleted: bool,
    gridlines: bool,
    title: Option<&str>,
    crosses: &str,
    between: &str,
) -> String {
    let mut x = format!(
        "<c:{tag}><c:axId val=\"{id}\"/><c:scaling><c:orientation val=\"minMax\"/></c:scaling><c:delete val=\"{}\"/><c:axPos val=\"{pos}\"/>",
        u8::from(deleted)
    );
    if gridlines {
        x.push_str("<c:majorGridlines/>");
    }
    if let Some(t) = title {
        x.push_str(&rich("c:title", t));
    }
    if tag == "valAx" {
        x.push_str("<c:numFmt formatCode=\"General\" sourceLinked=\"1\"/>");
    }
    let _ = write!(
        x,
        "<c:majorTickMark val=\"out\"/><c:minorTickMark val=\"none\"/><c:tickLblPos val=\"nextTo\"/><c:crossAx val=\"{cross}\"/><c:crosses val=\"{crosses}\"/>"
    );
    if tag == "valAx" {
        let _ = write!(x, "<c:crossBetween val=\"{between}\"/>");
    } else {
        x.push_str("<c:auto val=\"1\"/><c:lblAlgn val=\"ctr\"/><c:lblOffset val=\"100\"/>");
    }
    let _ = write!(x, "</c:{tag}>");
    x
}

/// `xl/charts/chartN.xml` for a non-histogram chart.
pub fn chart_xml(wb: &Workbook, sheet: usize, c: &Chart) -> String {
    let mut x = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<c:chartSpace xmlns:c=\"{NS_C}\" xmlns:a=\"{NS_A}\" xmlns:r=\"{NS_R}\"><c:roundedCorners val=\"0\"/><c:chart>"
    );
    if let Some(t) = &c.title {
        x.push_str(&rich("c:title", t));
    }
    let _ = write!(
        x,
        "<c:autoTitleDeleted val=\"{}\"/><c:plotArea><c:layout/>",
        u8::from(c.title.is_none())
    );
    let all: Vec<(usize, &Series)> = c.series.iter().enumerate().collect();
    let bar_pos = if c.kind == ChartKind::Bar {
        ("l", "b")
    } else {
        ("b", "l")
    };
    let xt = c.x_title.as_deref();
    let yt = c.y_title.as_deref();
    match c.kind {
        ChartKind::Pie | ChartKind::Doughnut => {
            let style = if c.kind == ChartKind::Pie {
                "pie"
            } else {
                "doughnut"
            };
            x.push_str(&group_xml(wb, sheet, c, style, &all, (0, 0)));
        }
        ChartKind::Scatter => {
            x.push_str(&group_xml(wb, sheet, c, "scatter", &all, (1, 2)));
            x.push_str(&axis_xml(
                "valAx", 1, 2, "b", false, false, xt, "autoZero", "midCat",
            ));
            x.push_str(&axis_xml(
                "valAx", 2, 1, "l", false, true, yt, "autoZero", "midCat",
            ));
        }
        ChartKind::Combo => {
            let style_of = |s: &Series| match s.kind {
                SeriesKind::Column => "bar",
                SeriesKind::Line => "line",
                SeriesKind::Area => "area",
            };
            let has_secondary = c.series.iter().any(|s| s.secondary);
            for style in ["area", "bar", "line"] {
                for secondary in [false, true] {
                    let part: Vec<(usize, &Series)> = all
                        .iter()
                        .filter(|(_, s)| style_of(s) == style && s.secondary == secondary)
                        .copied()
                        .collect();
                    if !part.is_empty() {
                        let ax = if secondary { (3, 4) } else { (1, 2) };
                        x.push_str(&group_xml(wb, sheet, c, style, &part, ax));
                    }
                }
            }
            x.push_str(&axis_xml(
                "catAx", 1, 2, "b", false, false, xt, "autoZero", "",
            ));
            x.push_str(&axis_xml(
                "valAx", 2, 1, "l", false, true, yt, "autoZero", "between",
            ));
            if has_secondary {
                x.push_str(&axis_xml(
                    "catAx", 3, 4, "b", true, false, None, "autoZero", "",
                ));
                x.push_str(&axis_xml(
                    "valAx", 4, 3, "r", false, false, None, "max", "between",
                ));
            }
        }
        _ => {
            let style = match c.kind {
                ChartKind::Line => "line",
                ChartKind::Area => "area",
                _ => "bar",
            };
            x.push_str(&group_xml(wb, sheet, c, style, &all, (1, 2)));
            x.push_str(&axis_xml(
                "catAx", 1, 2, bar_pos.0, false, false, xt, "autoZero", "",
            ));
            let between = if style == "area" { "midCat" } else { "between" };
            x.push_str(&axis_xml(
                "valAx", 2, 1, bar_pos.1, false, true, yt, "autoZero", between,
            ));
        }
    }
    x.push_str("</c:plotArea>");
    let pos = match c.legend {
        LegendPos::None => None,
        LegendPos::Right => Some("r"),
        LegendPos::Bottom => Some("b"),
        LegendPos::Top => Some("t"),
        LegendPos::Left => Some("l"),
    };
    if let Some(p) = pos {
        let _ = write!(
            x,
            "<c:legend><c:legendPos val=\"{p}\"/><c:overlay val=\"0\"/></c:legend>"
        );
    }
    x.push_str("<c:plotVisOnly val=\"1\"/><c:dispBlanksAs val=\"gap\"/></c:chart></c:chartSpace>");
    x
}

/// `xl/charts/chartExN.xml` for a histogram.
pub fn chartex_xml(wb: &Workbook, sheet: usize, c: &Chart) -> String {
    let s = c.series.first().cloned().unwrap_or_default();
    let vals = cached_values(wb, sheet, &s.values);
    let mut x = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<cx:chartSpace xmlns:a=\"{NS_A}\" xmlns:r=\"{NS_R}\" xmlns:cx=\"{NS_CX}\"><cx:chartData><cx:data id=\"0\"><cx:numDim type=\"val\"><cx:f>{}</cx:f><cx:lvl ptCount=\"{}\" formatCode=\"General\">",
        esc(&s.values),
        vals.len()
    );
    for (i, v) in vals.iter().enumerate() {
        if let Value::Number(n) = v
            && n.is_finite()
        {
            let _ = write!(x, "<cx:pt idx=\"{i}\">{n}</cx:pt>");
        }
    }
    x.push_str("</cx:lvl></cx:numDim></cx:data></cx:chartData><cx:chart>");
    if let Some(t) = &c.title {
        let _ = write!(
            x,
            "<cx:title pos=\"t\" align=\"ctr\" overlay=\"0\"><cx:tx><cx:txData><cx:v>{}</cx:v></cx:txData></cx:tx></cx:title>",
            esc(t)
        );
    }
    x.push_str("<cx:plotArea><cx:plotAreaRegion><cx:series layoutId=\"clusteredColumn\" uniqueId=\"{00000000-0000-0000-0000-000000000001}\">");
    if !s.name.trim().is_empty() {
        match literal(&s.name) {
            Some(t) => {
                let _ = write!(
                    x,
                    "<cx:tx><cx:txData><cx:v>{}</cx:v></cx:txData></cx:tx>",
                    esc(&t)
                );
            }
            None => {
                let shown = cached_values(wb, sheet, &s.name)
                    .first()
                    .map(Value::to_display)
                    .unwrap_or_default();
                let _ = write!(
                    x,
                    "<cx:tx><cx:txData><cx:f>{}</cx:f><cx:v>{}</cx:v></cx:txData></cx:tx>",
                    esc(&s.name),
                    esc(&shown)
                );
            }
        }
    }
    if let Some(col) = s.color {
        let _ = write!(
            x,
            "<cx:spPr><a:solidFill><a:srgbClr val=\"{}\"/></a:solidFill></cx:spPr>",
            hex(col)
        );
    }
    if c.data_labels {
        x.push_str("<cx:dataLabels pos=\"outEnd\"><cx:visibility seriesName=\"0\" categoryName=\"0\" value=\"1\"/></cx:dataLabels>");
    }
    x.push_str("<cx:dataId val=\"0\"/><cx:layoutPr><cx:binning intervalClosed=\"r\">");
    if let Some(b) = c.bins {
        let _ = write!(x, "<cx:binCount val=\"{b}\"/>");
    }
    x.push_str("</cx:binning></cx:layoutPr></cx:series></cx:plotAreaRegion><cx:axis id=\"0\"><cx:catScaling gapWidth=\"0\"/>");
    if let Some(t) = &c.x_title {
        let _ = write!(
            x,
            "<cx:title><cx:tx><cx:txData><cx:v>{}</cx:v></cx:txData></cx:tx></cx:title>",
            esc(t)
        );
    }
    x.push_str("<cx:tickLabels/></cx:axis><cx:axis id=\"1\"><cx:valScaling/>");
    if let Some(t) = &c.y_title {
        let _ = write!(
            x,
            "<cx:title><cx:tx><cx:txData><cx:v>{}</cx:v></cx:txData></cx:tx></cx:title>",
            esc(t)
        );
    }
    x.push_str("<cx:majorGridlines/><cx:tickLabels/></cx:axis></cx:plotArea>");
    let pos = match c.legend {
        LegendPos::None => None,
        LegendPos::Right => Some("r"),
        LegendPos::Bottom => Some("b"),
        LegendPos::Top => Some("t"),
        LegendPos::Left => Some("l"),
    };
    if let Some(p) = pos {
        let _ = write!(x, "<cx:legend pos=\"{p}\" align=\"ctr\" overlay=\"0\"/>");
    }
    x.push_str("</cx:chart></cx:chartSpace>");
    x
}

fn emu(px: f64) -> i64 {
    (px.max(0.0) * EMU_PER_PX).round().min(1e12) as i64
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

fn frame_xml(id: usize, name: &str, uri: &str, prefix: &str, ns: &str, rid: &str) -> String {
    format!(
        "<xdr:graphicFrame macro=\"\"><xdr:nvGraphicFramePr><xdr:cNvPr id=\"{id}\" name=\"{}\"/><xdr:cNvGraphicFramePr/></xdr:nvGraphicFramePr><xdr:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"0\" cy=\"0\"/></xdr:xfrm><a:graphic><a:graphicData uri=\"{uri}\"><{prefix}:chart xmlns:{prefix}=\"{ns}\" r:id=\"{rid}\"/></a:graphicData></a:graphic></xdr:graphicFrame>",
        esc(name)
    )
}

/// One `twoCellAnchor` holding chart `c` (relationship `rid`); declares its own namespaces
/// so it can also be appended to a drawing part written by another application.
fn anchor_xml(id: usize, c: &Chart, rid: &str) -> String {
    let name = if c.name.is_empty() {
        format!("Chart {id}")
    } else {
        c.name.clone()
    };
    let mut d = format!(
        "<xdr:twoCellAnchor xmlns:xdr=\"{NS_XDR}\" xmlns:a=\"{NS_A}\" xmlns:r=\"{NS_R}\">{}{}",
        marker_xml("from", c.anchor.from, c.anchor.from_off),
        marker_xml("to", c.anchor.to, c.anchor.to_off)
    );
    if c.kind == ChartKind::Histogram {
        let _ = write!(
            d,
            "<mc:AlternateContent xmlns:mc=\"{NS_MC}\"><mc:Choice xmlns:cx1=\"{NS_CX1}\" Requires=\"cx1\">{}</mc:Choice><mc:Fallback><xdr:sp macro=\"\" textlink=\"\"><xdr:nvSpPr><xdr:cNvPr id=\"{id}\" name=\"{}\"/><xdr:cNvSpPr/></xdr:nvSpPr><xdr:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"0\" cy=\"0\"/></a:xfrm><a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></xdr:spPr></xdr:sp></mc:Fallback></mc:AlternateContent>",
            frame_xml(id, &name, NS_CX, "cx", NS_CX, rid),
            esc(&name)
        );
    } else {
        d.push_str(&frame_xml(id, &name, NS_C, "c", NS_C, rid));
    }
    d.push_str("<xdr:clientData/></xdr:twoCellAnchor>");
    d
}

/// The chart part for `c`: (relationship type, content type, xml).
fn chart_part(wb: &Workbook, si: usize, c: &Chart) -> (&'static str, &'static str, String) {
    if c.kind == ChartKind::Histogram {
        (REL_CHARTEX, CT_CHARTEX, chartex_xml(wb, si, c))
    } else {
        (REL_CHART, CT_CHART, chart_xml(wb, si, c))
    }
}

fn chart_path(c: &Chart, g: usize) -> String {
    // (used for charts appended to a passed-through drawing)
    if c.kind == ChartKind::Histogram {
        format!("xl/charts/chartEx{g}.xml")
    } else {
        format!("xl/charts/chart{g}.xml")
    }
}

/// The sparkline-groups `<ext>` for a sheet's worksheet `extLst`.
fn sparkline_ext(sheet: &Sheet) -> String {
    let mut x = format!(
        "<ext uri=\"{SPARKLINE_EXT}\" xmlns:x14=\"{NS_X14}\"><x14:sparklineGroups xmlns:xm=\"{NS_XM}\">"
    );
    for g in &sheet.drawings.sparklines {
        x.push_str("<x14:sparklineGroup displayEmptyCellsAs=\"gap\"");
        match g.kind {
            SparklineKind::Line => {}
            SparklineKind::Column => x.push_str(" type=\"column\""),
            SparklineKind::WinLoss => x.push_str(" type=\"stacked\""),
        }
        for (k, on) in [
            ("markers", g.markers),
            ("high", g.high),
            ("low", g.low),
            ("first", g.first),
            ("last", g.last),
            ("negative", g.negative),
        ] {
            if on {
                let _ = write!(x, " {k}=\"1\"");
            }
        }
        x.push('>');
        let rgb = |c: Color| format!("FF{}", hex(c));
        // Schema order: series, negative, axis, markers, first, last, high, low.
        for (tag, c) in [
            ("colorSeries", g.color.unwrap_or(Color(46, 110, 190))),
            (
                "colorNegative",
                g.negative_color.unwrap_or(Color(196, 66, 72)),
            ),
            (
                "colorMarkers",
                g.marker_color.unwrap_or(Color(46, 110, 190)),
            ),
            ("colorHigh", g.high_color.unwrap_or(Color(232, 126, 44))),
            ("colorLow", g.low_color.unwrap_or(Color(232, 126, 44))),
        ] {
            let _ = write!(x, "<x14:{tag} rgb=\"{}\"/>", rgb(c));
        }
        x.push_str("<x14:sparklines>");
        for s in &g.items {
            let _ = write!(
                x,
                "<x14:sparkline><xm:f>{}</xm:f><xm:sqref>{}</xm:sqref></x14:sparkline>",
                esc(&s.data.replace('$', "")),
                s.location.to_a1()
            );
        }
        x.push_str("</x14:sparklines></x14:sparklineGroup>");
    }
    x.push_str("</x14:sparklineGroups></ext>");
    x
}

/// Put the sheet's sparklines into the worksheet `extLst` being generated: on top of the
/// one already in `known` (conditional formatting may have built it) or else the captured
/// one, replacing any sparkline `<ext>` there so the URI appears once.
pub fn merge_sparkline_ext(known: &mut Vec<(String, String)>, sheet: &Sheet, pt: &Passthrough) {
    let generated = known.iter().position(|(n, _)| n == "extLst");
    let base: Option<String> = match generated {
        Some(i) => known.get(i).map(|(_, x)| x.clone()),
        None => pt
            .elements
            .iter()
            .find(|e| crate::xml::local(&e.name) == "extLst")
            .map(|e| e.xml.clone()),
    };
    let is_spark = |x: &str| {
        x.to_ascii_uppercase()
            .contains(&SPARKLINE_EXT.to_ascii_uppercase())
    };
    let mut kept: Vec<String> = Vec::new();
    let mut had = false;
    if let Some(b) = base.as_deref().filter(|b| !b.is_empty())
        && let Ok((_, children)) = crate::xlsx_passthrough::scan_children(b, true)
    {
        for ch in children {
            if is_spark(ch.xml) {
                had = true;
            } else {
                kept.push(ch.xml.to_string());
            }
        }
    }
    let ours = !sheet.drawings.sparklines.is_empty();
    if !ours && !had {
        return; // nothing of ours; whatever is there passes through unchanged
    }
    let mut x = String::new();
    if ours || !kept.is_empty() {
        x.push_str("<extLst>");
        for k in &kept {
            x.push_str(k);
        }
        if ours {
            x.push_str(&sparkline_ext(sheet));
        }
        x.push_str("</extLst>");
    }
    match generated.and_then(|i| known.get_mut(i)) {
        Some(slot) => slot.1 = x,
        None => known.push(("extLst".to_string(), x)),
    }
}

/// The captured `<drawing>` element of a sheet and the drawing part it points to.
fn captured_drawing(pt: &Passthrough) -> Option<(usize, String, String)> {
    let (i, el) = pt
        .elements
        .iter()
        .enumerate()
        .find(|(_, e)| crate::xml::local(&e.name) == "drawing")?;
    let rid = parse_dom(&el.xml).ok()?.attr("id")?.to_string();
    let rel = pt.rels.iter().find(|r| r.id == rid && !r.external)?;
    Some((i, rid, rel.target.clone()))
}

/// Whether this sheet's charts go into a drawing part the writer generates (no drawing was
/// passed through from the file).
fn writes_own_drawing(sheet: &Sheet) -> bool {
    !sheet.drawings.charts.is_empty()
        && sheet.passthrough.sheet_part.is_none()
        && captured_drawing(&sheet.passthrough).is_none()
}

/// Part names for the drawings and charts the writer generates, chosen to avoid every
/// captured part name (so pass-through parts keep theirs).
#[derive(Default)]
pub struct ChartPlan {
    /// Sheet index → (drawing part, chart parts in chart order).
    sheets: std::collections::HashMap<usize, (String, Vec<String>)>,
}

impl ChartPlan {
    pub fn new(wb: &Workbook) -> ChartPlan {
        let mut taken: std::collections::HashSet<String> = wb
            .passthrough
            .parts
            .iter()
            .chain(wb.sheets.iter().flat_map(|s| s.passthrough.parts.iter()))
            .chain(
                wb.sheets
                    .iter()
                    .filter_map(|s| s.passthrough.sheet_part.as_ref().map(|o| &o.part)),
            )
            .map(|p| p.path.to_ascii_lowercase())
            .collect();
        let mut fresh = |make: &dyn Fn(usize) -> String, start: usize| {
            let mut k = start;
            loop {
                let cand = make(k);
                if taken.insert(cand.to_ascii_lowercase()) || k > start + 1_000_000 {
                    return cand;
                }
                k += 1;
            }
        };
        let mut plan = ChartPlan::default();
        let mut g = 1;
        for (i, sh) in wb.sheets.iter().enumerate() {
            if !writes_own_drawing(sh) {
                continue;
            }
            let drawing = fresh(&|k| format!("xl/drawings/drawing{k}.xml"), i + 1);
            let charts = sh
                .drawings
                .charts
                .iter()
                .map(|c| {
                    let hist = c.kind == ChartKind::Histogram;
                    let p = fresh(
                        &|k| {
                            if hist {
                                format!("xl/charts/chartEx{k}.xml")
                            } else {
                                format!("xl/charts/chart{k}.xml")
                            }
                        },
                        g,
                    );
                    g += 1;
                    p
                })
                .collect();
            plan.sheets.insert(i, (drawing, charts));
        }
        plan
    }

    /// Every part name the plan generates (reserve before placing pass-through parts).
    pub fn part_names(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (d, charts) in self.sheets.values() {
            out.push(d.clone());
            out.push(crate::xlsx_passthrough::rels_path(d));
            out.extend(charts.iter().cloned());
        }
        out
    }
}

/// Charts added in SheetPro to a sheet whose drawing part was passed through from the file
/// (it holds pictures, shapes or chart kinds we don't model) are appended to that part, as
/// pass-through parts, so the sheet keeps a single drawing. Returns the workbook to write.
pub fn prepare_for_save(wb: &Workbook) -> std::borrow::Cow<'_, Workbook> {
    let needs = wb
        .sheets
        .iter()
        .any(|s| !s.drawings.charts.is_empty() && captured_drawing(&s.passthrough).is_some());
    if !needs {
        return std::borrow::Cow::Borrowed(wb);
    }
    let mut out = wb.clone();
    let mut taken: std::collections::HashSet<String> = wb
        .passthrough
        .parts
        .iter()
        .chain(wb.sheets.iter().flat_map(|s| s.passthrough.parts.iter()))
        .map(|p| p.path.to_ascii_lowercase())
        .collect();
    let mut m = 0usize;
    for (si, sheet) in out.sheets.iter_mut().enumerate() {
        if sheet.drawings.charts.is_empty() {
            continue;
        }
        let Some((_, _, target)) = captured_drawing(&sheet.passthrough) else {
            continue;
        };
        let charts = std::mem::take(&mut sheet.drawings.charts);
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
        let mut anchors = String::new();
        let mut new_parts = Vec::new();
        let mut new_rels = Vec::new();
        let existing_ids: std::collections::HashSet<String> = pt
            .parts
            .get(di)
            .map(|p| p.rels.iter().map(|r| r.id.clone()).collect())
            .unwrap_or_default();
        for (k, c) in charts.iter().enumerate() {
            let path = loop {
                m += 1;
                let cand = chart_path(c, 1000 + m);
                if taken.insert(cand.to_ascii_lowercase()) {
                    break cand;
                }
            };
            let mut j = k;
            let rid = loop {
                j += 1;
                let id = format!("rIdSp{j}");
                if !existing_ids.contains(&id) {
                    break id;
                }
            };
            anchors.push_str(&anchor_xml(5000 + k, c, &rid));
            let (rel_type, ct, xml) = chart_part(wb, si, c);
            new_rels.push(sp_model::passthrough::RawRel {
                id: rid,
                rel_type: rel_type.to_string(),
                target: path.clone(),
                external: false,
            });
            new_parts.push(sp_model::passthrough::RawPart {
                path,
                content_type: ct.to_string(),
                data: std::sync::Arc::from(xml.into_bytes()),
                rels: Vec::new(),
            });
        }
        text.insert_str(close, &anchors);
        if let Some(d) = pt.parts.get_mut(di) {
            d.data = std::sync::Arc::from(text.into_bytes());
            d.rels.extend(new_rels);
        }
        pt.parts.extend(new_parts);
    }
    std::borrow::Cow::Owned(out)
}

/// Sheet `si`'s charts as a drawing part plus chart parts (names from `plan`), with the
/// worksheet's `drawing` relationship registered in `rels`.
pub fn write_sheet_drawings(
    wb: &Workbook,
    si: usize,
    plan: &ChartPlan,
    rels: &mut SheetRels,
) -> DrawingParts {
    let mut out = DrawingParts::default();
    let (Some(sheet), Some((dpath, cpaths))) = (wb.sheet(si), plan.sheets.get(&si)) else {
        return out;
    };
    let dfile = dpath.rsplit('/').next().unwrap_or(dpath);
    let id = rels.add("drawing", &format!("../drawings/{dfile}"), false);
    out.element = Some(format!("<drawing r:id=\"{id}\"/>"));
    let mut d = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<xdr:wsDr xmlns:xdr=\"{NS_XDR}\" xmlns:a=\"{NS_A}\" xmlns:r=\"{NS_R}\">"
    );
    let mut drels = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
    );
    for (k, (c, path)) in sheet.drawings.charts.iter().zip(cpaths).enumerate() {
        let rid = format!("rId{}", k + 1);
        d.push_str(&anchor_xml(k + 2, c, &rid));
        let (rel_type, ct, xml) = chart_part(wb, si, c);
        let file = path.rsplit('/').next().unwrap_or(path);
        let _ = write!(
            drels,
            "<Relationship Id=\"{rid}\" Type=\"{rel_type}\" Target=\"../charts/{file}\"/>"
        );
        out.parts.push((path.clone(), xml, ct.to_string()));
    }
    d.push_str("</xdr:wsDr>");
    drels.push_str("</Relationships>");
    out.parts.push((dpath.clone(), d, CT_DRAWING.to_string()));
    out.parts.push((
        crate::xlsx_passthrough::rels_path(dpath),
        drels,
        String::new(),
    ));
    out
}

// ---------------------------------------------------------------------------------------
// Reader

fn find<'a>(n: &'a Node, name: &str) -> Option<&'a Node> {
    n.children.iter().find_map(|c| {
        if c.name == name {
            Some(c)
        } else {
            find(c, name)
        }
    })
}

/// The document element `name` (`parse_dom` returns it directly when it is the only one).
fn doc<'a>(root: &'a Node, name: &str) -> Option<&'a Node> {
    if root.name == name {
        Some(root)
    } else {
        root.child(name)
    }
}

fn val<'a>(n: &'a Node, child: &str) -> Option<&'a str> {
    n.child(child).and_then(|c| c.attr("val"))
}

fn flag(n: &Node, child: &str) -> Option<bool> {
    val(n, child).map(|v| v == "1" || v == "true")
}

fn text_of(n: &Node) -> String {
    n.all_text()
}

fn num_text(n: Option<&Node>) -> f64 {
    n.and_then(|x| x.text.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite())
        .unwrap_or(0.0)
}

fn marker(n: &Node) -> (CellAddr, (f64, f64)) {
    let u = |k: &str| {
        n.child(k)
            .and_then(|x| x.text.trim().parse::<u32>().ok())
            .unwrap_or(0)
    };
    (
        CellAddr::new(u("row").min(MAX_ROWS - 1), u("col").min(MAX_COLS - 1)),
        (
            num_text(n.child("colOff")) / EMU_PER_PX,
            num_text(n.child("rowOff")) / EMU_PER_PX,
        ),
    )
}

fn ext_px(n: Option<&Node>) -> (f64, f64) {
    let a = |k: &str| {
        n.and_then(|x| x.attr(k))
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite())
            .unwrap_or(0.0)
            / EMU_PER_PX
    };
    (a("cx"), a("cy"))
}

fn anchor_of(sheet: &Sheet, n: &Node) -> Anchor {
    let to_cell = |from: CellAddr, off: (f64, f64), size: (f64, f64)| {
        let (col, cx) = sp_render_free::col_at_px(sheet, from.col, off.0 + size.0);
        let (row, ry) = sp_render_free::row_at_px(sheet, from.row, off.1 + size.1);
        (CellAddr::new(row, col), (cx, ry))
    };
    match n.name.as_str() {
        "twoCellAnchor" => {
            let (from, from_off) = n.child("from").map(marker).unwrap_or_default();
            let (to, to_off) = n.child("to").map(marker).unwrap_or_default();
            Anchor {
                from,
                from_off,
                to,
                to_off,
            }
        }
        "oneCellAnchor" => {
            let (from, from_off) = n.child("from").map(marker).unwrap_or_default();
            let (to, to_off) = to_cell(from, from_off, ext_px(n.child("ext")));
            Anchor {
                from,
                from_off,
                to,
                to_off,
            }
        }
        _ => {
            let p = n.child("pos");
            let px = |k: &str| {
                p.and_then(|x| x.attr(k))
                    .and_then(|v| v.parse::<f64>().ok())
                    .filter(|v| v.is_finite())
                    .unwrap_or(0.0)
                    / EMU_PER_PX
            };
            let origin = CellAddr::new(0, 0);
            let (from, from_off) = to_cell(origin, (0.0, 0.0), (px("x"), px("y")));
            let (to, to_off) = to_cell(from, from_off, ext_px(n.child("ext")));
            Anchor {
                from,
                from_off,
                to,
                to_off,
            }
        }
    }
}

/// Pixel walking over column widths / row heights (mirrors `sp_render::chart`, which this
/// crate cannot depend on).
mod sp_render_free {
    use sp_model::Sheet;

    fn col_px(chars: f64) -> f64 {
        if chars <= 0.0 {
            0.0
        } else {
            (chars * 7.0 + 5.0).round()
        }
    }

    fn row_px(points: f64) -> f64 {
        if points <= 0.0 {
            0.0
        } else {
            (points * 4.0 / 3.0).round()
        }
    }

    pub fn col_at_px(sheet: &Sheet, origin: u32, px: f64) -> (u32, f64) {
        let mut c = origin;
        let mut left = px.max(0.0);
        while c + 1 < sp_core::MAX_COLS {
            let w = col_px(sheet.col_width(c));
            if left < w {
                break;
            }
            left -= w;
            c += 1;
        }
        (c, (left * 9525.0).round() / 9525.0)
    }

    pub fn row_at_px(sheet: &Sheet, origin: u32, px: f64) -> (u32, f64) {
        let mut r = origin;
        let mut top = px.max(0.0);
        let limit = origin.saturating_add(100_000).min(sp_core::MAX_ROWS - 1);
        while r < limit {
            let h = row_px(sheet.row_height(r));
            if top < h {
                break;
            }
            top -= h;
            r += 1;
        }
        (r, (top * 9525.0).round() / 9525.0)
    }
}

fn srgb(n: Option<&Node>) -> Option<Color> {
    let n = n?;
    let fill = n.child("solidFill").or_else(|| {
        n.child("ln")
            .filter(|l| l.child("noFill").is_none())
            .and_then(|l| l.child("solidFill"))
    })?;
    fill.child("srgbClr")
        .and_then(|c| c.attr("val"))
        .and_then(Color::from_hex)
}

fn ref_f(n: Option<&Node>) -> String {
    let Some(n) = n else {
        return String::new();
    };
    for k in ["numRef", "strRef", "multiLvlStrRef"] {
        if let Some(f) = n.child(k).and_then(|r| r.child("f")) {
            return f.text.trim().to_string();
        }
    }
    String::new()
}

fn series_of(ser: &Node, kind: SeriesKind) -> Series {
    let name = match ser.child("tx") {
        Some(tx) => match tx.child("strRef").and_then(|r| r.child("f")) {
            Some(f) => f.text.trim().to_string(),
            None => tx
                .child("v")
                .map(|v| format!("\"{}\"", v.text.replace('"', "\"\"")))
                .unwrap_or_default(),
        },
        None => String::new(),
    };
    Series {
        name,
        categories: ref_f(ser.child("cat").or_else(|| ser.child("xVal"))),
        values: ref_f(ser.child("val").or_else(|| ser.child("yVal"))),
        kind,
        secondary: false,
        color: srgb(ser.child("spPr")),
    }
}

fn group_kind(n: &Node) -> Option<(ChartKind, SeriesKind)> {
    Some(match n.name.as_str() {
        "barChart" => {
            if val(n, "barDir") == Some("bar") {
                (ChartKind::Bar, SeriesKind::Column)
            } else {
                (ChartKind::Column, SeriesKind::Column)
            }
        }
        "lineChart" => (ChartKind::Line, SeriesKind::Line),
        "areaChart" => (ChartKind::Area, SeriesKind::Area),
        "pieChart" => (ChartKind::Pie, SeriesKind::Column),
        "doughnutChart" => (ChartKind::Doughnut, SeriesKind::Column),
        "scatterChart" => (ChartKind::Scatter, SeriesKind::Line),
        _ => return None,
    })
}

fn rich_text(n: Option<&Node>) -> Option<String> {
    let n = n?;
    let t = n.child("tx").map(text_of).unwrap_or_default();
    Some(t)
}

/// Parse a `c:chartSpace` part.
pub fn parse_chart(xml: &str) -> Result<Option<Chart>, IoError> {
    let root = parse_dom(xml)?;
    let Some(space) = doc(&root, "chartSpace") else {
        return Ok(None);
    };
    if space.child("chartData").is_some() {
        return Ok(parse_chartex(space));
    }
    let Some(ch) = space.child("chart") else {
        return Ok(None);
    };
    let Some(plot) = ch.child("plotArea") else {
        return Ok(None);
    };
    // Any plot type we don't model (3D, stock, radar, bubble, surface…): not ours.
    if plot
        .children
        .iter()
        .any(|g| g.name.ends_with("Chart") && group_kind(g).is_none())
    {
        return Ok(None);
    }
    let groups: Vec<(&Node, ChartKind, SeriesKind)> = plot
        .children
        .iter()
        .filter_map(|g| group_kind(g).map(|(k, s)| (g, k, s)))
        .collect();
    let Some((first, kind0, _)) = groups.first() else {
        return Ok(None);
    };
    let primary_ax: Vec<&str> = first
        .children_named("axId")
        .filter_map(|a| a.attr("val"))
        .collect();
    let combo = groups.len() > 1;
    let mut c = Chart {
        kind: if combo { ChartKind::Combo } else { *kind0 },
        grouping: match val(first, "grouping") {
            Some("stacked") => Grouping::Stacked,
            Some("percentStacked") => Grouping::PercentStacked,
            _ => Grouping::Clustered,
        },
        ..Chart::default()
    };
    for (g, _, skind) in &groups {
        let axes: Vec<&str> = g
            .children_named("axId")
            .filter_map(|a| a.attr("val"))
            .collect();
        let secondary = combo && !axes.is_empty() && axes != primary_ax;
        for ser in g.children_named("ser") {
            let mut s = series_of(ser, if combo { *skind } else { SeriesKind::Column });
            s.secondary = secondary;
            if c.series.len() < 255 {
                c.series.push(s);
            }
        }
        if let Some(d) = g.child("dLbls")
            && (flag(d, "showVal") == Some(true) || flag(d, "showPercent") == Some(true))
        {
            c.data_labels = true;
        }
        if g.name == "lineChart" {
            let any_marker = g.children_named("ser").any(|s| {
                s.child("marker")
                    .is_none_or(|m| val(m, "symbol") != Some("none"))
            });
            c.markers = any_marker;
        }
    }
    if c.kind == ChartKind::Scatter {
        c.markers = true;
    }
    for ax in &plot.children {
        let title = rich_text(ax.child("title"));
        match ax.name.as_str() {
            "catAx" | "dateAx" if c.x_title.is_none() && val(ax, "delete") != Some("1") => {
                c.x_title = title;
            }
            "valAx" => {
                let horizontal = matches!(val(ax, "axPos"), Some("b" | "t"));
                if c.kind == ChartKind::Scatter && horizontal {
                    c.x_title = c.x_title.take().or(title);
                } else if c.kind == ChartKind::Bar && horizontal
                    || c.kind != ChartKind::Bar && !horizontal
                {
                    if c.y_title.is_none() && val(ax, "axPos") != Some("r") {
                        c.y_title = title;
                    }
                } else if c.x_title.is_none() {
                    c.x_title = title;
                }
            }
            _ => {}
        }
    }
    c.title = match ch.child("title") {
        Some(t) => {
            let txt = rich_text(Some(t)).unwrap_or_default();
            if txt.is_empty() {
                // Automatic title: the single series' cached name, else a placeholder.
                let cached = first
                    .children_named("ser")
                    .next()
                    .and_then(|s| s.child("tx"))
                    .map(text_of)
                    .filter(|t| !t.is_empty() && c.series.len() == 1);
                Some(cached.unwrap_or_else(|| "Chart Title".into()))
            } else {
                Some(txt)
            }
        }
        None => None,
    };
    c.legend = match ch.child("legend") {
        None => LegendPos::None,
        Some(l) => match val(l, "legendPos") {
            Some("b") => LegendPos::Bottom,
            Some("t") => LegendPos::Top,
            Some("l") => LegendPos::Left,
            _ => LegendPos::Right,
        },
    };
    Ok(Some(c))
}

fn cx_text(n: Option<&Node>) -> Option<String> {
    let td = n?.child("tx")?.child("txData")?;
    td.child("v").map(|v| v.text.clone())
}

fn parse_chartex(space: &Node) -> Option<Chart> {
    let ch = space.child("chart")?;
    let ser = find(ch, "series")?;
    let layout = ser.attr("layoutId").unwrap_or("");
    let binning = find(ser, "binning");
    if layout != "clusteredColumn" || binning.is_none() {
        return None;
    }
    let data_id = ser
        .child("dataId")
        .and_then(|d| d.attr("val"))
        .unwrap_or("0");
    let data = space
        .child("chartData")?
        .children_named("data")
        .find(|d| d.attr("id") == Some(data_id))?;
    let values = find(data, "f")
        .map(|f| f.text.trim().to_string())
        .unwrap_or_default();
    let name = ser
        .child("tx")
        .and_then(|t| t.child("txData"))
        .map(|td| match td.child("f") {
            Some(f) => f.text.trim().to_string(),
            None => td
                .child("v")
                .map(|v| format!("\"{}\"", v.text.replace('"', "\"\"")))
                .unwrap_or_default(),
        })
        .unwrap_or_default();
    let mut axes = find(ch, "plotArea")
        .map(|p| p.children_named("axis").collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter();
    let x_ax = axes.next();
    let y_ax = axes.next();
    Some(Chart {
        kind: ChartKind::Histogram,
        series: vec![Series {
            name,
            values,
            color: srgb(ser.child("spPr")),
            ..Series::default()
        }],
        title: ch
            .child("title")
            .map(|t| cx_text(Some(t)).unwrap_or_else(|| "Chart Title".into())),
        x_title: x_ax.and_then(|a| cx_text(a.child("title"))),
        y_title: y_ax.and_then(|a| cx_text(a.child("title"))),
        legend: match ch.child("legend").map(|l| l.attr("pos")) {
            None => LegendPos::None,
            Some(Some("b")) => LegendPos::Bottom,
            Some(Some("t")) => LegendPos::Top,
            Some(Some("l")) => LegendPos::Left,
            Some(_) => LegendPos::Right,
        },
        data_labels: ser.child("dataLabels").is_some(),
        bins: binning
            .and_then(|b| b.child("binCount"))
            .and_then(|b| b.attr("val"))
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|b| (1..=500).contains(b)),
        ..Chart::default()
    })
}

fn parse_sparklines(sheet_xml: &str, sheet: &mut Sheet) -> Result<(), IoError> {
    let Some(pos) = sheet_xml.find("sparklineGroups") else {
        return Ok(());
    };
    let Some(start) = sheet_xml.get(..pos).and_then(|h| h.rfind('<')) else {
        return Ok(());
    };
    let Some(end_rel) = sheet_xml
        .get(pos..)
        .and_then(|t| t.rfind("sparklineGroups>"))
    else {
        return Ok(());
    };
    let Some(frag) = sheet_xml.get(start..pos + end_rel + "sparklineGroups>".len()) else {
        return Ok(());
    };
    let root = parse_dom(frag)?;
    let Some(groups) = doc(&root, "sparklineGroups") else {
        return Ok(());
    };
    for g in groups.children_named("sparklineGroup") {
        let on = |k: &str| matches!(g.attr(k), Some("1" | "true"));
        let col = |k: &str| {
            g.child(k)
                .and_then(|c| c.attr("rgb"))
                .and_then(Color::from_hex)
        };
        let mut items = Vec::new();
        if let Some(list) = g.child("sparklines") {
            for s in list.children_named("sparkline") {
                let data = s
                    .child("f")
                    .map(|f| f.text.trim().to_string())
                    .unwrap_or_default();
                let Some(loc) = s
                    .child("sqref")
                    .and_then(|q| CellAddr::parse(q.text.trim()).ok())
                else {
                    continue;
                };
                if items.len() < 100_000 {
                    items.push(Sparkline {
                        location: loc,
                        data,
                    });
                }
            }
        }
        if items.is_empty() {
            continue;
        }
        sheet.drawings.sparklines.push(SparklineGroup {
            kind: match g.attr("type") {
                Some("column") => SparklineKind::Column,
                Some("stacked") => SparklineKind::WinLoss,
                _ => SparklineKind::Line,
            },
            items,
            color: col("colorSeries"),
            negative_color: col("colorNegative"),
            markers: on("markers"),
            high: on("high"),
            low: on("low"),
            first: on("first"),
            last: on("last"),
            negative: on("negative"),
            marker_color: col("colorMarkers"),
            high_color: col("colorHigh"),
            low_color: col("colorLow"),
        });
    }
    Ok(())
}

/// Read a sheet's sparklines (worksheet `extLst`).
pub fn read_sheet_sparklines(sheet_xml: &str, sheet: &mut Sheet) -> Result<(), IoError> {
    parse_sparklines(sheet_xml, sheet)
}

/// Chart parts' own relationships that carry nothing we must keep (style/colour presets).
const COSMETIC_CHART_RELS: &[&str] = &["chartStyle", "chartColorStyle", "themeOverride"];

/// The charts of a captured drawing part, if every object in it is a chart we model
/// (otherwise `None`: the drawing stays passed through). Also returns the part names the
/// charts occupy.
fn model_drawing(
    sheet: &Sheet,
    parts: &[&sp_model::passthrough::RawPart],
    drawing: &sp_model::passthrough::RawPart,
) -> Option<(Vec<Chart>, Vec<String>)> {
    let text = std::str::from_utf8(&drawing.data).ok()?;
    let root = parse_dom(text).ok()?;
    let ws = doc(&root, "wsDr")?;
    let mut charts = Vec::new();
    let mut paths = vec![drawing.path.clone()];
    for anchor in &ws.children {
        if !matches!(
            anchor.name.as_str(),
            "twoCellAnchor" | "oneCellAnchor" | "absoluteAnchor"
        ) || charts.len() >= MAX_CHARTS
        {
            return None;
        }
        // Exactly one chart frame and nothing else drawable.
        if ["pic", "sp", "grpSp", "cxnSp", "contentPart"]
            .iter()
            .any(|k| anchor.children.iter().any(|c| c.name == *k))
        {
            return None;
        }
        let rid = find(anchor, "chart").and_then(|c| c.attr("id"))?;
        let rel = drawing.rels.iter().find(|r| r.id == rid && !r.external)?;
        let part = parts.iter().find(|p| p.path == rel.target)?;
        if part
            .rels
            .iter()
            .any(|r| !COSMETIC_CHART_RELS.contains(&crate::xlsx_passthrough::rel_kind(&r.rel_type)))
        {
            return None;
        }
        let mut c = parse_chart(std::str::from_utf8(&part.data).ok()?).ok()??;
        c.anchor = anchor_of(sheet, anchor);
        c.name = find(anchor, "cNvPr")
            .and_then(|n| n.attr("name"))
            .unwrap_or("")
            .to_string();
        charts.push(c);
        paths.push(part.path.clone());
        paths.extend(
            part.rels
                .iter()
                .filter(|r| !r.external)
                .map(|r| r.target.clone()),
        );
    }
    (!charts.is_empty()).then_some((charts, paths))
}

/// After pass-through capture: a sheet whose drawing holds only charts we model takes them
/// into `Sheet::drawings` and drops the captured `drawing` element, relationship and parts,
/// so each chart is written exactly once (by `write_sheet_drawings`). Drawings with anything
/// else stay passed through verbatim, their charts unmodelled.
pub fn adopt_captured_charts(wb: &mut Workbook) {
    let wb_parts = wb.passthrough.parts.clone();
    for sheet in &mut wb.sheets {
        let Some((el, rid, target)) = captured_drawing(&sheet.passthrough) else {
            continue;
        };
        let modelled = {
            let all: Vec<&sp_model::passthrough::RawPart> = sheet
                .passthrough
                .parts
                .iter()
                .chain(wb_parts.iter())
                .collect();
            match all.iter().find(|p| p.path == target) {
                Some(d) => model_drawing(sheet, &all, d),
                None => None,
            }
        };
        let Some((charts, paths)) = modelled else {
            continue;
        };
        sheet.drawings.charts.extend(charts);
        let pt = &mut sheet.passthrough;
        if el < pt.elements.len() {
            pt.elements.remove(el);
        }
        pt.rels.retain(|r| r.id != rid);
        pt.parts.retain(|p| !paths.contains(&p.path));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sp_model::Cell;

    fn book() -> Workbook {
        let mut wb = Workbook::new();
        let s = &mut wb.sheets[0];
        for (r, (m, a, b)) in [("Jan", 4.0, 7.0), ("Feb", 6.0, 3.0), ("Mar", 5.0, 9.0)]
            .iter()
            .enumerate()
        {
            let r = r as u32 + 1;
            s.set(CellAddr::new(r, 0), Cell::constant(Value::text(*m)));
            s.set(CellAddr::new(r, 1), Cell::constant(Value::Number(*a)));
            s.set(CellAddr::new(r, 2), Cell::constant(Value::Number(*b)));
        }
        s.set(CellAddr::new(0, 1), Cell::constant(Value::text("North")));
        s.set(CellAddr::new(0, 2), Cell::constant(Value::text("South")));
        let ser = |col: &str, name: &str| Series {
            name: name.into(),
            categories: "Sheet1!$A$2:$A$4".into(),
            values: format!("Sheet1!${col}$2:${col}$4"),
            ..Series::default()
        };
        let kinds = [
            ChartKind::Column,
            ChartKind::Bar,
            ChartKind::Line,
            ChartKind::Pie,
            ChartKind::Doughnut,
            ChartKind::Area,
            ChartKind::Scatter,
            ChartKind::Histogram,
            ChartKind::Combo,
        ];
        for (i, k) in kinds.iter().enumerate() {
            let mut series = vec![ser("B", "Sheet1!$B$1"), ser("C", "\"Lit \"\"x\"\"\"")];
            if k.is_round() || *k == ChartKind::Histogram {
                series.truncate(1);
            }
            if *k == ChartKind::Combo {
                series[1].kind = SeriesKind::Line;
                series[1].secondary = true;
            }
            if *k == ChartKind::Column {
                series[0].color = Some(Color(10, 20, 30));
            }
            let i = i as u32;
            s.drawings.charts.push(Chart {
                name: format!("Chart {}", i + 1),
                kind: *k,
                grouping: if *k == ChartKind::Area {
                    Grouping::Stacked
                } else if *k == ChartKind::Bar {
                    Grouping::PercentStacked
                } else {
                    Grouping::Clustered
                },
                anchor: Anchor {
                    from: CellAddr::new(i * 16, 5),
                    from_off: (3.0, 2.0),
                    to: CellAddr::new(i * 16 + 15, 12),
                    to_off: (0.0, 0.0),
                },
                series,
                title: i.is_multiple_of(2).then(|| format!("Title {i}")),
                x_title: (*k != ChartKind::Pie)
                    .then(|| "X axis".to_string())
                    .filter(|_| !k.is_round()),
                y_title: (!k.is_round()).then(|| "Y axis".to_string()),
                legend: if i.is_multiple_of(3) {
                    LegendPos::None
                } else {
                    LegendPos::Bottom
                },
                data_labels: i % 2 == 1,
                markers: matches!(k, ChartKind::Line | ChartKind::Scatter),
                bins: (*k == ChartKind::Histogram).then_some(4),
            });
        }
        s.drawings.sparklines.push(SparklineGroup {
            kind: SparklineKind::Column,
            items: vec![
                Sparkline {
                    location: CellAddr::new(1, 3),
                    data: "Sheet1!B2:C2".into(),
                },
                Sparkline {
                    location: CellAddr::new(2, 3),
                    data: "Sheet1!B3:C3".into(),
                },
            ],
            color: Some(Color(1, 2, 3)),
            negative_color: Some(Color(4, 5, 6)),
            markers: true,
            high: true,
            low: false,
            first: true,
            last: false,
            negative: true,
            marker_color: Some(Color(7, 8, 9)),
            high_color: Some(Color(10, 11, 12)),
            low_color: Some(Color(13, 14, 15)),
        });
        s.drawings.sparklines.push(SparklineGroup {
            kind: SparklineKind::WinLoss,
            items: vec![Sparkline {
                location: CellAddr::new(5, 3),
                data: "Sheet1!B2:B4".into(),
            }],
            color: Some(Color(1, 2, 3)),
            negative_color: Some(Color(4, 5, 6)),
            marker_color: Some(Color(7, 8, 9)),
            high_color: Some(Color(10, 11, 12)),
            low_color: Some(Color(13, 14, 15)),
            ..SparklineGroup::default()
        });
        wb
    }

    fn round_trip(wb: &Workbook) -> Workbook {
        let mut buf = std::io::Cursor::new(Vec::new());
        crate::xlsx_write::write_xlsx(wb, &mut buf).unwrap();
        buf.set_position(0);
        crate::xlsx_read::read_xlsx(buf).unwrap().0
    }

    #[test]
    fn charts_and_sparklines_round_trip() {
        let wb = book();
        let back = round_trip(&wb);
        let (a, b) = (&wb.sheets[0].drawings, &back.sheets[0].drawings);
        assert_eq!(a.sparklines, b.sparklines);
        assert_eq!(a.charts.len(), b.charts.len());
        for (x, y) in a.charts.iter().zip(&b.charts) {
            let mut x = x.clone();
            // Pie/doughnut/histogram carry no axis titles in the file.
            if x.kind.is_round() {
                x.x_title = None;
                x.y_title = None;
            }
            if x.kind == ChartKind::Histogram {
                x.series[0].categories.clear();
            }
            // Offsets survive to EMU precision.
            assert!((x.anchor.from_off.0 - y.anchor.from_off.0).abs() < 1e-6);
            x.anchor = y.anchor;
            assert_eq!(&x, y, "{:?}", x.kind);
        }
        // Saving again is stable.
        let again = round_trip(&back);
        assert_eq!(again.sheets[0].drawings, back.sheets[0].drawings);
    }

    #[test]
    fn hostile_parts_do_not_panic() {
        for xml in [
            "",
            "<c:chartSpace/>",
            "<c:chartSpace><c:chart><c:plotArea><c:barChart><c:ser/></c:barChart></c:plotArea></c:chart></c:chartSpace>",
            "<cx:chartSpace><cx:chartData/><cx:chart><cx:plotArea><cx:plotAreaRegion><cx:series layoutId=\"clusteredColumn\"><cx:layoutPr><cx:binning><cx:binCount val=\"99999\"/></cx:binning></cx:layoutPr></cx:series></cx:plotAreaRegion></cx:plotArea></cx:chart></cx:chartSpace>",
        ] {
            let _ = parse_chart(xml);
        }
        let mut sh = Sheet::new("S");
        parse_sparklines("<x14:sparklineGroups><x14:sparklineGroup><x14:sparklines><x14:sparkline><xm:f>A1</xm:f><xm:sqref>ZZZZZ9</xm:sqref></x14:sparkline></x14:sparklines></x14:sparklineGroup></x14:sparklineGroups>", &mut sh).unwrap();
        assert!(sh.drawings.sparklines.is_empty());
        let _ = parse_sparklines("sparklineGroups", &mut sh);
        let n = parse_dom("<xdr:absoluteAnchor><xdr:pos x=\"1e300\" y=\"-5\"/><xdr:ext cx=\"NaN\" cy=\"9e99\"/></xdr:absoluteAnchor>").unwrap();
        if let Some(a) = doc(&n, "absoluteAnchor") {
            let _ = anchor_of(&sh, a);
        }
    }

    #[test]
    fn written_parts_are_wired_up() {
        let wb = book();
        let mut buf = std::io::Cursor::new(Vec::new());
        crate::xlsx_write::write_xlsx(&wb, &mut buf).unwrap();
        buf.set_position(0);
        let mut z = zip::ZipArchive::new(buf).unwrap();
        let mut get = |p: &str| {
            let mut s = String::new();
            std::io::Read::read_to_string(&mut z.by_name(p).unwrap(), &mut s).unwrap();
            s
        };
        let ct = get("[Content_Types].xml");
        assert!(ct.contains("/xl/drawings/drawing1.xml"));
        assert!(ct.contains("/xl/charts/chart1.xml"));
        assert!(ct.contains("/xl/charts/chartEx8.xml"));
        let sheet = get("xl/worksheets/sheet1.xml");
        assert!(sheet.contains("<drawing r:id="));
        assert!(sheet.contains("x14:sparklineGroups"));
        assert!(sheet.trim_end().ends_with("</extLst></worksheet>"));
        let rels = get("xl/worksheets/_rels/sheet1.xml.rels");
        assert!(rels.contains("../drawings/drawing1.xml"));
        let c1 = get("xl/charts/chart1.xml");
        assert!(c1.contains("<c:v>Jan</c:v>") && c1.contains("<c:v>4</c:v>"));
    }

    #[test]
    fn reads_corpus_style_pie() {
        let xml = r#"<c:chartSpace xmlns:c="x" xmlns:a="y"><c:chart><c:autoTitleDeleted val="0"/><c:plotArea><c:pieChart><c:varyColors val="1"/><c:ser><c:idx val="0"/><c:tx><c:strRef><c:f>Sheet1!$B$1</c:f><c:strCache><c:pt idx="0"><c:v>Sales</c:v></c:pt></c:strCache></c:strRef></c:tx><c:cat><c:strRef><c:f>Sheet1!$A$2:$A$5</c:f></c:strRef></c:cat><c:val><c:numRef><c:f>Sheet1!$B$2:$B$5</c:f></c:numRef></c:val></c:ser></c:pieChart></c:plotArea><c:legend><c:legendPos val="r"/></c:legend></c:chart></c:chartSpace>"#;
        let c = parse_chart(xml).unwrap().unwrap();
        assert_eq!(c.kind, ChartKind::Pie);
        assert_eq!(c.series[0].values, "Sheet1!$B$2:$B$5");
        assert_eq!(c.legend, LegendPos::Right);
        assert_eq!(c.title, None);
    }
}

#[cfg(test)]
mod passthrough_tests {
    use super::*;
    use sp_model::passthrough::{RawElement, RawPart, RawRel};

    fn rel(id: &str, kind: &str, target: &str) -> RawRel {
        RawRel {
            id: id.into(),
            rel_type: format!("{NS_R}/{kind}"),
            target: target.into(),
            external: false,
        }
    }

    fn save(wb: &Workbook) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        crate::xlsx_write::write_xlsx(wb, &mut buf).unwrap();
        buf.into_inner()
    }

    fn text(bytes: &[u8], name: &str) -> Option<String> {
        let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut s = String::new();
        std::io::Read::read_to_string(&mut z.by_name(name).ok()?, &mut s).ok()?;
        Some(s)
    }

    fn chart() -> Chart {
        Chart {
            name: "New".into(),
            series: vec![Series {
                values: "Sheet1!$A$1:$A$3".into(),
                ..Series::default()
            }],
            anchor: Anchor {
                from: CellAddr::new(1, 1),
                to: CellAddr::new(10, 6),
                ..Anchor::default()
            },
            ..Chart::default()
        }
    }

    /// A drawing with a picture stays passed through; a chart added in SheetPro is appended
    /// to it (one drawing per sheet), and reloading does not duplicate it.
    #[test]
    fn new_chart_joins_a_passed_through_drawing() {
        let mut wb = Workbook::new();
        let s = &mut wb.sheets[0];
        let drawing = format!(
            "<xdr:wsDr xmlns:xdr=\"{NS_XDR}\" xmlns:a=\"{NS_A}\" xmlns:r=\"{NS_R}\"><xdr:oneCellAnchor><xdr:from><xdr:col>0</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>0</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from><xdr:ext cx=\"9525\" cy=\"9525\"/><xdr:pic><xdr:blipFill><a:blip r:embed=\"rId1\"/></xdr:blipFill></xdr:pic><xdr:clientData/></xdr:oneCellAnchor></xdr:wsDr>"
        );
        s.passthrough.elements.push(RawElement {
            name: "drawing".into(),
            after: "pageMargins".into(),
            xml: "<drawing r:id=\"rId1\"/>".into(),
        });
        s.passthrough
            .rels
            .push(rel("rId1", "drawing", "xl/drawings/drawing1.xml"));
        s.passthrough.parts = vec![
            RawPart {
                path: "xl/drawings/drawing1.xml".into(),
                content_type: CT_DRAWING.into(),
                data: std::sync::Arc::from(drawing.into_bytes()),
                rels: vec![rel("rId1", "image", "xl/media/image1.png")],
            },
            RawPart {
                path: "xl/media/image1.png".into(),
                content_type: "image/png".into(),
                data: std::sync::Arc::from(b"\x89PNG".to_vec()),
                rels: Vec::new(),
            },
        ];
        s.drawings.charts.push(chart());
        let out = save(&wb);
        assert_eq!(
            crate::xlsx_passthrough::validate_package(&out),
            Vec::<String>::new()
        );
        let d = text(&out, "xl/drawings/drawing1.xml").unwrap();
        assert!(d.contains("xdr:pic") && d.contains("c:chart"), "{d}");
        let (back, _) = crate::xlsx_read::read_xlsx(std::io::Cursor::new(out.clone())).unwrap();
        assert!(back.sheets[0].drawings.charts.is_empty());
        let out2 = save(&back);
        assert_eq!(
            crate::xlsx_passthrough::validate_package(&out2),
            Vec::<String>::new()
        );
        let d2 = text(&out2, "xl/drawings/drawing1.xml").unwrap();
        assert_eq!(d2.matches("c:chart ").count(), 1);
        let sheet = text(&out2, "xl/worksheets/sheet1.xml").unwrap();
        assert_eq!(sheet.matches("<drawing ").count(), 1);
    }

    /// Our own drawing never collides with a captured part name.
    #[test]
    fn generated_parts_avoid_captured_names() {
        let mut wb = Workbook::new();
        wb.sheets[0].passthrough.parts.push(RawPart {
            path: "xl/charts/chart1.xml".into(),
            content_type: CT_CHART.into(),
            data: std::sync::Arc::from(b"<x/>".to_vec()),
            rels: Vec::new(),
        });
        wb.sheets[0].drawings.charts.push(chart());
        let out = save(&wb);
        assert_eq!(text(&out, "xl/charts/chart1.xml").as_deref(), Some("<x/>"));
        assert!(text(&out, "xl/charts/chart2.xml").is_some());
        let (back, _) = crate::xlsx_read::read_xlsx(std::io::Cursor::new(out)).unwrap();
        assert_eq!(back.sheets[0].drawings.charts.len(), 1);
    }

    /// The sparkline `<ext>` replaces a captured one and joins other captured extensions.
    #[test]
    fn sparkline_ext_is_written_once() {
        let mut wb = Workbook::new();
        let s = &mut wb.sheets[0];
        s.passthrough.elements.push(RawElement {
            name: "extLst".into(),
            after: "pageMargins".into(),
            xml: format!(
                "<extLst><ext uri=\"u\"><y/></ext><ext uri=\"{SPARKLINE_EXT}\" xmlns:x14=\"{NS_X14}\"><x14:sparklineGroups/></ext></extLst>"
            ),
        });
        s.drawings.sparklines.push(SparklineGroup {
            items: vec![Sparkline {
                location: CellAddr::new(0, 3),
                data: "Sheet1!A1:C1".into(),
            }],
            ..SparklineGroup::default()
        });
        let out = save(&wb);
        assert_eq!(
            crate::xlsx_passthrough::validate_package(&out),
            Vec::<String>::new()
        );
        let sheet = text(&out, "xl/worksheets/sheet1.xml").unwrap();
        assert_eq!(sheet.matches(SPARKLINE_EXT).count(), 1);
        assert!(sheet.contains("<ext uri=\"u\"><y/></ext>"));
        // Deleting the sparklines drops the stale captured ext too.
        let (mut back, _) = crate::xlsx_read::read_xlsx(std::io::Cursor::new(out)).unwrap();
        assert_eq!(back.sheets[0].drawings.sparklines.len(), 1);
        back.sheets[0].drawings.sparklines.clear();
        let sheet = text(&save(&back), "xl/worksheets/sheet1.xml").unwrap();
        assert_eq!(sheet.matches(SPARKLINE_EXT).count(), 0);
        assert!(sheet.contains("<ext uri=\"u\">"));
    }
}

#[cfg(test)]
mod corpus_tests {
    /// Oracle files with charts (skipped when the corpus has not been fetched).
    #[test]
    fn corpus_charts_load_and_round_trip() {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/files/xlsx/tests");
        for (f, min) in [
            ("libreoffice_888_example.xlsx", 1),
            ("custom_theme_colors.xlsx", 1),
            ("example.xlsx", 0),
        ] {
            let Ok(file) = std::fs::File::open(dir.join(f)) else {
                continue;
            };
            let (wb, _) = crate::xlsx_read::read_xlsx(std::io::BufReader::new(file)).unwrap();
            let n: usize = wb.sheets.iter().map(|s| s.drawings.charts.len()).sum();
            assert!(n >= min, "{f}: {n} charts");
            let mut buf = std::io::Cursor::new(Vec::new());
            crate::xlsx_write::write_xlsx(&wb, &mut buf).unwrap();
            buf.set_position(0);
            let (back, _) = crate::xlsx_read::read_xlsx(buf).unwrap();
            for (a, b) in wb.sheets.iter().zip(&back.sheets) {
                assert_eq!(a.drawings, b.drawings, "{f}");
            }
        }
    }
}
