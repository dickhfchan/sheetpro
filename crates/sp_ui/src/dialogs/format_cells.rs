//! Format Cells: Number, Alignment, Font, Border, Fill and Protection tabs. OK sends only
//! the attributes the user changed, in one `format.cells` command (one undo step); the
//! merge checkbox maps to the merge / unmerge commands.

use super::widgets::{color_combo, list_box, swatch_grid};
use super::{Flow, error_line, label, ok_cancel, window};
use crate::App;
use crate::theme::{LIGHT as T, from_hex, hex};
use egui::{Context, Pos2, Rect, RichText, Stroke, StrokeKind, Vec2};
use serde::{Deserialize, Serialize};
use serde_json::{Map, json};
use sp_core::Value;
use sp_engine::{Json, Session};
use sp_model::{Border, BorderStyle, HAlign, Style, StyleId, VAlign};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FcTab {
    #[default]
    Number,
    Alignment,
    Font,
    Border,
    Fill,
    Protection,
}

const TABS: [(FcTab, &str); 6] = [
    (FcTab::Number, "Number"),
    (FcTab::Alignment, "Alignment"),
    (FcTab::Font, "Font"),
    (FcTab::Border, "Border"),
    (FcTab::Fill, "Fill"),
    (FcTab::Protection, "Protection"),
];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NumCat {
    #[default]
    General,
    Number,
    Currency,
    Accounting,
    Date,
    Time,
    Percentage,
    Fraction,
    Scientific,
    Text,
    Custom,
}

const CATS: [(NumCat, &str); 11] = [
    (NumCat::General, "General"),
    (NumCat::Number, "Number"),
    (NumCat::Currency, "Currency"),
    (NumCat::Accounting, "Accounting"),
    (NumCat::Date, "Date"),
    (NumCat::Time, "Time"),
    (NumCat::Percentage, "Percentage"),
    (NumCat::Fraction, "Fraction"),
    (NumCat::Scientific, "Scientific"),
    (NumCat::Text, "Text"),
    (NumCat::Custom, "Custom"),
];

const DATE_CODES: [&str; 7] = [
    "m/d/yyyy",
    "d-mmm-yy",
    "d-mmm",
    "mmm-yy",
    "mmmm d, yyyy",
    "dddd, mmmm d, yyyy",
    "yyyy-mm-dd",
];
const TIME_CODES: [&str; 5] = [
    "h:mm AM/PM",
    "h:mm:ss AM/PM",
    "h:mm",
    "h:mm:ss",
    "[h]:mm:ss",
];
const FRACTION_CODES: [&str; 5] = ["# ?/?", "# ??/??", "# ?/2", "# ?/4", "# ?/8"];
const CUSTOM_CODES: [&str; 12] = [
    "General", "0", "0.00", "#,##0", "#,##0.00", "0%", "0.00%", "0.00E+00", "# ?/?", "m/d/yyyy",
    "h:mm", "@",
];

/// Font names offered by the Font tab and the ribbon picker.
pub const FONT_FAMILIES: [&str; 15] = [
    "Calibri",
    "Calibri Light",
    "Aptos",
    "Arial",
    "Arial Black",
    "Cambria",
    "Candara",
    "Consolas",
    "Courier New",
    "Georgia",
    "Segoe UI",
    "Tahoma",
    "Times New Roman",
    "Trebuchet MS",
    "Verdana",
];

/// Point sizes offered by the Font tab and the ribbon picker.
pub const FONT_SIZES: [f32; 16] = [
    8.0, 9.0, 10.0, 11.0, 12.0, 14.0, 16.0, 18.0, 20.0, 22.0, 24.0, 26.0, 28.0, 36.0, 48.0, 72.0,
];

const H_ALIGNS: [(&str, &str); 7] = [
    ("general", "General"),
    ("left", "Left (Indent)"),
    ("center", "Center"),
    ("right", "Right (Indent)"),
    ("fill", "Fill"),
    ("justify", "Justify"),
    ("center_across", "Center Across Selection"),
];
const V_ALIGNS: [(&str, &str); 3] = [("top", "Top"), ("center", "Center"), ("bottom", "Bottom")];
const UNDERLINES: [(u8, &str); 3] = [(0, "None"), (1, "Single"), (2, "Double")];
const LINE_STYLES: [&str; 6] = ["thin", "medium", "thick", "dashed", "dotted", "double"];
/// Border edges in `BorderEdit::edges` order, with the `format.cells` key for each.
const EDGES: [(&str, &str); 6] = [
    ("top", "Top"),
    ("bottom", "Bottom"),
    ("left", "Left"),
    ("right", "Right"),
    ("inside_h", "Inside horizontal"),
    ("inside_v", "Inside vertical"),
];

