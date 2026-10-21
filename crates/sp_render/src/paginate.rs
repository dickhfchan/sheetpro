//! Print pagination and page scenes (UI-free).
//!
//! [`paginate`] splits a sheet's print area (or used range) into pages from its
//! [`PageSetup`]: paper size and orientation, margins, scale or fit-to N×M pages, manual and
//! automatic breaks, page order, repeated title rows/columns, headings and hidden rows/cols.
//! [`page_scene`] lays one page out as positioned primitives in points (top-left origin):
//! fills, gridlines, borders, cell text measured with the standard-14 metrics, merged cells,
//! charts, sparklines, headings and the header/footer. The PDF writer and the print preview
//! both draw these scenes.

use crate::chart::{self, ChartData, HAnchor, Paint, Prim, VAnchor};
use crate::objects::{self as geo, ObjPrim, ZItem};
use crate::std14::{self, Face};
use crate::{Align, BorderSeg, DisplayCell, INDENT_PX, col_width_px, layout_lines, row_height_px};
use sp_core::addr::split_sheet_prefix;
use sp_core::{CellAddr, CellRange, MAX_COLS, MAX_ROWS, col_to_letters};
use sp_model::objects::ImageData;
use sp_model::{BorderStyle, Color, Orientation, PageSetup, Sheet, VAlign, Workbook};
use std::collections::{BTreeSet, HashMap};

/// Points per pixel at 100% (96 px = 72 pt = 1 inch).
pub const PX_TO_PT: f32 = 0.75;
/// Most pages one sheet produces (bounds work on hostile page setups).
pub const MAX_PAGES: usize = 10_000;
/// Built-in name holding a sheet's print area.
pub const PRINT_AREA: &str = "_xlnm.Print_Area";
/// Built-in name holding a sheet's repeated rows/columns.
pub const PRINT_TITLES: &str = "_xlnm.Print_Titles";

/// Cell padding, px at 100%.
const PAD_X: f32 = 3.0;
const PAD_Y: f32 = 1.0;
/// Height of the printed column-heading row, px at 100%.
const HEADING_H_PX: f32 = 20.0;
/// Header/footer font size when the code doesn't set one.
const HF_SIZE: f32 = 11.0;
/// Most wrapped lines laid out for one cell.
const MAX_WRAP_LINES: usize = 500;

/// Paper size in points (portrait) for a file-format paper code; unknown codes are Letter.
pub fn paper_size(code: u32) -> (f32, f32) {
    let mm = |w: f32, h: f32| (w * 72.0 / 25.4, h * 72.0 / 25.4);
    match code {
        3 => (792.0, 1224.0),
        4 => (1224.0, 792.0),
        5 => (612.0, 1008.0),
        6 => (396.0, 612.0),
        7 => (522.0, 756.0),
        8 => mm(297.0, 420.0),
        9 | 10 => mm(210.0, 297.0),
        11 => mm(148.0, 210.0),
        12 => mm(250.0, 353.0),
        13 => mm(176.0, 250.0),
        14 => (612.0, 936.0),
        20 => (297.0, 684.0),
        27 => mm(110.0, 220.0),
        _ => (612.0, 792.0),
    }
}

/// An inclusive run of rows or columns.
pub type Span = (u32, u32);

/// What to print instead of the sheet's print area.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PrintOptions {
    /// Print exactly these ranges (e.g. the selection) instead of the print area.
    pub ranges: Option<Vec<CellRange>>,
    /// Ignore the defined print area and print the used range.
    pub ignore_print_area: bool,
}

/// One printed page: an inclusive block of body rows and columns plus the title lines
/// repeated on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Page {
    /// Index into [`Pagination::areas`].
    pub area: usize,
    pub rows: (u32, u32),
    pub cols: (u32, u32),
    pub title_rows: Option<(u32, u32)>,
    pub title_cols: Option<(u32, u32)>,
}

/// A sheet split into pages.
#[derive(Debug, Clone, PartialEq)]
pub struct Pagination {
    pub sheet: usize,
    /// Oriented paper size, points.
    pub width: f32,
    pub height: f32,
    /// Effective print scale (1.0 = 100%), after fit-to.
    pub scale: f32,
    pub setup: PageSetup,
    pub areas: Vec<CellRange>,
    pub title_rows: Option<(u32, u32)>,
    pub title_cols: Option<(u32, u32)>,
    pub pages: Vec<Page>,
    /// Width of the printed row-heading column, px at 100% (0 without headings).
    pub heading_w_px: f32,
    pub heading_h_px: f32,
}

/// How a page edge is drawn in page break preview.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind {
    /// Outer edge of a print area (solid).
    Area,
    /// A manual page break (solid).
    Manual,
    /// An automatic page break (dashed).
    Auto,
}

/// A page's body rectangle in cell coordinates, with the kind of each edge, for the shell's
/// page break preview and page layout views.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageBound {
    /// 1-based page number within this sheet.
    pub number: usize,
    pub rows: (u32, u32),
    pub cols: (u32, u32),
    pub top: EdgeKind,
    pub bottom: EdgeKind,
    pub left: EdgeKind,
    pub right: EdgeKind,
}

impl Pagination {
    /// Page bodies with edge kinds (an edge between two pages is `Manual` when a manual
    /// break starts the following page; fit-to-page ignores manual breaks).
    pub fn bounds(&self) -> Vec<PageBound> {
        let fit = self.setup.fit_width.is_some() || self.setup.fit_height.is_some();
        let kind = |at_edge: bool, breaks: &BTreeSet<u32>, line: Option<u32>| {
            if at_edge {
                EdgeKind::Area
            } else if !fit && line.is_some_and(|l| breaks.contains(&l)) {
                EdgeKind::Manual
            } else {
                EdgeKind::Auto
            }
        };
        let (rb, cb) = (&self.setup.row_breaks, &self.setup.col_breaks);
        self.pages
            .iter()
            .enumerate()
            .filter_map(|(i, p)| {
                let a = self.areas.get(p.area)?;
                Some(PageBound {
                    number: i + 1,
                    rows: p.rows,
                    cols: p.cols,
                    top: kind(p.rows.0 <= a.start.row, rb, Some(p.rows.0)),
                    bottom: kind(p.rows.1 >= a.end.row, rb, p.rows.1.checked_add(1)),
                    left: kind(p.cols.0 <= a.start.col, cb, Some(p.cols.0)),
                    right: kind(p.cols.1 >= a.end.col, cb, p.cols.1.checked_add(1)),
                })
            })
            .collect()
    }

    /// Index of the page whose body shows `a`.
    pub fn page_of(&self, a: CellAddr) -> Option<usize> {
        self.pages.iter().position(|p| {
            (p.rows.0..=p.rows.1).contains(&a.row) && (p.cols.0..=p.cols.1).contains(&a.col)
        })
    }
}

fn row_px(sheet: &Sheet, r: u32) -> f32 {
    row_height_px(sheet.row_height(r))
}

fn col_px(sheet: &Sheet, c: u32) -> f32 {
    col_width_px(sheet.col_width(c))
}

/// Split a defined-name formula on top-level commas (quoted sheet names may contain commas).
fn split_refs(formula: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for ch in formula.chars() {
        match ch {
            '\'' => {
                quoted = !quoted;
                cur.push(ch);
            }
            ',' if !quoted => out.push(std::mem::take(&mut cur)),
            _ => cur.push(ch),
        }
    }
    out.push(cur);
    out.into_iter()
        .map(|s| {
            s.trim()
                .trim_start_matches('(')
                .trim_end_matches(')')
                .to_string()
        })
        .filter(|s| !s.is_empty())
        .collect()
}

fn parse_ref(text: &str) -> Option<CellRange> {
    let (_, body) = split_sheet_prefix(text);
    CellRange::parse(&body.replace('$', "")).ok()
}

fn builtin<'a>(wb: &'a Workbook, sheet: usize, name: &str) -> Option<&'a str> {
    wb.names
        .iter()
        .find(|n| n.scope == Some(sheet) && n.name.eq_ignore_ascii_case(name))
        .map(|n| n.formula.as_str())
}

/// Used range grown to cover charts and objects (printing includes floating objects).
pub fn content_range(sheet: &Sheet) -> Option<CellRange> {
    let mut r = sheet.used_range();
    let objects = sheet.objects.iter().filter(|o| !o.hidden).map(|o| {
        let (x, y, w, h) = geo::object_rect_abs(sheet, &o.anchor);
        let (c0, _) = chart::col_at_px(sheet, 0, x);
        let (r0, _) = chart::row_at_px(sheet, 0, y);
        let (c1, _) = chart::col_at_px(
            sheet,
            c0,
            (x + w - 0.5).max(x) - chart::col_offset_px(sheet, 0, c0) as f64,
        );
        let (r1, _) = chart::row_at_px(
            sheet,
            r0,
            (y + h - 0.5).max(y) - chart::row_offset_px(sheet, 0, r0) as f64,
        );
        CellRange::new(CellAddr::new(r0, c0), CellAddr::new(r1, c1))
    });
    let anchors = sheet
        .drawings
        .charts
        .iter()
        .map(|c| CellRange::new(c.anchor.from, c.anchor.to))
        .chain(
            sheet
                .analysis
                .slicers
                .iter()
                .map(|sl| CellRange::new(sl.anchor.from, sl.anchor.to)),
        )
        .chain(objects);
    for a in anchors {
        r = Some(match r {
            Some(u) => CellRange::new(
                CellAddr::new(u.start.row.min(a.start.row), u.start.col.min(a.start.col)),
                CellAddr::new(u.end.row.max(a.end.row), u.end.col.max(a.end.col)),
            ),
            None => a,
        });
    }
    r
}

/// Clamp whole-row / whole-column references to the content.
fn clamp_area(r: CellRange, content: Option<CellRange>) -> CellRange {
    let Some(u) = content else {
        return r;
    };
    let mut r = r;
    if r.start.row == 0 && r.end.row >= MAX_ROWS - 1 {
        r.end.row = u.end.row.max(r.start.row);
    }
    if r.start.col == 0 && r.end.col >= MAX_COLS - 1 {
        r.end.col = u.end.col.max(r.start.col);
    }
    r
}

