//! Table dialogs: Create Table (range, "My table has headers", style gallery) and Table
//! Design (name, style, header / total / banded rows and columns, first / last column,
//! filter button, resize, convert to range).
//!
//! OK runs `insert.table` / `home.styles.format_as_table` / `table.design`; the style
//! thumbnails are painted from `sp_render::tablestyle`, the same source the grid uses.

use super::{Flow, error_line, label, ok_cancel, window};
use crate::App;
use crate::theme::{LIGHT as T, color};
use egui::{Context, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use sp_core::{CellAddr, CellRange, Value};
use sp_engine::Session;
use sp_model::Table;

/// Every built-in style name: Light 1–21, Medium 1–28, Dark 1–11.
pub fn style_names() -> Vec<String> {
    let mut v = Vec::with_capacity(60);
    for (fam, n) in [("Light", 21), ("Medium", 28), ("Dark", 11)] {
        for i in 1..=n {
            v.push(format!("TableStyle{fam}{i}"));
        }
    }
    v
}

/// Thumbnail of a style (5 rows × 4 columns) with the given options.
fn thumbnail(
    ui: &mut egui::Ui,
    size: Vec2,
    style: &str,
    opts: &DesignOpts,
    selected: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let p = ui.painter_at(rect.expand(2.0));
    p.rect_filled(rect, 1.0, T.preview_bg);
    let (rows, cols) = (5u32, 4u32);
    let mut t = Table::new(
        0,
        "Preview",
        CellRange::new(CellAddr::new(0, 0), CellAddr::new(rows - 1, cols - 1)),
        (0..cols).map(|c| format!("C{c}")).collect(),
    );
    t.style = style.to_string();
    t.header_row = opts.header_row;
    t.totals_row = opts.total_row;
    t.banded_rows = opts.banded_rows;
    t.banded_cols = opts.banded_cols;
    t.first_col = opts.first_col;
    t.last_col = opts.last_col;
    let inner = rect.shrink(3.0);
    let cw = inner.width() / cols as f32;
    let rh = inner.height() / rows as f32;
    for r in 0..rows {
        for c in 0..cols {
            let cell = Rect::from_min_size(
                Pos2::new(inner.min.x + c as f32 * cw, inner.min.y + r as f32 * rh),
                Vec2::new(cw, rh),
            );
            let d = sp_render::tablestyle::cell_style(&t, CellAddr::new(r, c));
            if let Some(f) = d.as_ref().and_then(|d| d.fill) {
                p.rect_filled(cell, 0.0, color(f));
            }
            // A text dash in each cell, bold rows thicker.
            let text = d
                .as_ref()
                .and_then(|d| d.font_color)
                .map_or(T.cell_text, color);
            let bold = d.as_ref().and_then(|d| d.bold) == Some(true);
            let y = cell.center().y;
            p.line_segment(
                [
                    Pos2::new(cell.min.x + cw * 0.2, y),
                    Pos2::new(cell.max.x - cw * 0.2, y),
                ],
                Stroke::new(if bold { 1.6 } else { 0.8 }, text),
            );
            let (top, bottom) = sp_render::tablestyle::cell_edges(&t, CellAddr::new(r, c));
            for (b, yy) in [(top, cell.min.y), (bottom, cell.max.y)] {
                if let Some(b) = b {
                    let col = b.color.map_or(T.border_auto, color);
                    p.hline(cell.x_range(), yy, Stroke::new(1.0, col));
                }
            }
        }
    }
    let (w, c) = if selected {
        (2.0, T.accent)
    } else if resp.hovered() {
        (1.5, T.handle_border)
    } else {
        (1.0, T.list_border)
    };
    p.rect_stroke(rect, 1.0, Stroke::new(w, c), StrokeKind::Outside);
    resp
}

/// Style gallery; returns true when the selection changed.
fn gallery(
    ui: &mut egui::Ui,
    style: &mut String,
    opts: &DesignOpts,
    id: &str,
    scroll_to_selected: bool,
) -> bool {
    let mut changed = false;
    egui::Frame::NONE
        .fill(T.list_bg)
        .stroke(Stroke::new(1.0, T.list_border))
        .inner_margin(egui::Margin::same(6))
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt(id)
                .max_height(170.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    let names = style_names();
                    for fam in ["Light", "Medium", "Dark"] {
                        label(ui, fam);
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                            for n in names
                                .iter()
                                .filter(|n| n.starts_with(&format!("TableStyle{fam}")))
                            {
                                let r = thumbnail(ui, Vec2::new(52.0, 38.0), n, opts, style == n)
                                    .on_hover_text(n.as_str());
                                if scroll_to_selected && style == n {
                                    r.scroll_to_me(Some(egui::Align::Center));
                                }
                                if r.clicked() && style != n {
                                    style.clone_from(n);
                                    changed = true;
                                }
                            }
                        });
                    }
                });
        });
    changed
}

