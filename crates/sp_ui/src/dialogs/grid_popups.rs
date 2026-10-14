//! Popups opened from objects in the grid: the AutoFilter drop-down (value checklist with
//! search, sort, number/text/date conditions, clear), the data-validation list, the note /
//! comment card, and the validation alert shown when an entry breaks a rule.
//!
//! Every action runs a command (`data.filter`, `data.sort`, `cell.set`, `insert.note`,
//! `insert.comment`, `review.comment.delete`, `edit.undo`).

use super::{Flow, label, window};
use crate::App;
use crate::theme::LIGHT as T;
use egui::{Context, Key, Order, Pos2, RichText, Stroke};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sp_core::{CellAddr, CellRange, Value, col_to_letters};
use sp_engine::Session;
use sp_engine::commands::filter_cmds::cell_text;
use sp_model::{CommentKind, ErrorStyle, FilterCriteria};

/// Cap on distinct values listed in a filter drop-down.
const MAX_ITEMS: usize = 10_000;
/// Cap on rows scanned to build the list.
const MAX_ROWS_SCANNED: u32 = 200_000;

/// A non-modal popup at `pos` (screen px). Returns the body's value and whether it was
/// dismissed (Escape, or a press outside it on the grid / chrome).
pub(crate) fn popup<R>(
    ctx: &Context,
    id: &str,
    pos: Option<[f32; 2]>,
    width: f32,
    body: impl FnOnce(&mut egui::Ui) -> R,
) -> (R, bool) {
    let pos = pos.map_or_else(|| ctx.content_rect().center(), |p| Pos2::new(p[0], p[1]));
    let style = ctx.global_style();
    let frame = egui::Frame::popup(&style)
        .fill(T.dialog_bg)
        .stroke(Stroke::new(1.0, T.dialog_border))
        .corner_radius(4.0)
        .inner_margin(egui::Margin::same(8));
    let r = egui::Area::new(egui::Id::new(id))
        .order(Order::Foreground)
        .fixed_pos(pos)
        .constrain(true)
        .show(ctx, |ui| {
            frame
                .show(ui, |ui| {
                    ui.set_width(width);
                    ui.visuals_mut().selection.bg_fill = T.list_selected;
                    ui.visuals_mut().selection.stroke.color = T.ribbon_text;
                    body(ui)
                })
                .inner
        });
    let rect = r.response.rect;
    let outside = ctx.input(|i| {
        i.pointer.any_pressed()
            && i.pointer.press_origin().is_some_and(|p| {
                !rect.contains(p)
                    && ctx
                        .layer_id_at(p)
                        .is_none_or(|l| l.order == Order::Background)
            })
    });
    let esc = ctx.input(|i| i.key_pressed(Key::Escape));
    (r.inner, outside || esc)
}

fn menu_row(ui: &mut egui::Ui, text: &str, enabled: bool) -> bool {
    let layout = egui::Layout::top_down_justified(egui::Align::Min);
    ui.with_layout(layout, |ui| {
        ui.add_enabled(
            enabled,
            egui::Button::new(RichText::new(text).size(12.5)).frame(false),
        )
        .clicked()
    })
    .inner
}

// ── AutoFilter drop-down ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FilterItem {
    /// Display text; empty = "(Blanks)".
    pub text: String,
    pub checked: bool,
}

/// A custom condition being edited (Number / Text / Date Filters ▸ …).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct CustomCond {
    pub op1: String,
    pub val1: String,
    pub and: bool,
    pub op2: String,
    pub val2: String,
    /// Top 10 form instead of comparisons.
    pub top: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FilterDropdown {
    /// Filter range (header row first).
    pub range: CellRange,
    /// Sheet column of the button.
    pub col: u32,
    /// Table name when the button belongs to a table header.
    pub table: Option<String>,
    pub pos: Option<[f32; 2]>,
    pub search: String,
    pub items: Vec<FilterItem>,
    /// "text" | "number" | "date": which condition submenu applies. Serialized as
    /// `value_kind`: `kind` is the [`super::Dialog`] tag.
    #[serde(rename = "value_kind")]
    pub kind: String,
    /// Column currently filtered.
    pub active: bool,
    pub custom: Option<CustomCond>,
    pub error: Option<String>,
}

