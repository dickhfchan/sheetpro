//! L3 render model: what each visible cell shows and where. No UI libraries here; the shell
//! draws a `GridView` with whatever toolkit it uses.

pub mod condfmt;
pub mod tablestyle;

use sp_core::numfmt::{FmtColor, format_value};
use sp_core::{CellAddr, CellRange, MAX_COLS, MAX_ROWS, Value};
use sp_model::{Border, BorderStyle, Color, DiffStyle, HAlign, Sheet, Style, VAlign, Workbook};
use std::collections::HashMap;

/// Horizontal indent per `Style.indent` level, in px at 100% zoom (about one default-font
/// space-and-a-half, matching the target app's ~9 px step).
pub const INDENT_PX: f32 = 9.0;

/// Pixel width of a column of `chars` default-font characters (Calibri 11 at 96 dpi).
pub fn col_width_px(chars: f64) -> f32 {
    if chars <= 0.0 {
        0.0
    } else {
        (chars * 7.0 + 5.0).round() as f32
    }
}

/// Pixel height of a row of `points`.
pub fn row_height_px(points: f64) -> f32 {
    if points <= 0.0 {
        0.0
    } else {
        (points * 4.0 / 3.0).round() as f32
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DisplayCell {
    pub addr: CellAddr,
    pub text: String,
    pub align: Align,
    pub v_align: VAlign,
    pub color: Option<Color>,
    pub fill: Option<Color>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub wrap: bool,
    pub font_size: f32,
    /// Font family name as stored (e.g. "Calibri"); the shell maps it to a font it has.
    pub font_name: String,
    /// Indent level (each level is [`INDENT_PX`] at 100% zoom), from the aligned edge.
    pub indent: u8,
    /// Text rotation in degrees, counter-clockwise, -90..=90.
    pub rotation: i16,
    /// The cell's own four borders as stored (shared edges are resolved in [`GridView::borders`]).
    pub borders: CellBorders,
    /// The value is a number (numbers show `###` instead of overflowing).
    pub is_number: bool,
    /// Conditional-format data bar drawn behind the text.
    pub data_bar: Option<condfmt::DataBarDraw>,
    /// Conditional-format icon drawn at the left edge.
    pub icon: Option<condfmt::IconDraw>,
}

/// The four borders of one cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CellBorders {
    pub left: Border,
    pub right: Border,
    pub top: Border,
    pub bottom: Border,
}

impl CellBorders {
    pub fn of(style: &Style) -> Self {
        CellBorders {
            left: style.border_left,
            right: style.border_right,
            top: style.border_top,
            bottom: style.border_bottom,
        }
    }
}

/// Precedence when two cells disagree about a shared edge: the heavier line wins.
pub fn border_weight(b: BorderStyle) -> u8 {
    match b {
        BorderStyle::None => 0,
        BorderStyle::Dotted => 1,
        BorderStyle::Thin => 2,
        BorderStyle::Dashed => 3,
        BorderStyle::Medium => 4,
        BorderStyle::Double => 5,
        BorderStyle::Thick => 6,
    }
}

/// Pick the line drawn on an edge shared by two cells (`a` = top/left, `b` = bottom/right).
/// On a tie the bottom/right cell wins.
pub fn resolve_edge(a: Border, b: Border) -> Border {
    if border_weight(a.style) > border_weight(b.style) {
        a
    } else {
        b
    }
}

/// One straight border line, in px relative to the top-left of the cell area. Collinear
/// neighbours with the same style and colour are joined, so each edge is drawn once.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BorderSeg {
    /// Horizontal: from (x, y) to (x + len, y). Vertical: from (x, y) to (x, y + len).
    pub horizontal: bool,
    pub x: f32,
    pub y: f32,
    pub len: f32,
    pub style: BorderStyle,
    /// `None` = automatic (the theme's default border colour).
    pub color: Option<Color>,
}