/// Table style options shown by both dialogs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DesignOpts {
    pub header_row: bool,
    pub total_row: bool,
    pub banded_rows: bool,
    pub banded_cols: bool,
    pub first_col: bool,
    pub last_col: bool,
    pub filter_button: bool,
}

impl Default for DesignOpts {
    fn default() -> Self {
        DesignOpts {
            header_row: true,
            total_row: false,
            banded_rows: true,
            banded_cols: false,
            first_col: false,
            last_col: false,
            filter_button: true,
        }
    }
}

// ── Create Table ─────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CreateTable {
    /// `insert.table` or `home.styles.format_as_table`.
    pub cmd: String,
    pub range: String,
    pub has_headers: bool,
    pub style: String,
    /// Show the style gallery (Format as Table).
    pub pick_style: bool,
    pub error: Option<String>,
    #[serde(skip)]
    pub scrolled: bool,
}

/// The first row reads as headers: all text, and some row below holds data.
fn guess_headers(s: &Session, r: CellRange) -> bool {
    let Some(sh) = s.wb.sheet(s.sheet_index()) else {
        return true;
    };
    if r.rows() < 2 {
        return false;
    }
    let cols = r.start.col..=r.end.col.min(r.start.col.saturating_add(256));
    cols.clone().all(|c| {
        sh.get(CellAddr::new(r.start.row, c))
            .is_some_and(|x| matches!(x.value, Value::Text(_)))
    })
}

impl CreateTable {
    pub fn new(s: &Session, cmd: &str) -> Self {
        let sel = s.sel.range();
        let r = if sel.is_single() {
            sp_engine::commands::nav_cmds::current_region(s, s.sel.active)
        } else {
            sel
        };
        CreateTable {
            cmd: cmd.to_string(),
            range: r.to_a1(),
            has_headers: guess_headers(s, r),
            style: sp_model::table::DEFAULT_TABLE_STYLE.to_string(),
            pick_style: cmd == "home.styles.format_as_table",
            error: None,
            scrolled: false,
        }
    }

    pub fn params(&self) -> (String, Json) {
        (
            if self.cmd.is_empty() {
                "insert.table".to_string()
            } else {
                self.cmd.clone()
            },
            json!({
                "range": self.range.trim().trim_start_matches('='),
                "has_headers": self.has_headers,
                "style": self.style,
            }),
        )
    }

    pub fn ui(&mut self, ctx: &Context, app: &mut App) -> Flow {
        let title = if self.pick_style {
            "Format as Table"
        } else {
            "Create Table"
        };
        let width = if self.pick_style { 440.0 } else { 300.0 };
        let ((ok, cancel), esc) = window(ctx, title, width, |ui| {
            if self.pick_style {
                let opts = DesignOpts::default();
                gallery(ui, &mut self.style, &opts, "ct-gallery", !self.scrolled);
                self.scrolled = true;
                ui.add_space(4.0);
            }
            label(ui, "Where is the data for your table?");
            ui.add(egui::TextEdit::singleline(&mut self.range).desired_width(width - 10.0));
            ui.checkbox(&mut self.has_headers, "My table has headers");
            error_line(ui, &self.error);
            ok_cancel(ui, "OK")
        });
        if cancel || esc {
            return Flow::Close;
        }
        if ok || (super::enter_pressed(ctx) && !self.range.trim().is_empty()) {
            let (id, p) = self.params();
            return match app.try_exec(&id, p) {
                Ok(_) => Flow::Close,
                Err(e) => {
                    self.error = Some(e);
                    Flow::Keep
                }
            };
        }
        Flow::Keep
    }
}

