//! More purpose-built dialogs: Move or Copy Sheet, Insert Slicer, Link, Symbol, Scale to
//! Fit, Custom Margins, Evaluate Formula, Advanced Filter, Consolidate, Scenario Manager,
//! Data Table and Forecast Sheet. Each is seeded from the workbook and runs one command.

use super::{Flow, error_line, label, ok_cancel, window};
use crate::App;
use crate::theme::LIGHT as T;
use egui::{Context, RichText};
use serde::{Deserialize, Serialize};
use serde_json::{Map, json};
use sp_engine::{Json, Session};

fn text_box(ui: &mut egui::Ui, value: &mut String, width: f32, hint: &str) {
    ui.add(
        egui::TextEdit::singleline(value)
            .hint_text(hint)
            .desired_width(width),
    );
}

/// A single right-aligned Close for dialogs whose actions apply as you go.
fn close_button(ui: &mut egui::Ui) -> bool {
    ui.add_space(4.0);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.button("Close").clicked()
    })
    .inner
}

fn sheet_names(s: &Session) -> Vec<String> {
    s.wb.sheets.iter().map(|sh| sh.name.clone()).collect()
}

/// Run `id` and close on success; otherwise keep the dialog open with the error.
fn run(app: &mut App, id: &str, p: Json, error: &mut Option<String>) -> Flow {
    match app.try_exec(id, p) {
        Ok(_) => Flow::Close,
        Err(e) => {
            *error = Some(e);
            Flow::Keep
        }
    }
}

fn num(s: &str) -> Result<f64, String> {
    s.trim()
        .parse::<f64>()
        .map_err(|_| format!("'{}' is not a number.", s.trim()))
}

// ---------------------------------------------------------------------------------------
// Move or Copy Sheet

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct MoveCopy {
    pub sheet: String,
    pub sheets: Vec<String>,
    /// Index in `sheets` to put it before; `sheets.len()` = move to end.
    pub before: usize,
    pub copy: bool,
    pub error: Option<String>,
}

impl MoveCopy {
    pub fn new(s: &Session, sheet: Option<usize>) -> Self {
        let i = sheet.unwrap_or_else(|| s.sheet_index());
        let sheets = sheet_names(s);
        MoveCopy {
            sheet: sheets.get(i).cloned().unwrap_or_default(),
            before: (i + 1).min(sheets.len()),
            sheets,
            ..Default::default()
        }
    }

    /// The target position for `home.cells.move_copy_sheet` (`to` = final index).
    pub fn params(&self) -> Json {
        let cur = self
            .sheets
            .iter()
            .position(|n| *n == self.sheet)
            .unwrap_or(0);
        let to = if self.copy || self.before <= cur {
            self.before
        } else {
            self.before - 1
        };
        json!({ "sheet": self.sheet, "to": to, "copy": self.copy })
    }

    pub fn ui(&mut self, ctx: &Context, app: &mut App) -> Flow {
        let ((ok, cancel), esc) = window(ctx, "Move or Copy", 300.0, |ui| {
            label(ui, &format!("Move selected sheet '{}'", self.sheet));
            label(ui, "Before sheet:");
            egui::Frame::group(ui.style()).show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(160.0)
                    .show(ui, |ui| {
                        for (i, n) in self.sheets.iter().enumerate() {
                            ui.selectable_value(&mut self.before, i, n);
                        }
                        ui.selectable_value(&mut self.before, self.sheets.len(), "(move to end)");
                    });
            });
            ui.checkbox(&mut self.copy, "Create a copy");
            error_line(ui, &self.error);
            ok_cancel(ui, "OK")
        });
        if cancel || esc {
            return Flow::Close;
        }
        if ok {
            return run(
                app,
                "home.cells.move_copy_sheet",
                self.params(),
                &mut self.error,
            );
        }
        Flow::Keep
    }
}

// ---------------------------------------------------------------------------------------
// Insert Slicer

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct InsertSlicer {
    pub pivot: String,
    pub fields: Vec<(String, bool)>,
    pub error: Option<String>,
}

impl InsertSlicer {
    pub fn new(s: &Session) -> Self {
        let sheet = s.sheet_index();
        let pv = sp_engine::commands::pivot_cmds::pivot_at(&s.wb, sheet, s.sel.active)
            .and_then(|i| s.wb.sheet(sheet)?.analysis.pivots.get(i));
        match pv {
            Some(p) => InsertSlicer {
                pivot: p.name.clone(),
                fields: p.cache.fields.iter().map(|f| (f.clone(), false)).collect(),
                error: None,
            },
            None => InsertSlicer {
                error: Some("Select a cell in a PivotTable first.".into()),
                ..Default::default()
            },
        }
    }

