//! Macros (`view.macros`): a macro is a named, recorded sequence of command invocations,
//! not VBA. Record start/stop, run, list, delete, and edit as JSON-lines text
//! (`{"id": "home.font.bold", "params": {"range": "A1"}}` per line).
//!
//! Macros live in the workbook as a custom XML part (`customXml/sheetproMacros.xml`,
//! related from the workbook part) kept in the workbook's pass-through store, so they
//! round-trip through .xlsx without any reader/writer changes. A file's `vbaProject.bin`
//! is a separate pass-through part and is never executed. Running a macro is one undo
//! step; a failing step rolls the whole run back.

use crate::params::str_opt;
use crate::{CmdError, CmdResult, CommandSpec, Json, Session, UNDO_LIMIT};
use serde_json::json;
use sp_model::Workbook;
use sp_model::passthrough::{RawPart, RawRel};
use sp_ops::{History, Tx};
use std::sync::Arc;

/// Namespace of the macro part's root element (how the part is recognised on load).
pub const MACROS_NS: &str = "urn:sheetpro:macros:v1";
/// Part name used when the workbook has no macro part yet.
pub const MACROS_PART: &str = "customXml/sheetproMacros.xml";
const CUSTOM_XML_REL: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/customXml";
const XML_CONTENT_TYPE: &str = "application/xml";

/// Caps (macro text comes from files and agents: hostile).
pub const MAX_MACROS: usize = 256;
pub const MAX_STEPS: usize = 10_000;
pub const MAX_NAME_CHARS: usize = 64;
/// Largest JSONL text accepted for one macro, and largest macro part read from a file.
pub const MAX_MACRO_BYTES: usize = 1 << 20;
const MAX_PART_BYTES: usize = 8 << 20;

/// One recorded command invocation.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Step {
    pub id: String,
    #[serde(default, skip_serializing_if = "Json::is_null")]
    pub params: Json,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Macro {
    pub name: String,
    pub description: String,
    pub steps: Vec<Step>,
}

/// Recorder and runner state of a session (not saved).
#[derive(Debug, Clone, Default)]
pub struct MacroState {
    pub recording: Option<Recording>,
    /// Nesting depth of macro runs in progress (macros cannot run macros).
    pub running: u32,
}

#[derive(Debug, Clone, Default)]
pub struct Recording {
    pub name: String,
    pub description: String,
    pub steps: Vec<Step>,
    /// Steps dropped because the recording hit `MAX_STEPS`.
    pub dropped: usize,
}

const PARAMS: &str = r#"{"action"?: "list|record_start|record_stop|record_cancel|run|delete|get|set", "name"?: "Macro1", "description"?: "text", "jsonl"?: "one {\"id\":…,\"params\":{…}} per line (set)"}"#;

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec::new("view.macros", PARAMS, macros)]
}

/// Called by `Session::execute` after every successful command: append it to the
/// recording in progress (the macro commands themselves and steps run by a macro are not
/// recorded).
pub fn record(s: &mut Session, id: &str, params: &Json) {
    if s.macros.running > 0 || id == "view.macros" {
        return;
    }
    let Some(rec) = s.macros.recording.as_mut() else {
        return;
    };
    if rec.steps.len() >= MAX_STEPS {
        rec.dropped = rec.dropped.saturating_add(1);
        return;
    }
    let params = match params {
        Json::Object(m) if m.is_empty() => Json::Null,
        Json::Object(_) => params.clone(),
        _ => Json::Null,
    };
    rec.steps.push(Step {
        id: id.to_string(),
        params,
    });
}

