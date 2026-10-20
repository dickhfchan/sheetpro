//! The generic dialog for a command without a dedicated one: a form generated from the
//! command's params doc (`{"cell"?: "B2", "type": "whole|decimal", "unique"?: bool}`):
//! text boxes with the documented example as a hint, drop-downs for `a|b|c` choices,
//! check boxes for booleans, cell/range/sheet fields filled from the selection.

use super::{Flow, error_line, label, ok_cancel, window};
use crate::App;
use crate::theme::LIGHT as T;
use egui::{Context, RichText};
use serde::{Deserialize, Serialize};
use sp_engine::{Json, Session, registry};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    #[default]
    Text,
    Number,
    Bool,
    Choice,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Field {
    pub key: String,
    pub required: bool,
    pub kind: Kind,
    /// Documented example, shown as the hint of an empty box.
    pub hint: String,
    pub choices: Vec<String>,
    pub value: String,
    pub checked: bool,
}

/// A documented choice list: `whole|decimal|notBetween` (identifiers only, so example text
/// such as `RRGGBB|none` or `Heading 1..4|Title` stays a text box).
fn choices(s: &str) -> Option<Vec<String>> {
    let alts: Vec<String> = s.split('|').map(|a| a.trim().to_string()).collect();
    let ident = |a: &str| {
        a.chars().next().is_some_and(|c| c.is_ascii_lowercase())
            && a.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    (alts.len() > 1 && alts.iter().all(|a| ident(a))).then_some(alts)
}

/// First double-quoted string in `s`.
fn quoted(s: &str) -> Option<&str> {
    let a = s.find('"')?;
    let b = s.get(a + 1..)?.find('"')?;
    s.get(a + 1..a + 1 + b)
}

/// Split the inside of `{…}` at commas that are not in quotes or brackets.
fn split_top(inner: &str) -> Vec<String> {
    let (mut out, mut cur, mut depth, mut q) = (Vec::new(), String::new(), 0i32, false);
    for c in inner.chars() {
        match c {
            '"' => q = !q,
            '[' | '{' | '(' if !q => depth += 1,
            ']' | '}' | ')' if !q => depth -= 1,
            ',' if !q && depth == 0 => {
                out.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

/// `"key"?: value` → (key, optional, value text). `None` when it isn't a key.
fn key_value(seg: &str) -> Option<(String, bool, String)> {
    let seg = seg.trim();
    let rest = seg.strip_prefix('"')?;
    let end = rest.find('"')?;
    let key = rest.get(..end)?.to_string();
    let mut tail = rest.get(end + 1..)?.trim_start();
    let optional = tail.starts_with('?');
    if optional {
        tail = tail.get(1..)?.trim_start();
    }
    let value = tail.strip_prefix(':').unwrap_or(tail).trim().to_string();
    (!key.is_empty()).then_some((key, !optional, value))
}

fn field(key: String, required: bool, value: &str) -> Field {
    let v = value.trim();
    let mut f = Field {
        key,
        required,
        ..Field::default()
    };
    if v.starts_with("bool") || v.starts_with("true") || v.starts_with("false") {
        f.kind = Kind::Bool;
        f.checked = v.starts_with("true");
    } else if let Some(q) = v.starts_with('"').then(|| quoted(v)).flatten() {
        match choices(q) {
            Some(c) => {
                f.kind = Kind::Choice;
                f.choices = c;
                if required {
                    f.value = f.choices.first().cloned().unwrap_or_default();
                }
            }
            None => f.hint = q.to_string(),
        }
    } else if v.starts_with(|c: char| c.is_ascii_digit() || c == '-') {
        f.kind = Kind::Number;
        f.hint = v.split_whitespace().next().unwrap_or("").to_string();
    } else {
        f.hint = v.to_string();
    }
    f
}

/// The fields a params doc describes, in order.
pub fn parse_doc(doc: &str) -> Vec<Field> {
    let Some(open) = doc.find('{') else {
        return Vec::new();
    };
    // The matching close brace (quotes and nested brackets respected).
    let (mut depth, mut q, mut close) = (0i32, false, None);
    for (i, c) in doc.char_indices().skip_while(|(i, _)| *i < open) {
        match c {
            '"' => q = !q,
            '{' | '[' if !q => depth += 1,
            '}' | ']' if !q => {
                depth -= 1;
                if depth == 0 {
                    close = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let Some(inner) = close.and_then(|c| doc.get(open + 1..c)) else {
        return Vec::new();
    };
    let mut out: Vec<Field> = Vec::new();
    for seg in split_top(inner) {
        // `"cell"?: "A1" | "range"?: "A1:B2"`: alternatives, each its own field.
        let mut parts = Vec::new();
        let mut rest = seg.as_str();
        while let Some(i) = rest.find("| \"") {
            let (a, b) = rest.split_at(i);
            if key_value(b.trim_start_matches('|')).is_some_and(|(_, _, v)| !v.is_empty()) {
                parts.push(a.to_string());
                rest = b.trim_start_matches('|');
            } else {
                break;
            }
        }
        parts.push(rest.to_string());
        let alternatives = parts.len() > 1;
        for p in parts {
            if let Some((k, req, v)) = key_value(&p)
                && !out.iter().any(|f| f.key == k)
            {
                out.push(field(k, req && !alternatives, &v));
            }
        }
    }
    out
}

/// "set_cell" → "Set cell".
fn pretty(key: &str) -> String {
    let s = key.replace('_', " ");
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Params {
    pub cmd: String,
    pub label: String,
    /// Text after the `{…}` in the doc (what the command returns, caveats).
    pub note: String,
    pub fields: Vec<Field>,
    pub error: Option<String>,
    /// What the command reported, for commands whose doc promises a result (`→ …`).
    pub result: Option<String>,
    #[serde(skip)]
    pub shows_result: bool,
}

impl Params {
    pub fn new(id: &str, s: &Session) -> Result<Self, String> {
        let c = registry()
            .get(id)
            .ok_or_else(|| format!("unknown command '{id}'"))?;
        let mut fields = parse_doc(c.params);
        // The selection fills cell/range/sheet (a single cell prefers "cell").
        let sel = s.sel.range();
        let single = sel.start == sel.end;
        let has_cell = fields.iter().any(|f| f.key == "cell");
        let sheet =
            s.wb.sheet(s.sheet_index())
                .map(|sh| sh.name.clone())
                .unwrap_or_default();
        for f in &mut fields {
            match f.key.as_str() {
                "cell" if single || !fields_has_range(c.params) => f.value = s.sel.active.to_a1(),
                "range" if !single || !has_cell => f.value = sel.to_a1(),
                "sheet" => f.value = sheet.clone(),
                _ => {}
            }
        }
        // A parenthesised remark after the `{…}` is for the user ("(adds a reply when the
        // cell has a thread)"); return-value docs (`→ …`) and notes for scripts are not.
        let tail = c
            .params
            .rfind('}')
            .and_then(|i| c.params.get(i + 1..))
            .unwrap_or("")
            .trim();
        let note = tail
            .strip_prefix('(')
            .and_then(|t| t.strip_suffix(')'))
            .map(|t| {
                let mut c = t.chars();
                c.next()
                    .map(|f| f.to_uppercase().collect::<String>() + c.as_str() + ".")
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        Ok(Params {
            cmd: id.to_string(),
            label: c.label.clone(),
            note,
            fields,
            error: None,
            result: None,
            shows_result: c.params.contains('→'),
        })
    }

    /// Fill fields from a JSON object (e.g. the sheet tab menu's Move or Copy).
    pub fn set(&mut self, values: &Json) {
        let Some(m) = values.as_object() else {
            return;
        };
        for f in &mut self.fields {
            if let Some(v) = m.get(&f.key) {
                match (f.kind, v) {
                    (Kind::Bool, Json::Bool(b)) => f.checked = *b,
                    (_, Json::String(s)) => f.value = s.clone(),
                    (_, other) => f.value = other.to_string(),
                }
            }
        }
    }

    /// The command's params from the form; `Err` names a missing required field.
    pub fn params(&self) -> Result<Json, String> {
        let mut m = serde_json::Map::new();
        for f in &self.fields {
            let v = f.value.trim();
            let value = match f.kind {
                Kind::Bool => Some(Json::Bool(f.checked)),
                _ if v.is_empty() => None,
                Kind::Number => Some(
                    v.parse::<f64>()
                        .ok()
                        .and_then(|n| serde_json::Number::from_f64(n).map(Json::Number))
                        .unwrap_or_else(|| Json::String(v.to_string())),
                ),
                Kind::Choice => Some(Json::String(v.to_string())),
                Kind::Text => Some(if v.starts_with('[') || v.starts_with('{') {
                    serde_json::from_str(v).unwrap_or_else(|_| Json::String(v.to_string()))
                } else {
                    Json::String(v.to_string())
                }),
            };
            match value {
                Some(x) => {
                    // An unticked optional check box means "leave it to the default".
                    if !(f.kind == Kind::Bool && !f.required && !f.checked) {
                        m.insert(f.key.clone(), x);
                    }
                }
                None if f.required => return Err(format!("Fill in {}.", pretty(&f.key))),
                None => {}
            }
        }
        Ok(Json::Object(m))
    }

    pub fn ui(&mut self, ctx: &Context, app: &mut App) -> Flow {
        let ((ok, cancel), esc) = window(ctx, &self.label.clone(), 420.0, |ui| {
            egui::Grid::new("params-form")
                .num_columns(2)
                .spacing([10.0, 6.0])
                .show(ui, |ui| {
                    for f in &mut self.fields {
                        let name = if f.required {
                            format!("{}*", pretty(&f.key))
                        } else {
                            pretty(&f.key)
                        };
                        label(ui, &name);
                        match f.kind {
                            Kind::Bool => {
                                ui.checkbox(&mut f.checked, "");
                            }
                            Kind::Choice => {
                                let shown = if f.value.is_empty() {
                                    "(default)".to_string()
                                } else {
                                    f.value.clone()
                                };
                                egui::ComboBox::from_id_salt(("param", &f.key))
                                    .selected_text(shown)
                                    .width(240.0)
                                    .show_ui(ui, |ui| {
                                        if !f.required {
                                            ui.selectable_value(
                                                &mut f.value,
                                                String::new(),
                                                "(default)",
                                            );
                                        }
                                        for c in f.choices.clone() {
                                            ui.selectable_value(&mut f.value, c.clone(), c);
                                        }
                                    });
                            }
                            Kind::Text | Kind::Number => {
                                ui.add(
                                    egui::TextEdit::singleline(&mut f.value)
                                        .hint_text(&f.hint)
                                        .desired_width(240.0),
                                );
                            }
                        }
                        ui.end_row();
                    }
                });
            if let Some(r) = &self.result {
                egui::ScrollArea::vertical()
                    .max_height(220.0)
                    .show(ui, |ui| {
                        ui.label(RichText::new(r).monospace().size(11.5));
                    });
            }
            if !self.note.is_empty() {
                ui.label(RichText::new(&self.note).size(11.0).color(T.group_label));
            }
            error_line(ui, &self.error);
            ok_cancel(ui, "OK")
        });
        if cancel || esc {
            return Flow::Close;
        }
        if ok {
            match self.params() {
                Ok(p) => match app.try_exec(&self.cmd, p) {
                    // Text for the formula being typed (Use in Formula).
                    Ok(v) if v.get("insert").is_some_and(Json::is_string) => {
                        let text = v.get("insert").and_then(Json::as_str).unwrap_or("");
                        insert_text(app, text);
                        return Flow::Close;
                    }
                    // A result to read (Evaluate Formula, Flash Fill): stay open and show it.
                    Ok(v) if self.shows_result && !v.is_null() => {
                        self.error = None;
                        self.result = Some(describe(&v, 0));
                    }
                    Ok(_) => return Flow::Close,
                    Err(e) => self.error = Some(e),
                },
                Err(e) => self.error = Some(e),
            }
        }
        Flow::Keep
    }
}

/// Insert `text` at the end of the formula being typed, or start one (`=Sales`).
fn insert_text(app: &mut App, text: &str) {
    let editing = app.editing_cell.is_some();
    let t = if editing && app.edit_buf.starts_with('=') {
        format!("{}{text}", app.edit_buf)
    } else {
        format!("={text}")
    };
    app.begin_edit(Some(t));
}

/// Keys in reading order: the ones whose order carries meaning first (a formula, the part
/// being evaluated, its result), the rest alphabetically (JSON objects here are sorted).
fn ordered(m: &serde_json::Map<String, Json>) -> impl Iterator<Item = (&String, &Json)> {
    const FIRST: [&str; 6] = [
        "formula", "evaluate", "steps", "result", "filled", "program",
    ];
    let mut v: Vec<(&String, &Json)> = m.iter().collect();
    v.sort_by_key(|(k, _)| FIRST.iter().position(|f| f == k).unwrap_or(FIRST.len()));
    v.into_iter()
}

/// A command result as readable lines: `key: value`, lists one item per line.
pub fn describe(v: &Json, depth: usize) -> String {
    let pad = "  ".repeat(depth);
    let scalar = |v: &Json| match v {
        Json::String(s) => s.clone(),
        Json::Null => "—".into(),
        other => other.to_string(),
    };
    let mut out = String::new();
    match v {
        Json::Object(m) => {
            for (k, x) in ordered(m) {
                if x.is_object() || x.is_array() {
                    out.push_str(&format!("{pad}{}:\n{}", pretty(k), describe(x, depth + 1)));
                } else {
                    out.push_str(&format!("{pad}{}: {}\n", pretty(k), scalar(x)));
                }
            }
        }
        Json::Array(a) => {
            for x in a.iter().take(200) {
                if let Some(m) = x.as_object() {
                    let parts: Vec<String> = ordered(m).map(|(_, v)| scalar(v)).collect();
                    out.push_str(&format!("{pad}{}\n", parts.join("  →  ")));
                } else {
                    out.push_str(&format!("{pad}{}\n", scalar(x)));
                }
            }
        }
        other => out.push_str(&format!("{pad}{}\n", scalar(other))),
    }
    out
}

fn fields_has_range(doc: &str) -> bool {
    doc.contains("\"range\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn keys(doc: &str) -> Vec<(String, bool, Kind)> {
        parse_doc(doc)
            .into_iter()
            .map(|f| (f.key, f.required, f.kind))
            .collect()
    }

    #[test]
    fn docs_become_fields() {
        assert_eq!(
            keys(r#"{"cell"?: "B2", "text": "…", "author"?: "Name"} (adds a reply)"#),
            vec![
                ("cell".into(), false, Kind::Text),
                ("text".into(), true, Kind::Text),
                ("author".into(), false, Kind::Text),
            ]
        );
        let f = parse_doc(
            r#"{"range"?: "A1:A9", "type": "whole|decimal|list", "min"?: 1, "source"?: "a,b,c" | ["a","b"], "allow_blank"?: true, "clear"?: bool}"#,
        );
        assert_eq!(f[1].kind, Kind::Choice);
        assert_eq!(f[1].choices, vec!["whole", "decimal", "list"]);
        assert_eq!(f[1].value, "whole");
        assert_eq!(f[2].kind, Kind::Number);
        assert_eq!(f[3].kind, Kind::Text);
        assert!(f[4].kind == Kind::Bool && f[4].checked);
        assert!(f[5].kind == Kind::Bool && !f[5].checked);
        // Alternatives are separate optional fields.
        assert_eq!(
            keys(r#"{"cell"?: "A1" | "range"?: "A1:B2", "address"?: "https://…"}"#),
            vec![
                ("cell".into(), false, Kind::Text),
                ("range".into(), false, Kind::Text),
                ("address".into(), false, Kind::Text),
            ]
        );
        // Bare keys and free-text examples.
        assert_eq!(
            keys(r#"{"path": "/p.png" (PNG), "width"?, "height"?: px (default: size), "rect"?}"#),
            vec![
                ("path".into(), true, Kind::Text),
                ("width".into(), false, Kind::Text),
                ("height".into(), false, Kind::Text),
                ("rect".into(), false, Kind::Text),
            ]
        );
        // `RRGGBB|none` is an example, not a choice list.
        assert_eq!(parse_doc(r#"{"line"?: "RRGGBB|none"}"#)[0].kind, Kind::Text);
        assert!(parse_doc("{}").is_empty());
        assert!(parse_doc("no braces").is_empty());
    }

    #[test]
    fn every_command_doc_parses_into_named_fields() {
        for c in registry().all() {
            for f in parse_doc(c.params) {
                assert!(
                    !f.key.is_empty() && !f.key.contains(' '),
                    "{}: bad key {:?}",
                    c.id,
                    f.key
                );
            }
        }
    }

    #[test]
    fn results_read_as_lines() {
        let v =
            json!({"steps": [{"formula": "=A1+1", "evaluate": "A1", "result": 2}], "result": 2});
        assert_eq!(describe(&v, 0), "Steps:\n  =A1+1  →  A1  →  2\nResult: 2\n");
        assert_eq!(describe(&json!({"filled": 3}), 0), "Filled: 3\n");
    }

    #[test]
    fn form_builds_params_and_requires_fields() {
        let mut s = Session::new();
        s.select_range(
            sp_core::CellRange::parse("B2:C4").unwrap(),
            sp_core::CellAddr::new(1, 1),
        );
        let mut p = Params::new("data.what_if.goal_seek", &s).unwrap();
        assert_eq!(p.params().unwrap_err(), "Fill in Set cell.");
        p.set(&json!({"set_cell": "B4", "to_value": 1000, "by_changing": "B1"}));
        assert_eq!(
            p.params().unwrap(),
            json!({"set_cell": "B4", "to_value": 1000.0, "by_changing": "B1"})
        );
        // The selection fills range; numbers parse; JSON lists pass through.
        let mut v = Params::new("data.validation", &s).unwrap();
        v.set(&json!({"type": "list", "source": "[\"a\",\"b\"]", "min": "3"}));
        let out = v.params().unwrap();
        assert_eq!(out["range"], "B2:C4");
        assert_eq!(out["type"], "list");
        assert_eq!(out["source"], json!(["a", "b"]));
        assert_eq!(out["min"], json!(3.0));
        // A single selected cell fills "cell".
        s.select_range(
            sp_core::CellRange::parse("D7").unwrap(),
            sp_core::CellAddr::new(6, 3),
        );
        let n = Params::new("insert.note", &s).unwrap();
        assert_eq!(n.fields[0].value, "D7");
        assert!(Params::new("nope", &s).is_err());
    }
}