    pub fn ui(&mut self, ctx: &Context, app: &mut App) -> Flow {
        let ((ok, cancel), esc) = window(ctx, "Insert Slicers", 280.0, |ui| {
            if !self.pivot.is_empty() {
                label(ui, &format!("Fields of {}:", self.pivot));
                egui::ScrollArea::vertical()
                    .max_height(220.0)
                    .show(ui, |ui| {
                        for (f, on) in &mut self.fields {
                            ui.checkbox(on, f.as_str());
                        }
                    });
            }
            error_line(ui, &self.error);
            ok_cancel(ui, "OK")
        });
        if cancel || esc {
            return Flow::Close;
        }
        if ok && !self.pivot.is_empty() {
            let picked: Vec<String> = self
                .fields
                .iter()
                .filter(|(_, on)| *on)
                .map(|(f, _)| f.clone())
                .collect();
            if picked.is_empty() {
                self.error = Some("Tick at least one field.".into());
                return Flow::Keep;
            }
            for f in picked {
                if let Err(e) =
                    app.try_exec("insert.slicer", json!({ "pivot": self.pivot, "field": f }))
                {
                    self.error = Some(e);
                    return Flow::Keep;
                }
            }
            return Flow::Close;
        }
        Flow::Keep
    }
}

// ---------------------------------------------------------------------------------------
// Insert Link

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LinkTo {
    #[default]
    Web,
    Place,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Hyperlink {
    pub cell: String,
    pub to: LinkTo,
    pub text: String,
    pub address: String,
    pub sheet: String,
    pub reference: String,
    pub tooltip: String,
    pub sheets: Vec<String>,
    pub error: Option<String>,
}

impl Hyperlink {
    pub fn new(s: &Session) -> Self {
        let a = s.sel.active;
        let sh = s.wb.sheet(s.sheet_index());
        let shown = sh
            .and_then(|sh| sh.get(a))
            .map(|c| c.value.to_display())
            .unwrap_or_default();
        let mut d = Hyperlink {
            cell: a.to_a1(),
            text: shown,
            sheets: sheet_names(s),
            sheet: sh.map(|sh| sh.name.clone()).unwrap_or_default(),
            reference: "A1".into(),
            ..Default::default()
        };
        if let Some(h) = sh.and_then(|sh| sh.hyperlinks.iter().find(|h| h.range.contains(a))) {
            if h.target.is_empty() {
                d.to = LinkTo::Place;
                let (sheet, r) = h.location.rsplit_once('!').unwrap_or(("", &h.location));
                if !sheet.is_empty() {
                    d.sheet = sheet.trim_matches('\'').to_string();
                }
                d.reference = r.to_string();
            } else {
                d.address = h.target.clone();
            }
            d.tooltip = h.tooltip.clone();
        }
        d
    }

    pub fn params(&self) -> Result<Json, String> {
        let mut m = Map::new();
        m.insert("cell".into(), json!(self.cell));
        match self.to {
            LinkTo::Web => {
                if self.address.trim().is_empty() {
                    return Err("Type the address to link to.".into());
                }
                m.insert("address".into(), json!(self.address.trim()));
            }
            LinkTo::Place => {
                let sheet = sp_core::addr::quote_sheet_name(&self.sheet);
                m.insert(
                    "location".into(),
                    json!(format!("{sheet}!{}", self.reference.trim())),
                );
            }
        }
        if !self.text.trim().is_empty() {
            m.insert("text".into(), json!(self.text));
        }
        if !self.tooltip.trim().is_empty() {
            m.insert("tooltip".into(), json!(self.tooltip));
        }
        Ok(Json::Object(m))
    }

    pub fn ui(&mut self, ctx: &Context, app: &mut App) -> Flow {
        let ((ok, cancel), esc) = window(ctx, "Insert Hyperlink", 400.0, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.to, LinkTo::Web, "Web page or file");
                ui.selectable_value(&mut self.to, LinkTo::Place, "Place in this document");
            });
            ui.separator();
            egui::Grid::new("link")
                .num_columns(2)
                .spacing([10.0, 6.0])
                .show(ui, |ui| {
                    label(ui, "Text to display:");
                    text_box(ui, &mut self.text, 260.0, "");
                    ui.end_row();
                    match self.to {
                        LinkTo::Web => {
                            label(ui, "Address:");
                            text_box(ui, &mut self.address, 260.0, "https://example.com");
                            ui.end_row();
                        }
                        LinkTo::Place => {
                            label(ui, "Sheet:");
                            egui::ComboBox::from_id_salt("link-sheet")
                                .selected_text(&self.sheet)
                                .width(260.0)
                                .show_ui(ui, |ui| {
                                    for n in self.sheets.clone() {
                                        ui.selectable_value(&mut self.sheet, n.clone(), n);
                                    }
                                });
                            ui.end_row();
                            label(ui, "Cell reference:");
                            text_box(ui, &mut self.reference, 260.0, "A1");
                            ui.end_row();
                        }
                    }
                    label(ui, "ScreenTip:");
                    text_box(ui, &mut self.tooltip, 260.0, "");
                    ui.end_row();
                });
            error_line(ui, &self.error);
            ok_cancel(ui, "OK")
        });
        if cancel || esc {
            return Flow::Close;
        }
        if ok {
            match self.params() {
                Ok(p) => return run(app, "insert.hyperlink", p, &mut self.error),
                Err(e) => self.error = Some(e),
            }
        }
        Flow::Keep
    }
}

// ---------------------------------------------------------------------------------------
// Symbol

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Symbol {
    pub sets: Vec<(String, Vec<String>)>,
    pub set: usize,
    pub append: bool,
    pub error: Option<String>,
}

