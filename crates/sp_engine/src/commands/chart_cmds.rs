//! Charts and sparklines (`insert.chart.*`, `insert.sparkline.*`).
//!
//! Every chart command inserts a chart from the selection (or `{"range": …}`). With
//! `{"chart": N}` it edits chart N of the active sheet instead: the command's type is applied
//! (`insert.chart.recommended` keeps the current type) along with any of the other params,
//! `{"anchor": "H2:O17"}` moves/resizes it, and `{"delete": true}` removes it. All undoable.
//!
//! Also the read side used by the shell and the file writer: `chart_data` resolves a
//! chart's series references to values, `sparkline_values` a sparkline's data.

use crate::params::{bool_opt, parse_ref, range, str_opt, u32_opt};
use crate::{CmdError, CmdResult, CommandSpec, Json, Session};
use serde_json::json;
use sp_core::addr::{quote_sheet_name, split_sheet_prefix};
use sp_core::{CellAddr, CellRange, MAX_COLS, MAX_ROWS, Value};
use sp_model::chart::{
    Anchor, Chart, ChartKind, Grouping, LegendPos, Series, SeriesKind, Sparkline, SparklineGroup,
    SparklineKind,
};
use sp_model::{Color, Sheet, Workbook};
use sp_render::chart::{ChartData, SeriesData};

/// Most points read from one reference.
pub const MAX_POINTS: u64 = 10_000;
/// Most series a chart built from a selection gets.
const MAX_SERIES: u32 = 255;
/// Most charts / sparklines per sheet.
const MAX_OBJECTS: usize = 10_000;

// ---------------------------------------------------------------------------------------
// Reference resolution

/// Resolve reference text (`Sheet1!$B$2:$B$6`, `B2:B6`, a defined name) against `sheet`.
pub fn resolve_ref(wb: &Workbook, sheet: usize, text: &str) -> Option<(usize, CellRange)> {
    let t = text.trim().trim_start_matches('=').trim();
    if t.is_empty() {
        return None;
    }
    let (sn, body) = split_sheet_prefix(t);
    let idx = match sn {
        Some(name) => wb.sheet_index(&name)?,
        None => sheet,
    };
    if let Ok(r) = CellRange::parse(body) {
        return Some((idx, r));
    }
    let n = wb.name_lookup(body, idx)?;
    let (ns, nb) = split_sheet_prefix(n.formula.trim_start_matches('='));
    let nidx = match ns {
        Some(name) => wb.sheet_index(&name)?,
        None => idx,
    };
    CellRange::parse(nb).ok().map(|r| (nidx, r))
}

/// Cell values of a reference, row-major, capped at `MAX_POINTS` and clamped to the used
/// range (whole-column references stay cheap).
fn ref_values(wb: &Workbook, sheet: usize, text: &str) -> Vec<Value> {
    let Some((si, r)) = resolve_ref(wb, sheet, text) else {
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
    if r.cell_count() > MAX_POINTS {
        return r
            .iter()
            .take(MAX_POINTS as usize)
            .map(|a| sh.value(a).clone())
            .collect();
    }
    r.iter().map(|a| sh.value(a).clone()).collect()
}

fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) if n.is_finite() => Some(*n),
        _ => None,
    }
}

/// Text shown for a category cell (formatted like the grid).
fn category_text(wb: &Workbook, sheet: usize, text: &str) -> Vec<String> {
    let Some((si, r)) = resolve_ref(wb, sheet, text) else {
        return Vec::new();
    };
    let Some(sh) = wb.sheet(si) else {
        return Vec::new();
    };
    let end = sh.used_range().map_or(r.start, |u| {
        CellAddr::new(
            r.end.row.min(u.end.row.max(r.start.row)),
            r.end.col.min(u.end.col.max(r.start.col)),
        )
    });
    CellRange::new(r.start, end)
        .iter()
        .take(MAX_POINTS as usize)
        .map(|a| match sh.get(a) {
            Some(c) => {
                sp_render::display_cell(a, &c.value, None, wb.styles.get(c.style), false).text
            }
            None => String::new(),
        })
        .collect()
}

/// A series name: the referenced cell's text, a quoted literal, or the text itself.
fn series_name(wb: &Workbook, sheet: usize, text: &str) -> String {
    let t = text.trim();
    if t.is_empty() {
        return String::new();
    }
    if let Some(lit) = t.strip_prefix('"').and_then(|x| x.strip_suffix('"')) {
        return lit.replace("\"\"", "\"");
    }
    if resolve_ref(wb, sheet, t).is_some() {
        return ref_values(wb, sheet, t)
            .iter()
            .filter(|v| !v.is_empty())
            .map(Value::to_display)
            .collect::<Vec<_>>()
            .join(" ");
    }
    match sp_formula::eval_formula(wb, sheet, CellAddr::default(), t) {
        Value::Text(s) => s.to_string(),
        Value::Number(n) => Value::Number(n).to_display(),
        _ => t.to_string(),
    }
}