fn macros(s: &mut Session, p: &Json) -> CmdResult {
    let action = str_opt(p, "action").unwrap_or("list");
    match action {
        "list" => Ok(list(s)),
        "record_start" => record_start(s, p),
        "record_stop" => record_stop(s),
        "record_cancel" => {
            let had = s.macros.recording.take().is_some();
            Ok(json!({ "cancelled": had }))
        }
        "run" => {
            let name = name_param(p)?;
            let m = find(&s.wb, &name)?;
            run_steps(s, &m.name, &m.steps)
        }
        "delete" => {
            let name = name_param(p)?;
            let mut all = load_macros(&s.wb);
            let before = all.len();
            all.retain(|m| !m.name.eq_ignore_ascii_case(&name));
            if all.len() == before {
                return Err(CmdError::Failed(format!("no macro named '{name}'")));
            }
            save_macros(s, all, "Delete Macro")?;
            Ok(json!({ "deleted": name }))
        }
        "get" => {
            let name = name_param(p)?;
            let m = find(&s.wb, &name)?;
            Ok(
                json!({ "name": m.name, "description": m.description, "jsonl": to_jsonl(&m.steps), "steps": m.steps.len() }),
            )
        }
        "set" | "edit" => {
            let name = name_param(p)?;
            let text = str_opt(p, "jsonl")
                .ok_or_else(|| CmdError::Param("missing string 'jsonl'".into()))?;
            let steps = parse_jsonl(text)?;
            let description = match str_opt(p, "description") {
                Some(d) => clean_description(d),
                None => find(&s.wb, &name)
                    .map(|m| m.description)
                    .unwrap_or_default(),
            };
            let n = steps.len();
            put_macro(
                s,
                Macro {
                    name: name.clone(),
                    description,
                    steps,
                },
                "Edit Macro",
            )?;
            Ok(json!({ "name": name, "steps": n }))
        }
        other => Err(CmdError::Param(format!("unknown macro action '{other}'"))),
    }
}

fn list(s: &Session) -> Json {
    let all = load_macros(&s.wb);
    json!({
        "macros": all.iter().map(|m| json!({"name": m.name, "description": m.description, "steps": m.steps.len()})).collect::<Vec<_>>(),
        "recording": s.macros.recording.as_ref().map(|r| json!({"name": r.name, "steps": r.steps.len()})),
    })
}

fn record_start(s: &mut Session, p: &Json) -> CmdResult {
    if s.macros.running > 0 {
        return Err(CmdError::Failed("cannot record while a macro runs".into()));
    }
    if let Some(r) = &s.macros.recording {
        return Err(CmdError::Failed(format!(
            "already recording macro '{}'",
            r.name
        )));
    }
    let name = match str_opt(p, "name") {
        Some(_) => name_param(p)?,
        None => next_free_name(&s.wb),
    };
    let description = str_opt(p, "description")
        .map(clean_description)
        .unwrap_or_default();
    s.macros.recording = Some(Recording {
        name: name.clone(),
        description,
        steps: Vec::new(),
        dropped: 0,
    });
    s.status = format!("Recording macro {name}");
    Ok(json!({ "recording": name }))
}

fn record_stop(s: &mut Session) -> CmdResult {
    let Some(rec) = s.macros.recording.take() else {
        return Err(CmdError::Failed("no macro is being recorded".into()));
    };
    let n = rec.steps.len();
    let name = rec.name.clone();
    let m = Macro {
        name: rec.name,
        description: rec.description,
        steps: rec.steps,
    };
    if let Err(e) = put_macro(s, m.clone(), "Record Macro") {
        // Keep the recording so nothing is lost.
        s.macros.recording = Some(Recording {
            name: m.name,
            description: m.description,
            steps: m.steps,
            dropped: rec.dropped,
        });
        return Err(e);
    }
    s.status = "Ready".into();
    Ok(json!({ "name": name, "steps": n, "dropped": rec.dropped }))
}

/// A trimmed macro name from `name`: 1-64 characters, no control characters.
fn name_param(p: &Json) -> Result<String, CmdError> {
    let raw = str_opt(p, "name").ok_or_else(|| CmdError::Param("missing string 'name'".into()))?;
    validate_name(raw)
}

pub fn validate_name(raw: &str) -> Result<String, CmdError> {
    let name = raw.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME_CHARS {
        return Err(CmdError::Param(format!(
            "macro name must be 1-{MAX_NAME_CHARS} characters"
        )));
    }
    if name.chars().any(char::is_control) {
        return Err(CmdError::Param(
            "macro name contains control characters".into(),
        ));
    }
    Ok(name.to_string())
}

fn clean_description(d: &str) -> String {
    d.chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .take(1024)
        .collect()
}