/// The sheet's print area ranges (empty when none is defined).
pub fn print_area(wb: &Workbook, sheet: usize) -> Vec<CellRange> {
    builtin(wb, sheet, PRINT_AREA)
        .map(|f| split_refs(f).iter().filter_map(|t| parse_ref(t)).collect())
        .unwrap_or_default()
}

/// Repeated title rows and columns from `_xlnm.Print_Titles`.
pub fn print_titles(wb: &Workbook, sheet: usize) -> (Option<Span>, Option<Span>) {
    let mut rows = None;
    let mut cols = None;
    if let Some(f) = builtin(wb, sheet, PRINT_TITLES) {
        for part in split_refs(f) {
            let Some(r) = parse_ref(&part) else {
                continue;
            };
            if r.is_full_rows() {
                rows = Some((r.start.row, r.end.row));
            } else if r.is_full_cols() {
                cols = Some((r.start.col, r.end.col));
            }
        }
    }
    (rows, cols)
}

/// Split `first..=last` into page spans no longer than `avail` px; `manual` lines start a
/// new page. A line longer than a page gets a page of its own.
fn segments(
    first: u32,
    last: u32,
    size: &dyn Fn(u32) -> f32,
    avail: f32,
    manual: Option<&BTreeSet<u32>>,
) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    if first > last {
        return out;
    }
    let mut start = first;
    let mut acc = 0.0f32;
    for l in first..=last {
        let w = size(l);
        let forced = l != start && manual.is_some_and(|m| m.contains(&l));
        if forced || (acc > 0.0 && acc + w > avail) {
            out.push((start, l - 1));
            if out.len() >= MAX_PAGES {
                return out;
            }
            start = l;
            acc = 0.0;
        }
        acc += w;
    }
    out.push((start, last));
    out
}

fn span_px(span: Option<(u32, u32)>, size: &dyn Fn(u32) -> f32) -> f32 {
    span.map_or(0.0, |(a, b)| (a..=b).map(size).sum())
}

/// Largest scale in 10%..=100% (1% steps) at which `count(scale) <= n`.
fn fit_scale(n: u32, count: &dyn Fn(f32) -> usize) -> f32 {
    let n = n as usize;
    if n == 0 || count(1.0) <= n {
        return 1.0;
    }
    if count(0.10) > n {
        return 0.10;
    }
    // Invariant: count(lo) <= n < count(hi).
    let (mut lo, mut hi) = (10u32, 100u32);
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if count(mid as f32 / 100.0) <= n {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo as f32 / 100.0
}

/// Split sheet `sheet_idx` into pages.
pub fn paginate(wb: &Workbook, sheet_idx: usize, opts: &PrintOptions) -> Pagination {
    let setup = wb
        .sheet(sheet_idx)
        .map(|s| s.page_setup.clone())
        .unwrap_or_default();
    let (pw, ph) = paper_size(setup.paper_size);
    let (width, height) = match setup.orientation {
        Orientation::Portrait => (pw, ph),
        Orientation::Landscape => (ph, pw),
    };
    let mut out = Pagination {
        sheet: sheet_idx,
        width,
        height,
        scale: 1.0,
        setup: setup.clone(),
        areas: Vec::new(),
        title_rows: None,
        title_cols: None,
        pages: Vec::new(),
        heading_w_px: 0.0,
        heading_h_px: 0.0,
    };
    let Some(sheet) = wb.sheet(sheet_idx) else {
        return out;
    };
    let content = content_range(sheet);
    let areas: Vec<CellRange> = match &opts.ranges {
        // Explicit ranges (a selection) print only where there is content.
        Some(r) => r
            .iter()
            .filter_map(|x| content.and_then(|c| x.intersect(&c)))
            .collect(),
        None => {
            let pa = if opts.ignore_print_area {
                Vec::new()
            } else {
                print_area(wb, sheet_idx)
            };
            if pa.is_empty() {
                content.into_iter().collect()
            } else {
                pa
            }
        }
    };
    let areas: Vec<CellRange> = areas
        .into_iter()
        .map(|r| clamp_area(r, content))
        .take(64)
        .collect();
    let (title_rows, title_cols) = print_titles(wb, sheet_idx);
    out.title_rows = title_rows;
    out.title_cols = title_cols;
    if setup.print_headings {
        let last = areas.iter().map(|a| a.end.row).max().unwrap_or(0);
        let digits = (last.saturating_add(1)).to_string().len() as f32;
        out.heading_w_px = (digits * 7.0 + 10.0).max(25.0);
        out.heading_h_px = HEADING_H_PX;
    }
    let rsize = |r: u32| row_px(sheet, r);
    let csize = |c: u32| col_px(sheet, c);
    let m = setup.margins;
    let inner_w = (width - ((m.left + m.right) * 72.0) as f32).max(36.0);
    let inner_h = (height - ((m.top + m.bottom) * 72.0) as f32).max(36.0);
    let title_w = span_px(title_cols, &csize) + out.heading_w_px;
    let title_h = span_px(title_rows, &rsize) + out.heading_h_px;
    let avail = |inner: f32, scale: f32, used: f32| (inner / (PX_TO_PT * scale) - used).max(1.0);
    let fit = setup.fit_width.is_some() || setup.fit_height.is_some();
    let scale = if fit {
        let count_cols = |s: f32| {
            areas
                .iter()
                .map(|a| {
                    segments(
                        a.start.col,
                        a.end.col,
                        &csize,
                        avail(inner_w, s, title_w),
                        None,
                    )
                    .len()
                })
                .max()
                .unwrap_or(0)
        };
        let count_rows = |s: f32| {
            areas
                .iter()
                .map(|a| {
                    segments(
                        a.start.row,
                        a.end.row,
                        &rsize,
                        avail(inner_h, s, title_h),
                        None,
                    )
                    .len()
                })
                .max()
                .unwrap_or(0)
        };
        let sw = fit_scale(setup.fit_width.unwrap_or(0), &count_cols);
        let sh = fit_scale(setup.fit_height.unwrap_or(0), &count_rows);
        sw.min(sh)
    } else {
        setup.scale.clamp(10, 400) as f32 / 100.0
    };
    out.scale = scale;
    // Fit-to ignores manual breaks (as the target app does).
    let (rb, cb) = if fit {
        (None, None)
    } else {
        (Some(&setup.row_breaks), Some(&setup.col_breaks))
    };
    for (ai, a) in areas.iter().enumerate() {
        let rows = segments(
            a.start.row,
            a.end.row,
            &rsize,
            avail(inner_h, scale, title_h),
            rb,
        );
        let cols = segments(
            a.start.col,
            a.end.col,
            &csize,
            avail(inner_w, scale, title_w),
            cb,
        );
        let mut push = |r: (u32, u32), c: (u32, u32)| {
            if out.pages.len() >= MAX_PAGES {
                return false;
            }
            out.pages.push(Page {
                area: ai,
                rows: r,
                cols: c,
                title_rows: title_rows.filter(|t| r.0 > t.1),
                title_cols: title_cols.filter(|t| c.0 > t.1),
            });
            true
        };
        // Stop as soon as the page cap is reached (rows × cols can be huge).
        if setup.over_then_down {
            'outer: for r in &rows {
                for c in &cols {
                    if !push(*r, *c) {
                        break 'outer;
                    }
                }
            }
        } else {
            'outer2: for c in &cols {
                for r in &rows {
                    if !push(*r, *c) {
                        break 'outer2;
                    }
                }
            }
        }
    }
    out.areas = areas;
    out
}

// ---------------------------------------------------------------------------------------
// Headers and footers

/// Values substituted for header/footer field codes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HfContext {
    pub page: usize,
    pub pages: usize,
    pub date: String,
    pub time: String,
    pub file: String,
    pub path: String,
    pub sheet: String,
}

/// A run of header/footer text in one style.
#[derive(Debug, Clone, PartialEq)]
pub struct HfRun {
    pub text: String,
    pub font: String,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub size: f32,
    pub color: Option<Color>,
}

#[derive(Debug, Clone, PartialEq)]
struct HfStyle {
    font: String,
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    size: f32,
    color: Option<Color>,
}

impl Default for HfStyle {
    fn default() -> Self {
        HfStyle {
            font: String::new(),
            bold: false,
            italic: false,
            underline: false,
            strike: false,
            size: HF_SIZE,
            color: None,
        }
    }
}

fn push_run(runs: &mut Vec<HfRun>, st: &HfStyle, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = runs.last_mut()
        && last.font == st.font
        && last.bold == st.bold
        && last.italic == st.italic
        && last.underline == st.underline
        && last.strike == st.strike
        && last.size == st.size
        && last.color == st.color
    {
        last.text.push_str(text);
        return;
    }
    runs.push(HfRun {
        text: text.to_string(),
        font: st.font.clone(),
        bold: st.bold,
        italic: st.italic,
        underline: st.underline,
        strike: st.strike,
        size: st.size,
        color: st.color,
    });
}