/// A merged area that touches the view: its full rect (may extend past the view) and the
/// anchor cell's display, drawn once across the whole area.
#[derive(Debug, Clone, PartialEq)]
pub struct MergeView {
    pub range: CellRange,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Display of the top-left cell (`None` when it is empty and unstyled).
    pub cell: Option<DisplayCell>,
}

fn fmt_color(c: FmtColor) -> Color {
    match c {
        FmtColor::Black => Color(0, 0, 0),
        FmtColor::Blue => Color(0, 0, 255),
        FmtColor::Cyan => Color(0, 255, 255),
        FmtColor::Green => Color(0, 128, 0),
        FmtColor::Magenta => Color(255, 0, 255),
        FmtColor::Red => Color(255, 0, 0),
        FmtColor::White => Color(255, 255, 255),
        FmtColor::Yellow => Color(255, 255, 0),
    }
}

/// Display text and alignment for a value with a style; `show_formulas` shows formula text.
pub fn display_cell(
    addr: CellAddr,
    value: &Value,
    formula: Option<&str>,
    style: &Style,
    show_formulas: bool,
) -> DisplayCell {
    let (text, color, is_num) = if show_formulas && let Some(f) = formula {
        (format!("={f}"), None, false)
    } else {
        let f = format_value(value, &style.num_fmt);
        (
            f.text,
            f.color.map(fmt_color),
            matches!(value, Value::Number(_)),
        )
    };
    let align = match style.h_align {
        HAlign::General if style.indent > 0 => Align::Left,
        HAlign::Left | HAlign::Fill | HAlign::Justify => Align::Left,
        HAlign::Center | HAlign::CenterAcross => Align::Center,
        HAlign::Right => Align::Right,
        HAlign::General => match value {
            _ if show_formulas && formula.is_some() => Align::Left,
            Value::Number(_) if is_num => Align::Right,
            Value::Bool(_) | Value::Error(_) => Align::Center,
            _ => Align::Left,
        },
    };
    DisplayCell {
        addr,
        text,
        align,
        v_align: style.v_align,
        color: color.or(style.font_color),
        fill: style.fill,
        bold: style.bold,
        italic: style.italic,
        underline: style.underline > 0,
        strike: style.strike,
        wrap: style.wrap,
        font_size: style.font_size() as f32,
        font_name: style.font_name.clone(),
        indent: style.indent,
        rotation: style.rotation.clamp(-90, 90),
        borders: CellBorders::of(style),
        is_number: is_num,
        data_bar: None,
        icon: None,
    }
}

/// Apply a differential format on top of `s`. With `weak`, only attributes the cell left
/// at their defaults change (table styles yield to direct formatting).
pub fn apply_diff(s: &mut Style, d: &DiffStyle, weak: bool) {
    let base = Style::default();
    let free = |own: bool| !weak || own;
    if let Some(c) = d.font_color
        && free(s.font_color.is_none())
    {
        s.font_color = Some(c);
    }
    if let Some(c) = d.fill
        && free(s.fill.is_none())
    {
        s.fill = Some(c);
    }
    if let Some(b) = d.bold
        && free(!s.bold)
    {
        s.bold = b;
    }
    if let Some(b) = d.italic
        && free(!s.italic)
    {
        s.italic = b;
    }
    if let Some(b) = d.underline
        && free(s.underline == 0)
    {
        s.underline = u8::from(b);
    }
    if let Some(b) = d.strike
        && free(!s.strike)
    {
        s.strike = b;
    }
    if let Some(f) = &d.num_fmt
        && free(s.num_fmt == base.num_fmt)
    {
        s.num_fmt.clone_from(f);
    }
    if let Some(b) = d.border {
        for side in [
            &mut s.border_left,
            &mut s.border_right,
            &mut s.border_top,
            &mut s.border_bottom,
        ] {
            if free(side.style == BorderStyle::None) {
                *side = b;
            }
        }
    }
}