impl Symbol {
    pub fn new(app: &mut App) -> Self {
        let sets = app
            .query("insert.symbol", json!({}))
            .ok()
            .and_then(|r| r.get("sets").cloned())
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
            .iter()
            .map(|g| {
                let name = g
                    .get("set")
                    .and_then(Json::as_str)
                    .unwrap_or("")
                    .to_string();
                let syms = g
                    .get("symbols")
                    .and_then(Json::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                (name, syms)
            })
            .collect();
        Symbol {
            sets,
            ..Default::default()
        }
    }

    pub fn ui(&mut self, ctx: &Context, app: &mut App) -> Flow {
        let mut picked: Option<String> = None;
        let (cancel, esc) = window(ctx, "Symbol", 380.0, |ui| {
            ui.horizontal_wrapped(|ui| {
                for (i, (name, _)) in self.sets.iter().enumerate() {
                    ui.selectable_value(&mut self.set, i, name);
                }
            });
            ui.separator();
            if let Some((_, syms)) = self.sets.get(self.set) {
                ui.horizontal_wrapped(|ui| {
                    for sym in syms {
                        if ui
                            .add(
                                egui::Button::new(RichText::new(sym).size(18.0))
                                    .min_size(egui::vec2(32.0, 32.0)),
                            )
                            .clicked()
                        {
                            picked = Some(sym.clone());
                        }
                    }
                });
            }
            ui.checkbox(&mut self.append, "Add to the end of the cell's text");
            error_line(ui, &self.error);
            close_button(ui)
        });
        if cancel || esc {
            return Flow::Close;
        }
        if let Some(sym) = picked {
            let cell = app.session.sel.active.to_a1();
            return run(
                app,
                "insert.symbol",
                json!({ "symbol": sym, "cell": cell, "append": self.append }),
                &mut self.error,
            );
        }
        Flow::Keep
    }
}

// ---------------------------------------------------------------------------------------
// Scale to Fit and Custom Margins (Page Setup)

const FIT: [&str; 11] = [
    "Automatic",
    "1",
    "2",
    "3",
    "4",
    "5",
    "6",
    "7",
    "8",
    "9",
    "10",
];

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ScaleToFit {
    pub width: String,
    pub height: String,
    pub scale: String,
    pub error: Option<String>,
}

impl ScaleToFit {
    pub fn new(s: &Session) -> Self {
        let ps = s.wb.sheet(s.sheet_index()).map(|sh| sh.page_setup.clone());
        let fit = |v: Option<u32>| {
            v.filter(|n| *n > 0)
                .map_or("Automatic".into(), |n| n.to_string())
        };
        ScaleToFit {
            width: fit(ps.as_ref().and_then(|p| p.fit_width)),
            height: fit(ps.as_ref().and_then(|p| p.fit_height)),
            scale: ps.map_or(100, |p| p.scale).to_string(),
            error: None,
        }
    }

    pub fn params(&self) -> Result<Json, String> {
        let pages = |v: &str| -> Json {
            match v.parse::<u32>() {
                Ok(n) => json!(n),
                Err(_) => json!("automatic"),
            }
        };
        let mut m = Map::new();
        m.insert("width".into(), pages(&self.width));
        m.insert("height".into(), pages(&self.height));
        if self.width == "Automatic" && self.height == "Automatic" {
            let s = num(&self.scale)?;
            if !(10.0..=400.0).contains(&s) {
                return Err("Scale must be between 10% and 400%.".into());
            }
            m.insert("scale".into(), json!(s));
        }
        Ok(Json::Object(m))
    }