/// Parse a header/footer code into left, centre and right runs with fields substituted.
/// Text before any section code belongs to the centre.
pub fn hf_sections(code: &str, ctx: &HfContext) -> [Vec<HfRun>; 3] {
    let ch: Vec<char> = code.chars().take(4_096).collect();
    let mut out: [Vec<HfRun>; 3] = Default::default();
    let mut sec = 1usize;
    let mut st = HfStyle::default();
    let mut i = 0usize;
    let at = |i: usize| ch.get(i).copied();
    while let Some(c) = at(i) {
        let Some(slot) = out.get_mut(sec) else {
            break;
        };
        if c != '&' {
            let mut s = String::new();
            while let Some(c) = at(i).filter(|c| *c != '&') {
                s.push(c);
                i += 1;
            }
            push_run(slot, &st, &s);
            continue;
        }
        let Some(code) = at(i + 1) else {
            push_run(slot, &st, "&");
            break;
        };
        i += 2;
        match code {
            '&' => push_run(slot, &st, "&"),
            'L' | 'C' | 'R' => {
                sec = match code {
                    'L' => 0,
                    'C' => 1,
                    _ => 2,
                };
                st = HfStyle::default();
            }
            'P' | 'N' => {
                let base = if code == 'P' { ctx.page } else { ctx.pages } as i64;
                let mut n = base;
                if let Some(sign) = at(i).filter(|c| *c == '+' || *c == '-') {
                    let mut j = i + 1;
                    let mut d: i64 = 0;
                    while let Some(dg) = at(j).and_then(|c| c.to_digit(10)) {
                        d = d
                            .saturating_mul(10)
                            .saturating_add(i64::from(dg))
                            .min(1_000_000);
                        j += 1;
                    }
                    if j > i + 1 {
                        n = if sign == '+' { base + d } else { base - d };
                        i = j;
                    }
                }
                push_run(slot, &st, &n.to_string());
            }
            'D' => push_run(slot, &st, &ctx.date),
            'T' => push_run(slot, &st, &ctx.time),
            'F' => push_run(slot, &st, &ctx.file),
            'Z' => push_run(slot, &st, &ctx.path),
            'A' => push_run(slot, &st, &ctx.sheet),
            'B' => st.bold = !st.bold,
            'I' => st.italic = !st.italic,
            'U' | 'E' => st.underline = !st.underline,
            'S' => st.strike = !st.strike,
            '"' => {
                let mut spec = String::new();
                while let Some(c) = at(i) {
                    i += 1;
                    if c == '"' {
                        break;
                    }
                    spec.push(c);
                }
                let (font, style) = spec.split_once(',').unwrap_or((spec.as_str(), ""));
                if font != "-" {
                    st.font = font.trim().to_string();
                }
                let style = style.to_ascii_lowercase();
                if !style.is_empty() {
                    st.bold = style.contains("bold");
                    st.italic = style.contains("italic") || style.contains("oblique");
                }
            }
            'K' => {
                let hex: String = (i..i + 6).filter_map(at).collect();
                i += 6;
                st.color = u32::from_str_radix(&hex, 16)
                    .ok()
                    .filter(|_| hex.len() == 6)
                    .map(|v| Color((v >> 16) as u8, (v >> 8) as u8, v as u8));
            }
            d if d.is_ascii_digit() => {
                let mut n = d.to_digit(10).unwrap_or(0);
                while let Some(dg) = at(i).and_then(|c| c.to_digit(10)) {
                    n = n.saturating_mul(10).saturating_add(dg).min(999);
                    i += 1;
                }
                st.size = (n as f32).clamp(1.0, 409.0);
            }
            // Pictures, super/subscript, outline, shadow: not drawn.
            _ => {}
        }
    }
    out
}

/// Plain text of a header/footer code per section, for summaries and tests.
pub fn hf_plain(code: &str, ctx: &HfContext) -> [String; 3] {
    let s = hf_sections(code, ctx);
    let join = |v: &[HfRun]| v.iter().map(|r| r.text.as_str()).collect::<String>();
    [join(&s[0]), join(&s[1]), join(&s[2])]
}

// ---------------------------------------------------------------------------------------
// Page scenes

/// A positioned drawing item in points, origin at the top-left of the page.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Fill {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        color: Color,
    },
    /// Polyline (closed when `closed`); `dash` = (on, off) lengths.
    Line {
        pts: Vec<(f32, f32)>,
        width: f32,
        color: Color,
        dash: Option<(f32, f32)>,
        closed: bool,
    },
    Poly {
        pts: Vec<(f32, f32)>,
        color: Color,
    },
    Circle {
        x: f32,
        y: f32,
        r: f32,
        color: Color,
    },
    /// Text whose baseline starts at (x, y), rotated `angle` degrees counter-clockwise.
    Text {
        x: f32,
        y: f32,
        text: String,
        face: Face,
        size: f32,
        color: Color,
        angle: f32,
    },
    /// A filled outline that may be concave (non-zero winding).
    Area {
        pts: Vec<(f32, f32)>,
        color: Color,
    },
    /// An embedded picture mapped onto a parallelogram: where its top-left, top-right,
    /// bottom-right and bottom-left corners land.
    Image {
        image: ImageData,
        corners: [(f32, f32); 4],
    },
    /// Items clipped to a rectangle.
    Clip {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        items: Vec<Item>,
    },
}

/// One laid-out page.
#[derive(Debug, Clone, PartialEq)]
pub struct PageScene {
    pub width: f32,
    pub height: f32,
    pub items: Vec<Item>,
}

impl PageScene {
    /// Every text string in drawing order (flattening clips), for tests and search.
    pub fn texts(&self) -> Vec<String> {
        fn walk(items: &[Item], out: &mut Vec<String>) {
            for it in items {
                match it {
                    Item::Text { text, .. } => out.push(text.clone()),
                    Item::Clip { items, .. } => walk(items, out),
                    _ => {}
                }
            }
        }
        let mut v = Vec::new();
        walk(&self.items, &mut v);
        v
    }
}

/// Chart and sparkline data resolved by the engine for one sheet: `charts[i]` belongs to
/// `sheet.drawings.charts[i]`, `sparklines[g][i]` to item `i` of sparkline group `g`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PrintData {
    pub charts: Vec<ChartData>,
    pub sparklines: Vec<Vec<Vec<Option<f64>>>>,
    /// `slicers[i]`: the buttons of `sheet.analysis.slicers[i]`, in display order.
    pub slicers: Vec<Vec<SlicerButton>>,
}

/// One slicer button as printed: its label, whether it is selected (not filtered out),
/// and whether any record would still show with it (otherwise it prints greyed).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SlicerButton {
    pub label: String,
    pub selected: bool,
    pub has_data: bool,
}

/// Print colours for chart paint roles (white paper).
const SERIES: [Color; 8] = [
    Color(46, 110, 190),
    Color(232, 126, 44),
    Color(86, 158, 80),
    Color(196, 66, 72),
    Color(128, 96, 178),
    Color(38, 160, 168),
    Color(214, 172, 36),
    Color(118, 118, 118),
];
const GRIDLINE: Color = Color(170, 170, 170);
const HEADING_FILL: Color = Color(242, 242, 242);
const HEADING_LINE: Color = Color(160, 160, 160);
const WHITE: Color = Color(255, 255, 255);
const CHECKBOX: Color = Color(64, 64, 64);

/// Colour of a chart paint role on paper.
pub fn paint_color(p: Paint) -> Color {
    match p {
        Paint::Series(i) => SERIES
            .get(i % SERIES.len())
            .copied()
            .unwrap_or(Color::BLACK),
        Paint::Rgb(c) => c,
        Paint::Background => WHITE,
        Paint::Border => Color(200, 200, 200),
        Paint::Axis => Color(170, 170, 170),
        Paint::Gridline => Color(228, 228, 228),
        Paint::Text => Color(64, 64, 64),
        Paint::SubText => Color(96, 96, 96),
    }
}

type Lines = Vec<(u32, f32, f32)>;

/// Rows or columns of one block with offsets (px at 100%) from the block start.
fn block(span: (u32, u32), size: &dyn Fn(u32) -> f32) -> Lines {
    let mut v = Vec::new();
    let mut at = 0.0;
    for l in span.0..=span.1 {
        let s = size(l);
        if s > 0.0 {
            v.push((l, at, s));
            at += s;
        }
    }
    v
}

fn block_len(b: &[(u32, f32, f32)]) -> f32 {
    b.last().map_or(0.0, |l| l.1 + l.2)
}

/// Shared parameters for drawing one page.
struct Ctx<'a> {
    wb: &'a Workbook,
    sheet: &'a Sheet,
    data: &'a PrintData,
    /// pt per px.
    k: f32,
    scale: f32,
    gridlines: bool,
}

/// Lay out `page` of `pg`; `hf` supplies the header/footer fields (page number etc.).
pub fn page_scene(
    wb: &Workbook,
    pg: &Pagination,
    page: &Page,
    data: &PrintData,
    hf: &HfContext,
) -> PageScene {
    let mut items = Vec::new();
    let Some(sheet) = wb.sheet(pg.sheet) else {
        return PageScene {
            width: pg.width,
            height: pg.height,
            items,
        };
    };
    let k = PX_TO_PT * pg.scale;
    let cx = Ctx {
        wb,
        sheet,
        data,
        k,
        scale: pg.scale,
        gridlines: pg.setup.print_gridlines,
    };
    let rsize = |r: u32| row_px(sheet, r);
    let csize = |c: u32| col_px(sheet, c);
    let m = pg.setup.margins;
    let ox = (m.left * 72.0) as f32;
    let oy = (m.top * 72.0) as f32;
    let hw = pg.heading_w_px * k;
    let hh = pg.heading_h_px * k;
    let mut col_blocks: Vec<(Lines, f32, bool)> = Vec::new();
    let mut x = ox + hw;
    for (span, body) in [(page.title_cols, false), (Some(page.cols), true)] {
        if let Some(s) = span {
            let b = block(s, &csize);
            let len = block_len(&b) * k;
            col_blocks.push((b, x, body));
            x += len;
        }
    }
    let mut row_blocks: Vec<(Lines, f32, bool)> = Vec::new();
    let mut y = oy + hh;
    for (span, body) in [(page.title_rows, false), (Some(page.rows), true)] {
        if let Some(s) = span {
            let b = block(s, &rsize);
            let len = block_len(&b) * k;
            row_blocks.push((b, y, body));
            y += len;
        }
    }
    for (rows, y0, rbody) in &row_blocks {
        for (cols, x0, cbody) in &col_blocks {
            if rows.is_empty() || cols.is_empty() {
                continue;
            }
            draw_quadrant(&cx, &mut items, rows, cols, (*x0, *y0), *rbody && *cbody);
        }
    }
    if pg.heading_w_px > 0.0 {
        draw_headings(&cx, &mut items, &row_blocks, &col_blocks, (ox, oy, hw, hh));
    }
    let hfs = &pg.setup.header_footer;
    let (head, foot) = hfs.for_page(hf.page);
    let hf_scale = if hfs.scale_with_doc { pg.scale } else { 1.0 };
    let (l, r) = if hfs.align_with_margins {
        (ox, pg.width - (m.right * 72.0) as f32)
    } else {
        (36.0, pg.width - 36.0)
    };
    if !head.is_empty() {
        let top = (m.header * 72.0) as f32;
        draw_hf(
            &mut items,
            &hf_sections(head, hf),
            (l, r),
            top,
            None,
            hf_scale,
        );
    }
    if !foot.is_empty() {
        let bottom = pg.height - (m.footer * 72.0) as f32;
        draw_hf(
            &mut items,
            &hf_sections(foot, hf),
            (l, r),
            0.0,
            Some(bottom),
            hf_scale,
        );
    }
    PageScene {
        width: pg.width,
        height: pg.height,
        items,
    }
}

