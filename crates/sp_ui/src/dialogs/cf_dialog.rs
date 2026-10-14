//! Conditional formatting dialogs: New Formatting Rule (rule type, criteria, format with a
//! live preview) and the Rules Manager (list in priority order, applies-to, stop if true,
//! move up / down, delete, new / edit).
//!
//! Every action runs a command: `home.styles.cf.new_rule` creates, `home.styles.cf.manage`
//! lists, edits, reorders and deletes. Nothing here touches the workbook directly.

use super::widgets::color_combo;
use super::{Flow, error_line, label, ok_cancel, window};
use crate::App;
use crate::theme::{LIGHT as T, color};
use egui::{Context, FontId, RichText, Sense, Stroke, StrokeKind, Vec2};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use sp_engine::Session;
use sp_model::{DiffStyle, TimePeriod};

/// "Select a Rule Type" entries, in the target app's order.
pub const CATEGORIES: [&str; 6] = [
    "Format all cells based on their values",
    "Format only cells that contain",
    "Format only top or bottom ranked values",
    "Format only values that are above or below average",
    "Format only unique or duplicate values",
    "Use a formula to determine which cells to format",
];

/// Category indices.
pub const CAT_VALUES: usize = 0;
pub const CAT_CONTAINS: usize = 1;
pub const CAT_TOP: usize = 2;
pub const CAT_AVERAGE: usize = 3;
pub const CAT_UNIQUE: usize = 4;
pub const CAT_FORMULA: usize = 5;

/// "Format all cells based on their values" styles: `(param type, label)`.
const VALUE_STYLES: [(&str, &str); 4] = [
    ("color_scale_2", "2-Color Scale"),
    ("color_scale_3", "3-Color Scale"),
    ("data_bar", "Data Bar"),
    ("icon_set", "Icon Sets"),
];
const SCALES_2: [&str; 6] = [
    "green_white",
    "white_green",
    "red_white",
    "white_red",
    "green_yellow",
    "yellow_green",
];
const SCALES_3: [&str; 6] = [
    "green_yellow_red",
    "red_yellow_green",
    "green_white_red",
    "red_white_green",
    "blue_white_red",
    "red_white_blue",
];
const BAR_COLORS: [&str; 6] = ["blue", "green", "red", "orange", "light_blue", "purple"];

/// "Format only cells with": `(rule, label)`.
const CONTAINS: [(&str, &str); 7] = [
    ("cell_value", "Cell Value"),
    ("text", "Specific Text"),
    ("dates", "Dates Occurring"),
    ("blanks", "Blanks"),
    ("no_blanks", "No Blanks"),
    ("errors", "Errors"),
    ("no_errors", "No Errors"),
];
const VALUE_OPS: [(&str, &str); 8] = [
    ("between", "between"),
    ("not_between", "not between"),
    ("equal_to", "equal to"),
    ("not_equal", "not equal to"),
    ("greater_than", "greater than"),
    ("less_than", "less than"),
    ("greater_equal", "greater than or equal to"),
    ("less_equal", "less than or equal to"),
];
const TEXT_OPS: [(&str, &str); 4] = [
    ("text_contains", "containing"),
    ("not_contains", "not containing"),
    ("begins_with", "beginning with"),
    ("ends_with", "ending with"),
];

/// Format presets: `(preset param, label)`; the last entry means "custom".
pub const FORMAT_PRESETS: [(&str, &str); 7] = [
    ("red", "Light Red Fill with Dark Red Text"),
    ("yellow", "Yellow Fill with Dark Yellow Text"),
    ("green", "Green Fill with Dark Green Text"),
    ("light_red_fill", "Light Red Fill"),
    ("red_text", "Red Text"),
    ("red_border", "Red Border"),
    ("custom", "Custom Format…"),
];