// ── Table Design ─────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TableDesign {
    /// The table being edited (its name when the dialog opened).
    pub table: String,
    /// Table Name field.
    pub name: String,
    pub style: String,
    pub opts: DesignOpts,
    /// Resize Table: new range (A1).
    pub range: String,
    /// Values when the dialog opened, to send only what changed.
    pub orig: Option<Box<TableDesign>>,
    pub error: Option<String>,
    #[serde(skip)]
    pub scrolled: bool,
}

impl TableDesign {
    /// The table at the active cell, else the first on the sheet.
    pub fn new(s: &Session) -> Option<Self> {
        let sh = s.wb.sheet(s.sheet_index())?;
        let t = sh
            .tables
            .iter()
            .find(|t| t.range.contains(s.sel.active))
            .or_else(|| sh.tables.first())?;
        let mut d = TableDesign {
            table: t.name.clone(),
            name: t.name.clone(),
            style: t.style.clone(),
            opts: DesignOpts {
                header_row: t.header_row,
                total_row: t.totals_row,
                banded_rows: t.banded_rows,
                banded_cols: t.banded_cols,
                first_col: t.first_col,
                last_col: t.last_col,
                filter_button: t.autofilter,
            },
            range: t.range.to_a1(),
            orig: None,
            error: None,
            scrolled: false,
        };
        d.orig = Some(Box::new(d.clone()));
        Some(d)
    }

    /// Only the table under the active cell.
    pub fn at_active(s: &Session) -> Option<Self> {
        s.wb.sheet(s.sheet_index())?.table_at(s.sel.active)?;
        Self::new(s)
    }

    /// `table.design` params with only the fields that changed.
    pub fn params(&self) -> Json {
        let mut p = serde_json::Map::new();
        p.insert("name".into(), json!(self.table));
        let o = self.orig.as_deref();
        let changed_str = |cur: &str, f: fn(&TableDesign) -> &str| o.is_none_or(|o| f(o) != cur);
        if changed_str(&self.style, |o| &o.style) {
            p.insert("style".into(), json!(self.style));
        }
        let oo = o.map(|o| o.opts.clone());
        for (k, v, f) in [
            (
                "total_row",
                self.opts.total_row,
                oo.as_ref().map(|x| x.total_row),
            ),
            (
                "banded_rows",
                self.opts.banded_rows,
                oo.as_ref().map(|x| x.banded_rows),
            ),
            (
                "banded_cols",
                self.opts.banded_cols,
                oo.as_ref().map(|x| x.banded_cols),
            ),
            (
                "first_col",
                self.opts.first_col,
                oo.as_ref().map(|x| x.first_col),
            ),
            (
                "last_col",
                self.opts.last_col,
                oo.as_ref().map(|x| x.last_col),
            ),
            (
                "filter_button",
                self.opts.filter_button,
                oo.as_ref().map(|x| x.filter_button),
            ),
        ] {
            if f != Some(v) {
                p.insert(k.into(), json!(v));
            }
        }
        let range = self.range.trim().trim_start_matches('=');
        if changed_str(range, |o| &o.range) && !range.is_empty() {
            p.insert("resize".into(), json!(range));
        }
        let name = self.name.trim();
        if !name.is_empty() && name != self.table {
            p.insert("rename".into(), json!(name));
        }
        Json::Object(p)
    }