fn next_free_name(wb: &Workbook) -> String {
    let all = load_macros(wb);
    (1..=MAX_MACROS + 1)
        .map(|i| format!("Macro{i}"))
        .find(|n| !all.iter().any(|m| m.name.eq_ignore_ascii_case(n)))
        .unwrap_or_else(|| "Macro".into())
}

fn find(wb: &Workbook, name: &str) -> Result<Macro, CmdError> {
    load_macros(wb)
        .into_iter()
        .find(|m| m.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| CmdError::Failed(format!("no macro named '{name}'")))
}

fn put_macro(s: &mut Session, m: Macro, label: &str) -> Result<(), CmdError> {
    let mut all = load_macros(&s.wb);
    match all
        .iter_mut()
        .find(|x| x.name.eq_ignore_ascii_case(&m.name))
    {
        Some(slot) => *slot = m,
        None => {
            if all.len() >= MAX_MACROS {
                return Err(CmdError::Failed(format!(
                    "a workbook holds at most {MAX_MACROS} macros"
                )));
            }
            all.push(m);
        }
    }
    save_macros(s, all, label)
}

// ---------------------------------------------------------------------------------------
// JSON lines

/// Steps as JSON lines (one invocation per line).
pub fn to_jsonl(steps: &[Step]) -> String {
    steps
        .iter()
        .filter_map(|st| serde_json::to_string(st).ok())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Parse and validate JSON-lines macro text. Blank lines and `#` comments are skipped;
/// every step must name a registered command other than `view.macros`.
pub fn parse_jsonl(text: &str) -> Result<Vec<Step>, CmdError> {
    if text.len() > MAX_MACRO_BYTES {
        return Err(CmdError::Param(format!(
            "macro text is larger than {MAX_MACRO_BYTES} bytes"
        )));
    }
    let mut steps = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let n = i + 1;
        let v: Json = serde_json::from_str(line)
            .map_err(|e| CmdError::Param(format!("line {n}: invalid JSON: {e}")))?;
        let id = v
            .get("id")
            .and_then(Json::as_str)
            .ok_or_else(|| CmdError::Param(format!("line {n}: missing string 'id'")))?;
        if id == "view.macros" {
            return Err(CmdError::Param(format!(
                "line {n}: macros cannot run macro commands"
            )));
        }
        if crate::registry().get(id).is_none() {
            return Err(CmdError::Param(format!("line {n}: unknown command '{id}'")));
        }
        let params = match v.get("params") {
            None | Some(Json::Null) => Json::Null,
            Some(o @ Json::Object(_)) => o.clone(),
            Some(_) => {
                return Err(CmdError::Param(format!(
                    "line {n}: 'params' must be an object"
                )));
            }
        };
        if steps.len() >= MAX_STEPS {
            return Err(CmdError::Param(format!(
                "a macro holds at most {MAX_STEPS} steps"
            )));
        }
        steps.push(Step {
            id: id.to_string(),
            params,
        });
    }
    Ok(steps)
}

// ---------------------------------------------------------------------------------------
// Running

/// Run `steps` as one undo step. Any failing step rolls the whole run back.
pub fn run_steps(s: &mut Session, name: &str, steps: &[Step]) -> CmdResult {
    if s.macros.running > 0 {
        return Err(CmdError::Failed("macros cannot run other macros".into()));
    }
    let before_wb = s.wb.clone();
    let before_sel = s.sel;
    let saved = std::mem::replace(&mut s.history, History::new(UNDO_LIMIT));
    s.macros.running += 1;
    let mut failure = None;
    for (i, st) in steps.iter().enumerate() {
        if st.id == "view.macros" {
            failure = Some(format!("step {}: macros cannot run macro commands", i + 1));
            break;
        }
        let params = if st.params.is_null() {
            json!({})
        } else {
            st.params.clone()
        };
        if let Err(e) = s.execute(&st.id, &params) {
            failure = Some(format!("step {} ({}): {e}", i + 1, st.id));
            break;
        }
        if s.quit_requested {
            break;
        }
    }
    s.macros.running = s.macros.running.saturating_sub(1);
    let changed = s.history.can_undo();
    if let Some(msg) = failure {
        s.wb = before_wb;
        s.history = saved;
        s.sel = before_sel;
        s.after_change();
        return Err(CmdError::Failed(format!(
            "macro '{name}' stopped at {msg}; no changes were kept"
        )));
    }
    s.history = saved;
    if changed {
        let after = std::mem::replace(&mut s.wb, before_wb);
        let mut tx = Tx::new(&mut s.wb);
        tx.touch_book();
        *tx.wb = after;
        if let Some(t) = tx.commit(&format!("Run Macro {name}")) {
            s.history.push(t);
        }
        s.dirty = true;
    }
    Ok(json!({ "ran": name, "steps": steps.len() }))
}