impl Default for FilterDropdown {
    fn default() -> Self {
        FilterDropdown {
            range: CellRange::default(),
            col: 0,
            table: None,
            pos: None,
            search: String::new(),
            items: Vec::new(),
            kind: "text".into(),
            active: false,
            custom: None,
            error: None,
        }
    }
}

/// (label, op) pairs of each condition menu; `op` is a `data.filter` custom op, or
/// `between` / `top10` handled here.
pub fn condition_ops(kind: &str) -> &'static [(&'static str, &'static str)] {
    match kind {
        "number" => &[
            ("Equals…", "="),
            ("Does Not Equal…", "<>"),
            ("Greater Than…", ">"),
            ("Greater Than Or Equal To…", ">="),
            ("Less Than…", "<"),
            ("Less Than Or Equal To…", "<="),
            ("Between…", "between"),
            ("Top 10…", "top10"),
        ],
        "date" => &[
            ("Equals…", "="),
            ("Before…", "<"),
            ("After…", ">"),
            ("Between…", "between"),
        ],
        _ => &[
            ("Equals…", "="),
            ("Does Not Equal…", "<>"),
            ("Begins With…", "begins_with"),
            ("Ends With…", "ends_with"),
            ("Contains…", "contains"),
            ("Does Not Contain…", "not_contains"),
        ],
    }
}

/// Every op the two condition rows offer (labels for the combo boxes).
const ALL_OPS: [(&str, &str); 12] = [
    ("equals", "="),
    ("does not equal", "<>"),
    ("is greater than", ">"),
    ("is greater than or equal to", ">="),
    ("is less than", "<"),
    ("is less than or equal to", "<="),
    ("begins with", "begins_with"),
    ("does not begin with", "not_begins_with"),
    ("ends with", "ends_with"),
    ("does not end with", "not_ends_with"),
    ("contains", "contains"),
    ("does not contain", "not_contains"),
];

fn op_label(op: &str) -> &'static str {
    ALL_OPS
        .iter()
        .find(|(_, o)| *o == op)
        .map_or("", |(l, _)| *l)
}

impl FilterDropdown {
    pub fn new(s: &Session, range: CellRange, col: u32, table: Option<String>) -> Self {
        let mut d = FilterDropdown {
            range,
            col,
            table,
            ..Default::default()
        };
        let si = s.sheet_index();
        let Some(sh) = s.wb.sheet(si) else {
            return d;
        };
        let off = col.saturating_sub(range.start.col);
        let crit = sh
            .auto_filter
            .as_ref()
            .filter(|af| af.range == range && d.table.is_none())
            .and_then(|af| af.columns.iter().find(|c| c.col == off))
            .map(|c| c.criteria.clone());
        d.active = crit.is_some();
        // Distinct values (numbers in numeric order, then text, then blanks).
        let mut seen = std::collections::HashSet::new();
        let mut nums: Vec<(f64, String)> = Vec::new();
        let mut texts: Vec<String> = Vec::new();
        let mut blanks = false;
        // Values on rows still shown (custom / top-10 filters check these).
        let mut shown = std::collections::HashSet::new();
        let (mut n_num, mut n_date, mut n_text) = (0usize, 0usize, 0usize);
        let last = range
            .end
            .row
            .min(range.start.row.saturating_add(MAX_ROWS_SCANNED));
        for row in range.start.row.saturating_add(1)..=last {
            let a = CellAddr::new(row, col);
            let t = cell_text(&s.wb, sh, a);
            let v = sh.value(a);
            match &v {
                Value::Number(_) => {
                    let fmt = sh
                        .get(a)
                        .map(|c| s.wb.styles.get(c.style).num_fmt.clone())
                        .unwrap_or_default();
                    if sp_core::numfmt::is_date_format(&fmt) {
                        n_date += 1;
                    } else {
                        n_num += 1;
                    }
                }
                Value::Empty => {}
                _ => n_text += 1,
            }
            if !sh.hidden_rows.contains(&row) {
                shown.insert(t.to_lowercase());
            }
            if t.is_empty() {
                blanks = true;
                continue;
            }
            if seen.len() >= MAX_ITEMS || !seen.insert(t.to_lowercase()) {
                continue;
            }
            match v {
                Value::Number(n) => nums.push((*n, t)),
                _ => texts.push(t),
            }
        }
        nums.sort_by(|a, b| a.0.total_cmp(&b.0));
        texts.sort_by_key(|t| t.to_lowercase());
        d.kind = if n_date > n_num.max(n_text) {
            "date"
        } else if n_num > n_text {
            "number"
        } else {
            "text"
        }
        .into();
        let checked = |t: &str| match &crit {
            Some(FilterCriteria::Values { values, blanks }) => {
                if t.is_empty() {
                    *blanks
                } else {
                    values.iter().any(|v| v.eq_ignore_ascii_case(t))
                }
            }
            Some(_) => shown.contains(&t.to_lowercase()),
            None => true,
        };
        d.items = nums
            .into_iter()
            .map(|(_, t)| t)
            .chain(texts)
            .chain(blanks.then(String::new))
            .map(|t| FilterItem {
                checked: checked(&t),
                text: t,
            })
            .collect();
        d
    }

