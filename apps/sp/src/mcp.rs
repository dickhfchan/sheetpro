//! `sp mcp`: a Model Context Protocol server over stdio (JSON-RPC 2.0, one message per
//! line). Implements `initialize`, `ping`, `tools/list`, `tools/call` and ignores
//! notifications. Every tool drives one `Session` through registered commands; tool
//! failures are results with `isError: true`, protocol mistakes are JSON-RPC errors, and
//! nothing a client sends can crash the server (sizes are capped). No network access.

use serde_json::{Map, Value as Json, json};
use sp_core::{CellAddr, CellRange};
use sp_engine::control::value_json;
use sp_engine::params::parse_ref;
use sp_engine::{CmdError, Session, registry};
use std::io::{BufRead, Write};

/// Protocol versions we speak, newest first.
pub const PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];
/// Longest message accepted (bytes, one line).
pub const MAX_MESSAGE_BYTES: usize = 16 << 20;
/// Most cells `get_cells` returns / `set_cells` writes per call.
pub const MAX_TOOL_CELLS: u64 = 100_000;
/// Most messages in one JSON-RPC batch.
const MAX_BATCH: usize = 1_000;
/// Screenshot size limits (pixels).
const MAX_SHOT_SIDE: f64 = 4_096.0;

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// Renders a session to PNG bytes (the real offscreen renderer in the binary; a stub in
/// tests).
pub type Renderer = fn(&Session, (f32, f32)) -> Result<Vec<u8>, String>;

pub struct Server {
    pub session: Session,
    render: Renderer,
}

fn rpc_error(id: Json, code: i64, msg: impl std::fmt::Display) -> Json {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": msg.to_string() } })
}