/// Every editable attribute; OK diffs `v` against the snapshot taken on open.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FcValues {
    pub category: NumCat,
    pub decimals: u8,
    pub thousands: bool,
    pub date_code: String,
    pub time_code: String,
    pub fraction_code: String,
    pub custom_code: String,
    pub h_align: String,
    pub v_align: String,
    pub wrap: bool,
    pub indent: u8,
    pub rotation: i16,
    pub font: String,
    pub size: f32,
    pub bold: bool,
    pub italic: bool,
    pub underline: u8,
    pub strike: bool,
    pub font_color: Option<String>,
    pub fill: Option<String>,
    pub locked: bool,
    pub hidden: bool,
}

/// Border tab state: per edge, `None` = leave as is, `Some("none")` = remove, else a style.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BorderEdit {
    pub edges: [Option<String>; 6],
    pub style: String,
    pub color: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FormatCells {
    pub tab: FcTab,
    pub v: FcValues,
    pub init: FcValues,
    pub border: BorderEdit,
    pub border_init: BorderEdit,
    pub merge: bool,
    pub merge_init: bool,
    /// Selection spans several rows / columns (enables the inside border edges).
    pub multi_row: bool,
    pub multi_col: bool,
    pub error: Option<String>,
}

fn h_name(h: HAlign) -> &'static str {
    match h {
        HAlign::General => "general",
        HAlign::Left => "left",
        HAlign::Center => "center",
        HAlign::Right => "right",
        HAlign::Fill => "fill",
        HAlign::Justify => "justify",
        HAlign::CenterAcross => "center_across",
    }
}

fn v_name(v: VAlign) -> &'static str {
    match v {
        VAlign::Top => "top",
        VAlign::Center => "center",
        VAlign::Bottom => "bottom",
    }
}

fn style_name(b: BorderStyle) -> &'static str {
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

fn decimals_of(code: &str) -> u8 {
    let first = code.split(';').next().unwrap_or("");
    first.split_once('.').map_or(0, |(_, d)| {
        u8::try_from(d.chars().take_while(|c| *c == '0').count()).unwrap_or(30)
    })
}

/// Best-guess category for a stored code, so the dialog opens on the matching page.
pub fn classify(code: &str) -> NumCat {
    let lower = code.to_ascii_lowercase();
    let first = lower.split(';').next().unwrap_or("");
    let unquoted: String = {
        let mut out = String::new();
        let (mut q, mut br) = (false, false);
        for c in first.chars() {
            match c {
                '"' if !br => q = !q,
                '[' if !q => br = true,
                ']' if !q => br = false,
                _ if !q && !br => out.push(c),
                _ => {}
            }
        }
        out
    };
    if lower == "general" || lower.is_empty() {
        NumCat::General
    } else if lower == "@" {
        NumCat::Text
    } else if unquoted.contains("e+") {
        NumCat::Scientific
    } else if unquoted.contains('%') {
        NumCat::Percentage
    } else if unquoted.contains("?/") {
        NumCat::Fraction
    } else if first.contains("_(") && first.contains('*') {
        NumCat::Accounting
    } else if first.contains('$') {
        NumCat::Currency
    } else if unquoted.contains('h') || unquoted.contains(":s") || unquoted.contains("ss") {
        NumCat::Time
    } else if unquoted.contains('y') || unquoted.contains('d') || unquoted.contains('m') {
        NumCat::Date
    } else if unquoted
        .chars()
        .all(|c| matches!(c, '0' | '#' | ',' | '.' | '(' | ')' | '-'))
    {
        NumCat::Number
    } else {
        NumCat::Custom
    }
}