    pub fn header_row(&self) -> u32 {
        self.range.start.row
    }

    fn column(&self) -> String {
        col_to_letters(self.col)
    }

    fn visible(&self) -> Vec<usize> {
        let q = self.search.trim().to_lowercase();
        self.items
            .iter()
            .enumerate()
            .filter(|(_, it)| q.is_empty() || it.text.to_lowercase().contains(&q))
            .map(|(i, _)| i)
            .collect()
    }

    /// `data.filter` params for the checklist.
    pub fn values_params(&self) -> serde_json::Value {
        let vis = self.visible();
        let searching = !self.search.trim().is_empty();
        let keep: Vec<&FilterItem> = self
            .items
            .iter()
            .enumerate()
            .filter(|(i, it)| it.checked && (!searching || vis.contains(i)))
            .map(|(_, it)| it)
            .collect();
        if !searching && keep.len() == self.items.len() {
            return json!({ "column": self.column(), "clear": true });
        }
        let values: Vec<&str> = keep.iter().map(|it| it.text.as_str()).collect();
        json!({ "column": self.column(), "values": values })
    }

    /// Typed value → filter operand (dates become serials so they compare as numbers).
    fn operand(&self, v: &str) -> String {
        if self.kind == "date"
            && let Some((n, _)) = sp_core::parse::parse_numeric_input(v.trim(), 2000)
        {
            return sp_core::value::format_general(n);
        }
        v.trim().to_string()
    }

    /// `data.filter` params for the custom condition form.
    pub fn custom_params(&self, c: &CustomCond) -> Result<serde_json::Value, String> {
        if let Some(n) = c.top {
            return Ok(json!({ "column": self.column(), "top": n.clamp(1, 500) }));
        }
        let mut conds = Vec::new();
        for (op, val) in [(&c.op1, &c.val1), (&c.op2, &c.val2)] {
            if op.is_empty() || val.trim().is_empty() {
                continue;
            }
            // Negated text helpers the engine spells as wildcards.
            let (op, val) = match op.as_str() {
                "not_begins_with" => ("<>".to_string(), format!("{}*", val.trim())),
                "not_ends_with" => ("<>".to_string(), format!("*{}", val.trim())),
                o => (o.to_string(), self.operand(val)),
            };
            conds.push(json!({ "op": op, "val": val }));
        }
        if conds.is_empty() {
            return Err("Enter a value to compare with.".into());
        }
        Ok(json!({ "column": self.column(), "custom": conds, "and": c.and }))
    }

    fn sort(&self, app: &mut App, asc: bool) -> Result<(), String> {
        let order = if asc { "asc" } else { "desc" };
        app.try_exec(
            "data.sort",
            json!({
                "range": self.range.to_a1(),
                "keys": [{ "column": self.column(), "order": order }],
                "header": true
            }),
        )
        .map(|_| ())
    }