fn rpc_result(id: Json, result: Json) -> Json {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// Tool failure: a result the model can read, flagged `isError`.
fn tool_error(msg: impl std::fmt::Display) -> Json {
    json!({ "content": [{ "type": "text", "text": msg.to_string() }], "isError": true })
}

fn tool_ok(v: Json) -> Json {
    let text = serde_json::to_string(&v).unwrap_or_default();
    let structured = match v {
        Json::Object(_) => v,
        other => json!({ "result": other }),
    };
    json!({ "content": [{ "type": "text", "text": text }], "structuredContent": structured, "isError": false })
}

impl Server {
    pub fn new(session: Session, render: Renderer) -> Self {
        Server { session, render }
    }

    /// Handle one line; returns the reply line, if any.
    pub fn handle_line(&mut self, line: &str) -> Option<String> {
        let reply = match serde_json::from_str::<Json>(line) {
            Err(e) => Some(rpc_error(
                Json::Null,
                PARSE_ERROR,
                format!("parse error: {e}"),
            )),
            Ok(Json::Array(items)) => {
                if items.is_empty() || items.len() > MAX_BATCH {
                    Some(rpc_error(
                        Json::Null,
                        INVALID_REQUEST,
                        "batch must hold 1-1000 messages",
                    ))
                } else {
                    let out: Vec<Json> = items.iter().filter_map(|m| self.handle(m)).collect();
                    (!out.is_empty()).then_some(Json::Array(out))
                }
            }
            Ok(msg) => self.handle(&msg),
        };
        reply.map(|r| r.to_string())
    }

    /// Handle one JSON-RPC message; `None` for notifications and client responses.
    pub fn handle(&mut self, msg: &Json) -> Option<Json> {
        let Some(obj) = msg.as_object() else {
            return Some(rpc_error(
                Json::Null,
                INVALID_REQUEST,
                "message must be an object",
            ));
        };
        let id = obj.get("id").cloned();
        // Ids must be strings or numbers.
        if let Some(i) = &id
            && !(i.is_string() || i.is_number())
        {
            return Some(rpc_error(Json::Null, INVALID_REQUEST, "invalid id"));
        }
        if obj.get("jsonrpc").and_then(Json::as_str) != Some("2.0") {
            return id.map(|i| rpc_error(i, INVALID_REQUEST, "jsonrpc must be \"2.0\""));
        }
        let Some(method) = obj.get("method").and_then(Json::as_str) else {
            // A response to a server request (we send none) or garbage.
            if obj.contains_key("result") || obj.contains_key("error") {
                return None;
            }
            return Some(rpc_error(
                id.unwrap_or(Json::Null),
                INVALID_REQUEST,
                "missing method",
            ));
        };
        let params = obj.get("params").cloned().unwrap_or(Json::Null);
        let Some(id) = id else {
            // Notifications (initialized, cancelled, …) need no reply.
            return None;
        };
        if !(params.is_null() || params.is_object()) {
            return Some(rpc_error(id, INVALID_PARAMS, "params must be an object"));
        }
        Some(match method {
            "initialize" => rpc_result(id, initialize(&params)),
            "ping" => rpc_result(id, json!({})),
            "tools/list" => rpc_result(id, json!({ "tools": tool_list() })),
            "tools/call" => {
                let Some(name) = params.get("name").and_then(Json::as_str) else {
                    return Some(rpc_error(id, INVALID_PARAMS, "missing tool name"));
                };
                if !TOOLS.iter().any(|t| t.0 == name) {
                    return Some(rpc_error(
                        id,
                        INVALID_PARAMS,
                        format!("Unknown tool: {name}"),
                    ));
                }
                let args = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                let result = if args.is_object() {
                    self.call(name, &args)
                } else {
                    tool_error("arguments must be an object")
                };
                rpc_result(id, result)
            }
            "resources/list" => rpc_result(id, json!({ "resources": [] })),
            "prompts/list" => rpc_result(id, json!({ "prompts": [] })),
            other => rpc_error(id, METHOD_NOT_FOUND, format!("Method not found: {other}")),
        })
    }

    fn call(&mut self, name: &str, a: &Json) -> Json {
        let r = match name {
            "list_commands" => Ok(list_commands(&self.session, a)),
            "run_command" => run_command(&mut self.session, a),
            "get_cells" => get_cells(&self.session, a),
            "set_cells" => set_cells(&mut self.session, a),
            "open_workbook" => path_arg(a).and_then(|p| {
                exec(&mut self.session, "file.open", &json!({ "path": p }))
                    .map(|_| sheets(&self.session))
            }),
            "save_workbook" => save_workbook(&mut self.session, a),
            "list_sheets" => Ok(sheets(&self.session)),
            "screenshot" => return self.screenshot(a),
            "undo" => {
                exec(&mut self.session, "edit.undo", &json!({})).map(|r| json!({"undone": r}))
            }
            "redo" => {
                exec(&mut self.session, "edit.redo", &json!({})).map(|r| json!({"redone": r}))
            }
            other => Err(format!("Unknown tool: {other}")),
        };
        match r {
            Ok(v) => tool_ok(v),
            Err(e) => tool_error(e),
        }
    }

    fn screenshot(&mut self, a: &Json) -> Json {
        let dim = |k: &str, d: f64| -> Result<f32, String> {
            match a.get(k) {
                None | Some(Json::Null) => Ok(d as f32),
                Some(v) => v
                    .as_f64()
                    .filter(|n| (200.0..=MAX_SHOT_SIDE).contains(n))
                    .map(|n| n as f32)
                    .ok_or_else(|| format!("'{k}' must be a number from 200 to {MAX_SHOT_SIDE}")),
            }
        };
        let size = match (dim("width", 1280.0), dim("height", 800.0)) {
            (Ok(w), Ok(h)) => (w, h),
            (Err(e), _) | (_, Err(e)) => return tool_error(e),
        };
        match (self.render)(&self.session, size) {
            Ok(png) => json!({
                "content": [{ "type": "image", "data": base64(&png), "mimeType": "image/png" }],
                "isError": false,
            }),
            Err(e) => tool_error(format!("screenshot failed: {e}")),
        }
    }
}

fn initialize(params: &Json) -> Json {
    let asked = params
        .get("protocolVersion")
        .and_then(Json::as_str)
        .unwrap_or("");
    let version = PROTOCOL_VERSIONS
        .iter()
        .find(|v| **v == asked)
        .unwrap_or(&PROTOCOL_VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": "sheetpro", "title": "SheetPro", "version": env!("CARGO_PKG_VERSION") },
        "instructions": "SheetPro spreadsheet. Every action is a registered command: find ids with list_commands (filter by group like \"home.font\" or text), then run_command with the params its 'params' doc shows. get_cells/set_cells read and write ranges in A1 notation (Sheet2!A1:C3); strings starting with '=' are formulas. Each set_cells/run_command call is one undo step.",
    })
}

