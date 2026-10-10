//! Cell entry commands (`cell.*`).

use crate::params::{cell, range, str_opt, str_req};
use crate::{CmdError, CmdResult, CommandSpec, Json, Session};
use serde_json::json;
use sp_core::parse::{FormatHint, parse_numeric_input};
use sp_core::{CellAddr, ErrorKind, Value};
use sp_model::{Cell, Content, StyleId, StyleTable};

/// Interpret typed input the way cell entry does. Returns the new cell (style preserved
/// from `old`) or an error message for an invalid formula.
pub fn parse_input(
    input: &str,
    old: Option<&Cell>,
    styles: &mut StyleTable,
) -> Result<Option<Cell>, CmdError> {
    let style = old.map_or(StyleId::DEFAULT, |c| c.style);
    if input.is_empty() {
        return Ok(if style == StyleId::DEFAULT {
            None
        } else {
            Some(Cell {
                style,
                ..Cell::default()
            })
        });
    }
    if let Some(f) = input.strip_prefix('=').filter(|f| !f.trim().is_empty()) {
        let e = sp_formula::parse(f)
            .map_err(|e| CmdError::Param(format!("There's a problem with this formula: {e}")))?;
        return Ok(Some(Cell {
            content: Content::Formula(sp_formula::to_formula(&e)),
            value: Value::Empty,
            style,
        }));
    }
    if let Some(t) = input.strip_prefix('\'') {
        return Ok(Some(Cell {
            style,
            ..Cell::constant(Value::text(t))
        }));
    }
    if input.chars().count() > sp_core::MAX_TEXT_LEN {
        return Err(CmdError::Param(
            "text is longer than 32,767 characters".into(),
        ));
    }
    let text_fmt = styles.get(style).num_fmt == "@";
    if text_fmt {
        return Ok(Some(Cell {
            style,
            ..Cell::constant(Value::text(input))
        }));
    }
    if input.eq_ignore_ascii_case("TRUE") || input.eq_ignore_ascii_case("FALSE") {
        return Ok(Some(Cell {
            style,
            ..Cell::constant(Value::Bool(input.eq_ignore_ascii_case("TRUE")))
        }));
    }
    if let Some(e) = ErrorKind::parse(input.trim()) {
        return Ok(Some(Cell {
            style,
            ..Cell::constant(Value::Error(e))
        }));
    }
    let year = sp_core::date::serial_to_date(sp_core::date::now_serial()).map_or(2000, |d| d.0);
    if let Some((n, hint)) = parse_numeric_input(input, year) {
        // Typed formats apply only while the cell still has the General format.
        let style = match hint.format_code() {
            Some(code)
                if styles.get(style).num_fmt.eq_ignore_ascii_case("General")
                    && hint != FormatHint::None =>
            {
                styles.derive(style, |s| s.num_fmt = code.to_string())
            }
            _ => style,
        };
        return Ok(Some(Cell {
            style,
            ..Cell::constant(Value::Number(n))
        }));
    }
    Ok(Some(Cell {
        style,
        ..Cell::constant(Value::text(input))
    }))
}

fn set(s: &mut Session, p: &Json) -> CmdResult {
    let (sheet, at) = cell(s, p, "cell")?;
    let input = str_req(p, "input")?.to_string();
    let old = s.wb.sheet(sheet).and_then(|sh| sh.get(at)).cloned();
    let new = parse_input(&input, old.as_ref(), &mut s.wb.styles)?;
    s.transact("Typing", |tx, _| {
        tx.set_cell(sheet, at, new);
        Ok(())
    })?;
    s.view.editing = false;
    let v =
        s.wb.sheet(sheet)
            .map(|sh| sh.value(at).to_display())
            .unwrap_or_default();
    Ok(json!({ "cell": at.to_a1(), "value": v }))
}

fn fill_selection(s: &mut Session, p: &Json) -> CmdResult {
    let (sheet, r) = range(s, p, "range")?;
    let input = str_req(p, "input")?.to_string();
    if r.cell_count() > 1_000_000 {
        return Err(CmdError::Param("selection too large".into()));
    }
    let origin = r.start;
    let mut cells = Vec::new();
    for a in r.iter() {
        let old = s.wb.sheet(sheet).and_then(|sh| sh.get(a)).cloned();
        // Formulas entered with Ctrl+Enter shift relative refs per cell.
        let text = match input.strip_prefix('=') {
            Some(f) => format!(
                "={}",
                sp_ops::edit::shifted_formula(
                    f,
                    i64::from(a.row) - i64::from(origin.row),
                    i64::from(a.col) - i64::from(origin.col)
                )
            ),
            None => input.clone(),
        };
        cells.push((a, parse_input(&text, old.as_ref(), &mut s.wb.styles)?));
    }
    s.transact("Typing", |tx, _| {
        for (a, c) in cells {
            tx.set_cell(sheet, a, c);
        }
        Ok(())
    })?;
    Ok(Json::Null)
}

fn insert_now(s: &mut Session, time: bool) -> CmdResult {
    let now = sp_core::date::now_serial();
    let (v, code) = if time {
        (now.fract(), "h:mm AM/PM")
    } else {
        (now.floor(), "m/d/yyyy")
    };
    let (sheet, at) = (s.sheet_index(), s.sel.active);
    let old_style =
        s.wb.sheet(sheet)
            .and_then(|sh| sh.get(at))
            .map_or(StyleId::DEFAULT, |c| c.style);
    let style = s.wb.styles.derive(old_style, |st| st.num_fmt = code.into());
    s.transact(if time { "Insert Time" } else { "Insert Date" }, |tx, _| {
        tx.set_cell(
            sheet,
            at,
            Some(Cell {
                style,
                ..Cell::constant(Value::Number(v))
            }),
        );
        Ok(())
    })?;
    Ok(Json::Null)
}

