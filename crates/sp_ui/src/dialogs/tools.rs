//! Purpose-built dialogs for commands people use often: Data Validation, Goal Seek, New Note /
//! New Comment, Go To Special. Each builds the command's params from labelled controls.

use super::{Flow, error_line, label, ok_cancel, window};
use crate::App;
use crate::theme::LIGHT as T;
use egui::{Context, RichText};
use serde::{Deserialize, Serialize};
use serde_json::{Map, json};
use sp_engine::{Json, Session};
use sp_model::{CommentKind, ErrorStyle, ValidationKind};

fn combo(ui: &mut egui::Ui, id: &str, value: &mut String, options: &[(&str, &str)], width: f32) {
    let shown = options
        .iter()
        .find(|(k, _)| *k == value.as_str())
        .map_or(value.clone(), |(_, l)| (*l).to_string());
    egui::ComboBox::from_id_salt(id)
        .selected_text(shown)
        .width(width)
        .show_ui(ui, |ui| {
            for (k, l) in options {
                ui.selectable_value(value, (*k).to_string(), *l);
            }
        });
}

fn text_box(ui: &mut egui::Ui, value: &mut String, width: f32) -> egui::Response {
    ui.add(egui::TextEdit::singleline(value).desired_width(width))
}

// ---------------------------------------------------------------------------------------
// Data Validation

const ALLOW: [(&str, &str); 8] = [
    ("any", "Any value"),
    ("whole", "Whole number"),
    ("decimal", "Decimal"),
    ("list", "List"),
    ("date", "Date"),
    ("time", "Time"),
    ("textLength", "Text length"),
    ("custom", "Custom"),
];

const DATA: [(&str, &str); 8] = [
    ("between", "between"),
    ("notBetween", "not between"),
    ("equal", "equal to"),
    ("notEqual", "not equal to"),
    ("greaterThan", "greater than"),
    ("lessThan", "less than"),
    ("greaterThanOrEqual", "greater than or equal to"),
    ("lessThanOrEqual", "less than or equal to"),
];