/// (name, title, description, input schema)
type ToolDef = (&'static str, &'static str, &'static str, fn() -> Json);

const TOOLS: [ToolDef; 10] = [
    (
        "list_commands",
        "List commands",
        "List registered SheetPro commands (id, label, ribbon location, shortcut, params doc, enabled now). Filter by id group prefix (e.g. \"home.font\", \"data\") and/or free text matched against id, label and location.",
        || {
            json!({"type": "object", "properties": {
                "group": {"type": "string", "description": "Command id prefix, e.g. \"data\" or \"home.font\""},
                "query": {"type": "string", "description": "Case-insensitive text in id, label or location"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 1000, "default": 400}
            }, "additionalProperties": false})
        },
    ),
    (
        "run_command",
        "Run command",
        "Run one SheetPro command by id with JSON params (see list_commands for the params doc). Returns the command's result. One undo step for commands that change the workbook.",
        || {
            json!({"type": "object", "properties": {
                "id": {"type": "string", "description": "Command id, e.g. \"home.font.bold\""},
                "params": {"type": "object", "description": "Command params", "additionalProperties": true}
            }, "required": ["id"], "additionalProperties": false})
        },
    ),
    (
        "get_cells",
        "Get cells",
        "Read a range: per cell the value (number/text/bool/null/{error}), the formula (with '=') if any, the input text and the formatted display text. At most 100000 cells.",
        || {
            json!({"type": "object", "properties": {
                "range": {"type": "string", "description": "A1 range, optionally sheet-qualified: \"A1:C10\", \"Sheet2!B3\", a defined name"}
            }, "required": ["range"], "additionalProperties": false})
        },
    ),
    (
        "set_cells",
        "Set cells",
        "Write a 2-D block of values starting at the range's top-left cell, as one undo step. Strings are parsed like typed input ('=' starts a formula, '\\'' forces text, numbers/dates/percents are recognised); numbers and booleans are stored as-is; null or \"\" clears contents (formatting kept). At most 100000 cells.",
        || {
            json!({"type": "object", "properties": {
                "range": {"type": "string", "description": "Top-left cell or full range, optionally sheet-qualified"},
                "values": {"type": "array", "description": "Rows of cell inputs", "items": {"type": "array", "items": {"type": ["string", "number", "boolean", "null"]}}}
            }, "required": ["range", "values"], "additionalProperties": false})
        },
    ),
    (
        "open_workbook",
        "Open workbook",
        "Open a workbook (.xlsx, .xlsm, .csv, .tsv) from an absolute path, replacing the current one.",
        || {
            json!({"type": "object", "properties": {
                "path": {"type": "string", "description": "Absolute file path"}
            }, "required": ["path"], "additionalProperties": false})
        },
    ),
    (
        "save_workbook",
        "Save workbook",
        "Save the workbook. With a path: save as that file (format from the extension: .xlsx, .csv, .tsv). Without: save to the current path.",
        || {
            json!({"type": "object", "properties": {
                "path": {"type": "string", "description": "Absolute file path (optional)"}
            }, "additionalProperties": false})
        },
    ),
    (
        "list_sheets",
        "List sheets",
        "Sheets of the workbook with their used range, plus the active sheet, selection, file path, dirty flag and undo/redo availability.",
        || json!({"type": "object", "properties": {}, "additionalProperties": false}),
    ),
    (
        "screenshot",
        "Screenshot",
        "Render the SheetPro window (ribbon, grid, charts) offscreen and return it as a PNG image.",
        || {
            json!({"type": "object", "properties": {
                "width": {"type": "number", "minimum": 200, "maximum": 4096, "default": 1280},
                "height": {"type": "number", "minimum": 200, "maximum": 4096, "default": 800}
            }, "additionalProperties": false})
        },
    ),
    (
        "undo",
        "Undo",
        "Undo the last change.",
        || json!({"type": "object", "properties": {}, "additionalProperties": false}),
    ),
    (
        "redo",
        "Redo",
        "Redo the last undone change.",
        || json!({"type": "object", "properties": {}, "additionalProperties": false}),
    ),
];

fn tool_list() -> Vec<Json> {
    TOOLS
        .iter()
        .map(|(name, title, desc, schema)| {
            let read_only = matches!(
                *name,
                "list_commands" | "get_cells" | "list_sheets" | "screenshot"
            );
            json!({
                "name": name,
                "title": title,
                "description": desc,
                "inputSchema": schema(),
                "annotations": { "readOnlyHint": read_only, "openWorldHint": false },
            })
        })
        .collect()
}

fn exec(s: &mut Session, id: &str, params: &Json) -> Result<Json, String> {
    s.execute(id, params).map_err(|e| e.to_string())
}

fn str_arg<'a>(a: &'a Json, k: &str) -> Result<&'a str, String> {
    a.get(k)
        .and_then(Json::as_str)
        .ok_or_else(|| format!("missing string argument '{k}'"))
}