impl FcValues {
    fn from_style(st: &Style) -> Self {
        let code = st.num_fmt.clone();
        let category = classify(&code);
        let pick = |list: &[&str]| {
            list.iter()
                .find(|c| c.eq_ignore_ascii_case(&code))
                .map_or_else(
                    || list.first().copied().unwrap_or("").to_string(),
                    |c| (*c).to_string(),
                )
        };
        FcValues {
            category,
            decimals: if category == NumCat::General {
                2
            } else {
                decimals_of(&code)
            },
            thousands: code.contains("#,##"),
            date_code: pick(&DATE_CODES),
            time_code: pick(&TIME_CODES),
            fraction_code: pick(&FRACTION_CODES),
            custom_code: code,
            h_align: h_name(st.h_align).into(),
            v_align: v_name(st.v_align).into(),
            wrap: st.wrap,
            indent: st.indent,
            rotation: st.rotation,
            font: st.font_name.clone(),
            size: st.font_size() as f32,
            bold: st.bold,
            italic: st.italic,
            underline: st.underline.min(2),
            strike: st.strike,
            font_color: st.font_color.map(|c| hex(crate::theme::color(c))),
            fill: st.fill.map(|c| hex(crate::theme::color(c))),
            locked: st.locked,
            hidden: st.hidden_formula,
        }
    }

    /// The number format code the Number tab currently describes.
    pub fn code(&self) -> String {
        let d = usize::from(self.decimals.min(30));
        let frac = if d == 0 {
            String::new()
        } else {
            format!(".{}", "0".repeat(d))
        };
        match self.category {
            NumCat::General => "General".into(),
            NumCat::Number => {
                format!("{}{frac}", if self.thousands { "#,##0" } else { "0" })
            }
            NumCat::Currency => format!("\"$\"#,##0{frac}"),
            NumCat::Accounting => {
                format!("_(\"$\"* #,##0{frac}_);_(\"$\"* (#,##0{frac});_(\"$\"* \"-\"??_);_(@_)")
            }
            NumCat::Date => self.date_code.clone(),
            NumCat::Time => self.time_code.clone(),
            NumCat::Percentage => format!("0{frac}%"),
            NumCat::Fraction => self.fraction_code.clone(),
            NumCat::Scientific => format!("0{}E+00", if d == 0 { ".00".into() } else { frac }),
            NumCat::Text => "@".into(),
            NumCat::Custom => self.custom_code.clone(),
        }
    }
}

fn active_style(s: &Session) -> Style {
    let id =
        s.wb.sheet(s.sheet_index())
            .and_then(|sh| sh.get(s.sel.active))
            .map_or(StyleId::DEFAULT, |c| c.style);
    s.wb.styles.get(id).clone()
}

impl FormatCells {
    pub fn new(s: &Session, tab: FcTab) -> Self {
        let st = active_style(s);
        let v = FcValues::from_style(&st);
        let r = s.sel.range();
        let single = r.is_single();
        let edge = |b: Border| single.then(|| style_name(b.style).to_string());
        let border = BorderEdit {
            edges: [
                edge(st.border_top),
                edge(st.border_bottom),
                edge(st.border_left),
                edge(st.border_right),
                None,
                None,
            ],
            style: "thin".into(),
            color: None,
        };
        let merged =
            s.wb.sheet(s.sheet_index())
                .is_some_and(|sh| sh.merges.iter().any(|m| m.contains(s.sel.active)));
        FormatCells {
            tab,
            init: v.clone(),
            v,
            border_init: border.clone(),
            border,
            merge: merged,
            merge_init: merged,
            multi_row: r.rows() > 1,
            multi_col: r.cols() > 1,
            error: None,
        }
    }

    /// `format.cells` params for what changed (empty object = nothing to do).
    pub fn params(&self) -> Json {
        let init_border = &self.border_init;
        let (v, i) = (&self.v, &self.init);
        let mut m = Map::new();
        if v.code() != i.code() {
            m.insert("code".into(), json!(v.code()));
        }
        if v.h_align != i.h_align {
            m.insert("h_align".into(), json!(v.h_align));
        }
        if v.v_align != i.v_align {
            m.insert("v_align".into(), json!(v.v_align));
        }
        if v.wrap != i.wrap {
            m.insert("wrap".into(), json!(v.wrap));
        }
        if v.indent != i.indent {
            m.insert("indent".into(), json!(v.indent));
        }
        if v.rotation != i.rotation {
            m.insert("rotation".into(), json!(v.rotation));
        }
        if v.font != i.font && !v.font.trim().is_empty() {
            m.insert("font".into(), json!(v.font.trim()));
        }
        if (v.size - i.size).abs() > f32::EPSILON {
            m.insert("size".into(), json!(v.size));
        }
        if v.bold != i.bold {
            m.insert("bold".into(), json!(v.bold));
        }
        if v.italic != i.italic {
            m.insert("italic".into(), json!(v.italic));
        }
        if v.underline != i.underline {
            m.insert("underline".into(), json!(v.underline));
        }
        if v.strike != i.strike {
            m.insert("strike".into(), json!(v.strike));
        }
        if v.font_color != i.font_color {
            m.insert("font_color".into(), json!(v.font_color));
        }
        if v.fill != i.fill {
            m.insert("fill".into(), json!(v.fill));
        }
        if v.locked != i.locked {
            m.insert("locked".into(), json!(v.locked));
        }
        if v.hidden != i.hidden {
            m.insert("hidden".into(), json!(v.hidden));
        }
        let b = &self.border;
        let mut edges = Map::new();
        for ((key, _), (cur, old)) in EDGES
            .iter()
            .zip(b.edges.iter().zip(init_border.edges.iter()))
        {
            if let Some(cur) = cur
                && (Some(cur) != old.as_ref() || b.color != init_border.color)
            {
                edges.insert((*key).into(), json!(cur));
            }
        }
        if !edges.is_empty() {
            if let Some(c) = &b.color {
                edges.insert("color".into(), json!(c));
            }
            m.insert("border".into(), Json::Object(edges));
        }
        Json::Object(m)
    }