    pub fn ui(&mut self, ctx: &Context, app: &mut App) -> Flow {
        let mut flow = Flow::Keep;
        let title = format!("filter-{}", self.col);
        let (_, dismissed) = popup(ctx, &title, self.pos, 236.0, |ui| {
            if let Some(mut c) = self.custom.take() {
                flow = self.custom_form(ui, app, &mut c);
                if flow == Flow::Keep {
                    self.custom = Some(c);
                }
                return;
            }
            let (az, za) = if self.kind == "text" {
                ("Sort A to Z", "Sort Z to A")
            } else if self.kind == "date" {
                ("Sort Oldest to Newest", "Sort Newest to Oldest")
            } else {
                ("Sort Smallest to Largest", "Sort Largest to Smallest")
            };
            for (text, asc) in [(az, true), (za, false)] {
                if menu_row(ui, text, true) {
                    match self.sort(app, asc) {
                        Ok(()) => flow = Flow::Close,
                        Err(e) => self.error = Some(e),
                    }
                }
            }
            ui.separator();
            let table = self.table.is_some();
            let clear = format!("Clear Filter From \"{}\"", self.header_text(app));
            if menu_row(ui, &clear, self.active && !table) {
                match app.try_exec(
                    "data.filter",
                    json!({ "column": self.column(), "clear": true }),
                ) {
                    Ok(_) => flow = Flow::Close,
                    Err(e) => self.error = Some(e),
                }
            }
            let sub = match self.kind.as_str() {
                "number" => "Number Filters",
                "date" => "Date Filters",
                _ => "Text Filters",
            };
            ui.add_enabled_ui(!table, |ui| {
                ui.menu_button(RichText::new(format!("{sub}  ›")).size(12.5), |ui| {
                    for (text, op) in condition_ops(&self.kind) {
                        if ui.button(*text).clicked() {
                            self.custom = Some(start_cond(op));
                            ui.close();
                        }
                    }
                    if ui.button("Custom Filter…").clicked() {
                        self.custom = Some(CustomCond {
                            op1: "=".into(),
                            and: true,
                            ..Default::default()
                        });
                        ui.close();
                    }
                });
            });
            ui.add_space(2.0);
            ui.add_enabled(
                !table,
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text("Search")
                    .desired_width(f32::INFINITY),
            );
            let vis = self.visible();
            let all = vis
                .iter()
                .all(|i| self.items.get(*i).is_some_and(|it| it.checked));
            egui::Frame::NONE
                .fill(T.list_bg)
                .stroke(Stroke::new(1.0, T.list_border))
                .inner_margin(egui::Margin::same(3))
                .show(ui, |ui| {
                    ui.add_enabled_ui(!table, |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("filter-items")
                            .max_height(170.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                let mut all_now = all;
                                let label = if self.search.trim().is_empty() {
                                    "(Select All)"
                                } else {
                                    "(Select All Search Results)"
                                };
                                if ui.checkbox(&mut all_now, label).changed() {
                                    for i in &vis {
                                        if let Some(it) = self.items.get_mut(*i) {
                                            it.checked = all_now;
                                        }
                                    }
                                }
                                for i in &vis {
                                    if let Some(it) = self.items.get_mut(*i) {
                                        let text = if it.text.is_empty() {
                                            "(Blanks)".to_string()
                                        } else {
                                            it.text.clone()
                                        };
                                        ui.checkbox(&mut it.checked, text);
                                    }
                                }
                            });
                    });
                });
            if table {
                ui.label(
                    RichText::new("Filtering inside tables is not available yet; sorting is.")
                        .size(11.0)
                        .color(T.group_label),
                );
            }
            super::error_line(ui, &self.error);
            ui.add_space(4.0);
            let any = self.items.iter().any(|it| it.checked);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(egui::Button::new("Cancel").min_size(egui::vec2(64.0, 22.0)))
                    .clicked()
                {
                    flow = Flow::Close;
                }
                let ok = egui::Button::new(RichText::new("OK").color(T.accent_text))
                    .fill(T.accent)
                    .min_size(egui::vec2(64.0, 22.0));
                if ui.add_enabled(any && !table, ok).clicked() {
                    match app.try_exec("data.filter", self.values_params()) {
                        Ok(_) => flow = Flow::Close,
                        Err(e) => self.error = Some(e),
                    }
                }
            });
        });
        if dismissed { Flow::Close } else { flow }
    }

    fn header_text(&self, app: &App) -> String {
        let s = &app.session;
        s.wb.sheet(s.sheet_index())
            .map(|sh| cell_text(&s.wb, sh, CellAddr::new(self.header_row(), self.col)))
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| format!("Column {}", self.column()))
    }

    /// The Custom AutoFilter form shown inside the popup.
    fn custom_form(&mut self, ui: &mut egui::Ui, app: &mut App, c: &mut CustomCond) -> Flow {
        let mut flow = Flow::Keep;
        ui.label(
            RichText::new("Custom AutoFilter")
                .strong()
                .color(T.dialog_title),
        );
        label(ui, &format!("Show rows where {}:", self.header_text(app)));
        if let Some(n) = c.top.as_mut() {
            ui.horizontal(|ui| {
                label(ui, "Top");
                ui.add(egui::DragValue::new(n).range(1..=500));
                label(ui, "items");
            });
        } else {
            let ops: Vec<(&str, &str)> = ALL_OPS
                .iter()
                .copied()
                .filter(|(_, o)| {
                    self.kind == "text" || !o.contains('_') // numbers/dates: comparisons only
                })
                .collect();
            let row = |ui: &mut egui::Ui, id: &str, op: &mut String, val: &mut String| {
                ui.horizontal(|ui| {
                    egui::ComboBox::new(id, "")
                        .selected_text(op_label(op))
                        .width(118.0)
                        .show_ui(ui, |ui| {
                            if ui.selectable_label(op.is_empty(), " ").clicked() {
                                op.clear();
                            }
                            for (l, o) in &ops {
                                if ui.selectable_label(op == o, *l).clicked() {
                                    *op = (*o).to_string();
                                }
                            }
                        });
                    ui.add(egui::TextEdit::singleline(val).desired_width(90.0));
                });
            };
            row(ui, "cf-op1", &mut c.op1, &mut c.val1);
            ui.horizontal(|ui| {
                ui.radio_value(&mut c.and, true, "And");
                ui.radio_value(&mut c.and, false, "Or");
            });
            row(ui, "cf-op2", &mut c.op2, &mut c.val2);
            if self.kind == "date" {
                ui.label(
                    RichText::new("Dates as typed in a cell, e.g. 3/14/2026")
                        .size(10.5)
                        .color(T.group_label),
                );
            }
        }
        super::error_line(ui, &self.error);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add(egui::Button::new("Cancel").min_size(egui::vec2(64.0, 22.0)))
                .clicked()
            {
                flow = Flow::Close;
            }
            let ok = egui::Button::new(RichText::new("OK").color(T.accent_text))
                .fill(T.accent)
                .min_size(egui::vec2(64.0, 22.0));
            if ui.add(ok).clicked() {
                match self
                    .custom_params(c)
                    .and_then(|p| app.try_exec("data.filter", p))
                {
                    Ok(_) => flow = Flow::Close,
                    Err(e) => self.error = Some(e),
                }
            }
        });
        flow
    }
}