const STYLES: [(&str, &str); 3] = [
    ("stop", "Stop"),
    ("warning", "Warning"),
    ("information", "Information"),
];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DvTab {
    #[default]
    Settings,
    Input,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DataValidation {
    pub range: String,
    pub tab: DvTab,
    pub kind: String,
    pub op: String,
    pub value1: String,
    pub value2: String,
    pub source: String,
    pub formula: String,
    pub allow_blank: bool,
    pub dropdown: bool,
    pub show_prompt: bool,
    pub prompt_title: String,
    pub prompt: String,
    pub show_error: bool,
    pub error_style: String,
    pub error_title: String,
    pub error: String,
    pub message: Option<String>,
}

impl DataValidation {
    /// Seeded from the rule at the active cell, if any; the range is the selection.
    pub fn new(s: &Session) -> Self {
        let mut d = DataValidation {
            range: s.sel.range().to_a1(),
            kind: "any".into(),
            op: "between".into(),
            allow_blank: true,
            dropdown: true,
            show_error: true,
            error_style: "stop".into(),
            ..Default::default()
        };
        let Some(r) =
            s.wb.sheet(s.sheet_index())
                .and_then(|sh| sh.validation_at(s.sel.active))
        else {
            return d;
        };
        d.kind = match r.kind {
            ValidationKind::Any => "any",
            k => k.as_str(),
        }
        .into();
        d.op = r.op.as_str().into();
        match r.kind {
            ValidationKind::List => {
                d.source = match r
                    .formula1
                    .strip_prefix('"')
                    .and_then(|f| f.strip_suffix('"'))
                {
                    Some(items) => items.to_string(),
                    None => format!("={}", r.formula1),
                };
            }
            ValidationKind::Custom => d.formula = format!("={}", r.formula1),
            _ => {
                d.value1 = r.formula1.clone();
                d.value2 = r.formula2.clone();
            }
        }
        d.allow_blank = r.allow_blank;
        d.dropdown = r.dropdown;
        d.show_prompt = r.show_prompt;
        d.prompt_title = r.prompt_title.clone();
        d.prompt = r.prompt.clone();
        d.show_error = r.show_error;
        d.error_style = match r.error_style {
            ErrorStyle::Stop => "stop",
            ErrorStyle::Warning => "warning",
            ErrorStyle::Information => "information",
        }
        .into();
        d.error_title = r.error_title.clone();
        d.error = r.error.clone();
        d
    }

    fn two_values(&self) -> bool {
        matches!(self.op.as_str(), "between" | "notBetween")
    }

    fn compares(&self) -> bool {
        matches!(
            self.kind.as_str(),
            "whole" | "decimal" | "date" | "time" | "textLength"
        )
    }

    /// The `data.validation` params for the dialog's state.
    pub fn params(&self) -> Json {
        if self.kind == "any" {
            return json!({ "range": self.range, "clear": true });
        }
        let mut m = Map::new();
        m.insert("range".into(), json!(self.range));
        m.insert("type".into(), json!(self.kind));
        if self.compares() {
            m.insert("operator".into(), json!(self.op));
            m.insert("min".into(), json!(self.value1.trim()));
            if self.two_values() {
                m.insert("max".into(), json!(self.value2.trim()));
            }
        }
        match self.kind.as_str() {
            "list" => {
                m.insert("source".into(), json!(self.source.trim()));
                m.insert("dropdown".into(), json!(self.dropdown));
            }
            "custom" => {
                m.insert("formula".into(), json!(self.formula.trim()));
            }
            _ => {}
        }
        m.insert("allow_blank".into(), json!(self.allow_blank));
        if self.show_prompt {
            m.insert("prompt_title".into(), json!(self.prompt_title));
            m.insert("prompt".into(), json!(self.prompt));
        }
        m.insert("show_error".into(), json!(self.show_error));
        m.insert("error_style".into(), json!(self.error_style));
        m.insert("error_title".into(), json!(self.error_title));
        m.insert("error".into(), json!(self.error));
        Json::Object(m)
    }

    pub fn ui(&mut self, ctx: &Context, app: &mut App) -> Flow {
        let mut clear_all = false;
        let ((ok, cancel), esc) = window(ctx, "Data Validation", 420.0, |ui| {
            ui.horizontal(|ui| {
                for (t, name) in [
                    (DvTab::Settings, "Settings"),
                    (DvTab::Input, "Input Message"),
                    (DvTab::Error, "Error Alert"),
                ] {
                    if ui.selectable_label(self.tab == t, name).clicked() {
                        self.tab = t;
                    }
                }
            });
            ui.separator();
            egui::Grid::new("dv")
                .num_columns(2)
                .spacing([10.0, 6.0])
                .show(ui, |ui| match self.tab {
                    DvTab::Settings => {
                        label(ui, "Apply to:");
                        text_box(ui, &mut self.range, 220.0);
                        ui.end_row();
                        label(ui, "Allow:");
                        combo(ui, "dv-allow", &mut self.kind, &ALLOW, 220.0);
                        ui.end_row();
                        if self.compares() {
                            label(ui, "Data:");
                            combo(ui, "dv-data", &mut self.op, &DATA, 220.0);
                            ui.end_row();
                            let (a, b) = match self.op.as_str() {
                                "between" | "notBetween" => ("Minimum:", Some("Maximum:")),
                                "greaterThan" | "greaterThanOrEqual" => ("Minimum:", None),
                                "lessThan" | "lessThanOrEqual" => ("Maximum:", None),
                                _ => ("Value:", None),
                            };
                            label(ui, a);
                            text_box(ui, &mut self.value1, 220.0);
                            ui.end_row();
                            if let Some(b) = b {
                                label(ui, b);
                                text_box(ui, &mut self.value2, 220.0);
                                ui.end_row();
                            }
                        }
                        if self.kind == "list" {
                            label(ui, "Source:");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.source)
                                    .hint_text("Yes,No   or   =$D$1:$D$5")
                                    .desired_width(220.0),
                            );
                            ui.end_row();
                            label(ui, "");
                            ui.checkbox(&mut self.dropdown, "In-cell dropdown");
                            ui.end_row();
                        }
                        if self.kind == "custom" {
                            label(ui, "Formula:");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.formula)
                                    .hint_text("=A1>0")
                                    .desired_width(220.0),
                            );
                            ui.end_row();
                        }
                        if self.kind != "any" {
                            label(ui, "");
                            ui.checkbox(&mut self.allow_blank, "Ignore blank");
                            ui.end_row();
                        }
                    }
                    DvTab::Input => {
                        label(ui, "");
                        ui.checkbox(
                            &mut self.show_prompt,
                            "Show input message when cell is selected",
                        );
                        ui.end_row();
                        label(ui, "Title:");
                        text_box(ui, &mut self.prompt_title, 240.0);
                        ui.end_row();
                        label(ui, "Input message:");
                        ui.add(
                            egui::TextEdit::multiline(&mut self.prompt)
                                .desired_rows(3)
                                .desired_width(240.0),
                        );
                        ui.end_row();
                    }
                    DvTab::Error => {
                        label(ui, "");
                        ui.checkbox(
                            &mut self.show_error,
                            "Show error alert after invalid data is entered",
                        );
                        ui.end_row();
                        label(ui, "Style:");
                        combo(ui, "dv-style", &mut self.error_style, &STYLES, 240.0);
                        ui.end_row();
                        label(ui, "Title:");
                        text_box(ui, &mut self.error_title, 240.0);
                        ui.end_row();
                        label(ui, "Error message:");
                        ui.add(
                            egui::TextEdit::multiline(&mut self.error)
                                .desired_rows(3)
                                .desired_width(240.0),
                        );
                        ui.end_row();
                    }
                });
            error_line(ui, &self.message);
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.button("Clear All").clicked() {
                    clear_all = true;
                }
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
        let run = if clear_all {
            Some(json!({ "range": self.range, "clear": true }))
        } else if ok {
            Some(self.params())
        } else {
            None
        };
        if let Some(p) = run {
            match app.try_exec("data.validation", p) {
                Ok(_) => return Flow::Close,
                Err(e) => self.message = Some(e),
            }
        }
        Flow::Keep
    }
}