/// Table styles and conditional formats resolved for the visible cells: the effective style
/// of each affected cell plus any data bar / icon.
pub fn overlays(
    wb: &Workbook,
    sheet: &Sheet,
    rows: &[(u32, f32, f32)],
    cols: &[(u32, f32, f32)],
) -> HashMap<CellAddr, (Style, Option<condfmt::CfOverride>)> {
    let mut out = HashMap::new();
    let (Some(r0), Some(r1), Some(c0), Some(c1)) = (
        rows.iter().map(|r| r.0).min(),
        rows.iter().map(|r| r.0).max(),
        cols.iter().map(|c| c.0).min(),
        cols.iter().map(|c| c.0).max(),
    ) else {
        return out;
    };
    let span = CellRange::new(CellAddr::new(r0, c0), CellAddr::new(r1, c1));
    let tables: Vec<&sp_model::Table> = sheet
        .tables
        .iter()
        .filter(|t| t.range.intersect(&span).is_some())
        .collect();
    let cf_live = sheet
        .cond_formats
        .iter()
        .any(|f| f.ranges.iter().any(|r| r.intersect(&span).is_some()));
    if tables.is_empty() && !cf_live {
        return out;
    }
    let idx = wb
        .sheets
        .iter()
        .position(|s| std::ptr::eq(s, sheet))
        .or_else(|| wb.sheet_index(&sheet.name))
        .unwrap_or(0);
    let mut cf = condfmt::CfEval::new(wb, idx, sheet);
    for (r, _, _) in rows {
        for (c, _, _) in cols {
            let a = CellAddr::new(*r, *c);
            let tbl = tables.iter().find(|t| t.range.contains(a));
            let table = tbl.and_then(|t| tablestyle::cell_style(t, a));
            let edges = tbl.map_or((None, None), |t| tablestyle::cell_edges(t, a));
            let over = if cf_live && sheet.cond_formats.iter().any(|f| f.covers(a)) {
                cf.cell(a, sheet.value(a))
            } else {
                None
            };
            if table.is_none() && over.is_none() && edges == (None, None) {
                continue;
            }
            let mut st = sheet
                .get(a)
                .map_or_else(Style::default, |c| wb.styles.get(c.style).clone());
            if let Some(t) = &table {
                apply_diff(&mut st, t, true);
            }
            for (edge, side) in [
                (edges.0, &mut st.border_top),
                (edges.1, &mut st.border_bottom),
            ] {
                if let Some(e) = edge
                    && side.style == BorderStyle::None
                {
                    *side = e;
                }
            }
            if let Some(o) = &over {
                apply_diff(&mut st, &o.style, false);
            }
            out.insert(a, (st, over));
        }
    }
    out
}

/// Visible portion of a sheet at a scroll position and zoom.
#[derive(Debug, Clone)]
pub struct GridView {
    /// (row, y offset in px from top of the cell area, height px)
    pub rows: Vec<(u32, f32, f32)>,
    /// (col, x offset in px from left of the cell area, width px)
    pub cols: Vec<(u32, f32, f32)>,
    /// Visible cells, excluding cells covered by a merge (see `merges`).
    pub cells: Vec<DisplayCell>,
    /// Merged areas intersecting the view.
    pub merges: Vec<MergeView>,
    /// Resolved cell borders: shared edges once, nothing inside merged areas.
    pub borders: Vec<BorderSeg>,
}

