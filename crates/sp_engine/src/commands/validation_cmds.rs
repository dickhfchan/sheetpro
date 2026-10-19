//! Data validation (`data.validation`, `data.circle_invalid`) and the entry check
//! `Session::transact` runs after typing commands.

use crate::params::{bool_opt, range, str_opt};
use crate::{CmdError, CmdResult, CommandSpec, Json, Session};
use serde_json::json;
use sp_core::{CellAddr, CellRange, Value};
use sp_formula::Expr;
use sp_formula::ast::{RefExpr, RefKind};
use sp_model::{DataValidation, ErrorStyle, Sheet, ValidationKind, ValidationOp, Workbook};

/// Commands whose changed cells are checked against validation rules (typing, not paste).
pub fn is_typing_command(id: &str) -> bool {
    matches!(
        id,
        "cell.set"
            | "cell.fill_selection"
            | "cell.insert_date"
            | "cell.insert_time"
            | "cell.copy_above_formula"
            | "cell.copy_above_value"
    )
}

/// Cap on cells examined by list sources and circle-invalid.
const MAX_SCAN: u64 = 1_000_000;

/// Resolve a reference node to (sheet, range).
pub fn ref_range(wb: &Workbook, cur: usize, r: &RefExpr) -> Option<(usize, CellRange)> {
    let sheet = match &r.sheet {
        None => cur,
        Some(n) => wb.sheet_index(n)?,
    };
    let rg = match &r.kind {
        RefKind::Cell(c) => CellRange::single(CellAddr::new(c.row, c.col)),
        RefKind::Area(a, b) => {
            CellRange::new(CellAddr::new(a.row, a.col), CellAddr::new(b.row, b.col))
        }
        RefKind::Cols { c0, c1, .. } => CellRange::new(
            CellAddr::new(0, *c0),
            CellAddr::new(sp_core::MAX_ROWS - 1, *c1),
        ),
        RefKind::Rows { r0, r1, .. } => CellRange::new(
            CellAddr::new(*r0, 0),
            CellAddr::new(*r1, sp_core::MAX_COLS - 1),
        ),
    };
    Some((sheet, rg))
}

/// `src` (no leading `=`) rewritten for cell `at` of a rule anchored at `anchor`.
fn shifted(src: &str, anchor: CellAddr, at: CellAddr) -> Option<Expr> {
    let mut e = sp_formula::parse(src.trim().trim_start_matches('=')).ok()?;
    sp_formula::shift::shift_for_copy(
        &mut e,
        i64::from(at.row) - i64::from(anchor.row),
        i64::from(at.col) - i64::from(anchor.col),
    );
    Some(e)
}

fn eval_at(wb: &Workbook, sheet: usize, rule: &DataValidation, at: CellAddr, src: &str) -> Value {
    match shifted(src, rule.anchor(), at) {
        Some(e) => sp_formula::eval_formula(wb, sheet, at, &sp_formula::to_formula(&e)),
        None => Value::Error(sp_core::ErrorKind::Name),
    }
}

/// Items of a list rule: a quoted constant list or a range / name.
pub fn list_items(wb: &Workbook, sheet: usize, rule: &DataValidation, at: CellAddr) -> Vec<String> {
    let f = rule.formula1.trim().trim_start_matches('=');
    if let Some(inner) = f.strip_prefix('"') {
        let inner = inner.strip_suffix('"').unwrap_or(inner);
        return inner.split(',').map(|x| x.trim().to_string()).collect();
    }
    let mut expr = shifted(f, rule.anchor(), at);
    // A defined name: follow it once.
    if let Some(Expr::Name(n)) = &expr
        && let Some(dn) = wb.name_lookup(n, sheet)
    {
        expr = sp_formula::parse(&dn.formula).ok();
    }
    match expr {
        Some(Expr::Ref(r)) => {
            let Some((si, rg)) = ref_range(wb, sheet, &r) else {
                return Vec::new();
            };
            let Some(sh) = wb.sheet(si) else {
                return Vec::new();
            };
            if rg.cell_count() > MAX_SCAN {
                return Vec::new();
            }
            sh.cells_in(rg)
                .map(|(_, c)| c.value.to_display())
                .filter(|t| !t.is_empty())
                .collect()
        }
        // Anything else: evaluate and split its text.
        _ => eval_at(wb, sheet, rule, at, f)
            .to_display()
            .split(',')
            .map(|x| x.trim().to_string())
            .collect(),
    }
}