    pub fn ui(&mut self, ctx: &Context, app: &mut App) -> Flow {
        let ((ok, cancel), esc) = window(ctx, "Scale to Fit", 280.0, |ui| {
            egui::Grid::new("fit")
                .num_columns(2)
                .spacing([10.0, 6.0])
                .show(ui, |ui| {
                    for (name, v) in [("Width:", &mut self.width), ("Height:", &mut self.height)] {
                        label(ui, name);
                        egui::ComboBox::from_id_salt(name)
                            .selected_text(if v == "Automatic" {
                                "Automatic".to_string()
                            } else {
                                format!("{v} page(s)")
                            })
                            .width(140.0)
                            .show_ui(ui, |ui| {
                                for f in FIT {
                                    ui.selectable_value(v, f.to_string(), f);
                                }
                            });
                        ui.end_row();
                    }
                    let both_auto = self.width == "Automatic" && self.height == "Automatic";
                    label(ui, "Scale (%):");
                    ui.add_enabled(
                        both_auto,
                        egui::TextEdit::singleline(&mut self.scale).desired_width(140.0),
                    );
                    ui.end_row();
                });
            error_line(ui, &self.error);
            ok_cancel(ui, "OK")
        });
        if cancel || esc {
            return Flow::Close;
        }
        if ok {
            match self.params() {
                Ok(p) => return run(app, "layout.scale_to_fit", p, &mut self.error),
                Err(e) => self.error = Some(e),
            }
        }
        Flow::Keep
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CustomMargins {
    /// Top, bottom, left, right, header, footer (inches).
    pub values: [String; 6],
    pub error: Option<String>,
}

const MARGIN_KEYS: [(&str, &str); 6] = [
    ("top", "Top"),
    ("bottom", "Bottom"),
    ("left", "Left"),
    ("right", "Right"),
    ("header", "Header"),
    ("footer", "Footer"),
];

impl CustomMargins {
    pub fn new(s: &Session) -> Self {
        let m =
            s.wb.sheet(s.sheet_index())
                .map(|sh| sh.page_setup.margins)
                .unwrap_or_default();
        let f = |v: f64| format!("{}", (v * 100.0).round() / 100.0);
        CustomMargins {
            values: [
                f(m.top),
                f(m.bottom),
                f(m.left),
                f(m.right),
                f(m.header),
                f(m.footer),
            ],
            error: None,
        }
    }

    pub fn params(&self) -> Result<Json, String> {
        let mut m = Map::new();
        for ((k, _), v) in MARGIN_KEYS.iter().zip(&self.values) {
            let n = num(v)?;
            if !(0.0..=20.0).contains(&n) {
                return Err("Margins are 0 to 20 inches.".into());
            }
            m.insert((*k).to_string(), json!(n));
        }
        Ok(Json::Object(m))
    }

    pub fn ui(&mut self, ctx: &Context, app: &mut App) -> Flow {
        let ((ok, cancel), esc) = window(ctx, "Custom Margins", 280.0, |ui| {
            label(ui, "Margins in inches:");
            egui::Grid::new("margins")
                .num_columns(2)
                .spacing([10.0, 6.0])
                .show(ui, |ui| {
                    for ((_, name), v) in MARGIN_KEYS.iter().zip(self.values.iter_mut()) {
                        label(ui, &format!("{name}:"));
                        ui.add(egui::TextEdit::singleline(v).desired_width(80.0));
                        ui.end_row();
                    }
                });
            error_line(ui, &self.error);
            ok_cancel(ui, "OK")
        });
        if cancel || esc {
            return Flow::Close;
        }
        if ok {
            match self.params() {
                Ok(p) => return run(app, "layout.margins", p, &mut self.error),
                Err(e) => self.error = Some(e),
            }
        }
        Flow::Keep
    }
}

// ---------------------------------------------------------------------------------------
// Evaluate Formula

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct EvaluateFormula {
    pub cell: String,
    pub formula: String,
    /// (expression, evaluates to) per step, innermost first.
    pub steps: Vec<(String, String)>,
    pub result: String,
    pub error: Option<String>,
}

impl EvaluateFormula {
    pub fn new(app: &mut App) -> Self {
        let s = &app.session;
        let a = s.sel.active;
        let formula =
            s.wb.sheet(s.sheet_index())
                .and_then(|sh| sh.get(a))
                .and_then(|c| c.formula_src().map(|f| format!("={f}")))
                .unwrap_or_default();
        let mut d = EvaluateFormula {
            cell: a.to_a1(),
            formula,
            ..Default::default()
        };
        if d.formula.is_empty() {
            d.error = Some("The active cell doesn't contain a formula.".into());
            return d;
        }
        match app.query("formulas.evaluate_formula", json!({ "cell": d.cell })) {
            Ok(r) => {
                let text = |v: Option<&Json>| {
                    v.map(|x| x.as_str().map_or_else(|| x.to_string(), str::to_string))
                        .unwrap_or_default()
                };
                d.steps = r
                    .get("steps")
                    .and_then(Json::as_array)
                    .map(|a| {
                        a.iter()
                            .map(|st| (text(st.get("evaluate")), text(st.get("result"))))
                            .collect()
                    })
                    .unwrap_or_default();
                d.result = text(r.get("result"));
            }
            Err(e) => d.error = Some(e),
        }
        d
    }

    pub fn ui(&mut self, ctx: &Context, _app: &mut App) -> Flow {
        let (cancel, esc) = window(ctx, "Evaluate Formula", 460.0, |ui| {
            label(ui, &format!("Reference: {}", self.cell));
            ui.label(RichText::new(&self.formula).monospace());
            if !self.steps.is_empty() {
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height(240.0)
                    .show(ui, |ui| {
                        egui::Grid::new("eval-steps")
                            .num_columns(2)
                            .striped(true)
                            .spacing([14.0, 4.0])
                            .show(ui, |ui| {
                                ui.label(RichText::new("Evaluates").color(T.group_label));
                                ui.label(RichText::new("to").color(T.group_label));
                                ui.end_row();
                                for (e, r) in &self.steps {
                                    ui.label(RichText::new(e).monospace());
                                    ui.label(RichText::new(r).monospace());
                                    ui.end_row();
                                }
                            });
                    });
                ui.separator();
                ui.label(RichText::new(format!("Result: {}", self.result)).strong());
            }
            error_line(ui, &self.error);
            close_button(ui)
        });
        if cancel || esc {
            return Flow::Close;
        }
        Flow::Keep
    }
}

// ---------------------------------------------------------------------------------------
// Advanced Filter

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AdvancedFilter {
    pub copy: bool,
    pub list: String,
    pub criteria: String,
    pub copy_to: String,
    pub unique: bool,
    pub error: Option<String>,
}

impl AdvancedFilter {
    pub fn new(s: &Session) -> Self {
        let sel = s.sel.range();
        let list = if sel.start == sel.end {
            sp_engine::commands::nav_cmds::current_region(s, sel.start).to_a1()
        } else {
            sel.to_a1()
        };
        AdvancedFilter {
            list,
            ..Default::default()
        }
    }

    pub fn params(&self) -> Result<Json, String> {
        if self.criteria.trim().is_empty() {
            return Err("Give the criteria range.".into());
        }
        let mut m = Map::new();
        m.insert("range".into(), json!(self.list.trim()));
        m.insert("criteria".into(), json!(self.criteria.trim()));
        m.insert("unique".into(), json!(self.unique));
        if self.copy {
            if self.copy_to.trim().is_empty() {
                return Err("Give the cell to copy to.".into());
            }
            m.insert("copy_to".into(), json!(self.copy_to.trim()));
        }
        Ok(Json::Object(m))
    }

    pub fn ui(&mut self, ctx: &Context, app: &mut App) -> Flow {
        let ((ok, cancel), esc) = window(ctx, "Advanced Filter", 330.0, |ui| {
            label(ui, "Action");
            ui.radio_value(&mut self.copy, false, "Filter the list, in-place");
            ui.radio_value(&mut self.copy, true, "Copy to another location");
            egui::Grid::new("adv")
                .num_columns(2)
                .spacing([10.0, 6.0])
                .show(ui, |ui| {
                    label(ui, "List range:");
                    text_box(ui, &mut self.list, 170.0, "A1:D50");
                    ui.end_row();
                    label(ui, "Criteria range:");
                    text_box(ui, &mut self.criteria, 170.0, "F1:G3");
                    ui.end_row();
                    if self.copy {
                        label(ui, "Copy to:");
                        text_box(ui, &mut self.copy_to, 170.0, "I1");
                        ui.end_row();
                    }
                });
            ui.checkbox(&mut self.unique, "Unique records only");
            error_line(ui, &self.error);
            ok_cancel(ui, "OK")
        });
        if cancel || esc {
            return Flow::Close;
        }
        if ok {
            match self.params() {
                Ok(p) => return run(app, "data.advanced_filter", p, &mut self.error),
                Err(e) => self.error = Some(e),
            }
        }
        Flow::Keep
    }
}

// ---------------------------------------------------------------------------------------
// Consolidate

const CONSOLIDATE: [(&str, &str); 11] = [
    ("sum", "Sum"),
    ("count", "Count"),
    ("average", "Average"),
    ("max", "Max"),
    ("min", "Min"),
    ("product", "Product"),
    ("countNums", "Count Numbers"),
    ("stdDev", "StdDev"),
    ("stdDevp", "StdDevp"),
    ("var", "Var"),
    ("varp", "Varp"),
];

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Consolidate {
    pub func: String,
    pub reference: String,
    pub sources: Vec<String>,
    pub selected: Option<usize>,
    pub top_row: bool,
    pub left_col: bool,
    pub dest: String,
    pub error: Option<String>,
}

impl Consolidate {
    pub fn new(s: &Session) -> Self {
        Consolidate {
            func: "sum".into(),
            dest: s.sel.active.to_a1(),
            ..Default::default()
        }
    }

    pub fn params(&self) -> Result<Json, String> {
        if self.sources.is_empty() {
            return Err("Add at least one reference.".into());
        }
        Ok(json!({
            "sources": self.sources,
            "dest": self.dest.trim(),
            "func": self.func,
            "top_row": self.top_row,
            "left_col": self.left_col,
        }))
    }

    pub fn ui(&mut self, ctx: &Context, app: &mut App) -> Flow {
        let ((ok, cancel), esc) = window(ctx, "Consolidate", 360.0, |ui| {
            egui::Grid::new("cons")
                .num_columns(2)
                .spacing([10.0, 6.0])
                .show(ui, |ui| {
                    label(ui, "Function:");
                    let shown = CONSOLIDATE
                        .iter()
                        .find(|(k, _)| *k == self.func)
                        .map_or("Sum", |(_, l)| *l);
                    egui::ComboBox::from_id_salt("cons-func")
                        .selected_text(shown)
                        .width(180.0)
                        .show_ui(ui, |ui| {
                            for (k, l) in CONSOLIDATE {
                                ui.selectable_value(&mut self.func, k.to_string(), l);
                            }
                        });
                    ui.end_row();
                    label(ui, "Reference:");
                    ui.horizontal(|ui| {
                        text_box(ui, &mut self.reference, 140.0, "Sheet2!A1:C9");
                        if ui.button("Add").clicked() && !self.reference.trim().is_empty() {
                            self.sources.push(self.reference.trim().to_string());
                            self.reference.clear();
                        }
                    });
                    ui.end_row();
                    label(ui, "All references:");
                    ui.vertical(|ui| {
                        for (i, r) in self.sources.iter().enumerate() {
                            ui.selectable_value(&mut self.selected, Some(i), r);
                        }
                        if ui
                            .add_enabled(self.selected.is_some(), egui::Button::new("Delete"))
                            .clicked()
                            && let Some(i) = self.selected.take()
                            && i < self.sources.len()
                        {
                            self.sources.remove(i);
                        }
                    });
                    ui.end_row();
                    label(ui, "Put result at:");
                    text_box(ui, &mut self.dest, 140.0, "A1");
                    ui.end_row();
                });
            label(ui, "Use labels in");
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.top_row, "Top row");
                ui.checkbox(&mut self.left_col, "Left column");
            });
            error_line(ui, &self.error);
            ok_cancel(ui, "OK")
        });
        if cancel || esc {
            return Flow::Close;
        }
        if ok {
            match self.params() {
                Ok(p) => return run(app, "data.consolidate", p, &mut self.error),
                Err(e) => self.error = Some(e),
            }
        }
        Flow::Keep
    }
}