fn path_arg(a: &Json) -> Result<String, String> {
    let p = str_arg(a, "path")?;
    if p.is_empty() || p.len() > 4096 || p.contains('\0') {
        return Err("invalid path".into());
    }
    Ok(p.to_string())
}

fn list_commands(s: &Session, a: &Json) -> Json {
    let group = a.get("group").and_then(Json::as_str).unwrap_or("").trim();
    let query = a
        .get("query")
        .and_then(Json::as_str)
        .unwrap_or("")
        .trim()
        .to_lowercase();
    let limit = a
        .get("limit")
        .and_then(Json::as_u64)
        .unwrap_or(400)
        .clamp(1, 1000) as usize;
    let matches: Vec<_> = registry()
        .all()
        .iter()
        .filter(|c| {
            group.is_empty()
                || c.id == group
                || c.id.starts_with(&format!("{group}."))
                || c.location.to_lowercase().starts_with(&group.to_lowercase())
        })
        .filter(|c| {
            query.is_empty()
                || c.id.to_lowercase().contains(&query)
                || c.label.to_lowercase().contains(&query)
                || c.location.to_lowercase().contains(&query)
        })
        .collect();
    let total = matches.len();
    let list: Vec<Json> = matches
        .into_iter()
        .take(limit)
        .map(|c| {
            json!({
                "id": c.id, "label": c.label, "location": c.location, "shortcut": c.shortcut,
                "params": c.params, "enabled": (c.spec.enabled)(s),
            })
        })
        .collect();
    json!({ "total": total, "returned": list.len(), "commands": list })
}

fn run_command(s: &mut Session, a: &Json) -> Result<Json, String> {
    let id = str_arg(a, "id")?;
    let params = match a.get("params") {
        None | Some(Json::Null) => json!({}),
        Some(p @ Json::Object(_)) => p.clone(),
        Some(_) => return Err("'params' must be an object".into()),
    };
    if registry().get(id).is_none() {
        return Err(format!(
            "unknown command '{id}' (use list_commands to find ids)"
        ));
    }
    let r = exec(s, id, &params)?;
    // Commands that end the app do not end the MCP session.
    s.quit_requested = false;
    Ok(json!({ "id": id, "result": r }))
}