    /// Run the commands for OK. Returns the first error.
    pub fn apply(&self, app: &mut App) -> Result<(), String> {
        let p = self.params();
        if p.as_object().is_some_and(|m| !m.is_empty()) {
            app.try_exec("format.cells", p)?;
        }
        if self.merge != self.merge_init {
            let id = if self.merge {
                "home.align.merge_cells"
            } else {
                "home.align.unmerge"
            };
            app.try_exec(id, json!({}))?;
        }
        Ok(())
    }

    pub fn ui(&mut self, ctx: &Context, app: &mut App) -> Flow {
        let ((ok, cancel), esc) = window(ctx, "Format Cells", 520.0, |ui| {
            ui.horizontal(|ui| {
                for (t, name) in TABS {
                    let on = self.tab == t;
                    let text = RichText::new(name).color(if on { T.accent } else { T.ribbon_text });
                    let text = if on { text.strong() } else { text };
                    if ui.add(egui::Button::selectable(on, text)).clicked() {
                        self.tab = t;
                    }
                }
            });
            ui.separator();
            ui.allocate_ui(Vec2::new(520.0, 300.0), |ui| {
                ui.set_min_size(Vec2::new(520.0, 300.0));
                match self.tab {
                    FcTab::Number => self.number_tab(ui, &app.session),
                    FcTab::Alignment => self.alignment_tab(ui),
                    FcTab::Font => self.font_tab(ui),
                    FcTab::Border => self.border_tab(ui),
                    FcTab::Fill => self.fill_tab(ui),
                    FcTab::Protection => self.protection_tab(ui),
                }
            });
            error_line(ui, &self.error);
            ok_cancel(ui, "OK")
        });
        if cancel || esc {
            return Flow::Close;
        }
        if ok {
            return match self.apply(app) {
                Ok(()) => Flow::Close,
                Err(e) => {
                    self.error = Some(e);
                    Flow::Keep
                }
            };
        }
        Flow::Keep
    }

