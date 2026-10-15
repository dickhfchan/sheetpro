//! Spelling dialog (Review › Spelling, F7): steps through the misspellings that
//! `review.spelling` reports and runs its ignore / add / change actions.

use super::widgets::list_box;
use super::{Flow, error_line, label, window};
use crate::App;
use egui::{Context, RichText, Vec2};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sp_engine::Json;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Finding {
    pub cell: String,
    pub word: String,
    pub suggestions: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Spelling {
    /// Range being checked (sheet-qualified), or `None` for the whole sheet.
    pub range: Option<String>,
    pub findings: Vec<Finding>,
    pub change_to: String,
    pub selected: Option<usize>,
    pub error: Option<String>,
}

impl Spelling {
    /// Check the selection (or the whole sheet when one cell is selected).
    pub fn new(app: &mut App) -> Self {
        let s = &app.session;
        let sel = s.sel.range();
        let range = (sel.cell_count() > 1).then(|| {
            let name =
                s.wb.sheet(s.sheet_index())
                    .map(|x| x.name.replace('\'', "''"))
                    .unwrap_or_default();
            format!("'{name}'!{}", sel.to_a1())
        });
        let mut d = Spelling {
            range,
            ..Spelling::default()
        };
        d.refresh(app);
        d
    }

    fn scope(&self) -> Json {
        match &self.range {
            Some(r) => json!({ "range": r }),
            None => json!({}),
        }
    }

    /// Re-run the check and show the first remaining finding.
    pub fn refresh(&mut self, app: &mut App) {
        match app.query("review.spelling", self.scope()) {
            Ok(r) => {
                self.findings = r
                    .get("misspellings")
                    .cloned()
                    .and_then(|m| serde_json::from_value(m).ok())
                    .unwrap_or_default();
                self.error = None;
            }
            Err(e) => {
                self.findings.clear();
                self.error = Some(e);
            }
        }
        self.selected = None;
        self.change_to = self
            .findings
            .first()
            .and_then(|f| f.suggestions.first().cloned())
            .unwrap_or_default();
    }

    fn act(&mut self, app: &mut App, params: Json) {
        match app.try_exec("review.spelling", params) {
            Ok(_) => self.refresh(app),
            Err(e) => self.error = Some(e),
        }
    }

    pub fn ui(&mut self, ctx: &Context, app: &mut App) -> Flow {
        let current = self.findings.first().cloned();
        let user_words = sp_engine::settings::current().user_dictionary.len();
        let mut action: Option<&str> = None;
        let (close, esc) = window(ctx, "Spelling", 420.0, |ui| {
            match &current {
                None => {
                    ui.label("The spelling check is complete.");
                }
                Some(f) => {
                    label(ui, &format!("Not in Dictionary ({}):", f.cell));
                    ui.label(RichText::new(&f.word).strong().size(15.0));
                    ui.add_space(4.0);
                    label(ui, "Change to:");
                    ui.text_edit_singleline(&mut self.change_to);
                    label(ui, "Suggestions:");
                    if let Some((i, _)) = list_box(
                        ui,
                        "spelling-suggestions",
                        Vec2::new(260.0, 96.0),
                        &f.suggestions,
                        self.selected,
                    ) {
                        self.selected = Some(i);
                        if let Some(s) = f.suggestions.get(i) {
                            self.change_to = s.clone();
                        }
                    }
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        for (id, text) in [
                            ("ignore", "Ignore Once"),
                            ("ignore_all", "Ignore All"),
                            ("add", "Add to Dictionary"),
                            ("change", "Change"),
                            ("change_all", "Change All"),
                        ] {
                            if ui.button(text).clicked() {
                                action = Some(id);
                            }
                        }
                    });
                    label(
                        ui,
                        &format!(
                            "{} more to review · user dictionary: {user_words} words",
                            self.findings.len().saturating_sub(1)
                        ),
                    );
                }
            }
            error_line(ui, &self.error);
            ui.add_space(6.0);
            ui.button("Close").clicked()
        });
        if close || esc {
            return Flow::Close;
        }
        if let (Some(a), Some(f)) = (action, current) {
            let mut p = serde_json::Map::new();
            p.insert("action".into(), json!(a));
            p.insert("word".into(), json!(f.word));
            if a == "ignore" || a == "change" {
                let cell = match self.range.as_deref().and_then(|r| r.rsplit_once('!')) {
                    Some((sheet, _)) => format!("{sheet}!{}", f.cell),
                    None => f.cell.clone(),
                };
                p.insert("cell".into(), json!(cell));
            }
            if a == "change" || a == "change_all" {
                p.insert("to".into(), json!(self.change_to));
                if let Some(r) = &self.range {
                    p.insert("range".into(), json!(r));
                }
            }
            let p = Json::Object(p);
            self.act(app, p);
        }
        Flow::Keep
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_through_findings() {
        let mut s = sp_engine::Session::new();
        s.execute("cell.set", &json!({"cell": "A1", "input": "Teh qiuck fox"}))
            .unwrap();
        let mut app = App::new(s);
        let mut d = Spelling::new(&mut app);
        assert_eq!(d.findings.len(), 2);
        assert_eq!(d.findings[0].word, "Teh");
        assert_eq!(d.change_to, "The");
        d.act(
            &mut app,
            json!({"action": "change", "word": "Teh", "cell": "A1", "to": d.change_to.clone()}),
        );
        assert_eq!(d.findings.len(), 1);
        assert_eq!(d.change_to, "quick");
        d.act(&mut app, json!({"action": "ignore_all", "word": "qiuck"}));
        assert!(d.findings.is_empty());
        assert!(app.open_dialog("spelling").is_ok());
        assert_eq!(app.dialog_kind().as_deref(), Some("spelling"));
    }
}