/// Resolve every series of `c` (on sheet `sheet`) to values for drawing.
pub fn chart_data(wb: &Workbook, sheet: usize, c: &Chart) -> ChartData {
    let first_cats = c
        .series
        .first()
        .map(|s| s.categories.as_str())
        .unwrap_or("");
    let (categories, xs) = if c.kind == ChartKind::Scatter {
        (
            Vec::new(),
            ref_values(wb, sheet, first_cats).iter().map(num).collect(),
        )
    } else {
        (category_text(wb, sheet, first_cats), Vec::new())
    };
    ChartData {
        kind: c.kind,
        grouping: c.grouping,
        categories,
        xs,
        series: c
            .series
            .iter()
            .map(|s| SeriesData {
                name: series_name(wb, sheet, &s.name),
                values: ref_values(wb, sheet, &s.values).iter().map(num).collect(),
                kind: s.kind,
                secondary: s.secondary,
                color: s.color,
            })
            .collect(),
        title: c.title.clone(),
        x_title: c.x_title.clone(),
        y_title: c.y_title.clone(),
        title_italic: c.title_italic,
        x_title_italic: c.x_title_italic,
        y_title_italic: c.y_title_italic,
        legend: c.legend,
        data_labels: c.data_labels,
        markers: c.markers,
        bins: c.bins,
    }
}

/// Values of one sparkline (blank / text cells are gaps).
pub fn sparkline_values(wb: &Workbook, sheet: usize, sp: &Sparkline) -> Vec<Option<f64>> {
    ref_values(wb, sheet, &sp.data).iter().map(num).collect()
}

// ---------------------------------------------------------------------------------------
// Building charts from a selection

fn abs_ref(sheet_name: &str, r: CellRange) -> String {
    let a = |x: CellAddr| {
        format!(
            "${}${}",
            sp_core::col_to_letters(x.col),
            u64::from(x.row) + 1
        )
    };
    let body = if r.is_single() {
        a(r.start)
    } else {
        format!("{}:{}", a(r.start), a(r.end))
    };
    format!("{}!{body}", quote_sheet_name(sheet_name))
}

/// `Sheet1!C2:D2`: a sparkline's data reference, relative as the file stores it.
fn rel_ref(sheet_name: &str, r: CellRange) -> String {
    format!("{}!{}", quote_sheet_name(sheet_name), r.to_a1())
}

/// Expand a single cell to its current region (the block of non-empty neighbours).
fn current_region(sh: &Sheet, r: CellRange) -> CellRange {
    if !r.is_single() {
        return r;
    }
    let filled = |a: CellAddr| !sh.value(a).is_empty();
    let mut cur = r;
    for _ in 0..2000 {
        let mut grown = cur;
        let (r0, r1, c0, c1) = (cur.start.row, cur.end.row, cur.start.col, cur.end.col);
        let col_has = |c: u32| (r0..=r1).any(|row| filled(CellAddr::new(row, c)));
        let row_has = |row: u32| (c0..=c1).any(|c| filled(CellAddr::new(row, c)));
        if c0 > 0 && col_has(c0 - 1) {
            grown.start.col = c0 - 1;
        }
        if c1 + 1 < MAX_COLS && col_has(c1 + 1) {
            grown.end.col = c1 + 1;
        }
        if r0 > 0 && row_has(r0 - 1) {
            grown.start.row = r0 - 1;
        }
        if r1 + 1 < MAX_ROWS && row_has(r1 + 1) {
            grown.end.row = r1 + 1;
        }
        if grown == cur {
            break;
        }
        cur = grown;
    }
    cur
}

/// Shape of a selection for chart building.
struct Layout {
    data: CellRange,
    header_row: bool,
    label_col: bool,
}

fn analyse(sh: &Sheet, r: CellRange) -> Layout {
    let is_text = |a: CellAddr| matches!(sh.value(a), Value::Text(_));
    let empty = |a: CellAddr| sh.value(a).is_empty();
    let tl = r.start;
    let multi_cols = r.cols() > 1;
    let multi_rows = r.rows() > 1;
    // First column holds labels: text (or blank) cells below the top row.
    let label_col = multi_cols
        && (empty(tl) && multi_rows
            || (r.start.row.saturating_add(u32::from(multi_rows))..=r.end.row)
                .take(1000)
                .any(|row| is_text(CellAddr::new(row, r.start.col))));
    let first_data_col = r.start.col + u32::from(label_col);
    let header_row = multi_rows
        && (first_data_col..=r.end.col)
            .take(1000)
            .any(|c| is_text(CellAddr::new(r.start.row, c)))
        && (first_data_col..=r.end.col).take(1000).all(|c| {
            !matches!(sh.value(CellAddr::new(r.start.row, c)), Value::Number(_)) || empty(tl)
        });
    Layout {
        data: r,
        header_row,
        label_col,
    }
}

/// Series from a selection: by columns when there are at least as many rows as columns.
fn series_from(sh: &Sheet, sheet_name: &str, r: CellRange, scatter: bool) -> Vec<Series> {
    let mut l = analyse(sh, r);
    if scatter && r.cols() > 1 {
        // First column = X values.
        l.label_col = true;
    }
    let body = CellRange::new(
        CellAddr::new(
            l.data.start.row + u32::from(l.header_row),
            l.data.start.col + u32::from(l.label_col),
        ),
        l.data.end,
    );
    if body.start.row > body.end.row || body.start.col > body.end.col {
        return Vec::new();
    }
    let by_cols = body.rows() >= body.cols() || scatter;
    let mut out = Vec::new();
    if by_cols {
        let cats = l.label_col.then(|| {
            CellRange::new(
                CellAddr::new(body.start.row, l.data.start.col),
                CellAddr::new(body.end.row, l.data.start.col),
            )
        });
        for c in (body.start.col..=body.end.col).take(MAX_SERIES as usize) {
            out.push(Series {
                name: if l.header_row {
                    abs_ref(
                        sheet_name,
                        CellRange::single(CellAddr::new(l.data.start.row, c)),
                    )
                } else {
                    format!("\"Series{}\"", out.len() + 1)
                },
                categories: cats.map(|x| abs_ref(sheet_name, x)).unwrap_or_default(),
                values: abs_ref(
                    sheet_name,
                    CellRange::new(
                        CellAddr::new(body.start.row, c),
                        CellAddr::new(body.end.row, c),
                    ),
                ),
                ..Series::default()
            });
        }
    } else {
        let cats = l.header_row.then(|| {
            CellRange::new(
                CellAddr::new(l.data.start.row, body.start.col),
                CellAddr::new(l.data.start.row, body.end.col),
            )
        });
        for row in (body.start.row..=body.end.row).take(MAX_SERIES as usize) {
            out.push(Series {
                name: if l.label_col {
                    abs_ref(
                        sheet_name,
                        CellRange::single(CellAddr::new(row, l.data.start.col)),
                    )
                } else {
                    format!("\"Series{}\"", out.len() + 1)
                },
                categories: cats.map(|x| abs_ref(sheet_name, x)).unwrap_or_default(),
                values: abs_ref(
                    sheet_name,
                    CellRange::new(
                        CellAddr::new(row, body.start.col),
                        CellAddr::new(row, body.end.col),
                    ),
                ),
                ..Series::default()
            });
        }
    }
    out
}