fn sheet_range(s: &Session, a: &Json) -> Result<(usize, CellRange), String> {
    let text = str_arg(a, "range")?;
    if text.len() > 1024 {
        return Err("range text too long".into());
    }
    parse_ref(s, text).map_err(|e: CmdError| e.to_string())
}

fn get_cells(s: &Session, a: &Json) -> Result<Json, String> {
    let (si, r) = sheet_range(s, a)?;
    if r.cell_count() > MAX_TOOL_CELLS {
        return Err(format!(
            "range has {} cells; get_cells returns at most {MAX_TOOL_CELLS}",
            r.cell_count()
        ));
    }
    let sh = s.wb.sheet(si).ok_or("no such sheet")?;
    let mut rows = Vec::new();
    for row in r.start.row..=r.end.row {
        let mut cols = Vec::new();
        for col in r.start.col..=r.end.col {
            let at = CellAddr::new(row, col);
            let mut o = Map::new();
            match sh.get(at) {
                Some(c) => {
                    let d = sp_render::display_cell(
                        at,
                        &c.value,
                        c.formula_src(),
                        s.wb.styles.get(c.style),
                        false,
                    );
                    o.insert("value".into(), value_json(&c.value));
                    if let Some(f) = c.formula_src() {
                        o.insert("formula".into(), json!(format!("={f}")));
                    }
                    o.insert("input".into(), json!(c.input_text()));
                    o.insert("display".into(), json!(d.text));
                }
                None => {
                    o.insert("value".into(), Json::Null);
                    o.insert("input".into(), json!(""));
                    o.insert("display".into(), json!(""));
                }
            }
            cols.push(Json::Object(o));
        }
        rows.push(Json::Array(cols));
    }
    Ok(json!({
        "sheet": sh.name,
        "range": r.to_a1(),
        "rows": rows,
    }))
}

fn set_cells(s: &mut Session, a: &Json) -> Result<Json, String> {
    let (si, r) = sheet_range(s, a)?;
    let rows = a
        .get("values")
        .and_then(Json::as_array)
        .ok_or("missing 2-D array argument 'values'")?;
    let mut total: u64 = 0;
    let mut width = 0usize;
    for row in rows {
        let cols = row
            .as_array()
            .ok_or("'values' must be an array of arrays")?;
        width = width.max(cols.len());
        total = total.saturating_add(cols.len() as u64);
        if total > MAX_TOOL_CELLS {
            return Err(format!(
                "set_cells writes at most {MAX_TOOL_CELLS} cells per call"
            ));
        }
    }
    if total == 0 {
        return Err("'values' is empty".into());
    }
    let last_row = u64::from(r.start.row) + rows.len() as u64 - 1;
    let last_col = u64::from(r.start.col) + width as u64 - 1;
    if last_row >= u64::from(sp_core::MAX_ROWS) || last_col >= u64::from(sp_core::MAX_COLS) {
        return Err("values run past the edge of the sheet".into());
    }
    // Parse everything first so a bad formula leaves the sheet untouched.
    let mut cells = Vec::new();
    for (dr, row) in rows.iter().enumerate() {
        for (dc, v) in row.as_array().into_iter().flatten().enumerate() {
            let at = CellAddr::new(r.start.row + dr as u32, r.start.col + dc as u32);
            let input = match v {
                Json::Null => String::new(),
                Json::String(t) => t.clone(),
                Json::Number(n) => n.to_string(),
                Json::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
                _ => {
                    return Err(format!(
                        "{}: values must be strings, numbers, booleans or null",
                        at.to_a1()
                    ));
                }
            };
            let old = s.wb.sheet(si).and_then(|sh| sh.get(at)).cloned();
            let new =
                sp_engine::commands::cell_cmds::parse_input(&input, old.as_ref(), &mut s.wb.styles)
                    .map_err(|e| format!("{}: {e}", at.to_a1()))?;
            cells.push((at, new));
        }
    }
    let n = cells.len();
    s.transact("Set Cells", |tx, _| {
        for (at, c) in cells {
            tx.set_cell(si, at, c);
        }
        Ok(())
    })
    .map_err(|e| e.to_string())?;
    let end = CellAddr::new(last_row as u32, last_col as u32);
    Ok(json!({ "written": n, "range": CellRange::new(r.start, end).to_a1() }))
}