    fn number_tab(&mut self, ui: &mut egui::Ui, s: &Session) {
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                label(ui, "Category:");
                let names: Vec<String> = CATS.iter().map(|(_, n)| (*n).to_string()).collect();
                let sel = CATS.iter().position(|(c, _)| *c == self.v.category);
                if let Some((i, _)) = list_box(ui, "fc-cat", Vec2::new(130.0, 250.0), &names, sel)
                    && let Some((c, _)) = CATS.get(i)
                {
                    if *c == NumCat::Custom {
                        self.v.custom_code = self.v.code();
                    }
                    self.v.category = *c;
                }
            });
            ui.add_space(8.0);
            ui.vertical(|ui| {
                // Preview of the active cell with the code being built.
                let value =
                    s.wb.sheet(s.sheet_index())
                        .and_then(|sh| sh.get(s.sel.active))
                        .map(|c| c.value.clone())
                        .filter(|v| !v.is_empty())
                        .unwrap_or(Value::Number(1234.5678));
                let shown = sp_core::numfmt::format_value(&value, &self.v.code()).text;
                egui::Frame::group(ui.style())
                    .fill(T.preview_bg)
                    .show(ui, |ui| {
                        ui.set_width(340.0);
                        label(ui, "Sample");
                        ui.label(RichText::new(shown).size(14.0).color(T.cell_text));
                    });
                ui.add_space(6.0);
                match self.v.category {
                    NumCat::General => {
                        label(ui, "General cells have no specific number format.");
                    }
                    NumCat::Number
                    | NumCat::Currency
                    | NumCat::Accounting
                    | NumCat::Percentage
                    | NumCat::Scientific => {
                        ui.horizontal(|ui| {
                            label(ui, "Decimal places:");
                            ui.add(egui::DragValue::new(&mut self.v.decimals).range(0..=30));
                        });
                        if self.v.category == NumCat::Number {
                            ui.checkbox(&mut self.v.thousands, "Use 1000 separator (,)");
                        }
                    }
                    NumCat::Date => code_list(ui, "fc-date", &DATE_CODES, &mut self.v.date_code),
                    NumCat::Time => code_list(ui, "fc-time", &TIME_CODES, &mut self.v.time_code),
                    NumCat::Fraction => {
                        code_list(ui, "fc-frac", &FRACTION_CODES, &mut self.v.fraction_code);
                    }
                    NumCat::Text => {
                        label(ui, "Text cells show exactly what is typed, even numbers.");
                    }
                    NumCat::Custom => {
                        label(ui, "Type:");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.v.custom_code)
                                .desired_width(330.0)
                                .font(egui::FontId::monospace(12.5)),
                        );
                        code_list(ui, "fc-custom", &CUSTOM_CODES, &mut self.v.custom_code);
                    }
                }
            });
        });
    }

    fn alignment_tab(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new("Text alignment").strong());
                egui::Grid::new("fc-align").num_columns(2).show(ui, |ui| {
                    label(ui, "Horizontal:");
                    pick_combo(ui, "fc-h", &H_ALIGNS, &mut self.v.h_align, 190.0);
                    ui.end_row();
                    label(ui, "Vertical:");
                    pick_combo(ui, "fc-v", &V_ALIGNS, &mut self.v.v_align, 190.0);
                    ui.end_row();
                    label(ui, "Indent:");
                    ui.add(egui::DragValue::new(&mut self.v.indent).range(0..=15));
                    ui.end_row();
                });
                ui.add_space(8.0);
                ui.label(RichText::new("Text control").strong());
                ui.checkbox(&mut self.v.wrap, "Wrap text");
                ui.checkbox(&mut self.merge, "Merge cells");
            });
            ui.add_space(24.0);
            ui.vertical(|ui| {
                ui.label(RichText::new("Orientation").strong());
                let (rect, _) =
                    ui.allocate_exact_size(Vec2::new(130.0, 130.0), egui::Sense::hover());
                let p = ui.painter();
                p.rect_filled(rect, 4.0, T.preview_bg);
                p.rect_stroke(
                    rect,
                    4.0,
                    Stroke::new(1.0, T.list_border),
                    StrokeKind::Inside,
                );
                let c = Pos2::new(rect.min.x + 20.0, rect.center().y);
                let a = f32::from(self.v.rotation).to_radians();
                let tip = c + Vec2::new(a.cos(), -a.sin()) * 95.0;
                p.line_segment([c, tip], Stroke::new(2.0, T.accent));
                p.circle_filled(tip, 4.0, T.accent);
                p.text(
                    c + Vec2::new(a.cos(), -a.sin()) * 45.0,
                    egui::Align2::CENTER_BOTTOM,
                    "Text",
                    egui::FontId::proportional(13.0),
                    T.cell_text,
                );
                ui.add(
                    egui::Slider::new(&mut self.v.rotation, -90..=90)
                        .suffix("°")
                        .text("Degrees"),
                );
            });
        });
    }

    fn font_tab(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                label(ui, "Font:");
                ui.add(egui::TextEdit::singleline(&mut self.v.font).desired_width(200.0));
                let mut fonts: Vec<String> =
                    FONT_FAMILIES.iter().map(|f| (*f).to_string()).collect();
                if !fonts.iter().any(|f| f.eq_ignore_ascii_case(&self.v.font))
                    && !self.v.font.is_empty()
                {
                    fonts.insert(0, self.v.font.clone());
                }
                let sel = fonts
                    .iter()
                    .position(|f| f.eq_ignore_ascii_case(&self.v.font));
                if let Some((i, _)) = list_box(ui, "fc-fonts", Vec2::new(200.0, 140.0), &fonts, sel)
                    && let Some(f) = fonts.get(i)
                {
                    self.v.font.clone_from(f);
                }
            });
            ui.vertical(|ui| {
                label(ui, "Font style:");
                let styles = ["Regular", "Italic", "Bold", "Bold Italic"]
                    .map(str::to_string)
                    .to_vec();
                let cur = usize::from(self.v.italic) + 2 * usize::from(self.v.bold);
                if let Some((i, _)) =
                    list_box(ui, "fc-fstyle", Vec2::new(110.0, 162.0), &styles, Some(cur))
                {
                    self.v.italic = i % 2 == 1;
                    self.v.bold = i >= 2;
                }
            });
            ui.vertical(|ui| {
                label(ui, "Size:");
                ui.add(
                    egui::DragValue::new(&mut self.v.size)
                        .range(1.0..=409.0)
                        .speed(0.5),
                );
                let sizes: Vec<String> = FONT_SIZES.iter().map(|s| format!("{s}")).collect();
                let sel = FONT_SIZES
                    .iter()
                    .position(|s| (*s - self.v.size).abs() < 0.01);
                if let Some((i, _)) = list_box(ui, "fc-sizes", Vec2::new(60.0, 140.0), &sizes, sel)
                    && let Some(s) = FONT_SIZES.get(i)
                {
                    self.v.size = *s;
                }
            });
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            label(ui, "Underline:");
            egui::ComboBox::new("fc-ul", "")
                .selected_text(
                    UNDERLINES
                        .iter()
                        .find(|(u, _)| *u == self.v.underline)
                        .map_or("None", |x| x.1),
                )
                .show_ui(ui, |ui| {
                    for (u, n) in UNDERLINES {
                        ui.selectable_value(&mut self.v.underline, u, n);
                    }
                });
            ui.add_space(12.0);
            label(ui, "Color:");
            color_combo(ui, "fc-fcolor", &mut self.v.font_color, "Automatic");
        });
        ui.checkbox(&mut self.v.strike, "Strikethrough");
        // Preview.
        let mut text = RichText::new("AaBbCcYyZz").size(self.v.size.clamp(6.0, 40.0) * 1.14);
        if self.v.italic {
            text = text.italics();
        }
        if self.v.bold {
            text = text.strong();
        }
        if self.v.underline > 0 {
            text = text.underline();
        }
        if self.v.strike {
            text = text.strikethrough();
        }
        text = text.color(
            self.v
                .font_color
                .as_deref()
                .and_then(from_hex)
                .unwrap_or(T.cell_text),
        );
        egui::Frame::group(ui.style())
            .fill(T.preview_bg)
            .show(ui, |ui| {
                ui.set_width(500.0);
                ui.set_min_height(56.0);
                ui.vertical_centered(|ui| {
                    label(ui, "Preview");
                    ui.label(text);
                });
            });
    }

    fn border_tab(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new("Line").strong());
                label(ui, "Style:");
                for st in LINE_STYLES {
                    let on = self.border.style == st;
                    let (rect, resp) =
                        ui.allocate_exact_size(Vec2::new(120.0, 20.0), egui::Sense::click());
                    if on {
                        ui.painter().rect_filled(rect, 2.0, T.list_selected);
                    }
                    let y = rect.center().y;
                    draw_line(
                        ui.painter(),
                        Pos2::new(rect.min.x + 8.0, y),
                        Pos2::new(rect.max.x - 8.0, y),
                        st,
                        T.cell_text,
                    );
                    if resp.on_hover_text(st).clicked() {
                        self.border.style = st.to_string();
                        // Re-style edges already drawn, like picking a pen then re-applying.
                        for e in self.border.edges.iter_mut().flatten() {
                            if e != "none" {
                                *e = st.to_string();
                            }
                        }
                    }
                }
                ui.add_space(6.0);
                label(ui, "Color:");
                color_combo(ui, "fc-bcolor", &mut self.border.color, "Automatic");
            });
            ui.add_space(16.0);
            ui.vertical(|ui| {
                ui.label(RichText::new("Presets").strong());
                ui.horizontal(|ui| {
                    if ui.button("None").clicked() {
                        for e in &mut self.border.edges {
                            *e = Some("none".into());
                        }
                    }
                    if ui.button("Outline").clicked() {
                        for e in self.border.edges.iter_mut().take(4) {
                            *e = Some(self.border.style.clone());
                        }
                    }
                    let inside = ui.add_enabled(
                        self.multi_row || self.multi_col,
                        egui::Button::new("Inside"),
                    );
                    if inside.clicked() {
                        if self.multi_row
                            && let Some(e) = self.border.edges.get_mut(4)
                        {
                            *e = Some(self.border.style.clone());
                        }
                        if self.multi_col
                            && let Some(e) = self.border.edges.get_mut(5)
                        {
                            *e = Some(self.border.style.clone());
                        }
                    }
                });
                ui.add_space(6.0);
                ui.label(RichText::new("Border").strong());
                self.border_preview(ui);
                ui.horizontal_wrapped(|ui| {
                    ui.set_max_width(300.0);
                    for (i, (_, name)) in EDGES.iter().enumerate() {
                        let enabled = match i {
                            4 => self.multi_row,
                            5 => self.multi_col,
                            _ => true,
                        };
                        let Some(slot) = self.border.edges.get_mut(i) else {
                            continue;
                        };
                        let on = slot.as_deref().is_some_and(|s| s != "none");
                        if ui
                            .add_enabled(enabled, egui::Button::selectable(on, *name))
                            .clicked()
                        {
                            *slot = Some(if on {
                                "none".into()
                            } else {
                                self.border.style.clone()
                            });
                        }
                    }
                });
                label(
                    ui,
                    "Click a preset or an edge to apply the selected line style.",
                );
            });
        });
    }

    fn border_preview(&self, ui: &mut egui::Ui) {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(220.0, 110.0), egui::Sense::hover());
        let p = ui.painter();
        p.rect_filled(rect, 0.0, T.preview_bg);
        let r = rect.shrink(14.0);
        // Corner ticks.
        for c in [
            r.left_top(),
            r.right_top(),
            r.left_bottom(),
            r.right_bottom(),
        ] {
            p.circle_filled(c, 1.5, T.gridline);
        }
        let col = self
            .border
            .color
            .as_deref()
            .and_then(from_hex)
            .unwrap_or(T.border_auto);
        let mid = r.center();
        let edge = |i: usize| self.border.edges.get(i).cloned().flatten();
        let lines: [(usize, Pos2, Pos2); 6] = [
            (0, r.left_top(), r.right_top()),
            (1, r.left_bottom(), r.right_bottom()),
            (2, r.left_top(), r.left_bottom()),
            (3, r.right_top(), r.right_bottom()),
            (4, Pos2::new(r.min.x, mid.y), Pos2::new(r.max.x, mid.y)),
            (5, Pos2::new(mid.x, r.min.y), Pos2::new(mid.x, r.max.y)),
        ];
        for (i, a, b) in lines {
            if (i == 4 && !self.multi_row) || (i == 5 && !self.multi_col) {
                continue;
            }
            match edge(i) {
                Some(st) if st != "none" => draw_line(p, a, b, &st, col),
                _ => {
                    p.line_segment([a, b], Stroke::new(1.0, T.gridline));
                }
            }
        }
        let cells: Vec<Rect> = if self.multi_row || self.multi_col {
            let (cw, ch) = (
                if self.multi_col {
                    r.width() / 2.0
                } else {
                    r.width()
                },
                if self.multi_row {
                    r.height() / 2.0
                } else {
                    r.height()
                },
            );
            let mut v = Vec::new();
            for y in 0..(if self.multi_row { 2 } else { 1 }) {
                for x in 0..(if self.multi_col { 2 } else { 1 }) {
                    v.push(Rect::from_min_size(
                        r.min + Vec2::new(cw * x as f32, ch * y as f32),
                        Vec2::new(cw, ch),
                    ));
                }
            }
            v
        } else {
            vec![r]
        };
        for c in cells {
            p.text(
                c.center(),
                egui::Align2::CENTER_CENTER,
                "Text",
                egui::FontId::proportional(12.0),
                T.group_label,
            );
        }
    }

    fn fill_tab(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new("Background color").strong());
                if ui.button("No Color").clicked() {
                    self.v.fill = None;
                }
                ui.add_space(4.0);
                let cur = self.v.fill.as_deref().and_then(from_hex);
                if let Some(c) = swatch_grid(ui, cur) {
                    self.v.fill = Some(hex(c));
                }
            });
            ui.add_space(20.0);
            ui.vertical(|ui| {
                label(ui, "Sample");
                let (rect, _) =
                    ui.allocate_exact_size(Vec2::new(200.0, 70.0), egui::Sense::hover());
                let fill = self
                    .v
                    .fill
                    .as_deref()
                    .and_then(from_hex)
                    .unwrap_or(T.preview_bg);
                ui.painter().rect_filled(rect, 2.0, fill);
                ui.painter().rect_stroke(
                    rect,
                    2.0,
                    Stroke::new(1.0, T.list_border),
                    StrokeKind::Inside,
                );
            });
        });
    }

    fn protection_tab(&mut self, ui: &mut egui::Ui) {
        ui.checkbox(&mut self.v.locked, "Locked");
        ui.checkbox(&mut self.v.hidden, "Hidden");
        ui.add_space(8.0);
        label(
            ui,
            "Locking cells or hiding formulas has no effect until the worksheet is protected.",
        );
    }
}