fn draw_headings(
    cx: &Ctx,
    out: &mut Vec<Item>,
    row_blocks: &[(Lines, f32, bool)],
    col_blocks: &[(Lines, f32, bool)],
    (ox, oy, hw, hh): (f32, f32, f32, f32),
) {
    let k = cx.k;
    let size = 10.0 * cx.scale;
    let face = Face::default();
    let cell = |out: &mut Vec<Item>, x: f32, y: f32, w: f32, h: f32, label: &str| {
        out.push(Item::Fill {
            x,
            y,
            w,
            h,
            color: HEADING_FILL,
        });
        out.push(Item::Line {
            pts: vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h)],
            width: 0.5,
            color: HEADING_LINE,
            dash: None,
            closed: true,
        });
        if !label.is_empty() {
            let tw = std14::text_width(face, label, size);
            let (asc, desc) = face.ascent_descent();
            out.push(Item::Clip {
                x,
                y,
                w,
                h,
                items: vec![Item::Text {
                    x: x + (w - tw) / 2.0,
                    y: y + h / 2.0 + (asc - desc) * size / 2.0,
                    text: label.to_string(),
                    face,
                    size,
                    color: Color::BLACK,
                    angle: 0.0,
                }],
            });
        }
    };
    cell(out, ox, oy, hw, hh, "");
    for (cols, x0, _) in col_blocks {
        for (c, x, w) in cols {
            cell(out, x0 + x * k, oy, w * k, hh, &col_to_letters(*c));
        }
    }
    for (rows, y0, _) in row_blocks {
        for (r, y, h) in rows {
            cell(out, ox, y0 + y * k, hw, h * k, &(r + 1).to_string());
        }
    }
}

/// Draw the cells of one rows × cols block whose top-left sits at `(x0, y0)` pt.
fn draw_quadrant(
    cx: &Ctx,
    out: &mut Vec<Item>,
    rows: &[(u32, f32, f32)],
    cols: &[(u32, f32, f32)],
    (x0, y0): (f32, f32),
    body: bool,
) {
    let k = cx.k;
    let view = layout_lines(cx.wb, cx.sheet, rows.to_vec(), cols.to_vec(), 1.0, false);
    let qw = block_len(cols) * k;
    let qh = block_len(rows) * k;
    let ri: HashMap<u32, (f32, f32)> = rows.iter().map(|r| (r.0, (r.1, r.2))).collect();
    let ci: HashMap<u32, (usize, f32, f32)> = cols
        .iter()
        .enumerate()
        .map(|(i, c)| (c.0, (i, c.1, c.2)))
        .collect();
    let rect_of = |a: CellAddr| -> Option<(f32, f32, f32, f32)> {
        let (y, h) = ri.get(&a.row)?;
        let (_, x, w) = ci.get(&a.col)?;
        Some((x0 + x * k, y0 + y * k, w * k, h * k))
    };
    let mut items = Vec::new();
    // Fills.
    for d in &view.cells {
        if let (Some(color), Some((x, y, w, h))) = (d.fill, rect_of(d.addr)) {
            items.push(Item::Fill { x, y, w, h, color });
        }
    }
    for mv in &view.merges {
        if let Some(color) = mv.cell.as_ref().and_then(|c| c.fill) {
            items.push(Item::Fill {
                x: x0 + mv.x * k,
                y: y0 + mv.y * k,
                w: mv.w * k,
                h: mv.h * k,
                color,
            });
        }
    }
    // Gridlines, then blank merged interiors so lines don't cross them.
    if cx.gridlines {
        let line = |a: (f32, f32), b: (f32, f32)| Item::Line {
            pts: vec![a, b],
            width: 0.25,
            color: GRIDLINE,
            dash: None,
            closed: false,
        };
        items.push(line((x0, y0), (x0 + qw, y0)));
        for (_, y, h) in rows {
            let yy = y0 + (y + h) * k;
            items.push(line((x0, yy), (x0 + qw, yy)));
        }
        items.push(line((x0, y0), (x0, y0 + qh)));
        for (_, x, w) in cols {
            let xx = x0 + (x + w) * k;
            items.push(line((xx, y0), (xx, y0 + qh)));
        }
        for mv in &view.merges {
            if mv.cell.as_ref().and_then(|c| c.fill).is_none() {
                let inset = 0.3;
                items.push(Item::Fill {
                    x: x0 + mv.x * k + inset,
                    y: y0 + mv.y * k + inset,
                    w: (mv.w * k - 2.0 * inset).max(0.0),
                    h: (mv.h * k - 2.0 * inset).max(0.0),
                    color: WHITE,
                });
            }
        }
    }
    for seg in &view.borders {
        border_items(&mut items, seg, (x0, y0), k);
    }
    if body {
        sparkline_items(cx, &mut items, &rect_of);
    }
    // Text.
    let occupied = |a: CellAddr| {
        cx.sheet.get(a).is_some_and(|c| !c.value.is_empty())
            || view.merges.iter().any(|m| m.range.contains(a))
    };
    for d in &view.cells {
        let Some(rect) = rect_of(d.addr) else {
            continue;
        };
        if d.text.is_empty() {
            continue;
        }
        let ci0 = ci.get(&d.addr.col).map_or(0, |c| c.0);
        let mut clip = rect;
        let face = Face::for_font(&d.font_name, d.bold, d.italic);
        let need = std14::text_width(face, &d.text, d.font_size * cx.scale) + 2.0 * PAD_X * k;
        if !d.wrap && !d.is_number && d.rotation == 0 && need > rect.2 {
            // Spill into empty neighbours on the side(s) the alignment grows towards.
            if matches!(d.align, Align::Left | Align::Center) {
                for c in cols.iter().skip(ci0 + 1) {
                    if clip.2 >= need || occupied(CellAddr::new(d.addr.row, c.0)) {
                        break;
                    }
                    clip.2 += c.2 * k;
                }
            }
            if matches!(d.align, Align::Right | Align::Center) {
                for c in cols.iter().take(ci0).rev() {
                    if clip.2 >= need || occupied(CellAddr::new(d.addr.row, c.0)) {
                        break;
                    }
                    clip.0 -= c.2 * k;
                    clip.2 += c.2 * k;
                }
            }
        }
        cell_text(&mut items, d, rect, clip, k, cx.scale);
    }
    for mv in &view.merges {
        if let Some(d) = &mv.cell {
            let r = (x0 + mv.x * k, y0 + mv.y * k, mv.w * k, mv.h * k);
            cell_text(&mut items, d, r, r, k, cx.scale);
        }
    }
    for d in &view.cells {
        if let (Some(checked), Some(r)) = (
            geo::checkbox_state(cx.wb, cx.sheet, d.addr),
            rect_of(d.addr),
        ) {
            checkbox_items(cx, &mut items, d, r, checked);
        }
    }
    if body {
        drawing_items(cx, &mut items, rows, cols, (x0, y0, qw, qh));
    }
    out.push(Item::Clip {
        x: x0,
        y: y0,
        w: qw,
        h: qh,
        items,
    });
}

fn sparkline_items(
    cx: &Ctx,
    items: &mut Vec<Item>,
    rect_of: &dyn Fn(CellAddr) -> Option<(f32, f32, f32, f32)>,
) {
    let inset = 3.0 * cx.k;
    for (gi, g) in cx.sheet.drawings.sparklines.iter().enumerate() {
        for (ii, sp) in g.items.iter().enumerate() {
            let Some((x, y, w, h)) = rect_of(sp.location) else {
                continue;
            };
            let (iw, ih) = ((w - 2.0 * inset).max(0.0), (h - 2.0 * inset).max(0.0));
            let vals = cx
                .data
                .sparklines
                .get(gi)
                .and_then(|v| v.get(ii))
                .map_or(&[][..], Vec::as_slice);
            let prims = chart::sparkline_scene(g, vals, iw, ih);
            let mut inner = Vec::new();
            prim_items(&mut inner, &prims, x + inset, y + inset);
            items.push(Item::Clip {
                x,
                y,
                w,
                h,
                items: inner,
            });
        }
    }
}

/// A cell checkbox: outline square and, when checked, the check mark.
fn checkbox_items(
    cx: &Ctx,
    items: &mut Vec<Item>,
    d: &DisplayCell,
    r: (f32, f32, f32, f32),
    checked: bool,
) {
    let align = cx.sheet.get(d.addr).map_or(sp_model::HAlign::General, |c| {
        cx.wb.styles.get(c.style).h_align
    });
    let (x, y, w, h) = geo::checkbox_rect(r, cx.k, align);
    let color = d.color.unwrap_or(CHECKBOX);
    let width = (0.75 * cx.scale).max(0.3);
    items.push(Item::Line {
        pts: vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h)],
        width,
        color,
        dash: None,
        closed: true,
    });
    if checked {
        items.push(Item::Line {
            pts: geo::check_mark((x, y, w, h)),
            width: width * 1.6,
            color,
            dash: None,
            closed: false,
        });
    }
}

/// Mix a colour with white paper by `alpha` (PDF output has no transparency).
fn on_paper(c: Color, alpha: f32) -> Color {
    let a = alpha.clamp(0.0, 1.0);
    let mix = |v: u8| (f32::from(v) * a + 255.0 * (1.0 - a)).round() as u8;
    Color(mix(c.0), mix(c.1), mix(c.2))
}