// ---------------------------------------------------------------------------------------
// Goal Seek

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct GoalSeek {
    pub set_cell: String,
    pub to_value: String,
    pub by_changing: String,
    /// After a run: the status shown (OK keeps the change, Cancel undoes it).
    pub status: Option<String>,
    pub error: Option<String>,
}

impl GoalSeek {
    pub fn new(s: &Session) -> Self {
        GoalSeek {
            set_cell: s.sel.active.to_a1(),
            ..Default::default()
        }
    }

    pub fn params(&self) -> Result<Json, String> {
        let to: f64 = self
            .to_value
            .trim()
            .parse()
            .map_err(|_| "To value must be a number.".to_string())?;
        if self.set_cell.trim().is_empty() || self.by_changing.trim().is_empty() {
            return Err("Give the cell to set and the cell to change.".into());
        }
        Ok(json!({
            "set_cell": self.set_cell.trim(),
            "to_value": to,
            "by_changing": self.by_changing.trim(),
        }))
    }

    pub fn ui(&mut self, ctx: &Context, app: &mut App) -> Flow {
        if let Some(status) = self.status.clone() {
            let ((ok, cancel), esc) = window(ctx, "Goal Seek Status", 320.0, |ui| {
                ui.label(RichText::new(status).size(12.5));
                ok_cancel(ui, "OK")
            });
            if cancel || esc {
                // Cancel puts the changing cell back.
                let _ = app.try_exec("edit.undo", json!({}));
                return Flow::Close;
            }
            return if ok { Flow::Close } else { Flow::Keep };
        }
        let ((ok, cancel), esc) = window(ctx, "Goal Seek", 300.0, |ui| {
            egui::Grid::new("gs")
                .num_columns(2)
                .spacing([10.0, 6.0])
                .show(ui, |ui| {
                    label(ui, "Set cell:");
                    text_box(ui, &mut self.set_cell, 150.0);
                    ui.end_row();
                    label(ui, "To value:");
                    text_box(ui, &mut self.to_value, 150.0);
                    ui.end_row();
                    label(ui, "By changing cell:");
                    text_box(ui, &mut self.by_changing, 150.0);
                    ui.end_row();
                });
            error_line(ui, &self.error);
            ok_cancel(ui, "OK")
        });
        if cancel || esc {
            return Flow::Close;
        }
        if ok {
            match self
                .params()
                .and_then(|p| app.try_exec("data.what_if.goal_seek", p))
            {
                Ok(r) => {
                    let found = r.get("found").and_then(Json::as_bool).unwrap_or(false);
                    let current = r
                        .get("result")
                        .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_string))
                        .unwrap_or_default();
                    self.status = Some(format!(
                        "Goal Seeking with Cell {} {}.\n\nTarget value: {}\nCurrent value: {}",
                        self.set_cell.trim().to_ascii_uppercase(),
                        if found {
                            "found a solution"
                        } else {
                            "may not have found a solution"
                        },
                        self.to_value.trim(),
                        current
                    ));
                    self.error = None;
                }
                Err(e) => self.error = Some(e),
            }
        }
        Flow::Keep
    }
}