// ---------------------------------------------------------------------------------------
// Scenario Manager

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ScenarioManager {
    pub selected: Option<String>,
    /// Add form: name, changing cells, comment.
    pub adding: bool,
    pub name: String,
    pub changing: String,
    pub comment: String,
    pub result_cells: String,
    pub status: Option<String>,
    pub error: Option<String>,
}

impl ScenarioManager {
    pub fn new(s: &Session) -> Self {
        ScenarioManager {
            changing: s.sel.range().to_a1(),
            ..Default::default()
        }
    }

    fn names(app: &App) -> Vec<String> {
        let s = &app.session;
        s.wb.sheet(s.sheet_index())
            .map(|sh| {
                sh.analysis
                    .scenarios
                    .iter()
                    .map(|x| x.name.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn ui(&mut self, ctx: &Context, app: &mut App) -> Flow {
        let names = Self::names(app);
        let mut action: Option<(&str, Json)> = None;
        let (cancel, esc) = window(ctx, "Scenario Manager", 380.0, |ui| {
            label(ui, "Scenarios:");
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_min_height(90.0);
                if names.is_empty() {
                    ui.label(
                        RichText::new("No scenarios defined. Choose Add to add one.")
                            .color(T.group_label),
                    );
                }
                for n in &names {
                    ui.selectable_value(&mut self.selected, Some(n.clone()), n);
                }
            });
            ui.horizontal(|ui| {
                if ui.button("Add…").clicked() {
                    self.adding = true;
                }
                let sel = self.selected.clone();
                if ui
                    .add_enabled(sel.is_some(), egui::Button::new("Show"))
                    .clicked()
                    && let Some(n) = &sel
                {
                    action = Some(("show", json!({ "action": "show", "name": n })));
                }
                if ui
                    .add_enabled(sel.is_some(), egui::Button::new("Delete"))
                    .clicked()
                    && let Some(n) = &sel
                {
                    action = Some(("delete", json!({ "action": "delete", "name": n })));
                }
            });
            if self.adding {
                ui.separator();
                egui::Grid::new("scn-add")
                    .num_columns(2)
                    .spacing([10.0, 6.0])
                    .show(ui, |ui| {
                        label(ui, "Scenario name:");
                        text_box(ui, &mut self.name, 200.0, "Best case");
                        ui.end_row();
                        label(ui, "Changing cells:");
                        text_box(ui, &mut self.changing, 200.0, "B1,B2:B3");
                        ui.end_row();
                        label(ui, "Comment:");
                        text_box(ui, &mut self.comment, 200.0, "");
                        ui.end_row();
                    });
                ui.label(
                    RichText::new("The changing cells' current values are saved.")
                        .size(11.0)
                        .color(T.group_label),
                );
                if ui.button("Save scenario").clicked() {
                    action = Some((
                        "add",
                        json!({ "action": "add", "name": self.name.trim(),
                                "changing": self.changing.trim(), "comment": self.comment }),
                    ));
                }
            }
            ui.separator();
            ui.horizontal(|ui| {
                label(ui, "Result cells:");
                text_box(ui, &mut self.result_cells, 140.0, "B10,B11");
                if ui
                    .add_enabled(!names.is_empty(), egui::Button::new("Summary"))
                    .clicked()
                {
                    action = Some((
                        "summary",
                        json!({ "action": "summary", "result_cells": self.result_cells.trim() }),
                    ));
                }
            });
            if let Some(s) = &self.status {
                ui.label(RichText::new(s).size(11.5).color(T.status_text));
            }
            error_line(ui, &self.error);
            close_button(ui)
        });
        if cancel || esc {
            return Flow::Close;
        }
        if let Some((what, p)) = action {
            match app.try_exec("data.what_if.scenario", p) {
                Ok(_) => {
                    self.error = None;
                    self.status = Some(match what {
                        "add" => {
                            self.adding = false;
                            self.selected = Some(self.name.trim().to_string());
                            format!("Added '{}'.", self.name.trim())
                        }
                        "show" => "Scenario values entered in the changing cells.".into(),
                        "delete" => {
                            self.selected = None;
                            "Deleted.".into()
                        }
                        _ => "Summary sheet added.".into(),
                    });
                }
                Err(e) => self.error = Some(e),
            }
        }
        Flow::Keep
    }
}

// ---------------------------------------------------------------------------------------
// Data Table

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DataTable {
    pub range: String,
    pub row_input: String,
    pub col_input: String,
    pub error: Option<String>,
}

impl DataTable {
    pub fn new(s: &Session) -> Self {
        DataTable {
            range: s.sel.range().to_a1(),
            ..Default::default()
        }
    }

    pub fn params(&self) -> Result<Json, String> {
        if self.row_input.trim().is_empty() && self.col_input.trim().is_empty() {
            return Err("Give a row input cell, a column input cell, or both.".into());
        }
        let mut m = Map::new();
        m.insert("range".into(), json!(self.range.trim()));
        if !self.row_input.trim().is_empty() {
            m.insert("row_input".into(), json!(self.row_input.trim()));
        }
        if !self.col_input.trim().is_empty() {
            m.insert("col_input".into(), json!(self.col_input.trim()));
        }
        Ok(Json::Object(m))
    }

    pub fn ui(&mut self, ctx: &Context, app: &mut App) -> Flow {
        let ((ok, cancel), esc) = window(ctx, "Data Table", 300.0, |ui| {
            egui::Grid::new("dt")
                .num_columns(2)
                .spacing([10.0, 6.0])
                .show(ui, |ui| {
                    label(ui, "Table range:");
                    text_box(ui, &mut self.range, 130.0, "D2:F8");
                    ui.end_row();
                    label(ui, "Row input cell:");
                    text_box(ui, &mut self.row_input, 130.0, "B1");
                    ui.end_row();
                    label(ui, "Column input cell:");
                    text_box(ui, &mut self.col_input, 130.0, "B2");
                    ui.end_row();
                });
            error_line(ui, &self.error);
            ok_cancel(ui, "OK")
        });
        if cancel || esc {
            return Flow::Close;
        }
        if ok {
            match self.params() {
                Ok(p) => return run(app, "data.what_if.data_table", p, &mut self.error),
                Err(e) => self.error = Some(e),
            }
        }
        Flow::Keep
    }
}

// ---------------------------------------------------------------------------------------
// Forecast Sheet

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ForecastSheet {
    pub timeline: String,
    pub values: String,
    pub periods: String,
    pub auto_season: bool,
    pub season: String,
    pub confidence: String,
    pub error: Option<String>,
}

impl ForecastSheet {
    /// With two columns selected (timeline, then values), both ranges are filled in.
    pub fn new(s: &Session) -> Self {
        let r = s.sel.range();
        let (timeline, values) = if r.end.col == r.start.col + 1 && r.end.row > r.start.row {
            let col = |c: u32| {
                sp_core::CellRange::new(
                    sp_core::CellAddr::new(r.start.row, c),
                    sp_core::CellAddr::new(r.end.row, c),
                )
                .to_a1()
            };
            (col(r.start.col), col(r.end.col))
        } else {
            (String::new(), String::new())
        };
        ForecastSheet {
            timeline,
            values,
            auto_season: true,
            confidence: "95".into(),
            ..Default::default()
        }
    }

    pub fn params(&self) -> Result<Json, String> {
        if self.values.trim().is_empty() || self.timeline.trim().is_empty() {
            return Err("Give the timeline and values ranges.".into());
        }
        let mut m = Map::new();
        m.insert("values".into(), json!(self.values.trim()));
        m.insert("timeline".into(), json!(self.timeline.trim()));
        if !self.periods.trim().is_empty() {
            m.insert("periods".into(), json!(num(&self.periods)?));
        }
        m.insert(
            "seasonality".into(),
            json!(if self.auto_season {
                1.0
            } else {
                num(&self.season)?
            }),
        );
        let c = num(&self.confidence)?;
        if !(0.0..100.0).contains(&c) {
            return Err("Confidence interval is a percentage below 100.".into());
        }
        m.insert("confidence".into(), json!(c / 100.0));
        Ok(Json::Object(m))
    }

    pub fn ui(&mut self, ctx: &Context, app: &mut App) -> Flow {
        let ((ok, cancel), esc) = window(ctx, "Create Forecast Worksheet", 360.0, |ui| {
            egui::Grid::new("fc")
                .num_columns(2)
                .spacing([10.0, 6.0])
                .show(ui, |ui| {
                    label(ui, "Timeline range:");
                    text_box(ui, &mut self.timeline, 170.0, "A2:A25");
                    ui.end_row();
                    label(ui, "Values range:");
                    text_box(ui, &mut self.values, 170.0, "B2:B25");
                    ui.end_row();
                    label(ui, "Periods to forecast:");
                    text_box(ui, &mut self.periods, 170.0, "(a third of the history)");
                    ui.end_row();
                    label(ui, "Seasonality:");
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut self.auto_season, true, "Detect");
                        ui.radio_value(&mut self.auto_season, false, "Set:");
                        ui.add_enabled(
                            !self.auto_season,
                            egui::TextEdit::singleline(&mut self.season).desired_width(50.0),
                        );
                    });
                    ui.end_row();
                    label(ui, "Confidence (%):");
                    text_box(ui, &mut self.confidence, 170.0, "95");
                    ui.end_row();
                });
            error_line(ui, &self.error);
            ok_cancel(ui, "Create")
        });
        if cancel || esc {
            return Flow::Close;
        }
        if ok {
            match self.params() {
                Ok(p) => return run(app, "data.forecast_sheet", p, &mut self.error),
                Err(e) => self.error = Some(e),
            }
        }
        Flow::Keep
    }
}