fn copy_above(s: &mut Session, formula: bool) -> CmdResult {
    let (sheet, at) = (s.sheet_index(), s.sel.active);
    if at.row == 0 {
        return Err(CmdError::Failed("no cell above".into()));
    }
    let above =
        s.wb.sheet(sheet)
            .and_then(|sh| sh.get(CellAddr::new(at.row - 1, at.col)))
            .cloned();
    let Some(above) = above else {
        return Ok(Json::Null);
    };
    let style =
        s.wb.sheet(sheet)
            .and_then(|sh| sh.get(at))
            .map_or(above.style, |c| c.style);
    let new = if formula {
        Cell {
            style,
            ..above.clone()
        }
    } else {
        Cell {
            style,
            ..Cell::constant(above.value.clone())
        }
    };
    s.transact("Copy from Above", |tx, _| {
        tx.set_cell(sheet, at, Some(new));
        Ok(())
    })?;
    Ok(Json::Null)
}

fn autofill(s: &mut Session, p: &Json) -> CmdResult {
    let (sheet, src) = range(s, p, "source")?;
    let (_, dest) = crate::params::parse_ref(s, str_req(p, "dest")?)?;
    s.transact("AutoFill", |tx, _| {
        sp_ops::edit::autofill(tx, sheet, src, dest).map_err(CmdError::from)
    })?;
    s.select_range(dest, dest.start);
    Ok(Json::Null)
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new(
            "cell.set",
            r#"{"cell"?: "A1", "input": "text, number, or =formula"}"#,
            set,
        ),
        CommandSpec::new("cell.edit", "{}", |s, _| {
            s.view.editing = true;
            Ok(Json::Null)
        }),
        CommandSpec::new("cell.cancel_edit", "{}", |s, _| {
            s.view.editing = false;
            Ok(Json::Null)
        }),
        CommandSpec::new(
            "cell.fill_selection",
            r#"{"range"?: "A1:B5", "input": "…"}"#,
            fill_selection,
        ),
        CommandSpec::new("cell.insert_date", "{}", |s, _| insert_now(s, false)),
        CommandSpec::new("cell.insert_time", "{}", |s, _| insert_now(s, true)),
        CommandSpec::new("cell.copy_above_formula", "{}", |s, _| copy_above(s, true)),
        CommandSpec::new("cell.copy_above_value", "{}", |s, _| copy_above(s, false)),
        CommandSpec::new(
            "cell.autofill",
            r#"{"source"?: "A1:A2", "dest": "A1:A10"}"#,
            autofill,
        ),
        CommandSpec::new(
            "cell.pick_from_list",
            r#"{"cell"?: "A5"} → distinct values from the column above"#,
            |s, p| {
                let (sheet, at) = cell(s, p, "cell")?;
                let mut vals: Vec<String> = Vec::new();
                if let Some(sh) = s.wb.sheet(sheet) {
                    for (_, c) in sh.cells_in(sp_core::CellRange::new(
                        CellAddr::new(0, at.col),
                        CellAddr::new(sp_core::MAX_ROWS - 1, at.col),
                    )) {
                        if let Value::Text(t) = &c.value
                            && !vals.iter().any(|v| v.eq_ignore_ascii_case(t))
                        {
                            vals.push(t.to_string());
                        }
                    }
                }
                vals.sort_by_key(|v| v.to_lowercase());
                Ok(json!(vals))
            },
        ),
        CommandSpec::new(
            "cell.autocomplete",
            r#"{"prefix": "ab"} → completion from the active column or null"#,
            |s, p| {
                let prefix = str_opt(p, "prefix").unwrap_or("").to_lowercase();
                if prefix.is_empty() {
                    return Ok(Json::Null);
                }
                let col = s.sel.active.col;
                let sheet = s.sheet_index();
                let mut hits: Vec<String> = Vec::new();
                if let Some(sh) = s.wb.sheet(sheet) {
                    for (_, c) in sh.cells_in(sp_core::CellRange::new(
                        CellAddr::new(0, col),
                        CellAddr::new(sp_core::MAX_ROWS - 1, col),
                    )) {
                        if let Value::Text(t) = &c.value
                            && t.to_lowercase().starts_with(&prefix)
                            && !hits.iter().any(|h| h.eq_ignore_ascii_case(t))
                        {
                            hits.push(t.to_string());
                        }
                    }
                }
                // Complete only when the match is unambiguous.
                Ok(if hits.len() == 1 {
                    json!(hits.first())
                } else {
                    Json::Null
                })
            },
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_inputs() {
        let mut st = StyleTable::default();
        let c = parse_input("50%", None, &mut st).unwrap().unwrap();
        assert_eq!(c.value, Value::Number(0.5));
        assert_eq!(st.get(c.style).num_fmt, "0%");
        let c = parse_input("'007", None, &mut st).unwrap().unwrap();
        assert_eq!(c.value, Value::text("007"));
        let c = parse_input("true", None, &mut st).unwrap().unwrap();
        assert_eq!(c.value, Value::Bool(true));
        assert!(parse_input("=SUM(", None, &mut st).is_err());
        let c = parse_input("=sum(a1)", None, &mut st).unwrap().unwrap();
        assert_eq!(c.formula_src(), Some("SUM(A1)"));
        assert!(parse_input("", None, &mut st).unwrap().is_none());
        let c = parse_input("#N/A", None, &mut st).unwrap().unwrap();
        assert_eq!(c.value, Value::Error(ErrorKind::NA));
    }
}
