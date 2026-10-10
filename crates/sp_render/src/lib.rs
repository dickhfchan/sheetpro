//! L3 render model: what each visible cell shows and where. No UI libraries here; the shell
//! draws a `GridView` with whatever toolkit it uses.

use sp_core::numfmt::{FmtColor, format_value};
use sp_core::{CellAddr, MAX_COLS, MAX_ROWS, Value};
use sp_model::{Color, HAlign, Sheet, Style, VAlign, Workbook};

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
    }
}

/// Visible portion of a sheet at a scroll position and zoom.
#[derive(Debug, Clone)]
pub struct GridView {
    /// (row, y offset in px from top of the cell area, height px)
    pub rows: Vec<(u32, f32, f32)>,
    /// (col, x offset in px from left of the cell area, width px)
    pub cols: Vec<(u32, f32, f32)>,
    pub cells: Vec<DisplayCell>,
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
    let mut cells = Vec::new();
    let col_set: Vec<u32> = cols.iter().map(|c| c.0).collect();
    for (r, _, _) in &rows {
        let Some(row) = sheet.cells.get(r) else {
            continue;
        };
        for c in &col_set {
            if let Some(cell) = row.get(c) {
                let style = wb.styles.get(cell.style);
                cells.push(display_cell(
                    CellAddr::new(*r, *c),
                    &cell.value,
                    cell.formula_src(),
                    style,
                    show_formulas,
                ));
            }
        }
    }
    GridView { rows, cols, cells }
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
}