/// Charts and objects overlapping the block, in z-order, positioned relative to its first
/// row/col.
fn drawing_items(
    cx: &Ctx,
    items: &mut Vec<Item>,
    rows: &[(u32, f32, f32)],
    cols: &[(u32, f32, f32)],
    (x0, y0, qw, qh): (f32, f32, f32, f32),
) {
    let (Some(first_r), Some(first_c)) = (rows.first(), cols.first()) else {
        return;
    };
    let k = cx.k;
    let bx = chart::col_offset_px(cx.sheet, 0, first_c.0);
    let by = chart::row_offset_px(cx.sheet, 0, first_r.0);
    let place = |(ax, ay, aw, ah): (f32, f32, f32, f32)| {
        let (px, py, w, h) = (x0 + (ax - bx) * k, y0 + (ay - by) * k, aw * k, ah * k);
        // Rotated objects can reach past their box: test the circumscribed square.
        let reach = (w * w + h * h).sqrt();
        let (cx_, cy_) = (px + w / 2.0, py + h / 2.0);
        let outside =
            cx_ + reach < x0 || cy_ + reach < y0 || cx_ - reach > x0 + qw || cy_ - reach > y0 + qh;
        (!outside).then_some((px, py, w, h))
    };
    for z in geo::z_order(cx.sheet) {
        match z {
            ZItem::Chart(i) => {
                let Some(c) = cx.sheet.drawings.charts.get(i) else {
                    continue;
                };
                let r = chart::anchor_rect(cx.sheet, CellAddr::default(), &c.anchor);
                if r.2 < 2.0 || r.3 < 2.0 {
                    continue;
                }
                let (Some((px, py, w, h)), Some(d)) = (place(r), cx.data.charts.get(i)) else {
                    continue;
                };
                let prims = chart::chart_scene(d, w, h, k);
                prim_items(items, &prims, px, py);
            }
            ZItem::Object(i) => {
                let Some(o) = cx.sheet.objects.get(i) else {
                    continue;
                };
                let r = geo::object_rect(cx.sheet, CellAddr::default(), &o.anchor);
                if o.hidden {
                    continue;
                }
                if let Some(b) = place(r) {
                    object_items(cx, items, o, b);
                }
            }
        }
    }
    // Slicers float above charts and objects, as on screen.
    for (i, sl) in cx.sheet.analysis.slicers.iter().enumerate() {
        let r = chart::anchor_rect(cx.sheet, CellAddr::default(), &sl.anchor);
        if r.2 < 4.0 || r.3 < 4.0 {
            continue;
        }
        if let Some(b) = place(r) {
            let buttons = cx.data.slicers.get(i).map_or(&[][..], Vec::as_slice);
            slicer_items(cx, items, sl, buttons, b);
        }
    }
}

const SLICER_BORDER: Color = Color(190, 190, 190);
const SLICER_CAPTION: Color = Color(50, 50, 50);
const SLICER_SELECTED_BG: Color = Color(204, 226, 245);
const SLICER_SELECTED_BORDER: Color = Color(120, 170, 220);
const SLICER_ITEM_BORDER: Color = Color(200, 200, 200);
const SLICER_ITEM_TEXT: Color = Color(30, 30, 30);
const SLICER_NODATA_TEXT: Color = Color(170, 170, 170);
const SLICER_CLEAR: Color = Color(200, 60, 50);
const SLICER_CLEAR_OFF: Color = Color(190, 190, 190);

/// A slicer in the box `b` (pt): white body, caption, clear-filter icon and its buttons,
/// laid out like the screen (`objects::slicer_layout`).
fn slicer_items(
    cx: &Ctx,
    out: &mut Vec<Item>,
    sl: &sp_model::pivot::Slicer,
    buttons: &[SlicerButton],
    (x, y, w, h): (f32, f32, f32, f32),
) {
    let k = cx.k;
    let lay = geo::slicer_layout((x, y, w, h), buttons.len(), sl.columns, k);
    let mut items = Vec::new();
    let rect_line = |(rx, ry, rw, rh): (f32, f32, f32, f32), color: Color| Item::Line {
        pts: vec![(rx, ry), (rx + rw, ry), (rx + rw, ry + rh), (rx, ry + rh)],
        width: (0.75 * k).max(0.3),
        color,
        dash: None,
        closed: true,
    };
    items.push(Item::Fill {
        x,
        y,
        w,
        h,
        color: WHITE,
    });
    items.push(rect_line((x, y, w, h), SLICER_BORDER));
    // Caption, clipped before the clear button.
    let caption = if sl.caption.is_empty() {
        &sl.name
    } else {
        &sl.caption
    };
    let face = Face::default();
    let text_at = |text: &str, x: f32, mid_y: f32, size: f32, color: Color| {
        let (asc, desc) = face.ascent_descent();
        Item::Text {
            x,
            y: mid_y + (asc - desc) * size / 2.0,
            text: text.to_string(),
            face,
            size,
            color,
            angle: 0.0,
        }
    };
    let (hx, hy, _, hh) = lay.header;
    let cap_w = (lay.clear.0 - 2.0 * k - hx).max(0.0);
    items.push(Item::Clip {
        x: hx,
        y: hy,
        w: cap_w,
        h: hh,
        items: vec![text_at(
            caption,
            hx + 8.0 * k,
            hy + hh / 2.0,
            (11.0 * cx.scale).max(1.0),
            SLICER_CAPTION,
        )],
    });
    // Clear-filter icon: a funnel with a cross, live when something is filtered out.
    let filtered = buttons.iter().any(|b| !b.selected);
    let ic = if filtered {
        SLICER_CLEAR
    } else {
        SLICER_CLEAR_OFF
    };
    let (cx0, cy0, cw, ch) = lay.clear;
    let sd = cw.min(ch);
    let at = |fx: f32, fy: f32| (cx0 + fx * sd, cy0 + fy * sd);
    let lw = (0.9 * k).max(0.3);
    items.push(Item::Line {
        pts: vec![
            at(0.1, 0.2),
            at(0.65, 0.2),
            at(0.45, 0.5),
            at(0.45, 0.85),
            at(0.3, 0.75),
            at(0.3, 0.5),
        ],
        width: lw,
        color: ic,
        dash: None,
        closed: true,
    });
    for (a, b) in [((0.62, 0.55), (0.9, 0.85)), ((0.9, 0.55), (0.62, 0.85))] {
        items.push(Item::Line {
            pts: vec![at(a.0, a.1), at(b.0, b.1)],
            width: lw,
            color: ic,
            dash: None,
            closed: false,
        });
    }
    let size = (10.0 * cx.scale).max(1.0);
    for (it, &(bx, by, bw, bh)) in buttons.iter().zip(&lay.buttons) {
        let (fill, border) = if it.selected && it.has_data {
            (SLICER_SELECTED_BG, SLICER_SELECTED_BORDER)
        } else {
            (WHITE, SLICER_ITEM_BORDER)
        };
        items.push(Item::Fill {
            x: bx,
            y: by,
            w: bw,
            h: bh,
            color: fill,
        });
        items.push(rect_line((bx, by, bw, bh), border));
        let tc = if it.has_data {
            SLICER_ITEM_TEXT
        } else {
            SLICER_NODATA_TEXT
        };
        let pad = 2.0 * k;
        items.push(Item::Clip {
            x: bx + pad,
            y: by + pad,
            w: (bw - 2.0 * pad).max(0.0),
            h: (bh - 2.0 * pad).max(0.0),
            items: vec![text_at(&it.label, bx + 6.0 * k, by + bh / 2.0, size, tc)],
        });
    }
    out.push(Item::Clip { x, y, w, h, items });
}

/// One picture, shape, text box or line in the box `b` (pt).
fn object_items(
    cx: &Ctx,
    out: &mut Vec<Item>,
    o: &sp_model::objects::DrawObject,
    b: (f32, f32, f32, f32),
) {
    let sc = geo::object_scene(o, b, cx.k);
    for p in sc.prims {
        match p {
            ObjPrim::Fill { pts, color, alpha } => out.push(Item::Area {
                pts,
                color: on_paper(color, alpha),
            }),
            ObjPrim::Stroke {
                pts,
                closed,
                color,
                alpha,
                width,
                dash,
            } => out.push(Item::Line {
                pts,
                width: width.max(0.1),
                color: on_paper(color, alpha),
                dash,
                closed,
            }),
            ObjPrim::Image { corners } => {
                if let Some(pic) = o.picture() {
                    out.push(Item::Image {
                        image: pic.image.clone(),
                        corners,
                    });
                }
            }
        }
    }
    let Some(t) = sc.text else {
        return;
    };
    let (x, y, w, h) = t.rect;
    let lines: Vec<(String, Face, f32, Color, Align, bool)> = t
        .paras
        .iter()
        .flat_map(|p| {
            let face = Face::for_font(&p.font, p.bold, p.italic);
            let size = (p.size * cx.scale).max(1.0);
            let wrapped = if t.wrap {
                wrap_text(&p.text, face, size, w.max(1.0))
            } else {
                vec![p.text.clone()]
            };
            wrapped
                .into_iter()
                .map(move |l| (l, face, size, p.color, p.align, p.underline))
        })
        .collect();
    let total: f32 = lines.iter().map(|l| l.2 * 1.2).sum();
    let mut top = match t.v_align {
        VAlign::Top => y,
        VAlign::Center => y + (h - total) / 2.0,
        VAlign::Bottom => y + h - total,
    };
    for (text, face, size, color, align, underline) in lines {
        let line_h = size * 1.2;
        let (asc, desc) = face.ascent_descent();
        let tw = std14::text_width(face, &text, size);
        let tx = match align {
            Align::Left => x,
            Align::Center => x + (w - tw) / 2.0,
            Align::Right => x + w - tw,
        };
        let base = top + (line_h - (asc + desc) * size) / 2.0 + asc * size;
        top += line_h;
        if text.is_empty() {
            continue;
        }
        let (px, py) = geo::rotate_point((tx, base), t.center, t.rotation);
        out.push(Item::Text {
            x: px,
            y: py,
            text,
            face,
            size,
            color,
            // Item angles turn counter-clockwise; object rotation is clockwise.
            angle: -(t.rotation as f32),
        });
        if underline {
            let dy = size * 0.12;
            out.push(Item::Line {
                pts: vec![
                    geo::rotate_point((tx, base + dy), t.center, t.rotation),
                    geo::rotate_point((tx + tw, base + dy), t.center, t.rotation),
                ],
                width: (size / 18.0).max(0.3),
                color,
                dash: None,
                closed: false,
            });
        }
    }
}