/// The type the recommended-charts command picks for a selection.
fn recommend(sh: &Sheet, r: CellRange) -> ChartKind {
    let l = analyse(sh, r);
    let data_cols = r.cols() - u32::from(l.label_col);
    let data_rows = r.rows() - u32::from(l.header_row);
    if !l.label_col && data_cols == 2 && data_rows > 2 {
        return ChartKind::Scatter;
    }
    if !l.label_col && data_cols == 1 && data_rows >= 20 {
        return ChartKind::Histogram;
    }
    // Labels that look like time (dates or years) → line.
    let first_label = CellAddr::new(r.start.row + u32::from(l.header_row), r.start.col);
    let timeish = !l.label_col && data_rows > 6 && data_cols == 1
        || l.label_col
            && {
                let v = sh.value(first_label);
                let style_is_date = sh.get(first_label).is_some_and(|c| {
                    c.style != sp_model::StyleId::DEFAULT && matches!(v, Value::Number(_))
                });
                style_is_date
                    || matches!(v, Value::Number(n) if (1900.0..=2100.0).contains(n) && n.fract() == 0.0)
            };
    if timeish {
        return ChartKind::Line;
    }
    ChartKind::Column
}

fn default_anchor(r: CellRange) -> Anchor {
    let col = r.end.col.saturating_add(2).min(MAX_COLS - 9);
    let row = r.start.row.min(MAX_ROWS - 16);
    Anchor {
        from: CellAddr::new(row, col),
        from_off: (0.0, 0.0),
        to: CellAddr::new(row + 15, col + 8),
        to_off: (0.0, 0.0),
    }
}

fn anchor_param(s: &Session, p: &Json) -> Result<Option<Anchor>, CmdError> {
    let Some(t) = str_opt(p, "anchor") else {
        if p.get("anchor").is_some_and(|v| !v.is_null()) {
            return Err(CmdError::Param(
                "'anchor' must be a range like \"H2:O17\"".into(),
            ));
        }
        return Ok(None);
    };
    let (_, r) = parse_ref(s, t)?;
    if r.cell_count() > u64::from(MAX_ROWS) * 64 {
        return Err(CmdError::Param("anchor range is too large".into()));
    }
    Ok(Some(Anchor {
        from: r.start,
        from_off: (0.0, 0.0),
        to: CellAddr::new(
            r.end.row.saturating_add(1).min(MAX_ROWS - 1),
            r.end.col.saturating_add(1).min(MAX_COLS - 1),
        ),
        to_off: (0.0, 0.0),
    }))
}

fn opt_text(p: &Json, k: &str) -> Result<Option<Option<String>>, CmdError> {
    match p.get(k) {
        None => Ok(None),
        Some(Json::Null) => Ok(Some(None)),
        Some(Json::String(t)) => {
            if t.chars().count() > 255 {
                return Err(CmdError::Param(format!(
                    "'{k}' is longer than 255 characters"
                )));
            }
            Ok(Some(Some(t.clone()).filter(|t| !t.is_empty())))
        }
        Some(_) => Err(CmdError::Param(format!("'{k}' must be text"))),
    }
}

fn color_param(p: &Json, k: &str) -> Result<Option<Color>, CmdError> {
    match str_opt(p, k) {
        None => Ok(None),
        Some(h) => Color::from_hex(h)
            .map(Some)
            .ok_or_else(|| CmdError::Param(format!("'{k}' must be RRGGBB"))),
    }
}

/// Apply the optional formatting params to a chart.
fn apply_params(s: &Session, c: &mut Chart, p: &Json) -> Result<(), CmdError> {
    if let Some(t) = opt_text(p, "title")? {
        c.title = t;
    }
    if let Some(t) = opt_text(p, "x_title")? {
        c.x_title = t;
    }
    if let Some(t) = opt_text(p, "y_title")? {
        c.y_title = t;
    }
    if let Some(b) = bool_opt(p, "title_italic") {
        c.title_italic = b;
    }
    if let Some(b) = bool_opt(p, "x_title_italic") {
        c.x_title_italic = b;
    }
    if let Some(b) = bool_opt(p, "y_title_italic") {
        c.y_title_italic = b;
    }
    if let Some(l) = p.get("legend") {
        c.legend = match l {
            Json::Bool(false) => LegendPos::None,
            Json::Bool(true) => LegendPos::Right,
            Json::String(t) => LegendPos::from_name(t)
                .ok_or_else(|| CmdError::Param("legend: right|bottom|top|left|none".into()))?,
            _ => return Err(CmdError::Param("legend: right|bottom|top|left|none".into())),
        };
    }
    if let Some(g) = str_opt(p, "grouping") {
        c.grouping = Grouping::from_name(g)
            .ok_or_else(|| CmdError::Param("grouping: clustered|stacked|percent".into()))?;
    }
    if let Some(b) = bool_opt(p, "data_labels") {
        c.data_labels = b;
    }
    if let Some(b) = bool_opt(p, "markers") {
        c.markers = b;
    }
    if let Some(b) = u32_opt(p, "bins")? {
        if b == 0 || b as usize > sp_render::chart::MAX_BINS {
            return Err(CmdError::Param("'bins' must be 1..500".into()));
        }
        c.bins = Some(b);
    }
    if let Some(a) = anchor_param(s, p)? {
        c.anchor = a;
    }
    if let Some(col) = color_param(p, "color")?
        && let Some(first) = c.series.first_mut()
    {
        first.color = Some(col);
    }
    if let Some(n) = str_opt(p, "name") {
        c.name = n.chars().take(255).collect();
    }
    Ok(())
}