/// Lay out the rows/cols that fit in `width × height` px starting at `top_left`, frozen
/// panes first. Hidden rows/cols are skipped.
pub fn layout(
    wb: &Workbook,
    sheet: &Sheet,
    top_left: CellAddr,
    width: f32,
    height: f32,
    zoom: f32,
    show_formulas: bool,
) -> GridView {
    let zoom = zoom.clamp(0.1, 4.0);
    let freeze = sheet.freeze.unwrap_or_default();
    let mut rows = Vec::new();
    let mut y = 0.0;
    let row_iter = (0..freeze.row).chain(top_left.row.max(freeze.row)..MAX_ROWS);
    for r in row_iter {
        if y > height {
            break;
        }
        let h = row_height_px(sheet.row_height(r)) * zoom;
        if h <= 0.0 {
            continue;
        }
        rows.push((r, y, h));
        y += h;
    }
    let mut cols = Vec::new();
    let mut x = 0.0;
    let col_iter = (0..freeze.col).chain(top_left.col.max(freeze.col)..MAX_COLS);
    for c in col_iter {
        if x > width {
            break;
        }
        let w = col_width_px(sheet.col_width(c)) * zoom;
        if w <= 0.0 {
            continue;
        }
        cols.push((c, x, w));
        x += w;
    }
    let (Some(first_row), Some(last_row), Some(first_col), Some(last_col)) =
        (rows.first(), rows.last(), cols.first(), cols.last())
    else {
        return GridView {
            rows,
            cols,
            cells: Vec::new(),
            merges: Vec::new(),
            borders: Vec::new(),
        };
    };
    // Merges that touch any visible row and column (frozen panes make the view non-contiguous,
    // so test the visible span loosely; non-visible merges are cheap to carry).
    let row_lo = rows.iter().map(|r| r.0).min().unwrap_or(first_row.0);
    let col_lo = cols.iter().map(|c| c.0).min().unwrap_or(first_col.0);
    let span = CellRange::new(
        CellAddr::new(row_lo, col_lo),
        CellAddr::new(last_row.0, last_col.0),
    );
    let visible_merges: Vec<CellRange> = sheet
        .merges
        .iter()
        .copied()
        .filter(|m| m.intersect(&span).is_some())
        .collect();
    let merge_of = |a: CellAddr| visible_merges.iter().copied().find(|m| m.contains(a));

    let over = overlays(wb, sheet, &rows, &cols);
    let style_at = |a: CellAddr| {
        over.get(&a)
            .map(|o| &o.0)
            .or_else(|| sheet.get(a).map(|c| wb.styles.get(c.style)))
    };
    let decorate = |mut d: DisplayCell| {
        if let Some((_, Some(o))) = over.get(&d.addr) {
            d.data_bar = o.data_bar;
            d.icon.clone_from(&o.icon);
            if o.hide_value && !show_formulas {
                d.text.clear();
            }
        }
        d
    };

    let mut cells = Vec::new();
    for (r, _, _) in &rows {
        let row = sheet.cells.get(r);
        for (c, _, _) in &cols {
            let a = CellAddr::new(*r, *c);
            let cell = row.and_then(|row| row.get(c));
            if cell.is_none() && !over.contains_key(&a) {
                continue;
            }
            if merge_of(a).is_some() {
                continue;
            }
            let Some(style) = style_at(a) else {
                continue;
            };
            cells.push(decorate(display_cell(
                a,
                cell.map_or(&Value::Empty, |c| &c.value),
                cell.and_then(|c| c.formula_src()),
                style,
                show_formulas,
            )));
        }
    }

    let merges = visible_merges
        .iter()
        .map(|m| {
            let (x, w) = extent(&cols, m.start.col, m.end.col, |c| {
                col_width_px(sheet.col_width(c)) * zoom
            });
            let (y, h) = extent(&rows, m.start.row, m.end.row, |r| {
                row_height_px(sheet.row_height(r)) * zoom
            });
            let cell = sheet.get(m.start).map(|c| {
                decorate(display_cell(
                    m.start,
                    &c.value,
                    c.formula_src(),
                    style_at(m.start).unwrap_or_else(|| wb.styles.get(c.style)),
                    show_formulas,
                ))
            });
            MergeView {
                range: *m,
                x,
                y,
                w,
                h,
                cell,
            }
        })
        .collect();

    let borders = resolve_borders(&rows, &cols, &style_at, &merge_of);
    GridView {
        rows,
        cols,
        cells,
        merges,
        borders,
    }
}

/// Max hidden-side rows/cols walked when a merge extends past the view.
const EXTENT_WALK_CAP: u32 = 2_000;