fn save_workbook(s: &mut Session, a: &Json) -> Result<Json, String> {
    match a.get("path") {
        None | Some(Json::Null) => {
            exec(s, "file.save", &json!({}))?;
        }
        Some(_) => {
            let p = path_arg(a)?;
            exec(s, "file.save_as", &json!({ "path": p }))?;
        }
    }
    Ok(json!({ "saved": s.path.as_ref().map(|p| p.display().to_string()) }))
}

fn sheets(s: &Session) -> Json {
    json!({
        "sheets": s.wb.sheets.iter().enumerate().map(|(i, sh)| json!({
            "index": i,
            "name": sh.name,
            "used_range": sh.used_range().map(|r| r.to_a1()),
            "active": i == s.sheet_index(),
        })).collect::<Vec<_>>(),
        "active_sheet": s.wb.sheet(s.sheet_index()).map(|x| x.name.clone()),
        "selection": s.sel.range().to_a1(),
        "active_cell": s.sel.active.to_a1(),
        "path": s.path.as_ref().map(|p| p.display().to_string()),
        "dirty": s.dirty,
        "can_undo": s.history.can_undo(),
        "can_redo": s.history.can_redo(),
    })
}

/// Standard base64 (RFC 4648) with padding.
pub fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let sym = |i: u32| char::from(T.get((i & 63) as usize).copied().unwrap_or(b'A'));
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = |i: usize| u32::from(chunk.get(i).copied().unwrap_or(0));
        let n = (b(0) << 16) | (b(1) << 8) | b(2);
        out.push(sym(n >> 18));
        out.push(sym(n >> 12));
        out.push(if chunk.len() > 1 { sym(n >> 6) } else { '=' });
        out.push(if chunk.len() > 2 { sym(n) } else { '=' });
    }
    out
}

/// Read one `\n`-terminated line of at most `cap` bytes into `buf`. Returns `Ok(None)` at
/// end of input, `Ok(Some(false))` when the line was too long (it is skipped).
fn read_capped(
    r: &mut impl BufRead,
    buf: &mut Vec<u8>,
    cap: usize,
) -> std::io::Result<Option<bool>> {
    buf.clear();
    let mut too_long = false;
    let mut any = false;
    loop {
        let avail = r.fill_buf()?;
        if avail.is_empty() {
            return Ok(any.then_some(!too_long));
        }
        any = true;
        let (take, done) = match avail.iter().position(|b| *b == b'\n') {
            Some(i) => (i + 1, true),
            None => (avail.len(), false),
        };
        if !too_long {
            if buf.len() + take > cap.saturating_add(1) {
                too_long = true;
                buf.clear();
            } else {
                buf.extend_from_slice(avail.get(..take).unwrap_or(&[]));
            }
        }
        r.consume(take);
        if done {
            return Ok(Some(!too_long));
        }
    }
}