/// Explicit series list: `[{"values": "B2:B9", "name"?: "B1", "categories"?: "A2:A9",
/// "line"?: bool, "secondary"?: bool, "color"?: "RRGGBB"}]`.
fn series_param(s: &Session, p: &Json) -> Result<Option<Vec<Series>>, CmdError> {
    let Some(v) = p.get("series") else {
        return Ok(None);
    };
    let arr = v
        .as_array()
        .ok_or_else(|| CmdError::Param("'series' must be an array".into()))?;
    if arr.len() > MAX_SERIES as usize {
        return Err(CmdError::Param("too many series".into()));
    }
    let sheet_name =
        s.wb.sheet(s.sheet_index())
            .map(|x| x.name.clone())
            .unwrap_or_default();
    let norm = |t: &str| -> Result<String, CmdError> {
        let (si, r) = parse_ref(s, t)?;
        let name =
            s.wb.sheet(si)
                .map_or(sheet_name.as_str(), |x| x.name.as_str());
        Ok(abs_ref(name, r))
    };
    let mut out = Vec::new();
    for item in arr {
        let values = norm(
            str_opt(item, "values")
                .ok_or_else(|| CmdError::Param("series need 'values'".into()))?,
        )?;
        let name = match str_opt(item, "name") {
            Some(n) if parse_ref(s, n).is_ok() => norm(n)?,
            Some(n) => format!("\"{}\"", n.replace('"', "\"\"")),
            None => format!("\"Series{}\"", out.len() + 1),
        };
        let categories = match str_opt(item, "categories") {
            Some(c) => norm(c)?,
            None => String::new(),
        };
        out.push(Series {
            name,
            categories,
            values,
            kind: if bool_opt(item, "line") == Some(true) {
                SeriesKind::Line
            } else {
                SeriesKind::Column
            },
            secondary: bool_opt(item, "secondary").unwrap_or(false),
            color: color_param(item, "color")?,
        });
    }
    Ok(Some(out))
}

/// Make the series of a combo chart: every series a column except the last, which becomes
/// a line on the secondary axis (when there are at least two).
fn comboize(series: &mut [Series]) {
    let n = series.len();
    for (i, s) in series.iter_mut().enumerate() {
        let last = i + 1 == n && n > 1;
        s.kind = if last || n == 1 {
            SeriesKind::Line
        } else {
            SeriesKind::Column
        };
        s.secondary = last;
    }
}

fn chart_json(i: usize, c: &Chart) -> Json {
    json!({
        "chart": i,
        "name": c.name,
        "type": c.kind.name(),
        "grouping": c.grouping.name(),
        "series": c.series.len(),
        "title": c.title,
        "legend": c.legend.name(),
        "anchor": CellRange::new(c.anchor.from, c.anchor.to).to_a1(),
    })
}