// ---------------------------------------------------------------------------------------
// Storage (custom XML part in the workbook's pass-through store)

fn is_macro_part(p: &RawPart) -> bool {
    p.data.len() <= MAX_PART_BYTES
        && std::str::from_utf8(&p.data).is_ok_and(|t| t.contains(MACROS_NS))
}

/// Macros stored in the workbook (an unreadable part yields none).
pub fn load_macros(wb: &Workbook) -> Vec<Macro> {
    let Some(part) = wb.passthrough.parts.iter().find(|p| is_macro_part(p)) else {
        return Vec::new();
    };
    let Ok(text) = std::str::from_utf8(&part.data) else {
        return Vec::new();
    };
    let Ok(root) = sp_io::xml::parse_dom(text) else {
        return Vec::new();
    };
    let mut out: Vec<Macro> = Vec::new();
    for m in root.children_named("macro").take(MAX_MACROS) {
        let Some(name) = m.attr("name").and_then(|n| validate_name(n).ok()) else {
            continue;
        };
        if out.iter().any(|x| x.name.eq_ignore_ascii_case(&name)) {
            continue;
        }
        // Steps that no longer parse (e.g. a command this build lacks) are skipped.
        let steps = m
            .text
            .lines()
            .filter_map(|l| parse_jsonl(l).ok())
            .flatten()
            .take(MAX_STEPS)
            .collect();
        out.push(Macro {
            name,
            description: clean_description(m.attr("description").unwrap_or("")),
            steps,
        });
    }
    out
}

/// The XML text of the macro part.
pub fn macros_xml(all: &[Macro]) -> String {
    use sp_io::xml::esc;
    let mut x = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<macros xmlns=\"{MACROS_NS}\">"
    );
    for m in all {
        x.push_str(&format!(
            "<macro name=\"{}\" description=\"{}\">{}</macro>",
            esc(&m.name),
            esc(&m.description.replace('\n', " ")),
            esc(&to_jsonl(&m.steps))
        ));
    }
    x.push_str("</macros>");
    x
}

/// Replace the stored macros (one undo step). An empty list removes the part.
fn save_macros(s: &mut Session, all: Vec<Macro>, label: &str) -> Result<(), CmdError> {
    s.transact(label, |tx, _| {
        tx.touch_book();
        write_macros(tx.wb, &all);
        Ok(())
    })
}

