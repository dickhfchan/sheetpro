//! JSON-lines control protocol (see docs/control-protocol.md). Pure: callers own the I/O.

use crate::params::parse_ref;
use crate::{Json, Session, registry};
use serde_json::json;
use sp_core::CellAddr;
use sp_render::display_cell;

/// Side effects the protocol asks the host to perform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    None,
    Screenshot(String),
    Quit,
}

fn ok(result: Json) -> Json {
    json!({ "ok": true, "result": result })
}

fn err(msg: impl std::fmt::Display) -> Json {
    json!({ "ok": false, "error": msg.to_string() })
}

/// Cap on cells returned by `get`.
const MAX_GET_CELLS: u64 = 100_000;

fn get(s: &Session, range: &str) -> Json {
    let (sheet, r) = match parse_ref(s, range) {
        Ok(x) => x,
        Err(e) => return err(e),
    };
    if r.cell_count() > MAX_GET_CELLS {
        return err(format!(
            "range too large for get (max {MAX_GET_CELLS} cells)"
        ));
    }
    let Some(sh) = s.wb.sheet(sheet) else {
        return err("no such sheet");
    };
    let mut rows = Vec::new();
    for row in r.start.row..=r.end.row {
        let mut cols = Vec::new();
        for col in r.start.col..=r.end.col {
            let a = CellAddr::new(row, col);
            match sh.get(a) {
                Some(c) => {
                    let style = s.wb.styles.get(c.style);
                    let d = display_cell(a, &c.value, c.formula_src(), style, false);
                    cols.push(json!({ "input": c.edit_text(&style.num_fmt), "value": value_json(&c.value), "display": d.text }));
                }
                None => cols.push(json!({ "input": "", "value": null, "display": "" })),
            }
        }
        rows.push(Json::Array(cols));
    }
    ok(Json::Array(rows))
}

pub fn value_json(v: &sp_core::Value) -> Json {
    match v {
        sp_core::Value::Empty => Json::Null,
        sp_core::Value::Number(n) => json!(n),
        sp_core::Value::Text(t) => json!(t.as_ref()),
        sp_core::Value::Bool(b) => json!(b),
        sp_core::Value::Error(e) => json!({ "error": e.as_str() }),
    }
}

pub fn state(s: &Session) -> Json {
    json!({
        "sheet": s.wb.sheets.get(s.sheet_index()).map(|x| x.name.clone()),
        "sheets": s.wb.sheets.iter().map(|x| x.name.clone()).collect::<Vec<_>>(),
        "active": s.sel.active.to_a1(),
        "selection": s.sel.range().to_a1(),
        "dirty": s.dirty,
        "path": s.path.as_ref().map(|p| p.display().to_string()),
        "can_undo": s.history.can_undo(),
        "can_redo": s.history.can_redo(),
        "status": s.status,
        "view": serde_json::to_value(&s.view).unwrap_or(Json::Null),
        "calc": { "formulas": s.last_recalc.formulas, "cycles": s.last_recalc.cycles },
    })
}

/// Handle one request object.
pub fn handle(s: &mut Session, req: &Json) -> (Json, Action) {
    let op = req.get("op").and_then(Json::as_str).unwrap_or("");
    let none = Action::None;
    match op {
        "exec" => {
            let Some(id) = req.get("id").and_then(Json::as_str) else {
                return (err("missing 'id'"), none);
            };
            let params = req.get("params").cloned().unwrap_or_else(|| json!({}));
            match s.execute(id, &params) {
                Ok(r) => (ok(r), if s.quit_requested { Action::Quit } else { none }),
                Err(e) => (err(e), none),
            }
        }
        "set" => {
            let params = json!({ "cell": req.get("cell"), "input": req.get("input") });
            match s.execute("cell.set", &params) {
                Ok(r) => (ok(r), none),
                Err(e) => (err(e), none),
            }
        }
        "get" => (
            get(s, req.get("range").and_then(Json::as_str).unwrap_or("A1")),
            none,
        ),
        "state" => (ok(state(s)), none),
        "commands" => (
            ok(serde_json::to_value(registry().all()).unwrap_or(Json::Null)),
            none,
        ),
        "functions" => (ok(json!(sp_formula::function_names())), none),
        "screenshot" => match req.get("out").and_then(Json::as_str) {
            Some(p) => (ok(json!(p)), Action::Screenshot(p.to_string())),
            None => (err("missing 'out' path"), none),
        },
        "quit" => (ok(Json::Null), Action::Quit),
        other => (err(format!("unknown op '{other}'")), none),
    }
}

/// Handle one line of text; invalid JSON yields an error reply, never a panic.
pub fn handle_line(s: &mut Session, line: &str) -> (String, Action) {
    let (reply, action) = match serde_json::from_str::<Json>(line) {
        Ok(req) => handle(s, &req),
        Err(e) => (err(format!("invalid JSON: {e}")), Action::None),
    };
    (reply.to_string(), action)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_roundtrip() {
        let mut s = Session::new();
        let (r, _) = handle_line(&mut s, r#"{"op":"set","cell":"A1","input":"10"}"#);
        assert!(r.contains("\"ok\":true"), "{r}");
        handle_line(
            &mut s,
            r#"{"op":"exec","id":"cell.set","params":{"cell":"A2","input":"=A1*2"}}"#,
        );
        let (r, _) = handle_line(&mut s, r#"{"op":"get","range":"A1:A2"}"#);
        assert!(
            r.contains("\"value\":20.0") || r.contains("\"value\":20"),
            "{r}"
        );
        let (r, _) = handle_line(&mut s, "not json");
        assert!(r.contains("\"ok\":false"));
        let (_, a) = handle_line(&mut s, r#"{"op":"quit"}"#);
        assert_eq!(a, Action::Quit);
        let (r, _) = handle_line(&mut s, r#"{"op":"get","range":"A:XFD"}"#);
        assert!(r.contains("too large"));
    }
}