fn start_cond(op: &str) -> CustomCond {
    match op {
        "between" => CustomCond {
            op1: ">=".into(),
            op2: "<=".into(),
            and: true,
            ..Default::default()
        },
        "top10" => CustomCond {
            top: Some(10),
            ..Default::default()
        },
        o => CustomCond {
            op1: o.into(),
            and: true,
            ..Default::default()
        },
    }
}

// ── Data validation list ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ValidationList {
    pub cell: CellAddr,
    pub items: Vec<String>,
    pub pos: Option<[f32; 2]>,
    pub error: Option<String>,
}

impl ValidationList {
    pub fn new(s: &Session, cell: CellAddr) -> Self {
        let si = s.sheet_index();
        let items =
            s.wb.sheet(si)
                .and_then(|sh| sh.validation_at(cell))
                .map(|rule| sp_engine::commands::validation_cmds::list_items(&s.wb, si, rule, cell))
                .unwrap_or_default();
        ValidationList {
            cell,
            items: items.into_iter().take(MAX_ITEMS).collect(),
            pos: None,
            error: None,
        }
    }

    pub fn ui(&mut self, ctx: &Context, app: &mut App) -> Flow {
        let mut flow = Flow::Keep;
        let (_, dismissed) = popup(ctx, "dv-list", self.pos, 150.0, |ui| {
            egui::ScrollArea::vertical()
                .max_height(180.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for it in &self.items {
                        if menu_row(ui, it, true) {
                            let r = app.try_exec(
                                "cell.set",
                                json!({ "cell": self.cell.to_a1(), "input": it }),
                            );
                            match r {
                                Ok(_) => flow = Flow::Close,
                                Err(e) => self.error = Some(e),
                            }
                        }
                    }
                    if self.items.is_empty() {
                        label(ui, "(empty list)");
                    }
                });
            super::error_line(ui, &self.error);
        });
        if dismissed { Flow::Close } else { flow }
    }
}