fn bound(
    wb: &Workbook,
    sheet: usize,
    rule: &DataValidation,
    at: CellAddr,
    src: &str,
) -> Option<f64> {
    if src.trim().is_empty() {
        return None;
    }
    match eval_at(wb, sheet, rule, at, src) {
        Value::Number(n) => Some(n),
        Value::Bool(b) => Some(f64::from(u8::from(b))),
        Value::Text(t) => sp_formula::eval::text_to_number(&t),
        _ => None,
    }
}

/// Does the value now in `at` satisfy `rule`?
pub fn is_valid(wb: &Workbook, sheet: usize, rule: &DataValidation, at: CellAddr) -> bool {
    let Some(sh) = wb.sheet(sheet) else {
        return true;
    };
    let v = sh.value(at);
    if v.is_empty() {
        return rule.allow_blank || rule.kind == ValidationKind::Any;
    }
    let num_test = |x: f64| {
        let Some(a) = bound(wb, sheet, rule, at, &rule.formula1) else {
            return false;
        };
        let b = bound(wb, sheet, rule, at, &rule.formula2).unwrap_or(a);
        rule.op.test(x, a, b)
    };
    match rule.kind {
        ValidationKind::Any => true,
        ValidationKind::Whole => matches!(v, Value::Number(n) if n.fract() == 0.0 && num_test(*n)),
        ValidationKind::Decimal | ValidationKind::Date => {
            matches!(v, Value::Number(n) if num_test(*n))
        }
        ValidationKind::Time => {
            matches!(v, Value::Number(n) if (0.0..1.0).contains(n) && num_test(*n))
        }
        ValidationKind::TextLength => num_test(v.to_display().chars().count() as f64),
        ValidationKind::List => {
            let t = v.to_display();
            list_items(wb, sheet, rule, at)
                .iter()
                .any(|i| i.eq_ignore_ascii_case(&t))
        }
        ValidationKind::Custom => match eval_at(wb, sheet, rule, at, &rule.formula1) {
            Value::Bool(b) => b,
            Value::Number(n) => n != 0.0,
            _ => false,
        },
    }
}

/// After a typing command: `Err` for a Stop rule violation, `Ok(Some(msg))` for warnings.
pub fn check_entries(s: &Session, cells: &[(usize, CellAddr)]) -> Result<Option<String>, CmdError> {
    let mut warning = None;
    for (si, a) in cells {
        let Some(rule) = s.wb.sheet(*si).and_then(|sh| sh.validation_at(*a)) else {
            continue;
        };
        if is_valid(&s.wb, *si, rule, *a) {
            continue;
        }
        let msg = if rule.error.is_empty() {
            "This value doesn't match the data validation restrictions defined for this cell."
                .to_string()
        } else {
            rule.error.clone()
        };
        match rule.error_style {
            ErrorStyle::Stop if rule.show_error => return Err(CmdError::Failed(msg)),
            _ => warning = Some(msg),
        }
    }
    Ok(warning)
}

/// Stored cells violating their sheet's rules (capped scan).
pub fn invalid_cells(wb: &Workbook, sheet: usize) -> Vec<CellAddr> {
    let Some(sh) = wb.sheet(sheet) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut seen = 0u64;
    for (a, _) in sh.iter() {
        seen += 1;
        if seen > MAX_SCAN {
            break;
        }
        if let Some(rule) = sh.validation_at(a)
            && !is_valid(wb, sheet, rule, a)
        {
            out.push(a);
        }
    }
    out
}