// ---------------------------------------------------------------------------------------
// New Note / New Comment

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct NewComment {
    /// A note (`true`) or a threaded comment.
    pub note: bool,
    pub cell: String,
    pub title: String,
    pub text: String,
    pub author: String,
    pub error: Option<String>,
}

impl NewComment {
    pub fn new(s: &Session, note: bool) -> Self {
        let at = s.sel.active;
        let existing =
            s.wb.sheet(s.sheet_index())
                .and_then(|sh| sh.comment_at(at))
                .cloned();
        let (title, text) = match (&existing, note) {
            (Some(c), true) if c.kind == CommentKind::Note => ("Edit Note", c.text.clone()),
            (Some(c), false) if c.kind == CommentKind::Thread => ("Reply", String::new()),
            (_, true) => ("New Note", String::new()),
            (_, false) => ("New Comment", String::new()),
        };
        NewComment {
            note,
            cell: at.to_a1(),
            title: title.into(),
            text,
            ..Default::default()
        }
    }

    pub fn params(&self) -> Result<(&'static str, Json), String> {
        if self.text.trim().is_empty() {
            return Err("Type the text first.".into());
        }
        let mut p = json!({ "cell": self.cell, "text": self.text });
        if !self.author.trim().is_empty()
            && let Some(m) = p.as_object_mut()
        {
            m.insert("author".into(), json!(self.author.trim()));
        }
        Ok((
            if self.note {
                "insert.note"
            } else {
                "insert.comment"
            },
            p,
        ))
    }

    pub fn ui(&mut self, ctx: &Context, app: &mut App) -> Flow {
        let ((ok, cancel), esc) = window(ctx, &self.title.clone(), 340.0, |ui| {
            ui.label(RichText::new(format!("Cell {}", self.cell)).color(T.group_label));
            ui.add(
                egui::TextEdit::multiline(&mut self.text)
                    .desired_rows(5)
                    .desired_width(320.0),
            );
            if !self.note {
                ui.horizontal(|ui| {
                    label(ui, "Author:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.author)
                            .hint_text("(default)")
                            .desired_width(200.0),
                    );
                });
            }
            error_line(ui, &self.error);
            ok_cancel(ui, if self.note { "Save" } else { "Post" })
        });
        if cancel || esc {
            return Flow::Close;
        }
        if ok {
            match self.params().and_then(|(id, p)| app.try_exec(id, p)) {
                Ok(_) => return Flow::Close,
                Err(e) => self.error = Some(e),
            }
        }
        Flow::Keep
    }
}

// ---------------------------------------------------------------------------------------
// Go To Special