// ── Note / comment card ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CommentCard {
    pub cell: CellAddr,
    pub pos: Option<[f32; 2]>,
    /// Note text being edited, or the reply being typed.
    pub draft: String,
    pub error: Option<String>,
}

impl CommentCard {
    pub fn new(s: &Session, cell: CellAddr) -> Self {
        let draft =
            s.wb.sheet(s.sheet_index())
                .and_then(|sh| sh.comment_at(cell))
                .filter(|c| c.kind == CommentKind::Note)
                .map(|c| c.text.clone())
                .unwrap_or_default();
        CommentCard {
            cell,
            pos: None,
            draft,
            error: None,
        }
    }

    pub fn ui(&mut self, ctx: &Context, app: &mut App) -> Flow {
        let s = &app.session;
        let Some(c) =
            s.wb.sheet(s.sheet_index())
                .and_then(|sh| sh.comment_at(self.cell))
                .cloned()
        else {
            return Flow::Close;
        };
        let mut flow = Flow::Keep;
        let cell = self.cell.to_a1();
        let (_, dismissed) = popup(ctx, "comment-card", self.pos, 250.0, |ui| {
            let head = |ui: &mut egui::Ui, who: &str, text: &str| {
                ui.label(
                    RichText::new(who)
                        .strong()
                        .size(12.0)
                        .color(T.comment_author),
                );
                ui.label(RichText::new(text).size(12.0).color(T.note_text));
            };
            match c.kind {
                CommentKind::Note => {
                    ui.label(
                        RichText::new(format!("Note · {}", c.author))
                            .strong()
                            .color(T.comment_author),
                    );
                    ui.add(
                        egui::TextEdit::multiline(&mut self.draft)
                            .desired_rows(4)
                            .desired_width(f32::INFINITY),
                    );
                    ui.horizontal(|ui| {
                        if ui.button("Save").clicked() {
                            match app.try_exec(
                                "insert.note",
                                json!({ "cell": cell, "text": self.draft }),
                            ) {
                                Ok(_) => flow = Flow::Close,
                                Err(e) => self.error = Some(e),
                            }
                        }
                        if ui.button("Delete Note").clicked() {
                            match app.try_exec("review.comment.delete", json!({ "range": cell })) {
                                Ok(_) => flow = Flow::Close,
                                Err(e) => self.error = Some(e),
                            }
                        }
                    });
                }
                CommentKind::Thread => {
                    egui::ScrollArea::vertical()
                        .max_height(220.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            head(ui, &c.author, &c.text);
                            for r in &c.replies {
                                ui.separator();
                                head(ui, &r.author, &r.text);
                            }
                        });
                    ui.separator();
                    ui.add(
                        egui::TextEdit::multiline(&mut self.draft)
                            .hint_text("Reply…")
                            .desired_rows(2)
                            .desired_width(f32::INFINITY),
                    );
                    ui.horizontal(|ui| {
                        let can = !self.draft.trim().is_empty();
                        if ui.add_enabled(can, egui::Button::new("Reply")).clicked() {
                            match app.try_exec(
                                "insert.comment",
                                json!({ "cell": cell, "text": self.draft }),
                            ) {
                                Ok(_) => self.draft.clear(),
                                Err(e) => self.error = Some(e),
                            }
                        }
                        if ui.button("Delete Thread").clicked() {
                            match app.try_exec("review.comment.delete", json!({ "range": cell })) {
                                Ok(_) => flow = Flow::Close,
                                Err(e) => self.error = Some(e),
                            }
                        }
                    });
                }
            }
            super::error_line(ui, &self.error);
        });
        if dismissed { Flow::Close } else { flow }
    }
}