fn code_list(ui: &mut egui::Ui, id: &str, codes: &[&str], value: &mut String) {
    label(ui, "Type:");
    let items: Vec<String> = codes.iter().map(|c| (*c).to_string()).collect();
    let sel = items.iter().position(|c| c == value);
    if let Some((i, _)) = list_box(ui, id, Vec2::new(330.0, 150.0), &items, sel)
        && let Some(c) = items.get(i)
    {
        value.clone_from(c);
    }
}

fn pick_combo(ui: &mut egui::Ui, id: &str, opts: &[(&str, &str)], value: &mut String, w: f32) {
    let cur = opts
        .iter()
        .find(|(k, _)| k == value)
        .map_or(value.as_str(), |(_, n)| n)
        .to_string();
    egui::ComboBox::new(id, "")
        .selected_text(cur)
        .width(w)
        .show_ui(ui, |ui| {
            for (k, n) in opts {
                if ui.selectable_label(value == k, *n).clicked() {
                    *value = (*k).to_string();
                }
            }
        });
}

/// Paint a border line sample in the given style.
fn draw_line(p: &egui::Painter, a: Pos2, b: Pos2, style: &str, c: egui::Color32) {
    match style {
        "medium" => {
            p.line_segment([a, b], Stroke::new(2.0, c));
        }
        "thick" => {
            p.line_segment([a, b], Stroke::new(3.0, c));
        }
        "dashed" => {
            p.extend(egui::Shape::dashed_line(
                &[a, b],
                Stroke::new(1.0, c),
                5.0,
                3.0,
            ));
        }
        "dotted" => {
            p.extend(egui::Shape::dashed_line(
                &[a, b],
                Stroke::new(1.0, c),
                1.5,
                2.0,
            ));
        }
        "double" => {
            let d = b - a;
            let n = Vec2::new(-d.y, d.x).normalized() * 1.5;
            p.line_segment([a + n, b + n], Stroke::new(1.0, c));
            p.line_segment([a - n, b - n], Stroke::new(1.0, c));
        }
        _ => {
            p.line_segment([a, b], Stroke::new(1.0, c));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_codes() {
        assert_eq!(classify("General"), NumCat::General);
        assert_eq!(classify("#,##0.00"), NumCat::Number);
        assert_eq!(classify("0%"), NumCat::Percentage);
        assert_eq!(classify("0.00E+00"), NumCat::Scientific);
        assert_eq!(classify("d-mmm-yy"), NumCat::Date);
        assert_eq!(classify("h:mm AM/PM"), NumCat::Time);
        assert_eq!(classify("\"$\"#,##0.00;(\"$\"#,##0.00)"), NumCat::Currency);
        assert_eq!(classify("@"), NumCat::Text);
        assert_eq!(classify("# ?/?"), NumCat::Fraction);
        assert_eq!(classify("[Red]0;0"), NumCat::Number);
        assert_eq!(classify("0 \"units\""), NumCat::Custom);
    }

    #[test]
    fn codes_from_category() {
        let mut v = FcValues {
            category: NumCat::Number,
            decimals: 2,
            thousands: true,
            ..FcValues::default()
        };
        assert_eq!(v.code(), "#,##0.00");
        v.category = NumCat::Percentage;
        v.decimals = 1;
        assert_eq!(v.code(), "0.0%");
        v.category = NumCat::Scientific;
        assert_eq!(v.code(), "0.0E+00");
    }

    #[test]
    fn unchanged_dialog_sends_nothing() {
        let s = Session::new();
        let fc = FormatCells::new(&s, FcTab::Number);
        assert_eq!(fc.params(), json!({}));
    }

    #[test]
    fn params_only_hold_changes() {
        let s = Session::new();
        let mut fc = FormatCells::new(&s, FcTab::Font);
        fc.v.bold = true;
        fc.v.fill = Some("FFFF00".into());
        fc.v.category = NumCat::Percentage;
        fc.v.decimals = 0;
        if let Some(e) = fc.border.edges.get_mut(0) {
            *e = Some("thick".into());
        }
        let p = fc.params();
        assert_eq!(
            p,
            json!({"bold": true, "fill": "FFFF00", "code": "0%", "border": {"top": "thick"}})
        );
    }
}