    pub fn ui(&mut self, ctx: &Context, app: &mut App) -> Flow {
        let mut convert = false;
        let ((ok, cancel), esc) = window(ctx, "Table Design", 460.0, |ui| {
            egui::Grid::new("td-props")
                .num_columns(2)
                .spacing([8.0, 6.0])
                .show(ui, |ui| {
                    label(ui, "Table Name:");
                    ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(200.0));
                    ui.end_row();
                    label(ui, "Resize Table:");
                    ui.add(egui::TextEdit::singleline(&mut self.range).desired_width(200.0));
                    ui.end_row();
                });
            ui.add_space(4.0);
            label(ui, "Table Style Options");
            egui::Grid::new("td-opts")
                .num_columns(3)
                .spacing([18.0, 2.0])
                .show(ui, |ui| {
                    ui.add_enabled(
                        false,
                        egui::Checkbox::new(&mut self.opts.header_row, "Header Row"),
                    );
                    ui.checkbox(&mut self.opts.first_col, "First Column");
                    ui.checkbox(&mut self.opts.filter_button, "Filter Button");
                    ui.end_row();
                    ui.checkbox(&mut self.opts.total_row, "Total Row");
                    ui.checkbox(&mut self.opts.last_col, "Last Column");
                    ui.label("");
                    ui.end_row();
                    ui.checkbox(&mut self.opts.banded_rows, "Banded Rows");
                    ui.checkbox(&mut self.opts.banded_cols, "Banded Columns");
                    ui.end_row();
                });
            ui.add_space(4.0);
            label(ui, "Table Styles");
            let opts = self.opts.clone();
            gallery(ui, &mut self.style, &opts, "td-gallery", !self.scrolled);
            self.scrolled = true;
            error_line(ui, &self.error);
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                convert = ui.button("Convert to Range").clicked();
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ok_cancel(ui, "OK")
                })
                .inner
            })
            .inner
        });
        if cancel || esc {
            return Flow::Close;
        }
        if convert {
            let p = json!({"name": self.table, "convert_to_range": true});
            return match app.try_exec("table.design", p) {
                Ok(_) => Flow::Close,
                Err(e) => {
                    self.error = Some(e);
                    Flow::Keep
                }
            };
        }
        if ok {
            let p = self.params();
            // Only the name key: nothing changed.
            if p.as_object().is_some_and(|o| o.len() == 1) {
                return Flow::Close;
            }
            return match app.try_exec("table.design", p) {
                Ok(_) => Flow::Close,
                Err(e) => {
                    self.error = Some(e);
                    Flow::Keep
                }
            };
        }
        Flow::Keep
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        let mut s = Session::new();
        for (c, v) in [
            ("A1", "Name"),
            ("B1", "Qty"),
            ("A2", "x"),
            ("B2", "1"),
            ("A3", "y"),
            ("B3", "2"),
        ] {
            s.execute("cell.set", &json!({"cell": c, "input": v}))
                .unwrap();
        }
        s
    }

    #[test]
    fn create_guesses_region_and_headers() {
        let s = session();
        let d = CreateTable::new(&s, "insert.table");
        assert_eq!(d.range, "A1:B3");
        assert!(d.has_headers);
        assert!(!d.pick_style);
        assert_eq!(style_names().len(), 60);
    }

    #[test]
    fn design_sends_only_changes() {
        let mut s = session();
        let (id, p) = CreateTable::new(&s, "insert.table").params();
        s.execute(&id, &p).unwrap();
        let mut d = TableDesign::new(&s).unwrap();
        assert_eq!(d.params().as_object().unwrap().len(), 1);
        d.opts.total_row = true;
        d.name = "Sales".into();
        d.style = "TableStyleDark2".into();
        let p = d.params();
        assert_eq!(p["total_row"], true);
        assert_eq!(p["rename"], "Sales");
        assert!(p.get("banded_rows").is_none());
        assert!(p.get("resize").is_none());
        s.execute("table.design", &p).unwrap();
        let t = &s.wb.sheet(0).unwrap().tables[0];
        assert_eq!(t.name, "Sales");
        assert!(t.totals_row);
        assert_eq!(t.style, "TableStyleDark2");
    }
}