// ── Validation alert ─────────────────────────────────────────────────────────────────────

/// Shown when typed input breaks a validation rule: Stop refused it (Retry / Cancel);
/// Warning and Information accepted it and ask whether to keep it.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ValidationAlert {
    /// "stop" | "warning" | "information"
    pub style: String,
    pub title: String,
    pub message: String,
    pub sheet: usize,
    pub cell: CellAddr,
    pub input: String,
}

impl ValidationAlert {
    pub fn new(s: &Session, sheet: usize, cell: CellAddr, input: &str, msg: &str) -> Self {
        let rule = s.wb.sheet(sheet).and_then(|sh| sh.validation_at(cell));
        let style = match rule.map(|r| r.error_style) {
            Some(ErrorStyle::Warning) => "warning",
            Some(ErrorStyle::Information) => "information",
            _ => "stop",
        };
        let title = rule
            .map(|r| r.error_title.clone())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "SheetPro".into());
        ValidationAlert {
            style: style.into(),
            title,
            message: msg.to_string(),
            sheet,
            cell,
            input: input.to_string(),
        }
    }

    fn retry(&self, app: &mut App) {
        app.exec("sheet.activate", json!({ "sheet": self.sheet }));
        app.session.select(self.cell);
        app.begin_edit(Some(self.input.clone()));
    }

    pub fn ui(&mut self, ctx: &Context, app: &mut App) -> Flow {
        let title = self.title.clone();
        let mut choice: Option<&str> = None;
        let (_, esc) = window(ctx, &title, 360.0, |ui| {
            ui.horizontal(|ui| {
                let (glyph, color) = match self.style.as_str() {
                    "warning" => ("!", T.alert_warning),
                    "information" => ("i", T.alert_info),
                    _ => ("×", T.alert_stop),
                };
                let (r, _) = ui.allocate_exact_size(egui::vec2(30.0, 30.0), egui::Sense::hover());
                ui.painter().circle_filled(r.center(), 14.0, color);
                ui.painter().text(
                    r.center(),
                    egui::Align2::CENTER_CENTER,
                    glyph,
                    egui::FontId::proportional(18.0),
                    T.accent_text,
                );
                ui.vertical(|ui| {
                    ui.label(RichText::new(&self.message).color(T.dialog_title));
                    if self.style == "warning" {
                        ui.label(RichText::new("Continue?").color(T.dialog_label));
                    }
                });
            });
            ui.add_space(8.0);
            let buttons: &[&str] = match self.style.as_str() {
                "warning" => &["Cancel", "No", "Yes"],
                "information" => &["Cancel", "OK"],
                _ => &["Cancel", "Retry"],
            };
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                for b in buttons {
                    let primary = matches!(*b, "Yes" | "OK" | "Retry");
                    let btn = if primary {
                        egui::Button::new(RichText::new(*b).color(T.accent_text)).fill(T.accent)
                    } else {
                        egui::Button::new(*b)
                    };
                    if ui.add(btn.min_size(egui::vec2(68.0, 24.0))).clicked() {
                        choice = Some(b);
                    }
                }
            });
        });
        let choice = match choice {
            Some(c) => c,
            None if esc => "Cancel",
            None => return Flow::Keep,
        };
        match (self.style.as_str(), choice) {
            ("stop", "Retry") => self.retry(app),
            ("stop", _) => app.sync_buffer(),
            (_, "Yes" | "OK") => {}
            ("warning", "No") => {
                app.exec("edit.undo", json!({}));
                self.retry(app);
            }
            _ => app.exec("edit.undo", json!({})),
        }
        Flow::Close
    }
}