/// Offset and size spanned by indexes `lo..=hi` given the visible `(index, offset, size)`
/// list; parts outside the view are measured with `size_of` (bounded walk).
fn extent(list: &[(u32, f32, f32)], lo: u32, hi: u32, size_of: impl Fn(u32) -> f32) -> (f32, f32) {
    let inside: Vec<&(u32, f32, f32)> = list.iter().filter(|e| e.0 >= lo && e.0 <= hi).collect();
    let (Some(first), Some(last)) = (inside.first(), inside.last()) else {
        return (0.0, 0.0);
    };
    let mut start = first.1;
    for i in lo.max(first.0.saturating_sub(EXTENT_WALK_CAP))..first.0 {
        start -= size_of(i);
    }
    let mut end = last.1 + last.2;
    if last.0 < hi {
        for i in last.0 + 1..=hi.min(last.0.saturating_add(EXTENT_WALK_CAP)) {
            end += size_of(i);
        }
    }
    (start, end - start)
}

/// Resolve every visible cell edge to at most one line.
fn resolve_borders<'a>(
    rows: &[(u32, f32, f32)],
    cols: &[(u32, f32, f32)],
    style_at: &dyn Fn(CellAddr) -> Option<&'a Style>,
    merge_of: &dyn Fn(CellAddr) -> Option<CellRange>,
) -> Vec<BorderSeg> {
    let nr = rows.len();
    let nc = cols.len();
    // Borders of the visible cells, row-major, plus the off-screen neighbours on each side.
    let mut grid: Vec<CellBorders> = Vec::with_capacity(nr * nc);
    for (r, _, _) in rows {
        for (c, _, _) in cols {
            grid.push(cell_borders(CellAddr::new(*r, *c), style_at, merge_of));
        }
    }
    let at = |ri: usize, ci: usize| grid.get(ri * nc + ci).copied().unwrap_or_default();
    let outside = |r: Option<u32>, c: Option<u32>| match (r, c) {
        (Some(r), Some(c)) => cell_borders(CellAddr::new(r, c), style_at, merge_of),
        _ => CellBorders::default(),
    };
    let same_merge = |a: CellAddr, b: CellAddr| match (merge_of(a), merge_of(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    };
    let mut segs: Vec<BorderSeg> = Vec::new();
    let mut push = |seg: BorderSeg| {
        if seg.style == BorderStyle::None || seg.len <= 0.0 {
            return;
        }
        if let Some(prev) = segs.last_mut()
            && prev.horizontal == seg.horizontal
            && prev.style == seg.style
            && prev.color == seg.color
            && (if seg.horizontal {
                prev.y == seg.y && (prev.x + prev.len - seg.x).abs() < 0.01
            } else {
                prev.x == seg.x && (prev.y + prev.len - seg.y).abs() < 0.01
            })
        {
            prev.len += seg.len;
            return;
        }
        segs.push(seg);
    };
    // Horizontal edges: above row ri (ri = nr is the bottom of the last row).
    for ri in 0..=nr {
        let above = ri.checked_sub(1).and_then(|i| rows.get(i));
        let below = rows.get(ri);
        let y = match (above, below) {
            (_, Some(b)) => b.1,
            (Some(a), None) => a.1 + a.2,
            (None, None) => continue,
        };
        for (ci, (c, x, w)) in cols.iter().enumerate() {
            let top = match above {
                Some(_) => at(ri - 1, ci).bottom,
                None => outside(below.and_then(|b| b.0.checked_sub(1)), Some(*c)).bottom,
            };
            let bottom = match below {
                Some(_) => at(ri, ci).top,
                None => outside(above.and_then(|a| a.0.checked_add(1)), Some(*c)).top,
            };
            if top.style == BorderStyle::None && bottom.style == BorderStyle::None {
                continue;
            }
            if let (Some(a), Some(b)) = (above, below)
                && same_merge(CellAddr::new(a.0, *c), CellAddr::new(b.0, *c))
            {
                continue;
            }
            let e = resolve_edge(top, bottom);
            push(BorderSeg {
                horizontal: true,
                x: *x,
                y,
                len: *w,
                style: e.style,
                color: e.color,
            });
        }
    }
    // Vertical edges: left of column ci.
    for ci in 0..=nc {
        let left = ci.checked_sub(1).and_then(|i| cols.get(i));
        let right = cols.get(ci);
        let x = match (left, right) {
            (_, Some(r)) => r.1,
            (Some(l), None) => l.1 + l.2,
            (None, None) => continue,
        };
        for (ri, (r, y, h)) in rows.iter().enumerate() {
            let lb = match left {
                Some(_) => at(ri, ci - 1).right,
                None => outside(Some(*r), right.and_then(|c| c.0.checked_sub(1))).right,
            };
            let rb = match right {
                Some(_) => at(ri, ci).left,
                None => outside(Some(*r), left.and_then(|c| c.0.checked_add(1))).left,
            };
            if lb.style == BorderStyle::None && rb.style == BorderStyle::None {
                continue;
            }
            if let (Some(l), Some(rt)) = (left, right)
                && same_merge(CellAddr::new(*r, l.0), CellAddr::new(*r, rt.0))
            {
                continue;
            }
            let e = resolve_edge(lb, rb);
            push(BorderSeg {
                horizontal: false,
                x,
                y: *y,
                len: *h,
                style: e.style,
                color: e.color,
            });
        }
    }
    segs
}

/// A cell's borders; on a merge boundary a missing side falls back to the anchor's border, so
/// borders set on the merge's top-left cell frame the whole area.
fn cell_borders<'a>(
    a: CellAddr,
    style_at: &dyn Fn(CellAddr) -> Option<&'a Style>,
    merge_of: &dyn Fn(CellAddr) -> Option<CellRange>,
) -> CellBorders {
    let own = style_at(a).map(CellBorders::of).unwrap_or_default();
    let Some(m) = merge_of(a) else {
        return own;
    };
    let anchor = style_at(m.start).map(CellBorders::of).unwrap_or_default();
    let pick = |own: Border, anchor: Border, on_edge: bool| {
        if !on_edge {
            Border::default()
        } else if own.style == BorderStyle::None {
            anchor
        } else {
            own
        }
    };
    CellBorders {
        left: pick(own.left, anchor.left, a.col == m.start.col),
        right: pick(own.right, anchor.right, a.col == m.end.col),
        top: pick(own.top, anchor.top, a.row == m.start.row),
        bottom: pick(own.bottom, anchor.bottom, a.row == m.end.row),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sp_model::Cell;

    #[test]
    fn geometry() {
        assert_eq!(col_width_px(8.43), 64.0);
        assert_eq!(row_height_px(15.0), 20.0);
    }

    #[test]
    fn layout_skips_hidden_and_formats() {
        let mut wb = Workbook::new();
        let pct = wb
            .styles
            .derive(sp_model::StyleId::DEFAULT, |s| s.num_fmt = "0%".into());
        let s = &mut wb.sheets[0];
        s.hidden_rows.insert(1);
        s.set(
            CellAddr::new(0, 0),
            Cell {
                style: pct,
                ..Cell::constant(Value::Number(0.25))
            },
        );
        s.set(CellAddr::new(0, 1), Cell::constant(Value::text("hi")));
        let sheet = wb.sheets[0].clone();
        let v = layout(&wb, &sheet, CellAddr::new(0, 0), 300.0, 100.0, 1.0, false);
        assert!(v.rows.iter().all(|r| r.0 != 1));
        let a1 = v
            .cells
            .iter()
            .find(|c| c.addr == CellAddr::new(0, 0))
            .unwrap();
        assert_eq!((a1.text.as_str(), a1.align), ("25%", Align::Right));
        let b1 = v
            .cells
            .iter()
            .find(|c| c.addr == CellAddr::new(0, 1))
            .unwrap();
        assert_eq!(b1.align, Align::Left);
    }

    fn styled(wb: &mut Workbook, a: CellAddr, input: Value, f: impl FnOnce(&mut Style)) {
        let id = wb.styles.derive(sp_model::StyleId::DEFAULT, f);
        wb.sheets[0].set(
            a,
            Cell {
                style: id,
                ..Cell::constant(input)
            },
        );
    }

    fn view(wb: &Workbook) -> GridView {
        let sheet = wb.sheets[0].clone();
        layout(wb, &sheet, CellAddr::new(0, 0), 400.0, 200.0, 1.0, false)
    }

    #[test]
    fn display_cell_carries_alignment_fields() {
        let mut wb = Workbook::new();
        styled(&mut wb, CellAddr::new(0, 0), Value::text("x"), |s| {
            s.wrap = true;
            s.v_align = VAlign::Top;
            s.indent = 2;
            s.rotation = 90;
            s.font_name = "Courier New".into();
        });
        let v = view(&wb);
        let d = &v.cells[0];
        assert!(d.wrap);
        assert_eq!(d.v_align, VAlign::Top);
        assert_eq!(d.indent, 2);
        assert_eq!(d.rotation, 90);
        assert_eq!(d.font_name, "Courier New");
        assert_eq!(d.align, Align::Left);
        assert!(!d.is_number);
    }

    #[test]
    fn indent_on_general_number_aligns_left() {
        let mut wb = Workbook::new();
        styled(&mut wb, CellAddr::new(0, 0), Value::Number(5.0), |s| {
            s.indent = 1
        });
        assert_eq!(view(&wb).cells[0].align, Align::Left);
    }

    #[test]
    fn shared_edge_drawn_once_heavier_wins() {
        let mut wb = Workbook::new();
        let thin = Border {
            style: BorderStyle::Thin,
            color: None,
        };
        let thick = Border {
            style: BorderStyle::Thick,
            color: Some(Color(255, 0, 0)),
        };
        // A1 bottom thin, A2 top thick: one red thick line at y=20.
        styled(&mut wb, CellAddr::new(0, 0), Value::Empty, |s| {
            s.border_bottom = thin
        });
        styled(&mut wb, CellAddr::new(1, 0), Value::Empty, |s| {
            s.border_top = thick
        });
        let v = view(&wb);
        assert_eq!(v.borders.len(), 1);
        let b = v.borders[0];
        assert!(b.horizontal);
        assert_eq!((b.x, b.y, b.len), (0.0, 20.0, 64.0));
        assert_eq!(b.style, BorderStyle::Thick);
        assert_eq!(b.color, Some(Color(255, 0, 0)));
        assert_eq!(v.cells[0].borders.bottom, thin);
    }

    #[test]
    fn collinear_edges_join() {
        let mut wb = Workbook::new();
        let thin = Border {
            style: BorderStyle::Thin,
            color: None,
        };
        for c in 0..3 {
            styled(&mut wb, CellAddr::new(0, c), Value::Empty, |s| {
                s.border_top = thin
            });
        }
        let v = view(&wb);
        assert_eq!(v.borders.len(), 1);
        assert_eq!(v.borders[0].len, 192.0);
    }

    #[test]
    fn merge_hides_inner_cells_and_edges() {
        let mut wb = Workbook::new();
        let thin = Border {
            style: BorderStyle::Thin,
            color: None,
        };
        // Borders set only on the anchor frame the whole merge; covered cells are hidden.
        styled(&mut wb, CellAddr::new(1, 1), Value::text("m"), |s| {
            s.border_left = thin;
            s.border_right = thin;
            s.border_top = thin;
            s.border_bottom = thin;
        });
        styled(&mut wb, CellAddr::new(1, 2), Value::text("hidden"), |s| {
            s.border_left = thin;
        });
        wb.sheets[0]
            .merges
            .push(CellRange::new(CellAddr::new(1, 1), CellAddr::new(2, 2)));
        let v = view(&wb);
        assert!(v.cells.is_empty());
        assert_eq!(v.merges.len(), 1);
        let m = &v.merges[0];
        assert_eq!((m.x, m.y, m.w, m.h), (64.0, 20.0, 128.0, 40.0));
        assert_eq!(m.cell.as_ref().map(|d| d.text.as_str()), Some("m"));
        // Exactly the outline: top, bottom (each joined across 2 cols), left, right.
        let h: Vec<_> = v.borders.iter().filter(|b| b.horizontal).collect();
        let vt: Vec<_> = v.borders.iter().filter(|b| !b.horizontal).collect();
        assert_eq!(h.len(), 2);
        assert!(h.iter().all(|b| b.len == 128.0));
        assert_eq!(vt.len(), 2);
        assert!(
            vt.iter()
                .all(|b| b.len == 40.0 && (b.x == 64.0 || b.x == 192.0))
        );
    }

    #[test]
    fn border_of_offscreen_neighbour_shows_at_view_edge() {
        let mut wb = Workbook::new();
        let thin = Border {
            style: BorderStyle::Dashed,
            color: None,
        };
        styled(&mut wb, CellAddr::new(4, 0), Value::Empty, |s| {
            s.border_bottom = thin
        });
        let sheet = wb.sheets[0].clone();
        let v = layout(&wb, &sheet, CellAddr::new(5, 0), 100.0, 60.0, 1.0, false);
        assert_eq!(v.borders.len(), 1);
        assert_eq!(
            (v.borders[0].y, v.borders[0].style),
            (0.0, BorderStyle::Dashed)
        );
    }

    #[test]
    fn table_and_cf_overlays() {
        let mut wb = Workbook::new();
        let red = wb.styles.derive(sp_model::StyleId::DEFAULT, |s| {
            s.fill = Some(Color(255, 0, 0))
        });
        let s = &mut wb.sheets[0];
        s.set(CellAddr::new(0, 0), Cell::constant(Value::text("H")));
        s.set(
            CellAddr::new(1, 0),
            Cell {
                style: red,
                ..Cell::constant(Value::Number(1.0))
            },
        );
        s.set(CellAddr::new(2, 0), Cell::constant(Value::Number(50.0)));
        s.tables.push(sp_model::Table::new(
            1,
            "T",
            CellRange::parse("A1:B4").unwrap(),
            vec!["H".into(), "I".into()],
        ));
        s.cond_formats.push(sp_model::CondFormat {
            ranges: vec![CellRange::parse("A2:A4").unwrap()],
            rules: vec![sp_model::CfRule {
                kind: sp_model::CfKind::CellIs {
                    op: sp_model::CfOp::Greater,
                    f1: "10".into(),
                    f2: String::new(),
                },
                style: DiffStyle {
                    bold: Some(true),
                    ..DiffStyle::default()
                },
                priority: 1,
                stop_if_true: false,
            }],
        });
        let v = view(&wb);
        let at = |r, c| v.cells.iter().find(|d| d.addr == CellAddr::new(r, c));
        // Header styled, direct fill beats the band, CF bolds A3, empty B2 gets the band.
        assert_eq!(at(0, 0).unwrap().color, Some(Color::WHITE));
        assert_eq!(at(1, 0).unwrap().fill, Some(Color(255, 0, 0)));
        assert!(at(2, 0).unwrap().bold);
        assert!(at(1, 1).unwrap().fill.is_some());
        assert!(at(1, 1).unwrap().text.is_empty());
        assert!(at(5, 0).is_none());
    }

    #[test]
    fn edge_precedence() {
        assert!(border_weight(BorderStyle::Thick) > border_weight(BorderStyle::Double));
        assert!(border_weight(BorderStyle::Medium) > border_weight(BorderStyle::Thin));
        let a = Border {
            style: BorderStyle::Thin,
            color: Some(Color(1, 2, 3)),
        };
        let b = Border {
            style: BorderStyle::Thin,
            color: None,
        };
        assert_eq!(resolve_edge(a, b), b);
    }
}