fn border_items(out: &mut Vec<Item>, seg: &BorderSeg, (x0, y0): (f32, f32), k: f32) {
    let color = seg.color.unwrap_or(Color::BLACK);
    let a = (x0 + seg.x * k, y0 + seg.y * k);
    let b = if seg.horizontal {
        (a.0 + seg.len * k, a.1)
    } else {
        (a.0, a.1 + seg.len * k)
    };
    let line = |a: (f32, f32), b: (f32, f32), width: f32, dash| Item::Line {
        pts: vec![a, b],
        width,
        color,
        dash,
        closed: false,
    };
    match seg.style {
        BorderStyle::None => {}
        BorderStyle::Thin => out.push(line(a, b, 0.75 * k, None)),
        BorderStyle::Medium => out.push(line(a, b, 1.5 * k, None)),
        BorderStyle::Thick => out.push(line(a, b, 2.25 * k, None)),
        BorderStyle::Dashed => out.push(line(a, b, 0.75 * k, Some((3.0 * k, 1.5 * k)))),
        BorderStyle::Dotted => out.push(line(a, b, 0.75 * k, Some((0.75 * k, 1.5 * k)))),
        BorderStyle::Double => {
            let off = 0.9 * k;
            let (dx, dy) = if seg.horizontal {
                (0.0, off)
            } else {
                (off, 0.0)
            };
            out.push(line(
                (a.0 - dx, a.1 - dy),
                (b.0 - dx, b.1 - dy),
                0.5 * k,
                None,
            ));
            out.push(line(
                (a.0 + dx, a.1 + dy),
                (b.0 + dx, b.1 + dy),
                0.5 * k,
                None,
            ));
        }
    }
}

/// Greedy word wrap of `text` to `width` pt (explicit newlines kept; over-long words break).
pub fn wrap_text(text: &str, face: Face, size: f32, width: f32) -> Vec<String> {
    let mut lines = Vec::new();
    let w = |s: &str| std14::text_width(face, s, size);
    for para in text.split('\n') {
        let mut cur = String::new();
        for word in para.split(' ') {
            let cand = if cur.is_empty() {
                word.to_string()
            } else {
                format!("{cur} {word}")
            };
            if w(&cand) <= width {
                cur = cand;
                continue;
            }
            if !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
                if w(word) <= width {
                    cur = word.to_string();
                    continue;
                }
            }
            // Break a word wider than the line by characters.
            for ch in word.chars() {
                cur.push(ch);
                if w(&cur) > width && cur.chars().count() > 1 {
                    cur.pop();
                    lines.push(std::mem::take(&mut cur));
                    cur.push(ch);
                }
                if lines.len() >= MAX_WRAP_LINES {
                    return lines;
                }
            }
        }
        lines.push(cur);
        if lines.len() >= MAX_WRAP_LINES {
            break;
        }
    }
    lines
}

/// Draw a cell's text inside `rect` (pt), clipped to `clip`.
fn cell_text(
    out: &mut Vec<Item>,
    d: &DisplayCell,
    rect: (f32, f32, f32, f32),
    clip: (f32, f32, f32, f32),
    k: f32,
    scale: f32,
) {
    let (x, y, w, h) = rect;
    let face = Face::for_font(&d.font_name, d.bold, d.italic);
    let mut size = (d.font_size * scale).max(1.0);
    let color = d.color.unwrap_or(Color::BLACK);
    let pad = PAD_X * k;
    let indent = f32::from(d.indent) * INDENT_PX * k;
    let avail = (w - 2.0 * pad - indent).max(1.0);
    let (asc, desc) = face.ascent_descent();
    let mut items = Vec::new();
    if d.rotation != 0 && !d.wrap {
        let text = d.text.replace('\n', " ");
        let tw = std14::text_width(face, &text, size);
        let a = f32::from(d.rotation).to_radians();
        // Text direction and the "down" normal in page space (y grows downwards).
        let (dir, perp) = ((a.cos(), -a.sin()), (a.sin(), a.cos()));
        let (mx, my) = (x + w / 2.0, y + h / 2.0);
        let off = (asc - desc) * size / 2.0;
        items.push(Item::Text {
            x: mx - dir.0 * tw / 2.0 + perp.0 * off,
            y: my - dir.1 * tw / 2.0 + perp.1 * off,
            text,
            face,
            size,
            color,
            angle: f32::from(d.rotation),
        });
    } else {
        let mut lines = if d.wrap {
            wrap_text(&d.text, face, size, avail)
        } else {
            vec![d.text.replace('\n', " ")]
        };
        if d.is_number && !d.wrap {
            let tw = lines
                .first()
                .map_or(0.0, |t| std14::text_width(face, t, size));
            // Column widths are counted in digits of the sheet's default font, 7px wide at
            // 11pt, and the screen font is sized to match; the standard-14 faces are wider
            // (a Helvetica digit is ~17% wider). Decide ### the way the screen does, by the
            // text's width in those digits, so a number that fits on screen also prints;
            // when the PDF face is wider than the column, print it condensed to fit.
            let digit = std14::text_width(face, "0", size).max(0.01);
            let ref_digit = 7.0 * k * d.font_size / 11.0;
            let ratio = (ref_digit / digit).min(1.0);
            // Same threshold as the grid: the cell width less 4px.
            if tw * ratio > w - 4.0 * k {
                let hw = std14::text_width(face, "#", size).max(0.1);
                let n = ((avail / hw).floor() as usize).clamp(1, 255);
                lines = vec!["#".repeat(n)];
            } else if tw > avail {
                size = (size * avail / tw).max(1.0);
            }
        }
        let line_h = size * 1.2;
        let total = line_h * lines.len() as f32;
        let pad_y = PAD_Y * k;
        let top = match d.v_align {
            VAlign::Top => y + pad_y,
            VAlign::Center => y + (h - total) / 2.0,
            VAlign::Bottom => y + h - pad_y - total,
        };
        for (i, line) in lines.iter().enumerate() {
            if line.is_empty() {
                continue;
            }
            let tw = std14::text_width(face, line, size);
            let tx = match d.align {
                Align::Left => x + pad + indent,
                Align::Right => x + w - pad - indent - tw,
                Align::Center => x + (w - tw) / 2.0,
            };
            let base = top + i as f32 * line_h + (line_h - (asc + desc) * size) / 2.0 + asc * size;
            items.push(Item::Text {
                x: tx,
                y: base,
                text: line.clone(),
                face,
                size,
                color,
                angle: 0.0,
            });
            let deco = |dy: f32| Item::Line {
                pts: vec![(tx, base + dy), (tx + tw, base + dy)],
                width: (size / 18.0).max(0.3),
                color,
                dash: None,
                closed: false,
            };
            if d.underline {
                items.push(deco(size * 0.12));
            }
            if d.strike {
                items.push(deco(-size * 0.28));
            }
        }
    }
    if !items.is_empty() {
        out.push(Item::Clip {
            x: clip.0,
            y: clip.1,
            w: clip.2,
            h: clip.3,
            items,
        });
    }
}

/// Chart/sparkline primitives (already in pt) placed with their top-left at `(ox, oy)`.
fn prim_items(out: &mut Vec<Item>, prims: &[Prim], ox: f32, oy: f32) {
    let at = |x: f32, y: f32| (ox + x, oy + y);
    for p in prims {
        match p {
            Prim::Rect {
                x,
                y,
                w,
                h,
                fill,
                stroke,
            } => {
                let (px, py) = at(*x, *y);
                if let Some(f) = fill {
                    out.push(Item::Fill {
                        x: px,
                        y: py,
                        w: *w,
                        h: *h,
                        color: paint_color(*f),
                    });
                }
                if let Some(s) = stroke {
                    out.push(Item::Line {
                        pts: vec![(px, py), (px + w, py), (px + w, py + h), (px, py + h)],
                        width: 0.75,
                        color: paint_color(*s),
                        dash: None,
                        closed: true,
                    });
                }
            }
            Prim::Line { pts, width, paint } => out.push(Item::Line {
                pts: pts.iter().map(|(x, y)| at(*x, *y)).collect(),
                width: *width,
                color: paint_color(*paint),
                dash: None,
                closed: false,
            }),
            Prim::Poly { pts, fill } => out.push(Item::Poly {
                pts: pts.iter().map(|(x, y)| at(*x, *y)).collect(),
                color: paint_color(*fill),
            }),
            Prim::Circle { x, y, r, fill } => {
                let (px, py) = at(*x, *y);
                out.push(Item::Circle {
                    x: px,
                    y: py,
                    r: *r,
                    color: paint_color(*fill),
                });
            }
            Prim::Text {
                x,
                y,
                text,
                size,
                h,
                v,
                paint,
                vertical,
                bold,
                italic,
            } => {
                let face = Face {
                    bold: *bold,
                    italic: *italic,
                    ..Face::default()
                };
                let size = size.max(1.0);
                let tw = std14::text_width(face, text, size);
                let (asc, desc) = face.ascent_descent();
                let th = (asc + desc) * size;
                let (bw, bh) = if *vertical { (th, tw) } else { (tw, th) };
                let left = match h {
                    HAnchor::Left => *x,
                    HAnchor::Center => x - bw / 2.0,
                    HAnchor::Right => x - bw,
                };
                let top = match v {
                    VAnchor::Top => *y,
                    VAnchor::Center => y - bh / 2.0,
                    VAnchor::Bottom => y - bh,
                };
                let (tx, ty, angle) = if *vertical {
                    // Reading upwards: glyph tops face left, baseline `asc` from the left.
                    (left + asc * size, top + bh, 90.0)
                } else {
                    (left, top + asc * size, 0.0)
                };
                let (px, py) = at(tx, ty);
                out.push(Item::Text {
                    x: px,
                    y: py,
                    text: text.clone(),
                    face,
                    size,
                    color: paint_color(*paint),
                    angle,
                });
            }
        }
    }
}