/// `r` minus `cut`: up to four rectangles.
pub fn subtract(r: CellRange, cut: CellRange) -> Vec<CellRange> {
    let Some(i) = r.intersect(&cut) else {
        return vec![r];
    };
    let mut out = Vec::new();
    if i.start.row > r.start.row {
        out.push(CellRange::new(
            r.start,
            CellAddr::new(i.start.row - 1, r.end.col),
        ));
    }
    if i.end.row < r.end.row {
        out.push(CellRange::new(
            CellAddr::new(i.end.row + 1, r.start.col),
            r.end,
        ));
    }
    if i.start.col > r.start.col {
        out.push(CellRange::new(
            CellAddr::new(i.start.row, r.start.col),
            CellAddr::new(i.end.row, i.start.col - 1),
        ));
    }
    if i.end.col < r.end.col {
        out.push(CellRange::new(
            CellAddr::new(i.start.row, i.end.col + 1),
            CellAddr::new(i.end.row, r.end.col),
        ));
    }
    out
}

/// Remove `cut` from every rule's ranges (rules left empty are dropped).
fn carve(sh: &mut Sheet, cut: CellRange) {
    for v in &mut sh.validations {
        v.ranges = v.ranges.iter().flat_map(|r| subtract(*r, cut)).collect();
    }
    sh.validations.retain(|v| !v.ranges.is_empty());
}

/// Accept `10`, `=A1`, `2024-01-31`, `1/31/2024`, `13:30` as a rule operand.
fn operand(p: &Json, keys: &[&str], kind: ValidationKind) -> Result<String, CmdError> {
    let Some(v) = keys.iter().find_map(|k| p.get(*k)) else {
        return Ok(String::new());
    };
    let text = match v {
        Json::Number(n) => return Ok(n.to_string()),
        Json::String(t) => t.trim().to_string(),
        Json::Null => return Ok(String::new()),
        _ => return Err(CmdError::Param("rule operands are numbers or text".into())),
    };
    if let Some(f) = text.strip_prefix('=') {
        sp_formula::parse(f).map_err(|e| CmdError::Param(format!("bad formula '{text}': {e}")))?;
        return Ok(f.to_string());
    }
    if matches!(kind, ValidationKind::Date | ValidationKind::Time) && text.parse::<f64>().is_err() {
        let year = 2000;
        if let Some((n, _)) = sp_core::parse::parse_numeric_input(&text, year) {
            return Ok(sp_core::value::format_general(n));
        }
    }
    if text.parse::<f64>().is_err() && !text.is_empty() {
        return Err(CmdError::Param(format!(
            "'{text}' is not a number (use =formula for references)"
        )));
    }
    Ok(text)
}