const SPECIAL: [(&str, &str); 10] = [
    ("notes", "Notes"),
    ("comments", "Comments"),
    ("constants", "Constants"),
    ("formulas", "Formulas"),
    ("blanks", "Blanks"),
    ("errors", "Errors"),
    ("current_region", "Current region"),
    ("last_cell", "Last cell"),
    ("visible", "Visible cells only"),
    ("data_validation", "Data validation"),
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GoToSpecial {
    pub kind: String,
    pub numbers: bool,
    pub text: bool,
    pub logicals: bool,
    pub errors: bool,
    pub error: Option<String>,
}

impl Default for GoToSpecial {
    fn default() -> Self {
        GoToSpecial {
            kind: "constants".into(),
            numbers: true,
            text: true,
            logicals: true,
            errors: true,
            error: None,
        }
    }
}

impl GoToSpecial {
    pub fn params(&self) -> Json {
        let mut p = json!({ "kind": self.kind });
        if matches!(self.kind.as_str(), "constants" | "formulas") {
            let types: Vec<&str> = [
                (self.numbers, "numbers"),
                (self.text, "text"),
                (self.logicals, "logicals"),
                (self.errors, "errors"),
            ]
            .iter()
            .filter(|(on, _)| *on)
            .map(|(_, t)| *t)
            .collect();
            if types.len() < 4
                && let Some(m) = p.as_object_mut()
            {
                m.insert("types".into(), json!(types));
            }
        }
        p
    }

    pub fn ui(&mut self, ctx: &Context, app: &mut App) -> Flow {
        let ((ok, cancel), esc) = window(ctx, "Go To Special", 320.0, |ui| {
            label(ui, "Select");
            for (k, l) in SPECIAL {
                ui.radio_value(&mut self.kind, k.to_string(), l);
                if matches!(k, "formulas") && matches!(self.kind.as_str(), "constants" | "formulas")
                {
                    ui.horizontal(|ui| {
                        ui.add_space(22.0);
                        ui.checkbox(&mut self.numbers, "Numbers");
                        ui.checkbox(&mut self.text, "Text");
                        ui.checkbox(&mut self.logicals, "Logicals");
                        ui.checkbox(&mut self.errors, "Errors");
                    });
                }
            }
            error_line(ui, &self.error);
            ok_cancel(ui, "OK")
        });
        if cancel || esc {
            return Flow::Close;
        }
        if ok {
            match app.try_exec("home.editing.goto_special", self.params()) {
                Ok(_) => return Flow::Close,
                Err(e) => self.error = Some(e),
            }
        }
        Flow::Keep
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use sp_core::{CellAddr, CellRange};

    fn session() -> Session {
        let mut s = Session::new();
        for (c, v) in [("A1", "1"), ("A2", "=A1*2"), ("A3", "x")] {
            s.execute("cell.set", &json!({"cell": c, "input": v}))
                .unwrap();
        }
        s
    }

    #[test]
    fn data_validation_round_trips_through_the_command() {
        let mut s = session();
        s.select_range(CellRange::parse("B1:B5").unwrap(), CellAddr::new(0, 1));
        let mut d = DataValidation::new(&s);
        assert_eq!((d.range.as_str(), d.kind.as_str()), ("B1:B5", "any"));
        d.kind = "whole".into();
        d.op = "between".into();
        d.value1 = "1".into();
        d.value2 = "10".into();
        d.show_prompt = true;
        d.prompt_title = "Qty".into();
        d.prompt = "1 to 10".into();
        d.error_style = "warning".into();
        d.error = "Out of range".into();
        s.execute("data.validation", &d.params()).unwrap();
        // Reopening shows the rule as set.
        let e = DataValidation::new(&s);
        assert_eq!(
            (
                e.kind.as_str(),
                e.op.as_str(),
                e.value1.as_str(),
                e.value2.as_str()
            ),
            ("whole", "between", "1", "10")
        );
        assert!(e.show_prompt && e.prompt == "1 to 10" && e.prompt_title == "Qty");
        assert_eq!(
            (e.error_style.as_str(), e.error.as_str()),
            ("warning", "Out of range")
        );
        // A list, then Clear All via "Any value".
        let mut l = DataValidation::new(&s);
        l.kind = "list".into();
        l.source = "Yes,No".into();
        s.execute("data.validation", &l.params()).unwrap();
        assert_eq!(DataValidation::new(&s).source, "Yes,No");
        let mut c = DataValidation::new(&s);
        c.kind = "any".into();
        s.execute("data.validation", &c.params()).unwrap();
        assert_eq!(DataValidation::new(&s).kind, "any");
    }

    #[test]
    fn goal_seek_note_and_goto_special_params() {
        let mut s = session();
        let mut g = GoalSeek::new(&s);
        assert_eq!(g.set_cell, "A1");
        g.set_cell = "A2".into();
        g.to_value = "abc".into();
        assert!(g.params().is_err());
        g.to_value = "30".into();
        g.by_changing = "A1".into();
        let r = s
            .execute("data.what_if.goal_seek", &g.params().unwrap())
            .unwrap();
        assert_eq!(r["found"], json!(true));

        s.select_range(CellRange::parse("C3").unwrap(), CellAddr::new(2, 2));
        let mut n = NewComment::new(&s, true);
        assert_eq!(n.title, "New Note");
        assert!(n.params().is_err());
        n.text = "remember".into();
        let (id, p) = n.params().unwrap();
        s.execute(id, &p).unwrap();
        let again = NewComment::new(&s, true);
        assert_eq!(
            (again.title.as_str(), again.text.as_str()),
            ("Edit Note", "remember")
        );

        let mut gs = GoToSpecial::default();
        assert_eq!(gs.params(), json!({"kind": "constants"}));
        gs.kind = "formulas".into();
        gs.text = false;
        assert_eq!(
            gs.params(),
            json!({"kind": "formulas", "types": ["numbers", "logicals", "errors"]})
        );
    }
}