/// Draw header (`bottom = None`: first line from `top` down) or footer (last line ending
/// at `bottom`) sections between `left` and `right`.
fn draw_hf(
    out: &mut Vec<Item>,
    sections: &[Vec<HfRun>; 3],
    (left, right): (f32, f32),
    top: f32,
    bottom: Option<f32>,
    scale: f32,
) {
    let face_of = |r: &HfRun| Face::for_font(&r.font, r.bold, r.italic);
    for (si, runs) in sections.iter().enumerate() {
        // Split runs into lines at '\n'.
        let mut lines: Vec<Vec<HfRun>> = vec![Vec::new()];
        for r in runs {
            for (i, part) in r.text.split('\n').enumerate() {
                if i > 0 {
                    lines.push(Vec::new());
                }
                if !part.is_empty()
                    && let Some(l) = lines.last_mut()
                {
                    l.push(HfRun {
                        text: part.to_string(),
                        ..r.clone()
                    });
                }
            }
            if lines.len() > 50 {
                break;
            }
        }
        let metrics: Vec<(f32, f32)> = lines
            .iter()
            .map(|l| {
                let size = l.iter().map(|r| r.size).fold(HF_SIZE, f32::max) * scale;
                let w = l
                    .iter()
                    .map(|r| std14::text_width(face_of(r), &r.text, r.size * scale))
                    .sum();
                (size, w)
            })
            .collect();
        let total_h: f32 = metrics.iter().map(|m| m.0 * 1.2).sum();
        let mut y = bottom.map_or(top, |b| b - total_h);
        for (line, (size, w)) in lines.iter().zip(&metrics) {
            let base = y + size * 0.95;
            let mut x = match si {
                0 => left,
                1 => (left + right - w) / 2.0,
                _ => right - w,
            };
            for r in line {
                let face = face_of(r);
                let sz = r.size * scale;
                let tw = std14::text_width(face, &r.text, sz);
                let color = r.color.unwrap_or(Color::BLACK);
                out.push(Item::Text {
                    x,
                    y: base,
                    text: r.text.clone(),
                    face,
                    size: sz,
                    color,
                    angle: 0.0,
                });
                for (on, dy) in [(r.underline, sz * 0.12), (r.strike, -sz * 0.28)] {
                    if on {
                        out.push(Item::Line {
                            pts: vec![(x, base + dy), (x + tw, base + dy)],
                            width: (sz / 18.0).max(0.3),
                            color,
                            dash: None,
                            closed: false,
                        });
                    }
                }
                x += tw;
            }
            y += size * 1.2;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sp_core::Value;
    use sp_model::{Cell, DefinedName, Style};

    fn sheet_with(rows: u32, cols: u32) -> Workbook {
        let mut wb = Workbook::new();
        if let Some(sh) = wb.sheet_mut(0) {
            for r in 0..rows {
                for c in 0..cols {
                    sh.set(
                        CellAddr::new(r, c),
                        Cell::constant(Value::Number(f64::from(r * cols + c))),
                    );
                }
            }
        }
        wb
    }

    fn ps(wb: &mut Workbook) -> &mut PageSetup {
        &mut wb.sheets[0].page_setup
    }

    #[test]
    fn automatic_breaks_down_then_over() {
        // Letter portrait, normal margins: 7.1in × 9.5in printable = 681.6 × 912 px.
        // Default rows are 20px → 45 rows per page; default columns 64px → 10 per page.
        let wb = sheet_with(100, 25);
        let pg = paginate(&wb, 0, &PrintOptions::default());
        assert_eq!(pg.scale, 1.0);
        assert_eq!(pg.pages.len(), 3 * 3);
        assert_eq!(pg.pages[0].rows, (0, 44));
        assert_eq!(pg.pages[1].rows, (45, 89));
        assert_eq!(pg.pages[2].rows, (90, 99));
        assert_eq!(pg.pages[0].cols, (0, 9));
        assert_eq!(pg.pages[3].cols, (10, 19));
        assert_eq!(pg.pages[8].cols, (20, 24));
        assert_eq!(pg.page_of(CellAddr::new(50, 12)), Some(4));
    }

    #[test]
    fn over_then_down_and_landscape() {
        let mut wb = sheet_with(100, 25);
        ps(&mut wb).over_then_down = true;
        let pg = paginate(&wb, 0, &PrintOptions::default());
        assert_eq!(pg.pages[1].rows, (0, 44));
        assert_eq!(pg.pages[1].cols, (10, 19));
        ps(&mut wb).orientation = Orientation::Landscape;
        let pg = paginate(&wb, 0, &PrintOptions::default());
        assert!(pg.width > pg.height);
        // 9.6in × 7in printable → 921.6 × 672 px: 33 rows, 14 cols.
        assert_eq!(pg.pages[0].rows, (0, 32));
        assert_eq!(pg.pages[0].cols, (0, 13));
    }

    #[test]
    fn manual_breaks_and_hidden_rows() {
        let mut wb = sheet_with(60, 3);
        ps(&mut wb).row_breaks.insert(10);
        ps(&mut wb).col_breaks.insert(1);
        let pg = paginate(&wb, 0, &PrintOptions::default());
        let rows: Vec<_> = pg.pages.iter().map(|p| p.rows).collect();
        assert_eq!(
            rows,
            vec![(0, 9), (10, 54), (55, 59), (0, 9), (10, 54), (55, 59)]
        );
        assert_eq!(pg.pages[3].cols, (1, 2));
        // Hiding 20 rows lets the rest fit on the second page.
        for r in 20..40 {
            wb.sheets[0].hidden_rows.insert(r);
        }
        let pg = paginate(&wb, 0, &PrintOptions::default());
        assert_eq!(pg.pages[1].rows, (10, 59));
    }

    #[test]
    fn page_bounds_edge_kinds() {
        let mut wb = sheet_with(60, 3);
        ps(&mut wb).row_breaks.insert(10);
        let pg = paginate(&wb, 0, &PrintOptions::default());
        let b = pg.bounds();
        assert_eq!(b.len(), pg.pages.len());
        assert_eq!(b[0].number, 1);
        assert_eq!(b[0].top, EdgeKind::Area);
        assert_eq!(b[0].bottom, EdgeKind::Manual);
        assert_eq!(b[1].top, EdgeKind::Manual);
        assert_eq!(b[1].bottom, EdgeKind::Auto);
        assert_eq!(b[2].top, EdgeKind::Auto);
        assert_eq!(b[2].bottom, EdgeKind::Area);
        assert_eq!((b[0].left, b[0].right), (EdgeKind::Area, EdgeKind::Area));
        // Fit-to ignores manual breaks.
        ps(&mut wb).fit_height = Some(1);
        ps(&mut wb).fit_width = Some(1);
        let b = paginate(&wb, 0, &PrintOptions::default()).bounds();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].bottom, EdgeKind::Area);
    }

    #[test]
    fn fit_to_pages() {
        let mut wb = sheet_with(200, 30);
        ps(&mut wb).fit_width = Some(1);
        ps(&mut wb).fit_height = Some(1);
        ps(&mut wb).row_breaks.insert(5);
        let pg = paginate(&wb, 0, &PrintOptions::default());
        assert_eq!(pg.pages.len(), 1, "fit to 1×1 ignores manual breaks");
        assert!(pg.scale < 0.25 && pg.scale >= 0.10, "{}", pg.scale);
        // 1 page wide, any height: width governs (30 cols × 64px into 681.6px → ~35%).
        ps(&mut wb).fit_height = Some(0);
        let pg = paginate(&wb, 0, &PrintOptions::default());
        assert!((pg.scale - 0.35).abs() < 0.02, "{}", pg.scale);
        assert!(pg.pages.iter().all(|p| p.cols == (0, 29)));
        assert!(pg.pages.len() > 1);
        // 2 pages wide.
        ps(&mut wb).fit_width = Some(2);
        let pg = paginate(&wb, 0, &PrintOptions::default());
        let widths: BTreeSet<_> = pg.pages.iter().map(|p| p.cols).collect();
        assert_eq!(widths.len(), 2);
        // A small sheet never scales up.
        let mut small = sheet_with(3, 3);
        small.sheets[0].page_setup.fit_width = Some(1);
        small.sheets[0].page_setup.fit_height = Some(1);
        assert_eq!(paginate(&small, 0, &PrintOptions::default()).scale, 1.0);
    }

    #[test]
    fn print_area_titles_and_selection() {
        let mut wb = sheet_with(100, 5);
        wb.names.push(DefinedName {
            name: PRINT_AREA.into(),
            formula: "Sheet1!$A$1:$C$80".into(),
            scope: Some(0),
        });
        wb.names.push(DefinedName {
            name: PRINT_TITLES.into(),
            formula: "Sheet1!$1:$2,Sheet1!$A:$A".into(),
            scope: Some(0),
        });
        let pg = paginate(&wb, 0, &PrintOptions::default());
        assert_eq!(pg.areas.len(), 1);
        assert_eq!(pg.areas[0].end, CellAddr::new(79, 2));
        assert_eq!(pg.title_rows, Some((0, 1)));
        assert_eq!(pg.title_cols, Some((0, 0)));
        // Title rows (40px) and column A (64px) reduce the space: 43 body rows per page.
        assert_eq!(pg.pages[0].rows, (0, 42));
        assert_eq!(
            pg.pages[0].title_rows, None,
            "first page shows them in place"
        );
        assert_eq!(pg.pages[1].title_rows, Some((0, 1)));
        assert_eq!(pg.pages[1].title_cols, None, "column A is in the body");
        let sc = page_scene(
            &wb,
            &pg,
            &pg.pages[1],
            &PrintData::default(),
            &HfContext::default(),
        );
        let t = sc.texts();
        // Title rows print first ("0" is A1), then body row 44 (A44 = 215).
        assert_eq!(t.first().map(String::as_str), Some("0"));
        assert!(t.contains(&"215".to_string()));
        // Selection override.
        let opts = PrintOptions {
            ranges: Some(vec![CellRange::new(
                CellAddr::new(1, 1),
                CellAddr::new(2, 1),
            )]),
            ..Default::default()
        };
        let pg = paginate(&wb, 0, &opts);
        assert_eq!(pg.pages.len(), 1);
        assert_eq!(pg.pages[0].cols, (1, 1));
        // Empty sheet / missing sheet: nothing to print.
        let empty = Workbook::new();
        assert!(
            paginate(&empty, 0, &PrintOptions::default())
                .pages
                .is_empty()
        );
        assert!(
            paginate(&empty, 7, &PrintOptions::default())
                .pages
                .is_empty()
        );
    }

    #[test]
    fn header_footer_codes() {
        let ctx = HfContext {
            page: 3,
            pages: 9,
            date: "1/2/2026".into(),
            time: "10:00".into(),
            file: "Book.xlsx".into(),
            path: "/tmp/".into(),
            sheet: "Data".into(),
        };
        let p = hf_plain("&L&A&CPage &P of &N&R&F &D &T", &ctx);
        assert_eq!(
            p,
            [
                "Data".to_string(),
                "Page 3 of 9".into(),
                "Book.xlsx 1/2/2026 10:00".into()
            ]
        );
        assert_eq!(
            hf_plain("Plain && simple &P+10", &ctx)[1],
            "Plain & simple 13"
        );
        let s = hf_sections("&\"Arial,Bold Italic\"&14Big&B &KFF0000x", &ctx);
        assert_eq!(s[1][0].text, "Big");
        assert!(s[1][0].bold && s[1][0].italic);
        assert_eq!(s[1][0].size, 14.0);
        assert!(!s[1][1].bold);
        assert_eq!(s[1][2].color, Some(Color(255, 0, 0)));
        // Hostile input doesn't panic.
        for code in [
            "&",
            "&\"",
            "&K",
            "&P+",
            "&999999999",
            "&Z&Q&L&&&",
            "&P-99999999999",
        ] {
            let _ = hf_sections(code, &ctx);
        }
    }

    #[test]
    fn numbers_that_fit_on_screen_print_condensed_not_as_hashes() {
        let mut wb = Workbook::new();
        let date = wb.styles.intern(Style {
            num_fmt: "m/d/yyyy".into(),
            ..Style::default()
        });
        if let Some(sh) = wb.sheet_mut(0) {
            for c in 0..2 {
                let mut cell = Cell::constant(Value::Number(46081.0));
                cell.style = date;
                sh.set(CellAddr::new(0, c), cell);
            }
            let mut cell = Cell::constant(Value::Number(46315.0));
            cell.style = date;
            sh.set(CellAddr::new(2, 0), cell);
            for (r, n) in [(1, 12345678.0), (3, 1234.5678), (4, 123456789.0)] {
                sh.set(CellAddr::new(r, 0), Cell::constant(Value::Number(n)));
            }
            sh.col_widths.insert(1, 4.0);
        }
        let pg = paginate(&wb, 0, &PrintOptions::default());
        let sc = page_scene(
            &wb,
            &pg,
            &pg.pages[0],
            &PrintData::default(),
            &HfContext::default(),
        );
        fn walk<'a>(items: &'a [Item], out: &mut Vec<(&'a str, f32, f32)>) {
            for it in items {
                match it {
                    Item::Text {
                        text, size, face, ..
                    } => {
                        out.push((text, *size, std14::text_width(*face, text, *size)));
                    }
                    Item::Clip { items, .. } => walk(items, out),
                    _ => {}
                }
            }
        }
        let mut t = Vec::new();
        walk(&sc.items, &mut t);
        // A default column (64px = 48pt) holds 2/28/2026 on screen: it prints, condensed
        // to fit inside the cell padding.
        let (_, size, w) = t
            .iter()
            .find(|x| x.0 == "2/28/2026")
            .copied()
            .unwrap_or_else(|| panic!("{t:?}"));
        assert!(size < 11.0 && size > 9.0, "{size}");
        assert!(w <= 48.0 - 2.0 * PAD_X * PX_TO_PT + 0.01, "{w}");
        // As on screen: 12345678 and 1234.5678 print (condensed), 10/20/2026 and 123456789
        // don't fit a default column.
        assert!(t.iter().any(|x| x.0 == "12345678" && x.2 <= 43.51), "{t:?}");
        assert!(t.iter().any(|x| x.0 == "1234.5678"), "{t:?}");
        assert_eq!(
            t.iter().filter(|x| x.0.starts_with('#')).count(),
            3,
            "{t:?}"
        );
        // A narrow column still shows ###.
        assert!(t.iter().any(|x| x.0.starts_with('#')), "{t:?}");
    }

    #[test]
    fn scene_draws_cells_headings_header() {
        let mut wb = sheet_with(2, 2);
        let id = wb.styles.intern(Style {
            fill: Some(Color(255, 255, 0)),
            wrap: true,
            ..Style::default()
        });
        if let Some(sh) = wb.sheet_mut(0) {
            let mut c = Cell::constant(Value::Text("a long text that wraps onto lines".into()));
            c.style = id;
            sh.set(CellAddr::new(2, 0), c);
            sh.page_setup.print_headings = true;
            sh.page_setup.print_gridlines = true;
            sh.page_setup.header_footer.odd_header = "&CReport &P".into();
            sh.page_setup.header_footer.odd_footer = "&RPage &P of &N".into();
        }
        let pg = paginate(&wb, 0, &PrintOptions::default());
        assert_eq!(pg.pages.len(), 1);
        let ctx = HfContext {
            page: 1,
            pages: 1,
            ..Default::default()
        };
        let sc = page_scene(&wb, &pg, &pg.pages[0], &PrintData::default(), &ctx);
        let t = sc.texts();
        for want in ["A", "B", "1", "3", "Report 1", "Page 1 of 1"] {
            assert!(t.iter().any(|s| s == want), "{want} in {t:?}");
        }
        assert!(
            t.iter().filter(|s| s.contains("long")).count() == 1,
            "{t:?}"
        );
        assert!(t.iter().any(|s| s.contains("lines")), "wrapped: {t:?}");
        fn has_fill(items: &[Item]) -> bool {
            items.iter().any(|i| match i {
                Item::Fill { color, .. } => *color == Color(255, 255, 0),
                Item::Clip { items, .. } => has_fill(items),
                _ => false,
            })
        }
        assert!(has_fill(&sc.items));
    }

    #[test]
    fn wrap_breaks_long_words() {
        let f = Face::default();
        let lines = wrap_text("aaaaaaaaaaaaaaaaaaaa bb", f, 10.0, 30.0);
        assert!(lines.len() > 2);
        assert!(lines.iter().all(|l| std14::text_width(f, l, 10.0) <= 30.0));
        assert_eq!(wrap_text("x\ny", f, 10.0, 100.0), vec!["x", "y"]);
        assert_eq!(wrap_text("ab cd", f, 10.0, 15.0), vec!["ab", "cd"]);
    }

    #[test]
    fn paper_sizes() {
        assert_eq!(paper_size(1), (612.0, 792.0));
        let a4 = paper_size(9);
        assert!((a4.0 - 595.28).abs() < 0.1 && (a4.1 - 841.89).abs() < 0.1);
        assert_eq!(paper_size(9999), (612.0, 792.0));
    }

    #[test]
    fn objects_and_checkboxes_print() {
        use sp_model::objects::{DrawObject, ObjectKind, Picture, Shape, StyleRefs, TextBody};
        let mut wb = Workbook::new();
        let cb = wb.styles.intern(Style {
            checkbox: true,
            ..Style::default()
        });
        let png = crate::objects::encode_png(2, 2, vec![255; 16]).unwrap();
        let img = crate::objects::image_data(png).unwrap();
        if let Some(sh) = wb.sheet_mut(0) {
            let mut c = Cell::constant(Value::Bool(true));
            c.style = cb;
            sh.set(CellAddr::new(0, 0), c);
            let anchor = |x: f64, y: f64| {
                crate::objects::anchor_from_rect(
                    &Sheet::new("x"),
                    sp_model::objects::AnchorKind::TwoCell,
                    sp_model::objects::Placement::Move,
                    (x, y, 100.0, 60.0),
                )
            };
            sh.objects.push(DrawObject {
                anchor: anchor(200.0, 100.0),
                kind: ObjectKind::Shape(Shape {
                    preset: "star5".into(),
                    style: Some(StyleRefs::shape_default()),
                    text: Some(TextBody::from_plain(
                        "Star text",
                        &Default::default(),
                        Some("ctr"),
                    )),
                    ..Shape::default()
                }),
                rotation: 30.0,
                ..DrawObject::default()
            });
            sh.objects.push(DrawObject {
                anchor: anchor(400.0, 300.0),
                kind: ObjectKind::Picture(Picture {
                    image: img,
                    ..Picture::default()
                }),
                ..DrawObject::default()
            });
            sh.objects.push(DrawObject {
                anchor: anchor(10.0, 10.0),
                hidden: true,
                kind: ObjectKind::Shape(Shape {
                    preset: "rect".into(),
                    text: Some(TextBody::from_plain("hidden", &Default::default(), None)),
                    ..Shape::default()
                }),
                ..DrawObject::default()
            });
        }
        // The print range grows to cover the objects (the used range is just A1).
        let r = content_range(&wb.sheets[0]).unwrap();
        assert!(r.end.col >= 7 && r.end.row >= 17, "{r:?}");
        let pg = paginate(&wb, 0, &PrintOptions::default());
        let sc = page_scene(
            &wb,
            &pg,
            &pg.pages[0],
            &PrintData::default(),
            &HfContext::default(),
        );
        fn walk<'a>(items: &'a [Item], out: &mut Vec<&'a Item>) {
            for it in items {
                if let Item::Clip { items, .. } = it {
                    walk(items, out);
                } else {
                    out.push(it);
                }
            }
        }
        let mut all = Vec::new();
        walk(&sc.items, &mut all);
        let areas = all
            .iter()
            .filter(|i| matches!(i, Item::Area { .. }))
            .count();
        let images = all
            .iter()
            .filter(|i| matches!(i, Item::Image { .. }))
            .count();
        assert_eq!((areas, images), (1, 1), "star fill + picture");
        let star = all.iter().find_map(|i| match i {
            Item::Text { text, angle, .. } if text == "Star text" => Some(*angle),
            _ => None,
        });
        assert_eq!(star, Some(-30.0), "text turns with the shape");
        assert!(!sc.texts().iter().any(|t| t == "hidden"));
        // Checked box: an outline square and a check mark.
        let lines = all
            .iter()
            .filter(|i| matches!(i, Item::Line { closed: true, pts, .. } if pts.len() == 4))
            .count();
        assert!(lines >= 1);
        assert!(
            all.iter()
                .any(|i| matches!(i, Item::Line { closed: false, pts, .. } if pts.len() == 3))
        );
    }
}