fn label_of<'a>(list: &'a [(&str, &'a str)], key: &str) -> &'a str {
    list.iter().find(|(k, _)| *k == key).map_or("", |(_, l)| *l)
}

fn combo(ui: &mut egui::Ui, id: &str, width: f32, value: &mut String, list: &[(&str, &str)]) {
    egui::ComboBox::new(id, "")
        .selected_text(label_of(list, value))
        .width(width)
        .show_ui(ui, |ui| {
            for (k, l) in list {
                if ui.selectable_label(value == k, *l).clicked() {
                    *value = (*k).to_string();
                }
            }
        });
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CfMode {
    #[default]
    New,
    Manage,
}

/// One row of the Rules Manager, as `home.styles.cf.manage` lists it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RuleRow {
    pub priority: u32,
    pub text: String,
    pub applies_to: String,
    pub stop_if_true: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CfDialog {
    pub mode: CfMode,
    /// Opened from the Rules Manager: go back to it after OK / Cancel.
    pub from_manager: bool,
    /// Applies to (A1, defaults to the selection).
    pub range: String,
    /// Index into [`CATEGORIES`].
    pub category: usize,
    // Format all cells based on their values.
    pub value_style: String,
    pub scale: String,
    pub bar_color: String,
    pub icon_set: String,
    // Format only cells that contain.
    pub contains: String,
    pub op: String,
    pub value: String,
    pub value2: String,
    pub text_op: String,
    pub text: String,
    pub period: String,
    // Top / bottom.
    pub bottom: bool,
    pub rank: u32,
    pub percent: bool,
    // Average.
    pub below: bool,
    // Unique / duplicate.
    pub unique: bool,
    // Formula.
    pub formula: String,
    // Format.
    pub format: String,
    pub fill: Option<String>,
    pub font_color: Option<String>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub stop_if_true: bool,
    // Rules Manager.
    /// Show rules for the whole sheet (false: only those touching the selection).
    pub whole_sheet: bool,
    pub rules: Vec<RuleRow>,
    pub selected: Option<u32>,
    /// Applies-to text being edited for the selected rule.
    pub edit_applies: String,
    /// Rows need re-reading from the session.
    pub stale: bool,
    pub error: Option<String>,
}

impl Default for CfDialog {
    fn default() -> Self {
        CfDialog {
            mode: CfMode::New,
            from_manager: false,
            range: String::new(),
            category: CAT_CONTAINS,
            value_style: "color_scale_3".into(),
            scale: "green_yellow_red".into(),
            bar_color: "blue".into(),
            icon_set: "3TrafficLights1".into(),
            contains: "cell_value".into(),
            op: "greater_than".into(),
            value: String::new(),
            value2: String::new(),
            text_op: "text_contains".into(),
            text: String::new(),
            period: "today".into(),
            bottom: false,
            rank: 10,
            percent: false,
            below: false,
            unique: false,
            formula: String::new(),
            format: "red".into(),
            fill: None,
            font_color: None,
            bold: false,
            italic: false,
            underline: false,
            strike: false,
            stop_if_true: false,
            whole_sheet: true,
            rules: Vec::new(),
            selected: None,
            edit_applies: String::new(),
            stale: true,
            error: None,
        }
    }
}

/// Human description of a rule listed by `home.styles.cf.manage`.
pub fn describe_rule(r: &Json) -> String {
    let s = |k: &str| r.get(k).and_then(Json::as_str).unwrap_or("").to_string();
    let op_text = |op: &str| match op {
        "between" => "between",
        "notBetween" => "not between",
        "equal" => "=",
        "notEqual" => "<>",
        "greaterThan" => ">",
        "lessThan" => "<",
        "greaterThanOrEqual" => ">=",
        "lessThanOrEqual" => "<=",
        "containsText" => "contains",
        "notContainsText" => "does not contain",
        "beginsWith" => "begins with",
        "endsWith" => "ends with",
        _ => "?",
    };
    match s("type").as_str() {
        "cell_is" => {
            let op = s("operator");
            if op == "between" || op == "notBetween" {
                format!(
                    "Cell Value {} {} and {}",
                    op_text(&op),
                    s("value"),
                    s("value2")
                )
            } else {
                format!("Cell Value {} {}", op_text(&op), s("value"))
            }
        }
        "text" => format!("Cell value {} '{}'", op_text(&s("operator")), s("text")),
        "date_occurring" => format!("Dates occurring: {}", s("period")),
        "duplicates" => "Duplicate Values".into(),
        "unique" => "Unique Values".into(),
        "top" | "bottom" => {
            let rank = r.get("rank").and_then(Json::as_u64).unwrap_or(10);
            let pct = r.get("percent").and_then(Json::as_bool).unwrap_or(false);
            let word = if s("type") == "top" { "Top" } else { "Bottom" };
            format!("{word} {rank}{}", if pct { "%" } else { "" })
        }
        "above_average" => "Above Average".into(),
        "below_average" => "Below Average".into(),
        "blanks" => "Blanks".into(),
        "no_blanks" => "No Blanks".into(),
        "errors" => "Errors".into(),
        "no_errors" => "No Errors".into(),
        "formula" => format!("Formula: {}", s("formula")),
        "data_bar" => "Data Bar".into(),
        "color_scale" => "Graded Color Scale".into(),
        "icon_set" => format!("Icon Set ({})", s("set")),
        other => other.to_string(),
    }
}

/// The cell format a rule applies, by priority (read-only, for the preview swatch).
fn rule_style(s: &Session, priority: u32) -> Option<DiffStyle> {
    let sh = s.wb.sheet(s.sheet_index())?;
    sh.cond_formats
        .iter()
        .flat_map(|c| c.rules.iter())
        .find(|r| r.priority == priority)
        .map(|r| r.style.clone())
}

/// Paint a "AaBbCcYyZz" sample in a cell format.
fn preview(ui: &mut egui::Ui, size: Vec2, st: Option<&DiffStyle>, empty_text: &str) {
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let p = ui.painter_at(rect);
    let fill = st.and_then(|d| d.fill).map_or(T.preview_bg, color);
    p.rect_filled(rect, 2.0, fill);
    let border = st
        .and_then(|d| d.border.as_ref())
        .and_then(|b| b.color)
        .map_or(T.list_border, color);
    p.rect_stroke(rect, 2.0, Stroke::new(1.0, border), StrokeKind::Inside);
    let Some(d) = st.filter(|d| !d.is_empty()) else {
        p.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            empty_text,
            FontId::proportional(11.0),
            T.group_label,
        );
        return;
    };
    let mut rt = RichText::new("AaBbCcYyZz")
        .size(13.0)
        .color(d.font_color.map_or(T.cell_text, color));
    if d.italic == Some(true) {
        rt = rt.italics();
    }
    if d.underline == Some(true) {
        rt = rt.underline();
    }
    if d.strike == Some(true) {
        rt = rt.strikethrough();
    }
    let galley = egui::WidgetText::from(rt).into_galley(
        ui,
        Some(egui::TextWrapMode::Extend),
        f32::INFINITY,
        egui::TextStyle::Body,
    );
    let at = rect.center() - galley.size() / 2.0;
    let text_color = d.font_color.map_or(T.cell_text, color);
    if d.bold == Some(true) {
        // Faux bold: a second pass offset by a fraction of a pixel.
        p.galley(at + Vec2::new(0.7, 0.0), galley.clone(), text_color);
    }
    p.galley(at, galley, text_color);
}

impl CfDialog {
    /// New Formatting Rule over the selection, opened on a rule type.
    pub fn new_rule(s: &Session, category: usize) -> Self {
        CfDialog {
            range: s.sel.range().to_a1(),
            category: category.min(CATEGORIES.len() - 1),
            ..CfDialog::default()
        }
    }

    /// The Rules Manager.
    pub fn manager(s: &Session) -> Self {
        CfDialog {
            mode: CfMode::Manage,
            range: s.sel.range().to_a1(),
            ..CfDialog::default()
        }
    }

    /// The cell format the rule will apply (`None` for bar / scale / icon rules).
    pub fn diff_style(&self) -> Option<DiffStyle> {
        if self.category == CAT_VALUES {
            return None;
        }
        if self.format != "custom" {
            return Some(DiffStyle::preset(&self.format));
        }
        let c = |h: &Option<String>| h.as_deref().and_then(sp_model::Color::from_hex);
        let flag = |b: bool| b.then_some(true);
        Some(DiffStyle {
            fill: c(&self.fill),
            font_color: c(&self.font_color),
            bold: flag(self.bold),
            italic: flag(self.italic),
            underline: flag(self.underline),
            strike: flag(self.strike),
            ..DiffStyle::default()
        })
    }

    fn format_param(&self) -> Json {
        if self.format != "custom" {
            return json!(self.format);
        }
        let mut o = serde_json::Map::new();
        if let Some(f) = &self.fill {
            o.insert("fill".into(), json!(format!("#{f}")));
        }
        if let Some(f) = &self.font_color {
            o.insert("font_color".into(), json!(format!("#{f}")));
        }
        for (k, v) in [
            ("bold", self.bold),
            ("italic", self.italic),
            ("underline", self.underline),
            ("strike", self.strike),
        ] {
            if v {
                o.insert(k.into(), json!(true));
            }
        }
        Json::Object(o)
    }

    /// The command and params OK runs.
    pub fn params(&self) -> Result<(String, Json), String> {
        let mut p = serde_json::Map::new();
        if !self.range.trim().is_empty() {
            p.insert("range".into(), json!(self.range.trim()));
        }
        let rule = |p: &mut serde_json::Map<String, Json>, r: &str| {
            p.insert("type".into(), json!("highlight"));
            p.insert("rule".into(), json!(r));
        };
        match self.category {
            CAT_VALUES => match self.value_style.as_str() {
                "data_bar" => {
                    p.insert("type".into(), json!("data_bar"));
                    p.insert("color".into(), json!(self.bar_color));
                }
                "icon_set" => {
                    p.insert("type".into(), json!("icon_set"));
                    p.insert("set".into(), json!(self.icon_set));
                }
                _ => {
                    p.insert("type".into(), json!("color_scale"));
                    p.insert("scale".into(), json!(self.scale));
                }
            },
            CAT_CONTAINS => match self.contains.as_str() {
                "cell_value" => {
                    if self.value.trim().is_empty() {
                        return Err("Enter a value.".into());
                    }
                    rule(&mut p, &self.op);
                    p.insert("value".into(), json!(self.value.trim()));
                    if matches!(self.op.as_str(), "between" | "not_between") {
                        if self.value2.trim().is_empty() {
                            return Err("Enter both values.".into());
                        }
                        p.insert("value2".into(), json!(self.value2.trim()));
                    }
                }
                "text" => {
                    if self.text.is_empty() {
                        return Err("Enter the text to look for.".into());
                    }
                    rule(&mut p, &self.text_op);
                    p.insert("text".into(), json!(self.text));
                }
                "dates" => {
                    rule(&mut p, "date_occurring");
                    p.insert("period".into(), json!(self.period));
                }
                other => rule(&mut p, other),
            },
            CAT_TOP => {
                rule(&mut p, if self.bottom { "bottom" } else { "top" });
                p.insert("rank".into(), json!(self.rank.clamp(1, 1000)));
                p.insert("percent".into(), json!(self.percent));
            }
            CAT_AVERAGE => rule(
                &mut p,
                if self.below {
                    "below_average"
                } else {
                    "above_average"
                },
            ),
            CAT_UNIQUE => rule(&mut p, if self.unique { "unique" } else { "duplicates" }),
            _ => {
                let f = self.formula.trim();
                if f.trim_start_matches('=').is_empty() {
                    return Err("Enter a formula.".into());
                }
                p.insert("type".into(), json!("formula"));
                let f = if f.starts_with('=') {
                    f.to_string()
                } else {
                    format!("={f}")
                };
                p.insert("formula".into(), json!(f));
            }
        }
        if self.category != CAT_VALUES {
            p.insert("format".into(), self.format_param());
            if self.stop_if_true {
                p.insert("stop_if_true".into(), json!(true));
            }
        }
        Ok(("home.styles.cf.new_rule".into(), Json::Object(p)))
    }

    pub fn ui(&mut self, ctx: &Context, app: &mut App) -> Flow {
        match self.mode {
            CfMode::New => self.new_ui(ctx, app),
            CfMode::Manage => self.manage_ui(ctx, app),
        }
    }

    fn back_or_close(&mut self) -> Flow {
        if self.from_manager {
            self.mode = CfMode::Manage;
            self.from_manager = false;
            self.stale = true;
            self.error = None;
            Flow::Keep
        } else {
            Flow::Close
        }
    }

    fn new_ui(&mut self, ctx: &Context, app: &mut App) -> Flow {
        let ((ok, cancel), esc) = window(ctx, "New Formatting Rule", 520.0, |ui| {
            label(ui, "Select a Rule Type:");
            egui::Frame::NONE
                .fill(T.list_bg)
                .stroke(Stroke::new(1.0, T.list_border))
                .inner_margin(egui::Margin::same(2))
                .show(ui, |ui| {
                    ui.set_width(512.0);
                    let layout = egui::Layout::top_down_justified(egui::Align::Min);
                    ui.with_layout(layout, |ui| {
                        for (i, c) in CATEGORIES.iter().enumerate() {
                            if ui
                                .selectable_label(self.category == i, format!("  {c}"))
                                .clicked()
                            {
                                self.category = i;
                                self.error = None;
                            }
                        }
                    });
                });
            ui.add_space(6.0);
            label(ui, "Edit the Rule Description:");
            egui::Frame::group(ui.style())
                .fill(T.list_bg)
                .show(ui, |ui| {
                    ui.set_width(500.0);
                    self.criteria_ui(ui);
                    ui.add_space(6.0);
                    if self.category != CAT_VALUES {
                        self.format_ui(ui);
                    }
                });
            ui.horizontal(|ui| {
                label(ui, "Applies to:");
                ui.add(egui::TextEdit::singleline(&mut self.range).desired_width(160.0));
                if self.category != CAT_VALUES {
                    ui.checkbox(&mut self.stop_if_true, "Stop If True");
                }
            });
            error_line(ui, &self.error);
            ok_cancel(ui, "OK")
        });
        if cancel || esc {
            return self.back_or_close();
        }
        if ok {
            match self
                .params()
                .and_then(|(id, p)| app.try_exec(&id, p).map(|_| ()))
            {
                Ok(()) => return self.back_or_close(),
                Err(e) => self.error = Some(e),
            }
        }
        Flow::Keep
    }

    fn criteria_ui(&mut self, ui: &mut egui::Ui) {
        match self.category {
            CAT_VALUES => {
                ui.horizontal(|ui| {
                    label(ui, "Format Style:");
                    combo(ui, "cf-vstyle", 150.0, &mut self.value_style, &VALUE_STYLES);
                });
                ui.horizontal(|ui| match self.value_style.as_str() {
                    "data_bar" => {
                        label(ui, "Bar Color:");
                        let list: Vec<(&str, &str)> = BAR_COLORS.iter().map(|c| (*c, *c)).collect();
                        combo(ui, "cf-bar", 150.0, &mut self.bar_color, &list);
                    }
                    "icon_set" => {
                        label(ui, "Icon Style:");
                        let list: Vec<(&str, &str)> = sp_engine::commands::cf_cmds::ICON_SETS
                            .iter()
                            .map(|c| (*c, *c))
                            .collect();
                        combo(ui, "cf-icons", 180.0, &mut self.icon_set, &list);
                    }
                    style => {
                        let set: &[&str] = if style == "color_scale_2" {
                            &SCALES_2
                        } else {
                            &SCALES_3
                        };
                        if !set.contains(&self.scale.as_str()) {
                            self.scale = set.first().map_or_else(String::new, |s| s.to_string());
                        }
                        label(ui, "Colors (low → high):");
                        let list: Vec<(&str, &str)> = set.iter().map(|c| (*c, *c)).collect();
                        combo(ui, "cf-scale", 180.0, &mut self.scale, &list);
                    }
                });
            }
            CAT_CONTAINS => {
                label(ui, "Format only cells with:");
                ui.horizontal_wrapped(|ui| {
                    combo(ui, "cf-contains", 120.0, &mut self.contains, &CONTAINS);
                    match self.contains.as_str() {
                        "cell_value" => {
                            combo(ui, "cf-op", 160.0, &mut self.op, &VALUE_OPS);
                            ui.add(egui::TextEdit::singleline(&mut self.value).desired_width(80.0));
                            if matches!(self.op.as_str(), "between" | "not_between") {
                                label(ui, "and");
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.value2)
                                        .desired_width(80.0),
                                );
                            }
                        }
                        "text" => {
                            combo(ui, "cf-textop", 130.0, &mut self.text_op, &TEXT_OPS);
                            ui.add(egui::TextEdit::singleline(&mut self.text).desired_width(140.0));
                        }
                        "dates" => {
                            let list: Vec<(&str, &str)> =
                                TimePeriod::ALL.iter().map(|(_, n)| (*n, *n)).collect();
                            combo(ui, "cf-period", 130.0, &mut self.period, &list);
                        }
                        _ => {}
                    }
                });
            }
            CAT_TOP => {
                label(ui, "Format values that rank in the:");
                ui.horizontal(|ui| {
                    egui::ComboBox::new("cf-top", "")
                        .selected_text(if self.bottom { "Bottom" } else { "Top" })
                        .width(90.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.bottom, false, "Top");
                            ui.selectable_value(&mut self.bottom, true, "Bottom");
                        });
                    ui.add(egui::DragValue::new(&mut self.rank).range(1..=1000));
                    ui.checkbox(&mut self.percent, "% of the selected range");
                });
                if self.percent && self.rank > 100 {
                    self.rank = 100;
                }
            }
            CAT_AVERAGE => {
                label(ui, "Format values that are:");
                egui::ComboBox::new("cf-avg", "")
                    .selected_text(if self.below { "below" } else { "above" })
                    .width(90.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.below, false, "above");
                        ui.selectable_value(&mut self.below, true, "below");
                    });
                label(ui, "the average for the selected range");
            }
            CAT_UNIQUE => {
                ui.horizontal(|ui| {
                    label(ui, "Format all:");
                    egui::ComboBox::new("cf-uniq", "")
                        .selected_text(if self.unique { "unique" } else { "duplicate" })
                        .width(100.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.unique, false, "duplicate");
                            ui.selectable_value(&mut self.unique, true, "unique");
                        });
                    label(ui, "values in the selected range");
                });
            }
            _ => {
                label(ui, "Format values where this formula is true:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.formula)
                        .hint_text("=$A1>100")
                        .desired_width(480.0)
                        .font(egui::TextStyle::Monospace),
                );
            }
        }
    }

    fn format_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            label(ui, "Format with:");
            combo(ui, "cf-format", 240.0, &mut self.format, &FORMAT_PRESETS);
        });
        if self.format == "custom" {
            ui.horizontal(|ui| {
                label(ui, "Fill:");
                color_combo(ui, "cf-fill", &mut self.fill, "No Color");
                label(ui, "Font:");
                color_combo(ui, "cf-font", &mut self.font_color, "Automatic");
            });
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.bold, "Bold");
                ui.checkbox(&mut self.italic, "Italic");
                ui.checkbox(&mut self.underline, "Underline");
                ui.checkbox(&mut self.strike, "Strikethrough");
            });
        }
        ui.horizontal(|ui| {
            label(ui, "Preview:");
            let st = self.diff_style();
            preview(ui, Vec2::new(220.0, 26.0), st.as_ref(), "No Format Set");
        });
    }

    fn refresh(&mut self, app: &mut App) {
        let scope = if self.whole_sheet {
            "sheet"
        } else {
            "selection"
        };
        match app.query(
            "home.styles.cf.manage",
            json!({"action": "list", "scope": scope}),
        ) {
            Ok(Json::Array(rows)) => {
                self.rules = rows
                    .iter()
                    .map(|r| RuleRow {
                        priority: r
                            .get("priority")
                            .and_then(Json::as_u64)
                            .and_then(|p| u32::try_from(p).ok())
                            .unwrap_or(0),
                        text: describe_rule(r),
                        applies_to: r
                            .get("applies_to")
                            .and_then(Json::as_str)
                            .unwrap_or("")
                            .to_string(),
                        stop_if_true: r
                            .get("stop_if_true")
                            .and_then(Json::as_bool)
                            .unwrap_or(false),
                    })
                    .collect();
            }
            Ok(_) => self.rules.clear(),
            Err(e) => self.error = Some(e),
        }
        if !self.rules.iter().any(|r| Some(r.priority) == self.selected) {
            self.selected = self.rules.first().map(|r| r.priority);
        }
        self.edit_applies = self
            .rules
            .iter()
            .find(|r| Some(r.priority) == self.selected)
            .map(|r| r.applies_to.clone())
            .unwrap_or_default();
        self.stale = false;
    }

    /// Run a manage action on the selected rule; the list re-reads afterwards.
    fn manage_action(&mut self, app: &mut App, action: &str, extra: Json) {
        let Some(pri) = self.selected else {
            return;
        };
        let mut p = json!({"action": action, "priority": pri});
        if let (Some(o), Some(e)) = (p.as_object_mut(), extra.as_object()) {
            for (k, v) in e {
                o.insert(k.clone(), v.clone());
            }
        }
        match app.try_exec("home.styles.cf.manage", p) {
            Ok(_) => {
                self.error = None;
                self.selected = match action {
                    "move_up" => Some(pri.saturating_sub(1).max(1)),
                    "move_down" => Some(pri.saturating_add(1).min(self.rules.len() as u32)),
                    _ => self.selected,
                };
            }
            Err(e) => self.error = Some(e),
        }
        self.stale = true;
    }

    fn manage_ui(&mut self, ctx: &Context, app: &mut App) -> Flow {
        if self.stale {
            self.refresh(app);
        }
        let mut action: Option<(&str, Json)> = None;
        let mut new_rule = false;
        let mut rescope = false;
        let ((ok, cancel), esc) =
            window(ctx, "Conditional Formatting Rules Manager", 640.0, |ui| {
                ui.horizontal(|ui| {
                    label(ui, "Show formatting rules for:");
                    egui::ComboBox::new("cfm-scope", "")
                        .selected_text(if self.whole_sheet {
                            "This Worksheet"
                        } else {
                            "Current Selection"
                        })
                        .width(150.0)
                        .show_ui(ui, |ui| {
                            rescope |= ui
                                .selectable_value(&mut self.whole_sheet, false, "Current Selection")
                                .clicked();
                            rescope |= ui
                                .selectable_value(&mut self.whole_sheet, true, "This Worksheet")
                                .clicked();
                        });
                });
                ui.horizontal(|ui| {
                    new_rule = ui.button("New Rule…").clicked();
                    let has = self.selected.is_some();
                    if ui
                        .add_enabled(has, egui::Button::new("Delete Rule"))
                        .clicked()
                    {
                        action = Some(("delete", json!({})));
                    }
                    let first = self.rules.first().map(|r| r.priority) == self.selected;
                    let last = self.rules.last().map(|r| r.priority) == self.selected;
                    if ui
                        .add_enabled(has && !first, egui::Button::new("Move Up"))
                        .clicked()
                    {
                        action = Some(("move_up", json!({})));
                    }
                    if ui
                        .add_enabled(has && !last, egui::Button::new("Move Down"))
                        .clicked()
                    {
                        action = Some(("move_down", json!({})));
                    }
                });
                egui::Frame::NONE
                    .fill(T.list_bg)
                    .stroke(Stroke::new(1.0, T.list_border))
                    .inner_margin(egui::Margin::same(4))
                    .show(ui, |ui| {
                        ui.set_width(628.0);
                        ui.set_min_height(170.0);
                        egui::ScrollArea::vertical()
                            .max_height(220.0)
                            .show(ui, |ui| {
                                egui::Grid::new("cfm-rules")
                                    .num_columns(4)
                                    .spacing([12.0, 4.0])
                                    .striped(false)
                                    .show(ui, |ui| {
                                        for h in [
                                            "Rule (applied in order shown)",
                                            "Format",
                                            "Applies to",
                                            "Stop If True",
                                        ] {
                                            label(ui, h);
                                        }
                                        ui.end_row();
                                        if self.rules.is_empty() {
                                            label(ui, "(no rules)");
                                            ui.end_row();
                                        }
                                        let mut stop_change = None;
                                        for r in &self.rules {
                                            let sel = self.selected == Some(r.priority);
                                            let row = ui
                                                .allocate_ui_with_layout(
                                                    Vec2::new(240.0, 22.0),
                                                    egui::Layout::top_down_justified(
                                                        egui::Align::Min,
                                                    ),
                                                    |ui| {
                                                        ui.set_width(240.0);
                                                        ui.selectable_label(sel, r.text.as_str())
                                                    },
                                                )
                                                .inner;
                                            if row.clicked() {
                                                self.selected = Some(r.priority);
                                                self.edit_applies.clone_from(&r.applies_to);
                                            }
                                            let st = rule_style(&app.session, r.priority);
                                            preview(ui, Vec2::new(120.0, 20.0), st.as_ref(), "—");
                                            ui.label(&r.applies_to);
                                            let mut stop = r.stop_if_true;
                                            if ui.checkbox(&mut stop, "").changed() {
                                                stop_change = Some((r.priority, stop));
                                            }
                                            ui.end_row();
                                        }
                                        if let Some((pri, stop)) = stop_change {
                                            self.selected = Some(pri);
                                            action = Some(("edit", json!({"stop_if_true": stop})));
                                        }
                                    });
                            });
                    });
                ui.horizontal(|ui| {
                    label(ui, "Applies to (selected rule):");
                    ui.add_enabled(
                        self.selected.is_some(),
                        egui::TextEdit::singleline(&mut self.edit_applies).desired_width(220.0),
                    );
                    if ui
                        .add_enabled(self.selected.is_some(), egui::Button::new("Apply"))
                        .clicked()
                    {
                        action = Some(("edit", json!({"applies_to": self.edit_applies.trim()})));
                    }
                });
                error_line(ui, &self.error);
                ui.add_space(6.0);
                let close = ui
                    .with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add(egui::Button::new("Close").min_size(Vec2::new(72.0, 24.0)))
                            .clicked()
                    })
                    .inner;
                (close, false)
            });
        if let Some((a, extra)) = action {
            self.manage_action(app, a, extra);
        }
        if rescope {
            self.stale = true;
        }
        if new_rule {
            let range = self.range.clone();
            *self = CfDialog {
                range,
                from_manager: true,
                whole_sheet: self.whole_sheet,
                ..CfDialog::default()
            };
            return Flow::Keep;
        }
        if ok || cancel || esc {
            return Flow::Close;
        }
        Flow::Keep
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_per_category() {
        let mut d = CfDialog {
            range: "A1:A9".into(),
            value: "10".into(),
            ..CfDialog::default()
        };
        let (id, p) = d.params().unwrap();
        assert_eq!(id, "home.styles.cf.new_rule");
        assert_eq!(p["rule"], "greater_than");
        assert_eq!(p["value"], "10");
        assert_eq!(p["format"], "red");
        d.op = "between".into();
        assert!(d.params().is_err());
        d.value2 = "20".into();
        assert_eq!(d.params().unwrap().1["value2"], "20");
        d.category = CAT_FORMULA;
        assert!(d.params().is_err());
        d.formula = "$A1>5".into();
        assert_eq!(d.params().unwrap().1["formula"], "=$A1>5");
        d.category = CAT_VALUES;
        d.value_style = "data_bar".into();
        let p = d.params().unwrap().1;
        assert_eq!(p["type"], "data_bar");
        assert!(p.get("format").is_none());
        d.category = CAT_TOP;
        d.bottom = true;
        d.rank = 5;
        assert_eq!(d.params().unwrap().1["rule"], "bottom");
        d.category = CAT_CONTAINS;
        d.format = "custom".into();
        d.fill = Some("FFFF00".into());
        d.bold = true;
        d.op = "less_than".into();
        let p = d.params().unwrap().1;
        assert_eq!(p["format"]["fill"], "#FFFF00");
        assert_eq!(p["format"]["bold"], true);
    }

    #[test]
    fn every_category_builds_a_rule_the_engine_accepts() {
        let mut s = Session::new();
        for (i, v) in ["5", "50", "95"].iter().enumerate() {
            s.execute(
                "cell.set",
                &json!({"cell": format!("A{}", i + 1), "input": v}),
            )
            .unwrap();
        }
        for cat in 0..CATEGORIES.len() {
            let d = CfDialog {
                range: "A1:A3".into(),
                category: cat,
                value: "10".into(),
                formula: "=A1>10".into(),
                ..CfDialog::default()
            };
            let (id, p) = d.params().unwrap();
            s.execute(&id, &p).unwrap_or_else(|e| panic!("{cat}: {e}"));
        }
        let rules = s.execute("home.styles.cf.manage", &json!({})).unwrap();
        assert_eq!(rules.as_array().unwrap().len(), CATEGORIES.len());
        for r in rules.as_array().unwrap() {
            assert!(!describe_rule(r).is_empty());
        }
    }
}