// ---------------------------------------------------------------------------------------
// Paste Name (F3)

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PasteName {
    pub names: Vec<String>,
    pub error: Option<String>,
}

impl PasteName {
    pub fn new(s: &Session) -> Self {
        let mut names: Vec<String> = s.wb.names.iter().map(|n| n.name.clone()).collect();
        names.sort_by_key(|n| n.to_ascii_lowercase());
        names.dedup();
        PasteName { names, error: None }
    }

    pub fn ui(&mut self, ctx: &Context, app: &mut App) -> Flow {
        let mut picked: Option<String> = None;
        let (cancel, esc) = window(ctx, "Paste Name", 260.0, |ui| {
            if self.names.is_empty() {
                ui.label(RichText::new("No defined names").color(T.group_label));
            }
            egui::ScrollArea::vertical()
                .max_height(220.0)
                .show(ui, |ui| {
                    for n in &self.names {
                        if ui.button(n).clicked() {
                            picked = Some(n.clone());
                        }
                    }
                });
            error_line(ui, &self.error);
            close_button(ui)
        });
        if cancel || esc {
            return Flow::Close;
        }
        if let Some(n) = picked {
            match app.try_exec("formulas.use_in_formula", json!({ "name": n })) {
                Ok(r) => {
                    if let Some(t) = r.get("insert").and_then(Json::as_str) {
                        super::params_form::insert_text(app, t);
                    }
                    return Flow::Close;
                }
                Err(e) => self.error = Some(e),
            }
        }
        Flow::Keep
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;
    use sp_core::{CellAddr, CellRange};

    #[test]
    fn move_copy_targets() {
        let mut s = Session::new();
        for n in ["B", "C"] {
            s.execute("home.cells.insert_sheet", &json!({"name": n}))
                .unwrap();
        }
        s.execute("sheet.activate", &json!({"sheet": 0})).unwrap();
        let mut d = MoveCopy::new(&s, None);
        let first = d.sheets[0].clone();
        // Before the last sheet → index 1 after removing it from 0.
        d.before = 2;
        assert_eq!(d.params()["to"], json!(1));
        d.before = d.sheets.len();
        s.execute("home.cells.move_copy_sheet", &d.params())
            .unwrap();
        assert_eq!(s.wb.sheets.last().unwrap().name, first);
        // A copy keeps the original and lands where asked.
        let mut c = MoveCopy::new(&s, Some(0));
        c.copy = true;
        c.before = 0;
        s.execute("home.cells.move_copy_sheet", &c.params())
            .unwrap();
        assert_eq!(s.wb.sheets.len(), 4);
    }

    #[test]
    fn link_scale_margins_and_data_table_params() {
        let mut s = Session::new();
        s.execute("cell.set", &json!({"cell": "A1", "input": "Docs"}))
            .unwrap();
        let mut h = Hyperlink::new(&s);
        assert_eq!(h.text, "Docs");
        assert!(h.params().is_err());
        h.address = "https://example.com".into();
        s.execute("insert.hyperlink", &h.params().unwrap()).unwrap();
        assert_eq!(Hyperlink::new(&s).address, "https://example.com");
        let mut place = Hyperlink::new(&s);
        place.to = LinkTo::Place;
        place.reference = "C3".into();
        assert_eq!(place.params().unwrap()["location"], json!("Sheet1!C3"));

        let mut f = ScaleToFit::new(&s);
        assert_eq!((f.width.as_str(), f.scale.as_str()), ("Automatic", "100"));
        f.width = "1".into();
        s.execute("layout.scale_to_fit", &f.params().unwrap())
            .unwrap();
        assert_eq!(ScaleToFit::new(&s).width, "1");

        let mut m = CustomMargins::new(&s);
        m.values[0] = "1.5".into();
        s.execute("layout.margins", &m.params().unwrap()).unwrap();
        assert_eq!(CustomMargins::new(&s).values[0], "1.5");
        m.values[1] = "x".into();
        assert!(m.params().is_err());

        let dt = DataTable::default();
        assert!(dt.params().is_err());
    }

    #[test]
    fn forecast_takes_two_selected_columns() {
        let mut s = Session::new();
        s.select_range(CellRange::parse("A2:B25").unwrap(), CellAddr::new(1, 0));
        let f = ForecastSheet::new(&s);
        assert_eq!(
            (f.timeline.as_str(), f.values.as_str()),
            ("A2:A25", "B2:B25")
        );
        let p = f.params().unwrap();
        assert_eq!(
            (p["seasonality"].clone(), p["confidence"].clone()),
            (json!(1.0), json!(0.95))
        );
    }

    #[test]
    fn consolidate_and_advanced_filter_need_their_ranges() {
        let s = Session::new();
        let mut c = Consolidate::new(&s);
        assert!(c.params().is_err());
        c.sources.push("Sheet1!A1:B3".into());
        assert_eq!(c.params().unwrap()["func"], json!("sum"));
        let mut a = AdvancedFilter::new(&s);
        assert!(a.params().is_err());
        a.criteria = "F1:G2".into();
        a.copy = true;
        assert!(a.params().is_err());
        a.copy_to = "I1".into();
        assert_eq!(a.params().unwrap()["copy_to"], json!("I1"));
    }
}