fn set_validation(s: &mut Session, p: &Json) -> CmdResult {
    let (sheet, r) = range(s, p, "range")?;
    let clear = bool_opt(p, "clear").unwrap_or(false);
    let kind_txt = str_opt(p, "type").unwrap_or(if clear { "any" } else { "" });
    if kind_txt.is_empty() {
        // No rule given: report the rule at the active cell.
        let rule = s.wb.sheet(sheet).and_then(|sh| sh.validation_at(r.start));
        return Ok(match rule {
            Some(v) => json!({
                "type": v.kind.as_str(), "operator": v.op.as_str(),
                "formula1": v.formula1, "formula2": v.formula2,
                "ranges": v.ranges.iter().map(CellRange::to_a1).collect::<Vec<_>>(),
            }),
            None => Json::Null,
        });
    }
    let kind = ValidationKind::parse(kind_txt)
        .ok_or_else(|| CmdError::Param(format!("unknown validation type '{kind_txt}'")))?;
    let op = match str_opt(p, "operator") {
        Some(o) => ValidationOp::parse(o)
            .ok_or_else(|| CmdError::Param(format!("unknown operator '{o}'")))?,
        None => ValidationOp::Between,
    };
    let (formula1, formula2) = match kind {
        ValidationKind::List => {
            let src = match p.get("source").or_else(|| p.get("formula1")) {
                Some(Json::Array(items)) => {
                    let items: Vec<String> = items
                        .iter()
                        .map(|i| i.as_str().map_or_else(|| i.to_string(), str::to_string))
                        .collect();
                    format!("\"{}\"", items.join(","))
                }
                Some(Json::String(t)) if t.trim_start().starts_with('=') => {
                    let f = t.trim().trim_start_matches('=');
                    sp_formula::parse(f)
                        .map_err(|e| CmdError::Param(format!("bad source '{t}': {e}")))?;
                    f.to_string()
                }
                Some(Json::String(t)) => format!("\"{}\"", t.replace('"', "")),
                _ => return Err(CmdError::Param("list rules need 'source'".into())),
            };
            if src.len() > 255 + 2 && src.starts_with('"') {
                return Err(CmdError::Param(
                    "list source is longer than 255 characters".into(),
                ));
            }
            (src, String::new())
        }
        ValidationKind::Custom => {
            let f = str_opt(p, "formula")
                .or_else(|| str_opt(p, "formula1"))
                .ok_or_else(|| CmdError::Param("custom rules need 'formula'".into()))?;
            let f = f.trim().trim_start_matches('=');
            sp_formula::parse(f).map_err(|e| CmdError::Param(format!("bad formula: {e}")))?;
            (f.to_string(), String::new())
        }
        ValidationKind::Any => (String::new(), String::new()),
        _ => {
            let a = operand(p, &["formula1", "min", "value"], kind)?;
            let b = operand(p, &["formula2", "max"], kind)?;
            if a.is_empty() {
                return Err(CmdError::Param("missing 'min'/'value'".into()));
            }
            if matches!(op, ValidationOp::Between | ValidationOp::NotBetween) && b.is_empty() {
                return Err(CmdError::Param("between rules need 'max'".into()));
            }
            (a, b)
        }
    };
    let error_style = match str_opt(p, "error_style").unwrap_or("stop") {
        "stop" => ErrorStyle::Stop,
        "warning" => ErrorStyle::Warning,
        "information" | "info" => ErrorStyle::Information,
        o => return Err(CmdError::Param(format!("unknown error_style '{o}'"))),
    };
    let text = |k: &str| {
        str_opt(p, k)
            .unwrap_or("")
            .chars()
            .take(255)
            .collect::<String>()
    };
    let rule = DataValidation {
        ranges: vec![r],
        kind,
        op,
        formula1,
        formula2,
        allow_blank: bool_opt(p, "allow_blank").unwrap_or(true),
        dropdown: bool_opt(p, "dropdown").unwrap_or(true),
        error_style,
        show_error: bool_opt(p, "show_error").unwrap_or(true),
        error_title: text("error_title"),
        error: text("error"),
        show_prompt: p.get("prompt").is_some(),
        prompt_title: text("prompt_title"),
        prompt: text("prompt"),
    };
    let count = s.wb.sheet(sheet).map_or(0, |sh| sh.validations.len());
    if count >= 65_534 {
        return Err(CmdError::Failed("too many validation rules".into()));
    }
    s.transact("Data Validation", |tx, _| {
        tx.touch_sheet(sheet);
        let sh = tx
            .wb
            .sheet_mut(sheet)
            .ok_or_else(|| CmdError::Failed("no sheet".into()))?;
        carve(sh, r);
        if rule.kind != ValidationKind::Any {
            sh.validations.push(rule);
        }
        Ok(())
    })?;
    Ok(json!({ "range": r.to_a1(), "type": kind.as_str() }))
}