fn chart_cmd(s: &mut Session, p: &Json, kind: Option<ChartKind>) -> CmdResult {
    let sheet = s.sheet_index();
    let doughnut = bool_opt(p, "doughnut") == Some(true);
    let kind = kind.map(|k| {
        if k == ChartKind::Pie && doughnut {
            ChartKind::Doughnut
        } else {
            k
        }
    });
    if let Some(idx) = u32_opt(p, "chart")? {
        let idx = idx as usize;
        let existing =
            s.wb.sheet(sheet)
                .and_then(|sh| sh.drawings.charts.get(idx))
                .cloned()
                .ok_or_else(|| CmdError::Param(format!("no chart {idx} on this sheet")))?;
        if bool_opt(p, "delete") == Some(true) {
            s.transact("Delete Chart", |tx, _| {
                tx.touch_sheet(sheet);
                if let Some(sh) = tx.wb.sheet_mut(sheet)
                    && idx < sh.drawings.charts.len()
                {
                    sh.drawings.charts.remove(idx);
                }
                Ok(())
            })?;
            return Ok(json!({"deleted": idx}));
        }
        let mut c = existing;
        if let Some(k) = kind {
            if k == ChartKind::Combo && c.kind != ChartKind::Combo {
                comboize(&mut c.series);
            }
            if k != ChartKind::Combo {
                for ser in &mut c.series {
                    ser.kind = SeriesKind::Column;
                    ser.secondary = false;
                }
            }
            c.kind = k;
        }
        if let Some(series) = series_param(s, p)? {
            c.series = series;
        }
        apply_params(s, &mut c, p)?;
        let out = chart_json(idx, &c);
        s.transact("Format Chart", |tx, _| {
            tx.touch_sheet(sheet);
            if let Some(slot) = tx
                .wb
                .sheet_mut(sheet)
                .and_then(|sh| sh.drawings.charts.get_mut(idx))
            {
                *slot = c;
            }
            Ok(())
        })?;
        return Ok(out);
    }
    // Insert a new chart.
    let sh =
        s.wb.sheet(sheet)
            .ok_or_else(|| CmdError::Failed("no sheet".into()))?;
    if sh.drawings.charts.len() >= MAX_OBJECTS {
        return Err(CmdError::Failed("too many charts on this sheet".into()));
    }
    let explicit = series_param(s, p)?;
    let (_, sel) = range(s, p, "range")?;
    let r = match sh.used_range() {
        Some(u) => match current_region(sh, sel).intersect(&u) {
            Some(r) => r,
            None if explicit.is_some() => sel,
            None => {
                return Err(CmdError::Failed(
                    "Select a range that contains data to chart.".into(),
                ));
            }
        },
        None if explicit.is_some() => sel,
        None => {
            return Err(CmdError::Failed(
                "Select a range that contains data to chart.".into(),
            ));
        }
    };
    let kind = kind.unwrap_or_else(|| recommend(sh, r));
    let mut series = match explicit {
        Some(x) => x,
        None => series_from(sh, &sh.name, r, kind == ChartKind::Scatter),
    };
    if series.is_empty() {
        return Err(CmdError::Failed(
            "Select a range that contains data to chart.".into(),
        ));
    }
    match kind {
        ChartKind::Pie | ChartKind::Doughnut | ChartKind::Histogram => series.truncate(1),
        ChartKind::Combo => comboize(&mut series),
        _ => {}
    }
    let single = series.len() == 1;
    let title = if single && kind != ChartKind::Histogram {
        let n = series_name(&s.wb, sheet, series.first().map_or("", |x| x.name.as_str()));
        Some(n).filter(|n| !n.is_empty() && !n.starts_with("Series"))
    } else {
        None
    };
    let n = sh.drawings.charts.len();
    let mut c = Chart {
        name: format!("Chart {}", n + 1),
        kind,
        grouping: Grouping::Clustered,
        anchor: default_anchor(r),
        series,
        title: title.or_else(|| Some("Chart Title".into())),
        legend: if single && !kind.is_round() {
            LegendPos::None
        } else {
            LegendPos::Bottom
        },
        markers: kind == ChartKind::Scatter,
        ..Chart::default()
    };
    apply_params(s, &mut c, p)?;
    let out = chart_json(n, &c);
    s.transact("Insert Chart", |tx, _| {
        tx.touch_sheet(sheet);
        if let Some(sh) = tx.wb.sheet_mut(sheet) {
            sh.drawings.charts.push(c);
        }
        Ok(())
    })?;
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// Sparklines

fn sparkline_cmd(s: &mut Session, p: &Json, kind: SparklineKind) -> CmdResult {
    let sheet = s.sheet_index();
    if bool_opt(p, "delete") == Some(true) {
        let (_, loc) = range(s, p, "location")?;
        let n = s.wb.sheet(sheet).map_or(0, |sh| {
            sh.drawings
                .sparklines
                .iter()
                .flat_map(|g| &g.items)
                .filter(|x| loc.contains(x.location))
                .count()
        });
        if n == 0 {
            return Err(CmdError::Failed(
                "No sparklines in the selected cells.".into(),
            ));
        }
        s.transact("Clear Sparklines", |tx, _| {
            tx.touch_sheet(sheet);
            if let Some(sh) = tx.wb.sheet_mut(sheet) {
                for g in &mut sh.drawings.sparklines {
                    g.items.retain(|x| !loc.contains(x.location));
                }
                sh.drawings.sparklines.retain(|g| !g.items.is_empty());
            }
            Ok(())
        })?;
        return Ok(json!({"deleted": n}));
    }
    let (dsheet, data) = range(s, p, "data")?;
    let dname =
        s.wb.sheet(dsheet)
            .map(|x| x.name.clone())
            .unwrap_or_default();
    let loc = match str_opt(p, "location") {
        Some(t) => {
            let (ls, r) = parse_ref(s, t)?;
            if ls != sheet {
                return Err(CmdError::Param(
                    "location must be on the active sheet".into(),
                ));
            }
            r
        }
        None => {
            // Default: the column just right of the data, one cell per data row.
            let c = data.end.col.saturating_add(1).min(MAX_COLS - 1);
            CellRange::new(
                CellAddr::new(data.start.row, c),
                CellAddr::new(data.end.row, c),
            )
        }
    };
    if loc.cell_count() > MAX_POINTS || data.cell_count() > MAX_POINTS * 100 {
        return Err(CmdError::Param("range is too large for sparklines".into()));
    }
    let mut items = Vec::new();
    if loc.is_single() {
        if data.rows() > 1 && data.cols() > 1 {
            return Err(CmdError::Failed(
                "Location reference is not valid: the data must be one row or column for a single cell."
                    .into(),
            ));
        }
        items.push(Sparkline {
            location: loc.start,
            data: rel_ref(&dname, data),
        });
    } else if loc.cols() == 1 && loc.rows() == data.rows() {
        for (i, row) in (data.start.row..=data.end.row).enumerate() {
            items.push(Sparkline {
                location: CellAddr::new(loc.start.row + i as u32, loc.start.col),
                data: rel_ref(
                    &dname,
                    CellRange::new(
                        CellAddr::new(row, data.start.col),
                        CellAddr::new(row, data.end.col),
                    ),
                ),
            });
        }
    } else if loc.rows() == 1 && loc.cols() == data.cols() {
        for (i, col) in (data.start.col..=data.end.col).enumerate() {
            items.push(Sparkline {
                location: CellAddr::new(loc.start.row, loc.start.col + i as u32),
                data: rel_ref(
                    &dname,
                    CellRange::new(
                        CellAddr::new(data.start.row, col),
                        CellAddr::new(data.end.row, col),
                    ),
                ),
            });
        }
    } else {
        return Err(CmdError::Failed(
            "Data reference does not match the location range.".into(),
        ));
    }
    let g = SparklineGroup {
        kind,
        items,
        color: color_param(p, "color")?,
        negative_color: color_param(p, "negative_color")?,
        markers: bool_opt(p, "markers").unwrap_or(false),
        high: bool_opt(p, "high").unwrap_or(false),
        low: bool_opt(p, "low").unwrap_or(false),
        first: bool_opt(p, "first").unwrap_or(false),
        last: bool_opt(p, "last").unwrap_or(false),
        negative: bool_opt(p, "negative").unwrap_or(kind == SparklineKind::WinLoss),
        marker_color: color_param(p, "marker_color")?,
        high_color: color_param(p, "high_color")?,
        low_color: color_param(p, "low_color")?,
    };
    let count = g.items.len();
    if s.wb
        .sheet(sheet)
        .is_some_and(|sh| sh.drawings.sparklines.len() >= MAX_OBJECTS)
    {
        return Err(CmdError::Failed(
            "too many sparkline groups on this sheet".into(),
        ));
    }
    s.transact("Insert Sparklines", |tx, _| {
        tx.touch_sheet(sheet);
        if let Some(sh) = tx.wb.sheet_mut(sheet) {
            // New sparklines replace any existing ones in the same cells.
            for old in &mut sh.drawings.sparklines {
                old.items.retain(|x| !loc.contains(x.location));
            }
            sh.drawings.sparklines.retain(|x| !x.items.is_empty());
            sh.drawings.sparklines.push(g);
        }
        Ok(())
    })?;
    Ok(json!({"sparklines": count, "location": loc.to_a1()}))
}

pub fn specs() -> Vec<CommandSpec> {
    const CHART: &str = r#"{"range"?: "A1:D9", "series"?: [{"values": "B2:B9", "name"?: "B1", "categories"?: "A2:A9", "line"?: bool, "secondary"?: bool, "color"?: "RRGGBB"}], "title"?: "…"|null, "x_title"?, "y_title"?, "title_italic"?: bool, "x_title_italic"?: bool, "y_title_italic"?: bool, "legend"?: "right|bottom|top|left|none", "grouping"?: "clustered|stacked|percent", "data_labels"?: bool, "markers"?: bool, "bins"?: 1-500, "doughnut"?: bool, "color"?: "RRGGBB", "anchor"?: "H2:O17", "chart"?: N (edit chart N instead: retype + params), "delete"?: bool (with chart)}"#;
    const SPARK: &str = r#"{"data"?: "A2:E4" (default selection), "location"?: "F2:F4" (default: column right of the data), "markers"?, "high"?, "low"?, "first"?, "last"?, "negative"?: bool, "color"?, "negative_color"?, "marker_color"?, "high_color"?, "low_color"?: "RRGGBB", "delete"?: bool (clears sparklines in location)}"#;
    vec![
        CommandSpec::new("insert.chart.recommended", CHART, |s, p| {
            chart_cmd(s, p, None)
        }),
        CommandSpec::new("insert.chart.column", CHART, |s, p| {
            chart_cmd(s, p, Some(ChartKind::Column))
        }),
        CommandSpec::new("insert.chart.bar", CHART, |s, p| {
            chart_cmd(s, p, Some(ChartKind::Bar))
        }),
        CommandSpec::new("insert.chart.line", CHART, |s, p| {
            chart_cmd(s, p, Some(ChartKind::Line))
        }),
        CommandSpec::new("insert.chart.pie", CHART, |s, p| {
            chart_cmd(s, p, Some(ChartKind::Pie))
        }),
        CommandSpec::new("insert.chart.area", CHART, |s, p| {
            chart_cmd(s, p, Some(ChartKind::Area))
        }),
        CommandSpec::new("insert.chart.scatter", CHART, |s, p| {
            chart_cmd(s, p, Some(ChartKind::Scatter))
        }),
        CommandSpec::new("insert.chart.histogram", CHART, |s, p| {
            chart_cmd(s, p, Some(ChartKind::Histogram))
        }),
        CommandSpec::new("insert.chart.combo", CHART, |s, p| {
            chart_cmd(s, p, Some(ChartKind::Combo))
        }),
        CommandSpec::new("insert.sparkline.line", SPARK, |s, p| {
            sparkline_cmd(s, p, SparklineKind::Line)
        }),
        CommandSpec::new("insert.sparkline.column", SPARK, |s, p| {
            sparkline_cmd(s, p, SparklineKind::Column)
        }),
        CommandSpec::new("insert.sparkline.winloss", SPARK, |s, p| {
            sparkline_cmd(s, p, SparklineKind::WinLoss)
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sales() -> Session {
        let mut s = Session::new();
        let rows = [
            ["Region", "Q1", "Q2", "Q3"],
            ["North", "10", "14", "9"],
            ["South", "7", "11", "15"],
            ["East", "12", "8", "13"],
            ["West", "5", "9", "6"],
        ];
        for (r, row) in rows.iter().enumerate() {
            for (c, v) in row.iter().enumerate() {
                let a = CellAddr::new(r as u32, c as u32).to_a1();
                s.execute("cell.set", &json!({"cell": a, "input": v}))
                    .unwrap();
            }
        }
        s
    }

    fn charts(s: &Session) -> &Vec<Chart> {
        &s.wb.sheets[0].drawings.charts
    }

    #[test]
    fn insert_column_from_table() {
        let mut s = sales();
        let out = s
            .execute("insert.chart.column", &json!({"range": "A1:D5"}))
            .unwrap();
        assert_eq!(out["type"], "column");
        let c = &charts(&s)[0];
        assert_eq!(c.series.len(), 3);
        assert_eq!(c.series[0].name, "Sheet1!$B$1");
        assert_eq!(c.series[0].categories, "Sheet1!$A$2:$A$5");
        assert_eq!(c.series[0].values, "Sheet1!$B$2:$B$5");
        assert_eq!(c.anchor.from, CellAddr::new(0, 5));
        let d = chart_data(&s.wb, 0, c);
        assert_eq!(d.categories, vec!["North", "South", "East", "West"]);
        assert_eq!(d.series[1].name, "Q2");
        assert_eq!(
            d.series[2].values,
            vec![Some(9.0), Some(15.0), Some(13.0), Some(6.0)]
        );
        // Undo removes it.
        s.execute("edit.undo", &json!({})).unwrap();
        assert!(charts(&s).is_empty());
        s.execute("edit.redo", &json!({})).unwrap();
        assert_eq!(charts(&s).len(), 1);
    }

    #[test]
    fn single_cell_selection_expands_to_region() {
        let mut s = sales();
        s.execute("cell.set", &json!({"cell": "B3", "input": "7"}))
            .unwrap();
        s.select(CellAddr::new(2, 1));
        s.execute("insert.chart.bar", &json!({})).unwrap();
        assert_eq!(charts(&s)[0].series.len(), 3);
        // Live data: editing a cell changes the resolved values.
        s.execute("cell.set", &json!({"cell": "B2", "input": "99"}))
            .unwrap();
        let d = chart_data(&s.wb, 0, &charts(&s)[0]);
        assert_eq!(d.series[0].values[0], Some(99.0));
    }

    #[test]
    fn edit_move_retype_delete() {
        let mut s = sales();
        s.execute("insert.chart.column", &json!({"range": "A1:D5"}))
            .unwrap();
        s.execute(
            "insert.chart.line",
            &json!({"chart": 0, "title": "Sales", "legend": "right", "anchor": "B8:G20", "grouping": "stacked", "markers": true}),
        )
        .unwrap();
        let c = &charts(&s)[0];
        assert_eq!(c.kind, ChartKind::Line);
        assert_eq!(c.title.as_deref(), Some("Sales"));
        assert_eq!(c.legend, LegendPos::Right);
        assert_eq!(c.grouping, Grouping::Stacked);
        assert_eq!(c.anchor.from, CellAddr::new(7, 1));
        assert_eq!(c.anchor.to, CellAddr::new(20, 7));
        s.execute(
            "insert.chart.recommended",
            &json!({"chart": 0, "title": null}),
        )
        .unwrap();
        assert_eq!(charts(&s)[0].kind, ChartKind::Line);
        assert_eq!(charts(&s)[0].title, None);
        s.execute("insert.chart.combo", &json!({"chart": 0}))
            .unwrap();
        let c = &charts(&s)[0];
        assert_eq!(c.series[2].kind, SeriesKind::Line);
        assert!(c.series[2].secondary);
        assert!(!c.series[0].secondary);
        s.execute("insert.chart.pie", &json!({"chart": 0, "delete": true}))
            .unwrap();
        assert!(charts(&s).is_empty());
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(charts(&s).len(), 1);
        assert!(s.execute("insert.chart.pie", &json!({"chart": 7})).is_err());
    }

    #[test]
    fn every_type_inserts() {
        let mut s = sales();
        for id in [
            "insert.chart.recommended",
            "insert.chart.column",
            "insert.chart.bar",
            "insert.chart.line",
            "insert.chart.pie",
            "insert.chart.area",
            "insert.chart.scatter",
            "insert.chart.histogram",
            "insert.chart.combo",
        ] {
            s.execute(id, &json!({"range": "A1:D5"})).unwrap();
        }
        let kinds: Vec<ChartKind> = charts(&s).iter().map(|c| c.kind).collect();
        assert_eq!(kinds[0], ChartKind::Column);
        assert_eq!(charts(&s)[4].series.len(), 1);
        s.execute(
            "insert.chart.pie",
            &json!({"range": "A1:B5", "doughnut": true}),
        )
        .unwrap();
        assert_eq!(charts(&s).last().map(|c| c.kind), Some(ChartKind::Doughnut));
        for c in charts(&s) {
            let d = chart_data(&s.wb, 0, c);
            let _ = sp_render::chart::chart_scene(&d, 480.0, 288.0, 1.0);
        }
    }

    #[test]
    fn recommended_picks_by_shape() {
        let mut s = Session::new();
        for r in 0..30u32 {
            let a = CellAddr::new(r, 0).to_a1();
            s.execute(
                "cell.set",
                &json!({"cell": a, "input": (r * 7 % 13).to_string()}),
            )
            .unwrap();
        }
        s.execute("insert.chart.recommended", &json!({"range": "A1:A30"}))
            .unwrap();
        assert_eq!(charts(&s)[0].kind, ChartKind::Histogram);
        for r in 0..5u32 {
            let a = CellAddr::new(r, 3).to_a1();
            let b = CellAddr::new(r, 4).to_a1();
            s.execute("cell.set", &json!({"cell": a, "input": r.to_string()}))
                .unwrap();
            s.execute(
                "cell.set",
                &json!({"cell": b, "input": (r * r).to_string()}),
            )
            .unwrap();
        }
        s.execute("insert.chart.recommended", &json!({"range": "D1:E5"}))
            .unwrap();
        assert_eq!(charts(&s)[1].kind, ChartKind::Scatter);
        let mut s = Session::new();
        for (r, (y, v)) in [(2019, 5), (2020, 7), (2021, 6), (2022, 9)]
            .iter()
            .enumerate()
        {
            s.execute(
                "cell.set",
                &json!({"cell": CellAddr::new(r as u32, 0).to_a1(), "input": y.to_string()}),
            )
            .unwrap();
            s.execute(
                "cell.set",
                &json!({"cell": CellAddr::new(r as u32, 1).to_a1(), "input": v.to_string()}),
            )
            .unwrap();
        }
        let _ = s
            .execute("insert.chart.recommended", &json!({"range": "A1:B4"}))
            .unwrap();
        assert!(matches!(
            charts(&s)[0].kind,
            ChartKind::Line | ChartKind::Scatter
        ));
    }

    #[test]
    fn explicit_series_and_errors() {
        let mut s = sales();
        s.execute(
            "insert.chart.line",
            &json!({"series": [{"values": "B2:B5", "name": "Total", "categories": "A2:A5", "color": "FF0000"}]}),
        )
        .unwrap();
        let c = &charts(&s)[0];
        assert_eq!(c.series[0].name, "\"Total\"");
        assert_eq!(c.series[0].color, Some(Color(255, 0, 0)));
        assert_eq!(chart_data(&s.wb, 0, c).series[0].name, "Total");
        let mut empty = Session::new();
        assert!(empty.execute("insert.chart.column", &json!({})).is_err());
        assert!(
            s.execute("insert.chart.column", &json!({"legend": 5}))
                .is_err()
        );
        assert!(
            s.execute("insert.chart.column", &json!({"bins": 0}))
                .is_err()
        );
        assert!(
            s.execute("insert.chart.column", &json!({"anchor": "nope"}))
                .is_err()
        );
        assert!(
            s.execute("insert.chart.column", &json!({"series": "x"}))
                .is_err()
        );
        assert!(
            s.execute("insert.chart.column", &json!({"chart": -1}))
                .is_err()
        );
        // Whole-column selection stays cheap.
        s.execute("insert.chart.column", &json!({"range": "A:D"}))
            .unwrap();
    }

    #[test]
    fn sparklines_insert_and_delete() {
        let mut s = sales();
        let out = s
            .execute(
                "insert.sparkline.line",
                &json!({"data": "B2:D5", "high": true}),
            )
            .unwrap();
        assert_eq!(out["sparklines"], 4);
        assert_eq!(out["location"], "E2:E5");
        let g = &s.wb.sheets[0].drawings.sparklines[0];
        // Relative, as the file stores it (an absolute one did not survive saving).
        assert_eq!(g.items[0].data, "Sheet1!B2:D2");
        assert_eq!(
            sparkline_values(&s.wb, 0, &g.items[1]),
            vec![Some(7.0), Some(11.0), Some(15.0)]
        );
        // Replacing in the same cells keeps one group.
        s.execute(
            "insert.sparkline.column",
            &json!({"data": "B2:D5", "location": "E2:E5"}),
        )
        .unwrap();
        assert_eq!(s.wb.sheets[0].drawings.sparklines.len(), 1);
        s.execute(
            "insert.sparkline.winloss",
            &json!({"data": "B2:B5", "location": "B7:E7"}),
        )
        .unwrap_err();
        s.execute(
            "insert.sparkline.winloss",
            &json!({"data": "B2:E5", "location": "B7:E7"}),
        )
        .unwrap();
        assert_eq!(s.wb.sheets[0].drawings.sparklines.len(), 2);
        s.execute(
            "insert.sparkline.line",
            &json!({"delete": true, "location": "E2:E3"}),
        )
        .unwrap();
        assert_eq!(s.wb.sheets[0].drawings.sparklines[0].items.len(), 2);
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(s.wb.sheets[0].drawings.sparklines[0].items.len(), 4);
        assert!(
            s.execute(
                "insert.sparkline.line",
                &json!({"delete": true, "location": "Z1"})
            )
            .is_err()
        );
        assert!(
            s.execute(
                "insert.sparkline.line",
                &json!({"data": "A1:D5", "location": "H1"})
            )
            .is_err()
        );
        assert!(
            s.execute(
                "insert.sparkline.line",
                &json!({"data": "A:D", "location": "H:H"})
            )
            .is_err()
        );
    }

    #[test]
    fn row_insert_moves_chart_and_undo_restores() {
        let mut s = sales();
        s.execute("insert.chart.column", &json!({"range": "A1:D5"}))
            .unwrap();
        s.execute("insert.sparkline.line", &json!({"data": "B2:D5"}))
            .unwrap();
        s.execute("home.cells.insert_rows", &json!({"range": "A1"}))
            .unwrap();
        let c = &charts(&s)[0];
        assert_eq!(c.anchor.from.row, 1);
        assert_eq!(c.series[0].values, "Sheet1!$B$3:$B$6");
        let d = chart_data(&s.wb, 0, c);
        assert_eq!(d.categories[0], "North");
        let g = &s.wb.sheets[0].drawings.sparklines[0];
        assert_eq!(g.items[0].location, CellAddr::new(2, 4));
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(charts(&s)[0].anchor.from.row, 0);
        assert_eq!(charts(&s)[0].series[0].values, "Sheet1!$B$2:$B$5");
    }
}