/// Serve until end of input.
pub fn serve(server: &mut Server, input: impl BufRead, mut out: impl Write) -> std::io::Result<()> {
    let mut input = input;
    let mut buf = Vec::new();
    while let Some(fits) = read_capped(&mut input, &mut buf, MAX_MESSAGE_BYTES)? {
        let reply = if fits {
            let line = String::from_utf8_lossy(&buf);
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            server.handle_line(line)
        } else {
            Some(
                rpc_error(
                    Json::Null,
                    INVALID_REQUEST,
                    format!("message larger than {MAX_MESSAGE_BYTES} bytes"),
                )
                .to_string(),
            )
        };
        if let Some(r) = reply {
            writeln!(out, "{r}")?;
            out.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stub(_: &Session, _: (f32, f32)) -> Result<Vec<u8>, String> {
        Ok(vec![0x89, b'P', b'N', b'G'])
    }

    fn call(s: &mut Server, msg: Json) -> Json {
        s.handle(&msg).unwrap()
    }

    fn tool(s: &mut Server, name: &str, args: Json) -> Json {
        call(
            s,
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}}),
        )["result"]
            .clone()
    }

    #[test]
    fn handshake_and_errors() {
        let mut s = Server::new(Session::new(), stub);
        let r = call(
            &mut s,
            json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}),
        );
        assert_eq!(r["result"]["protocolVersion"], "2025-03-26");
        assert!(r["result"]["capabilities"]["tools"].is_object());
        let r = call(
            &mut s,
            json!({"jsonrpc":"2.0","id":"x","method":"initialize","params":{"protocolVersion":"1999-01-01"}}),
        );
        assert_eq!(r["result"]["protocolVersion"], PROTOCOL_VERSIONS[0]);
        assert!(
            s.handle(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
                .is_none()
        );
        assert_eq!(
            call(&mut s, json!({"jsonrpc":"2.0","id":2,"method":"ping"}))["result"],
            json!({})
        );
        assert_eq!(
            call(&mut s, json!({"jsonrpc":"2.0","id":3,"method":"nope"}))["error"]["code"],
            METHOD_NOT_FOUND
        );
        assert_eq!(
            call(&mut s, json!({"jsonrpc":"1.0","id":3,"method":"ping"}))["error"]["code"],
            INVALID_REQUEST
        );
        assert_eq!(
            call(&mut s, json!({"jsonrpc":"2.0","id":[1],"method":"ping"}))["error"]["code"],
            INVALID_REQUEST
        );
        assert_eq!(
            call(
                &mut s,
                json!({"jsonrpc":"2.0","id":4,"method":"ping","params":[1]})
            )["error"]["code"],
            INVALID_PARAMS
        );
        assert_eq!(
            call(
                &mut s,
                json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"zzz"}})
            )["error"]["code"],
            INVALID_PARAMS
        );
        assert!(s.handle_line("{not json").unwrap().contains("-32700"));
        assert!(s.handle_line("[]").unwrap().contains("-32600"));
        let b = s
            .handle_line(
                r#"[{"jsonrpc":"2.0","id":1,"method":"ping"},{"jsonrpc":"2.0","method":"x"}]"#,
            )
            .unwrap();
        assert!(b.starts_with('[') && b.contains("\"id\":1"), "{b}");
        assert!(
            s.handle_line(r#"{"jsonrpc":"2.0","id":9,"result":{}}"#)
                .is_none()
        );
        assert!(s.handle_line("42").unwrap().contains("-32600"));
    }

    #[test]
    fn tools_list_has_valid_schemas() {
        let mut s = Server::new(Session::new(), stub);
        let r = call(
            &mut s,
            json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        );
        let tools = r["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 10);
        for t in tools {
            let sc = &t["inputSchema"];
            assert_eq!(sc["type"], "object", "{t}");
            assert!(sc["properties"].is_object(), "{t}");
            for req in sc["required"].as_array().into_iter().flatten() {
                assert!(sc["properties"].get(req.as_str().unwrap()).is_some(), "{t}");
            }
        }
    }

    #[test]
    fn tools_work_and_fail_softly() {
        let mut s = Server::new(Session::new(), stub);
        let r = tool(
            &mut s,
            "set_cells",
            json!({"range":"B2","values":[["2", 3, true],["=B2*C2", null, "'007"]]}),
        );
        assert_eq!(r["isError"], false, "{r}");
        assert_eq!(r["structuredContent"]["range"], "B2:D3");
        let r = tool(&mut s, "get_cells", json!({"range":"B2:D3"}));
        let rows = &r["structuredContent"]["rows"];
        assert_eq!(rows[1][0]["value"], 6.0);
        assert_eq!(rows[1][0]["formula"], "=B2*C2");
        assert_eq!(rows[0][2]["value"], true);
        assert_eq!(rows[1][2]["value"], "007");
        // One undo step.
        tool(&mut s, "undo", json!({}));
        assert_eq!(
            tool(&mut s, "get_cells", json!({"range":"B3"}))["structuredContent"]["rows"][0][0]["value"],
            Json::Null
        );
        tool(&mut s, "redo", json!({}));
        // Failures are tool errors.
        for (name, args) in [
            ("set_cells", json!({"range":"A1","values":[["=SUM("]]})),
            ("set_cells", json!({"range":"XFD1","values":[[1,2]]})),
            ("set_cells", json!({"range":"A1","values":"x"})),
            ("set_cells", json!({"range":"A1","values":[[{"a":1}]]})),
            ("get_cells", json!({"range":"A:XFD"})),
            ("get_cells", json!({"range":"Nope!A1"})),
            ("run_command", json!({"id":"no.such"})),
            ("run_command", json!({"id":"cell.set","params":5})),
            ("open_workbook", json!({"path":"/nonexistent/x.xlsx"})),
            ("save_workbook", json!({})),
            ("screenshot", json!({"width": 1e9})),
        ] {
            let r = tool(&mut s, name, args.clone());
            assert_eq!(r["isError"], true, "{name} {args} → {r}");
        }
        assert_eq!(
            call(
                &mut s,
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"undo","arguments":7}})
            )["result"]["isError"],
            true
        );
        let big: Vec<Json> = (0..400).map(|_| json!(vec![1; 300])).collect();
        assert_eq!(
            tool(&mut s, "set_cells", json!({"range":"A1","values":big}))["isError"],
            true
        );
        let r = tool(
            &mut s,
            "list_commands",
            json!({"group":"home.font","query":"bold"}),
        );
        assert_eq!(
            r["structuredContent"]["commands"][0]["id"],
            "home.font.bold"
        );
        let r = tool(
            &mut s,
            "run_command",
            json!({"id":"home.font.bold","params":{"range":"B2"}}),
        );
        assert_eq!(r["isError"], false);
        let r = tool(&mut s, "screenshot", json!({}));
        assert_eq!(r["content"][0]["type"], "image");
        assert_eq!(r["content"][0]["data"], "iVBORw==");
        let r = tool(&mut s, "list_sheets", json!({}));
        assert_eq!(r["structuredContent"]["sheets"][0]["used_range"], "B2:D3");
    }

    #[test]
    fn base64_and_capped_reader() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        let data = format!("{}\nshort\n{}", "x".repeat(50), "tail");
        let mut r = std::io::BufReader::with_capacity(8, data.as_bytes());
        let mut buf = Vec::new();
        assert_eq!(read_capped(&mut r, &mut buf, 10).unwrap(), Some(false));
        assert_eq!(read_capped(&mut r, &mut buf, 10).unwrap(), Some(true));
        assert_eq!(buf, b"short\n");
        assert_eq!(read_capped(&mut r, &mut buf, 10).unwrap(), Some(true));
        assert_eq!(buf, b"tail");
        assert_eq!(read_capped(&mut r, &mut buf, 10).unwrap(), None);
    }

    #[test]
    fn serve_loop_replies_per_request() {
        let mut s = Server::new(Session::new(), stub);
        let input = format!(
            "{}\n\n{}\n{}\n",
            r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            "x".repeat(100)
        );
        let mut out = Vec::new();
        serve(&mut s, input.as_bytes(), &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text.lines().count(), 2, "{text}");
    }
}