fn circle_invalid(s: &mut Session, p: &Json) -> CmdResult {
    let sheet = s.sheet_index();
    if bool_opt(p, "clear").unwrap_or(false) {
        s.view.review.circles.clear();
        return Ok(json!([]));
    }
    let bad = invalid_cells(&s.wb, sheet);
    s.view.review.circles = bad.iter().map(|a| (sheet, *a)).collect();
    s.status = format!("{} invalid cells circled", bad.len());
    Ok(json!(bad.iter().map(|a| a.to_a1()).collect::<Vec<_>>()))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new(
            "data.validation",
            r#"{"range"?: "A1:A9", "type": "whole|decimal|list|date|time|length|custom|any", "operator"?: "between|notBetween|equal|notEqual|greaterThan|lessThan|greaterThanOrEqual|lessThanOrEqual", "min"?: 1, "max"?: 10, "source"?: "a,b,c" | ["a","b"] | "=$D$1:$D$5", "formula"?: "=A1>0", "allow_blank"?: true, "error_style"?: "stop|warning|information", "error"?: "msg", "prompt"?: "msg", "clear"?: bool} — no type: report the rule"#,
            set_validation,
        ),
        CommandSpec::new("data.circle_invalid", r#"{"clear"?: bool}"#, circle_invalid),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(s: &mut Session, a: &str, v: &str) -> CmdResult {
        s.execute("cell.set", &json!({"cell": a, "input": v}))
    }

    #[test]
    fn whole_number_rule_rejects_typing() {
        let mut s = Session::new();
        s.execute(
            "data.validation",
            &json!({"range": "A1:A10", "type": "whole", "min": 1, "max": 10}),
        )
        .unwrap();
        set(&mut s, "A1", "5").unwrap();
        assert!(set(&mut s, "A2", "11").is_err());
        assert!(set(&mut s, "A3", "2.5").is_err());
        assert!(set(&mut s, "A4", "abc").is_err());
        assert!(s.wb.sheets[0].get(CellAddr::new(1, 0)).is_none());
        // Outside the rule: anything goes.
        set(&mut s, "B1", "99").unwrap();
        // Formula entries validate by result.
        assert!(set(&mut s, "A5", "=B1").is_err());
        set(&mut s, "A5", "=A1*2").unwrap();
        // Paste Values bypasses validation (and keeps the rule), as in the target app;
        // circle_invalid finds it. (A plain paste replaces the rule with the source's.)
        s.execute("home.clipboard.copy", &json!({"range": "B1"}))
            .unwrap();
        s.execute("home.clipboard.paste_values", &json!({"cell": "A6"}))
            .unwrap();
        let r = s.execute("data.circle_invalid", &json!({})).unwrap();
        assert_eq!(r, json!(["A6"]));
        // Undo history holds no failed entries.
        s.execute("edit.undo", &json!({})).unwrap();
        assert!(s.wb.sheets[0].get(CellAddr::new(5, 0)).is_none());
    }

    #[test]
    fn list_length_custom_and_warning() {
        let mut s = Session::new();
        set(&mut s, "D1", "Red").unwrap();
        set(&mut s, "D2", "Blue").unwrap();
        s.execute(
            "data.validation",
            &json!({"range": "A1:A5", "type": "list", "source": "=$D$1:$D$2"}),
        )
        .unwrap();
        set(&mut s, "A1", "blue").unwrap();
        assert!(set(&mut s, "A2", "Green").is_err());
        s.execute(
            "data.validation",
            &json!({"range": "B1:B5", "type": "list", "source": ["x", "y"]}),
        )
        .unwrap();
        set(&mut s, "B1", "y").unwrap();
        assert!(set(&mut s, "B2", "z").is_err());
        s.execute(
            "data.validation",
            &json!({"range": "C1:C5", "type": "length", "operator": "lessThanOrEqual", "value": 3}),
        )
        .unwrap();
        set(&mut s, "C1", "abc").unwrap();
        assert!(set(&mut s, "C2", "abcd").is_err());
        s.execute(
            "data.validation",
            &json!({"range": "E1:E5", "type": "custom", "formula": "=E1>D1", "error_style": "warning"}),
        )
        .unwrap();
        // Warning style: accepted, status explains.
        set(&mut s, "E1", "0").unwrap();
        assert!(s.status.contains("validation"));
        s.execute(
            "data.validation",
            &json!({"range": "F1", "type": "date", "operator": "greaterThan", "value": "2020-01-01"}),
        )
        .unwrap();
        assert!(set(&mut s, "F1", "2019-05-05").is_err());
        set(&mut s, "F1", "2021-05-05").unwrap();
        // Clearing part of a rule carves its range.
        s.execute("data.validation", &json!({"range": "A2:A5", "clear": true}))
            .unwrap();
        set(&mut s, "A3", "Green").unwrap();
        assert!(set(&mut s, "A1", "Green").is_err());
    }

    #[test]
    fn subtract_ranges() {
        let r = CellRange::parse("A1:C3").unwrap();
        let parts = subtract(r, CellRange::parse("B2").unwrap());
        assert_eq!(parts.iter().map(|x| x.cell_count()).sum::<u64>(), 8);
        assert!(subtract(r, CellRange::parse("A1:Z9").unwrap()).is_empty());
    }
}