/// Store `all` in `wb`'s pass-through as the macro part (removing it when empty).
pub fn write_macros(wb: &mut Workbook, all: &[Macro]) {
    let pt = &mut wb.passthrough;
    let existing = pt.parts.iter().position(is_macro_part);
    if all.is_empty() {
        if let Some(i) = existing {
            let path = pt.parts.remove(i).path;
            pt.rels.retain(|r| r.target != path);
        }
        return;
    }
    let data: Arc<[u8]> = Arc::from(macros_xml(all).into_bytes());
    match existing.and_then(|i| pt.parts.get_mut(i)) {
        Some(part) => part.data = data,
        None => {
            let mut path = MACROS_PART.to_string();
            let mut n = 2;
            while pt.parts.iter().any(|p| p.path == path) {
                path = format!("customXml/sheetproMacros{n}.xml");
                n += 1;
            }
            pt.parts.push(RawPart {
                path: path.clone(),
                content_type: XML_CONTENT_TYPE.into(),
                data,
                rels: Vec::new(),
            });
        }
    }
    // Make sure the workbook part relates to it.
    if let Some(part) = pt.parts.iter().find(|p| is_macro_part(p)) {
        let path = part.path.clone();
        if !pt.rels.iter().any(|r| r.target == path) {
            let mut i = 1;
            let id = loop {
                let id = format!("rIdSpMacros{i}");
                if !pt.rels.iter().any(|r| r.id == id) {
                    break id;
                }
                i += 1;
            };
            pt.rels.push(RawRel {
                id,
                rel_type: CUSTOM_XML_REL.into(),
                target: path,
                external: false,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sp_core::{CellAddr, Value};

    fn v(s: &Session, a: &str) -> Value {
        s.wb.sheets[0].value(CellAddr::parse(a).unwrap()).clone()
    }

    #[test]
    fn record_run_is_one_undo_step() {
        let mut s = Session::new();
        s.execute(
            "view.macros",
            &json!({"action": "record_start", "name": "Fill"}),
        )
        .unwrap();
        s.execute("cell.set", &json!({"cell": "A1", "input": "5"}))
            .unwrap();
        s.execute("cell.set", &json!({"cell": "A2", "input": "=A1*2"}))
            .unwrap();
        s.execute("home.font.bold", &json!({"range": "A1:A2"}))
            .unwrap();
        let r = s
            .execute("view.macros", &json!({"action": "record_stop"}))
            .unwrap();
        assert_eq!(r["steps"], 3);
        // Clear the sheet, run the macro, check, undo once.
        s.execute("home.editing.clear_all", &json!({"range": "A1:A2"}))
            .unwrap();
        assert_eq!(v(&s, "A2"), Value::Empty);
        s.execute("view.macros", &json!({"action": "run", "name": "fill"}))
            .unwrap();
        assert_eq!(v(&s, "A2"), Value::Number(10.0));
        assert_eq!(s.history.undo_label(), Some("Run Macro Fill"));
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(v(&s, "A2"), Value::Empty);
        assert_eq!(load_macros(&s.wb).len(), 1);
        s.execute("edit.redo", &json!({})).unwrap();
        assert_eq!(v(&s, "A2"), Value::Number(10.0));
    }

    #[test]
    fn edit_get_delete_and_failing_run_rolls_back() {
        let mut s = Session::new();
        let jsonl = "{\"id\":\"cell.set\",\"params\":{\"cell\":\"B1\",\"input\":\"x\"}}\n# comment\n\n{\"id\":\"cell.set\",\"params\":{\"cell\":\"Nope!A1\",\"input\":\"1\"}}";
        s.execute(
            "view.macros",
            &json!({"action": "set", "name": "Bad", "jsonl": jsonl}),
        )
        .unwrap();
        let g = s
            .execute("view.macros", &json!({"action": "get", "name": "Bad"}))
            .unwrap();
        assert_eq!(g["steps"], 2);
        let e = s
            .execute("view.macros", &json!({"action": "run", "name": "Bad"}))
            .unwrap_err();
        assert!(e.to_string().contains("step 2"), "{e}");
        assert_eq!(v(&s, "B1"), Value::Empty);
        // Invalid text is refused with a line number.
        let e = s
            .execute(
                "view.macros",
                &json!({"action": "set", "name": "X", "jsonl": "{\"id\":\"no.such\"}"}),
            )
            .unwrap_err();
        assert!(e.to_string().contains("line 1"), "{e}");
        assert!(
            s.execute(
                "view.macros",
                &json!({"action": "set", "name": "X", "jsonl": "{\"id\":\"view.macros\"}"})
            )
            .is_err()
        );
        s.execute("view.macros", &json!({"action": "delete", "name": "bad"}))
            .unwrap();
        assert!(load_macros(&s.wb).is_empty());
        assert!(s.wb.passthrough.parts.is_empty());
        assert!(s.wb.passthrough.rels.is_empty());
        assert!(
            s.execute("view.macros", &json!({"action": "delete", "name": "bad"}))
                .is_err()
        );
    }

    #[test]
    fn macros_round_trip_through_xlsx() {
        let mut s = Session::new();
        let jsonl = "{\"id\":\"cell.set\",\"params\":{\"cell\":\"C3\",\"input\":\"a <b> & \\\"c\\\"\"}}\n{\"id\":\"home.font.bold\"}";
        s.execute(
            "view.macros",
            &json!({"action": "set", "name": "Röund & <trip>", "description": "d\"q", "jsonl": jsonl}),
        )
        .unwrap();
        let mut buf = std::io::Cursor::new(Vec::new());
        sp_io::xlsx_write::write_xlsx(&s.wb, &mut buf).unwrap();
        let bytes = buf.into_inner();
        assert_eq!(
            sp_io::xlsx_passthrough::validate_package(&bytes),
            Vec::<String>::new()
        );
        let (wb, _) = sp_io::xlsx_read::read_xlsx(std::io::Cursor::new(bytes.clone())).unwrap();
        let back = load_macros(&wb);
        assert_eq!(back, load_macros(&s.wb));
        assert_eq!(back[0].name, "Röund & <trip>");
        assert_eq!(back[0].steps.len(), 2);
        // Second save of the reloaded book keeps one part and stays valid.
        let mut s2 = Session::with_workbook(wb);
        s2.execute(
            "view.macros",
            &json!({"action": "set", "name": "Two", "jsonl": "{\"id\":\"home.font.italic\"}"}),
        )
        .unwrap();
        let mut buf = std::io::Cursor::new(Vec::new());
        sp_io::xlsx_write::write_xlsx(&s2.wb, &mut buf).unwrap();
        let bytes = buf.into_inner();
        assert_eq!(
            sp_io::xlsx_passthrough::validate_package(&bytes),
            Vec::<String>::new()
        );
        let (wb, _) = sp_io::xlsx_read::read_xlsx(std::io::Cursor::new(bytes)).unwrap();
        assert_eq!(load_macros(&wb).len(), 2);
        assert_eq!(
            wb.passthrough
                .parts
                .iter()
                .filter(|p| is_macro_part(p))
                .count(),
            1
        );
        // Running a macro recorded in the file works after reload.
        let mut s3 = Session::with_workbook(wb);
        s3.execute(
            "view.macros",
            &json!({"action": "run", "name": "röund & <trip>"}),
        )
        .unwrap();
        assert_eq!(v(&s3, "C3"), Value::Text("a <b> & \"c\"".into()));
    }

    #[test]
    fn hostile_macro_part_is_ignored() {
        let mut wb = Workbook::new();
        wb.passthrough.parts.push(RawPart {
            path: MACROS_PART.into(),
            content_type: XML_CONTENT_TYPE.into(),
            data: Arc::from(
                format!(
                    "<macros xmlns=\"{MACROS_NS}\"><macro name=\"\">x</macro><macro name=\"A\">not json\n{{\"id\":\"view.macros\"}}\n{{\"id\":\"home.font.bold\",\"params\":5}}\n{{\"id\":\"home.font.bold\"}}</macro><macro"
                )
                .into_bytes(),
            ),
            rels: Vec::new(),
        });
        assert!(load_macros(&wb).is_empty()); // malformed XML
        if let Some(p) = wb.passthrough.parts.first_mut() {
            let mut t = String::from_utf8_lossy(&p.data).into_owned();
            t.truncate(t.len() - "<macro".len());
            t.push_str("</macros>");
            p.data = Arc::from(t.into_bytes());
        }
        let m = load_macros(&wb);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].steps.len(), 1);
    }

    #[test]
    fn recording_caps_and_guards() {
        let mut s = Session::new();
        assert!(
            s.execute("view.macros", &json!({"action": "record_stop"}))
                .is_err()
        );
        s.execute("view.macros", &json!({"action": "record_start"}))
            .unwrap();
        assert!(
            s.execute("view.macros", &json!({"action": "record_start"}))
                .is_err()
        );
        for _ in 0..(MAX_STEPS + 5) {
            record(&mut s, "home.font.bold", &json!({}));
        }
        let r = s
            .execute("view.macros", &json!({"action": "record_stop"}))
            .unwrap();
        assert_eq!(r["name"], "Macro1");
        assert_eq!(r["steps"], MAX_STEPS);
        assert_eq!(r["dropped"], 5);
        assert!(
            s.execute(
                "view.macros",
                &json!({"action": "record_start", "name": "x".repeat(65)})
            )
            .is_err()
        );
        assert!(
            s.execute("view.macros", &json!({"action": "explode"}))
                .is_err()
        );
        let l = s.execute("view.macros", &json!({})).unwrap();
        assert_eq!(l["macros"][0]["name"], "Macro1");
    }
}
