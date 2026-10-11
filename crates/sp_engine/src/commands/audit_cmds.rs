//! Formula auditing and names (`formulas.trace_*`, `formulas.remove_arrows`,
//! `formulas.error_checking`, `formulas.evaluate_formula`, `formulas.watch_window`,
//! `formulas.create_from_selection`) plus the review/audit view state they keep.

use crate::params::{bool_opt, cell, range, str_opt};
use crate::{CmdError, CmdResult, CommandSpec, Json, Session};
use serde::Serialize;
use serde_json::json;
use sp_core::addr::quote_sheet_name;
use sp_core::{CellAddr, CellRange, Value, col_to_letters};
use sp_formula::Expr;
use sp_model::{Content, DefinedName, Workbook};

/// A tracer arrow: from a precedent range to a formula cell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Arrow {
    pub from_sheet: usize,
    pub from: CellRange,
    pub to_sheet: usize,
    pub to: CellAddr,
}

/// Formulas/Review tab state the shell draws (arrows, circles, watch list, comments pane).
#[derive(Debug, Clone, Default, Serialize)]
pub struct ReviewState {
    pub arrows: Vec<Arrow>,
    pub watches: Vec<(usize, CellAddr)>,
    /// Cells circled by `data.circle_invalid`.
    pub circles: Vec<(usize, CellAddr)>,
    pub comments_pane: bool,
    /// Titles of password-protected allow-edit ranges unlocked this session.
    pub unlocked: Vec<String>,
}

/// Cap on formulas scanned by trace dependents / arrows kept.
const MAX_SCAN: usize = 2_000_000;
const MAX_ARROWS: usize = 10_000;

fn ref_text(wb: &Workbook, cur: usize, sheet: usize, r: CellRange) -> String {
    if sheet == cur {
        r.to_a1()
    } else {
        let name = wb.sheet(sheet).map_or("", |s| s.name.as_str());
        format!("{}!{}", quote_sheet_name(name), r.to_a1())
    }
}

fn formula_at(s: &Session, sheet: usize, a: CellAddr) -> Option<String> {
    s.wb.sheet(sheet)?.get(a)?.formula_src().map(str::to_string)
}

/// Direct precedents of the formula in (sheet, a).
fn precedents_of(wb: &Workbook, sheet: usize, a: CellAddr) -> Vec<(usize, CellRange)> {
    let Some(src) = wb.sheet(sheet).and_then(|s| s.get(a)).and_then(|c| c.formula_src()) else {
        return Vec::new();
    };
    let Ok(e) = sp_formula::parse(src) else {
        return Vec::new();
    };
    let p = sp_formula::deps::precedents(wb, sheet, &e);
    p.cells
        .iter()
        .map(|(si, c)| (*si, CellRange::single(*c)))
        .chain(p.ranges.iter().copied())
        .collect()
}

fn push_arrow(s: &mut Session, a: Arrow) {
    if s.view.review.arrows.len() < MAX_ARROWS && !s.view.review.arrows.contains(&a) {
        s.view.review.arrows.push(a);
    }
}

fn trace_precedents(s: &mut Session, p: &Json) -> CmdResult {
    let (sheet, at) = cell(s, p, "cell")?;
    if formula_at(s, sheet, at).is_none() {
        return Err(CmdError::Failed(
            "The active cell doesn't contain a formula that refers to other cells.".into(),
        ));
    }
    let levels = crate::params::u32_opt(p, "levels")?.unwrap_or(1).clamp(1, 16);
    let mut frontier = vec![(sheet, at)];
    let mut out = Vec::new();
    for _ in 0..levels {
        let mut next = Vec::new();
        for (si, a) in frontier {
            for (ps, pr) in precedents_of(&s.wb, si, a) {
                push_arrow(
                    s,
                    Arrow {
                        from_sheet: ps,
                        from: pr,
                        to_sheet: si,
                        to: a,
                    },
                );
                out.push(ref_text(&s.wb, sheet, ps, pr));
                if pr.is_single() {
                    next.push((ps, pr.start));
                }
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    out.dedup();
    Ok(json!(out))
}

/// Formula cells whose precedents include (sheet, a).
pub fn dependents_of(wb: &Workbook, sheet: usize, a: CellAddr) -> Vec<(usize, CellAddr)> {
    let mut out = Vec::new();
    let mut seen = 0usize;
    for (si, sh) in wb.sheets.iter().enumerate() {
        for (fa, c) in sh.iter() {
            let Some(src) = c.formula_src() else {
                continue;
            };
            seen += 1;
            if seen > MAX_SCAN {
                return out;
            }
            let Ok(e) = sp_formula::parse(src) else {
                continue;
            };
            let p = sp_formula::deps::precedents(wb, si, &e);
            if p.cells.contains(&(sheet, a))
                || p.ranges.iter().any(|(rs, r)| *rs == sheet && r.contains(a))
            {
                out.push((si, fa));
            }
        }
    }
    out
}

fn trace_dependents(s: &mut Session, p: &Json) -> CmdResult {
    let (sheet, at) = cell(s, p, "cell")?;
    let deps = dependents_of(&s.wb, sheet, at);
    if deps.is_empty() {
        return Err(CmdError::Failed(
            "The selected cell has no formulas that refer to it.".into(),
        ));
    }
    let mut out = Vec::new();
    for (ds, da) in deps {
        push_arrow(
            s,
            Arrow {
                from_sheet: sheet,
                from: CellRange::single(at),
                to_sheet: ds,
                to: da,
            },
        );
        out.push(ref_text(&s.wb, sheet, ds, CellRange::single(da)));
    }
    Ok(json!(out))
}

fn remove_arrows(s: &mut Session, p: &Json) -> CmdResult {
    let n = s.view.review.arrows.len();
    match str_opt(p, "which") {
        Some("precedents") | Some("dependents") => {
            let (sheet, at) = cell(s, p, "cell")?;
            let prec = str_opt(p, "which") == Some("precedents");
            s.view.review.arrows.retain(|a| {
                if prec {
                    !(a.to_sheet == sheet && a.to == at)
                } else {
                    !(a.from_sheet == sheet && a.from.contains(at))
                }
            });
        }
        _ => s.view.review.arrows.clear(),
    }
    Ok(json!(n - s.view.review.arrows.len()))
}

fn error_checking(s: &mut Session, p: &Json) -> CmdResult {
    let sheet = s.sheet_index();
    let Some(sh) = s.wb.sheet(sheet) else {
        return Err(CmdError::Failed("no sheet".into()));
    };
    let mut found = Vec::new();
    for (a, c) in sh.iter() {
        let issue = match (&c.content, &c.value) {
            (_, Value::Error(e)) => Some(format!("Error {}", e.as_str())),
            (Content::Value(Value::Text(t)), _)
                if sp_formula::eval::text_to_number(t).is_some() && !t.trim().is_empty() =>
            {
                Some("Number Stored as Text".to_string())
            }
            _ => None,
        };
        if let Some(i) = issue {
            found.push((a, i));
            if found.len() >= 10_000 {
                break;
            }
        }
    }
    if found.is_empty() {
        s.status = "The error check is complete for the entire sheet.".into();
        return Ok(json!([]));
    }
    // Move to the next issue after the active cell (row-major, wrapping).
    if bool_opt(p, "select").unwrap_or(true) {
        let cur = s.sel.active;
        let next = found
            .iter()
            .find(|(a, _)| (a.row, a.col) > (cur.row, cur.col))
            .or_else(|| found.first())
            .map(|(a, _)| *a);
        if let Some(a) = next {
            s.select(a);
        }
    }
    Ok(json!(
        found
            .iter()
            .map(|(a, i)| json!({"cell": a.to_a1(), "issue": i}))
            .collect::<Vec<_>>()
    ))
}

fn children(e: &Expr) -> Vec<&Expr> {
    match e {
        Expr::Unary(_, a) | Expr::Paren(a) => vec![a.as_ref()],
        Expr::Binary(_, a, b) => vec![a.as_ref(), b.as_ref()],
        Expr::Call(_, args) => args.iter().collect(),
        _ => Vec::new(),
    }
}

fn child_mut(e: &mut Expr, i: usize) -> Option<&mut Expr> {
    match e {
        Expr::Unary(_, a) | Expr::Paren(a) => (i == 0).then_some(a.as_mut()),
        Expr::Binary(_, a, b) => match i {
            0 => Some(a.as_mut()),
            1 => Some(b.as_mut()),
            _ => None,
        },
        Expr::Call(_, args) => args.get_mut(i),
        _ => None,
    }
}

fn node_at<'a>(e: &'a Expr, path: &[usize]) -> Option<&'a Expr> {
    let mut cur = e;
    for i in path {
        cur = children(cur).get(*i).copied()?;
    }
    Some(cur)
}

fn node_at_mut<'a>(e: &'a mut Expr, path: &[usize]) -> Option<&'a mut Expr> {
    let mut cur = e;
    for i in path {
        cur = child_mut(cur, *i)?;
    }
    Some(cur)
}

/// Nodes the evaluator steps through (post-order): single-cell refs, names, operators and
/// calls. Literals, parentheses and multi-cell ranges are shown as written.
fn reducible(e: &Expr) -> bool {
    match e {
        Expr::Ref(r) => matches!(r.kind, sp_formula::ast::RefKind::Cell(_)),
        Expr::Name(_) | Expr::Unary(..) | Expr::Binary(..) | Expr::Call(..) => true,
        _ => false,
    }
}

fn postorder(e: &Expr, path: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
    if out.len() > 10_000 {
        return;
    }
    for (i, c) in children(e).into_iter().enumerate() {
        path.push(i);
        postorder(c, path, out);
        path.pop();
    }
    if reducible(e) {
        out.push(path.clone());
    }
}

fn literal(v: &Value) -> Expr {
    match v {
        Value::Number(n) => Expr::Number(*n),
        Value::Text(t) => Expr::Text(t.to_string()),
        Value::Bool(b) => Expr::Bool(*b),
        Value::Error(e) => Expr::Error(*e),
        Value::Empty => Expr::Number(0.0),
    }
}

fn shown(v: &Value) -> String {
    match v {
        Value::Text(t) => format!("\"{t}\""),
        Value::Empty => "0".into(),
        other => other.to_display(),
    }
}

/// Step-by-step evaluation: each step shows the formula, the part being evaluated
/// (underlined in the target's dialog) and its result.
fn evaluate_formula(s: &mut Session, p: &Json) -> CmdResult {
    let (sheet, at) = cell(s, p, "cell")?;
    let src = formula_at(s, sheet, at)
        .ok_or_else(|| CmdError::Failed("The active cell doesn't contain a formula.".into()))?;
    if crate::commands::protect_cmds::visible_input(&s.wb, sheet, at).is_empty() {
        return Err(CmdError::Failed(
            "The formula is hidden on this protected sheet.".into(),
        ));
    }
    let orig = sp_formula::parse(&src)
        .map_err(|e| CmdError::Failed(format!("formula does not parse: {e}")))?;
    let mut work = orig.clone();
    let mut paths = Vec::new();
    postorder(&orig, &mut Vec::new(), &mut paths);
    let mut steps = Vec::new();
    for path in paths.iter().take(1_000) {
        let Some(node) = node_at(&orig, path) else {
            continue;
        };
        let text = sp_formula::to_formula(node);
        let v = sp_formula::eval_formula(&s.wb, sheet, at, &text);
        let before = sp_formula::to_formula(&work);
        if let Some(slot) = node_at_mut(&mut work, path) {
            *slot = literal(&v);
        }
        steps.push(json!({ "formula": format!("={before}"), "evaluate": text, "result": shown(&v) }));
    }
    let result = s.wb.sheet(sheet).map(|sh| sh.value(at).clone()).unwrap_or_default();
    Ok(json!({
        "cell": at.to_a1(),
        "formula": format!("={src}"),
        "steps": steps,
        "result": shown(&result),
    }))
}

fn watch_window(s: &mut Session, p: &Json) -> CmdResult {
    let action = str_opt(p, "action").unwrap_or(if p.get("range").is_some() {
        "add"
    } else {
        "list"
    });
    match action {
        "list" => {}
        "clear" => s.view.review.watches.clear(),
        "add" | "remove" => {
            let (sheet, r) = range(s, p, "range")?;
            if r.cell_count() > 1_000 {
                return Err(CmdError::Param("watch at most 1,000 cells at once".into()));
            }
            for a in r.iter() {
                let key = (sheet, a);
                if action == "add" {
                    if !s.view.review.watches.contains(&key) && s.view.review.watches.len() < 10_000
                    {
                        s.view.review.watches.push(key);
                    }
                } else {
                    s.view.review.watches.retain(|w| *w != key);
                }
            }
        }
        other => {
            return Err(CmdError::Param(format!(
                "unknown action '{other}' (list, add, remove, clear)"
            )));
        }
    }
    let rows: Vec<Json> = s
        .view
        .review
        .watches
        .iter()
        .filter_map(|(si, a)| {
            let sh = s.wb.sheet(*si)?;
            let c = sh.get(*a);
            Some(json!({
                "sheet": sh.name,
                "cell": a.to_a1(),
                "value": sh.value(*a).to_display(),
                "formula": c.and_then(|c| c.formula_src()).map(|f| format!("={f}")),
            }))
        })
        .collect();
    Ok(Json::Array(rows))
}

/// A defined-name candidate from header text (`Unit Price` → `Unit_Price`).
pub fn name_from_label(text: &str) -> Option<String> {
    let mut n: String = text
        .trim()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' || c == '.' || c == '\\' {
                c
            } else {
                '_'
            }
        })
        .take(255)
        .collect();
    if n.is_empty() {
        return None;
    }
    if n.chars().next().is_some_and(|c| c.is_ascii_digit() || c == '.') {
        n.insert(0, '_');
    }
    let looks_like_ref = CellAddr::parse(&n).is_ok()
        || n.eq_ignore_ascii_case("r")
        || n.eq_ignore_ascii_case("c")
        || n.eq_ignore_ascii_case("true")
        || n.eq_ignore_ascii_case("false");
    if looks_like_ref {
        n.push('_');
    }
    Some(n)
}

/// `'Sheet 1'!$B$2:$B$9`.
pub fn abs_ref(sheet: &str, r: CellRange) -> String {
    let a = |x: CellAddr| format!("${}${}", col_to_letters(x.col), x.row + 1);
    let body = if r.is_single() {
        a(r.start)
    } else {
        format!("{}:{}", a(r.start), a(r.end))
    };
    format!("{}!{body}", quote_sheet_name(sheet))
}

fn create_from_selection(s: &mut Session, p: &Json) -> CmdResult {
    let (sheet, r) = range(s, p, "range")?;
    if r.cell_count() > 1_000_000 || r.is_single() {
        return Err(CmdError::Param(
            "select a block with labels in its first row or column".into(),
        ));
    }
    let sh = s.wb.sheet(sheet).ok_or_else(|| CmdError::Failed("no sheet".into()))?;
    let is_text = |a: CellAddr| matches!(sh.value(a), Value::Text(_));
    let any = ["top", "left", "bottom", "right"]
        .iter()
        .any(|k| p.get(*k).is_some());
    let (top, left, bottom, right) = if any {
        (
            bool_opt(p, "top").unwrap_or(false),
            bool_opt(p, "left").unwrap_or(false),
            bool_opt(p, "bottom").unwrap_or(false),
            bool_opt(p, "right").unwrap_or(false),
        )
    } else {
        let top = r.rows() > 1
            && (r.start.col..=r.end.col).all(|c| is_text(CellAddr::new(r.start.row, c)));
        let first = if top { r.start.row + 1 } else { r.start.row };
        let left = r.cols() > 1
            && first <= r.end.row
            && (first..=r.end.row).all(|row| is_text(CellAddr::new(row, r.start.col)));
        (top, left, false, false)
    };
    let row0 = r.start.row + u32::from(top);
    let row1 = r.end.row.saturating_sub(u32::from(bottom));
    let col0 = r.start.col + u32::from(left);
    let col1 = r.end.col.saturating_sub(u32::from(right));
    if row0 > row1 || col0 > col1 {
        return Err(CmdError::Param("no cells left to name".into()));
    }
    let mut names: Vec<(String, CellRange)> = Vec::new();
    let mut label = |a: CellAddr, target: CellRange| {
        if let Some(n) = name_from_label(&sh.value(a).to_display()) {
            names.retain(|(x, _)| !x.eq_ignore_ascii_case(&n));
            names.push((n, target));
        }
    };
    for c in col0..=col1 {
        let target = CellRange::new(CellAddr::new(row0, c), CellAddr::new(row1, c));
        if top {
            label(CellAddr::new(r.start.row, c), target);
        }
        if bottom {
            label(CellAddr::new(r.end.row, c), target);
        }
    }
    for row in row0..=row1 {
        let target = CellRange::new(CellAddr::new(row, col0), CellAddr::new(row, col1));
        if left {
            label(CellAddr::new(row, r.start.col), target);
        }
        if right {
            label(CellAddr::new(row, r.end.col), target);
        }
    }
    if names.len() > 10_000 {
        return Err(CmdError::Param("too many names".into()));
    }
    let sheet_name = sh.name.clone();
    let created: Vec<String> = names.iter().map(|(n, _)| n.clone()).collect();
    s.transact("Create Names", |tx, _| {
        tx.touch_book();
        for (n, target) in names {
            tx.wb
                .names
                .retain(|d| !(d.scope.is_none() && d.name.eq_ignore_ascii_case(&n)));
            tx.wb.names.push(DefinedName {
                name: n,
                formula: abs_ref(&sheet_name, target),
                scope: None,
            });
        }
        Ok(())
    })?;
    Ok(json!(created))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new(
            "formulas.trace_precedents",
            r#"{"cell"?: "C5", "levels"?: 1} → precedent references"#,
            trace_precedents,
        ),
        CommandSpec::new(
            "formulas.trace_dependents",
            r#"{"cell"?: "A1"} → dependent formula cells"#,
            trace_dependents,
        ),
        CommandSpec::new(
            "formulas.remove_arrows",
            r#"{"which"?: "all|precedents|dependents", "cell"?: "C5"}"#,
            remove_arrows,
        ),
        CommandSpec::new(
            "formulas.error_checking",
            r#"{"select"?: true} → [{"cell", "issue"}], selects the next one"#,
            error_checking,
        ),
        CommandSpec::new(
            "formulas.evaluate_formula",
            r#"{"cell"?: "C5"} → {"steps": [{"formula", "evaluate", "result"}], "result"}"#,
            evaluate_formula,
        ),
        CommandSpec::new(
            "formulas.watch_window",
            r#"{"action"?: "list|add|remove|clear", "range"?: "B2:B4"}"#,
            watch_window,
        ),
        CommandSpec::new(
            "formulas.create_from_selection",
            r#"{"range"?: "A1:C10", "top"?: bool, "left"?: bool, "bottom"?: bool, "right"?: bool} (default: detect labels)"#,
            create_from_selection,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(cells: &[(&str, &str)]) -> Session {
        let mut s = Session::new();
        for (a, v) in cells {
            s.execute("cell.set", &json!({"cell": a, "input": v}))
                .unwrap();
        }
        s
    }

    #[test]
    fn trace_and_remove_arrows() {
        let mut s = session(&[
            ("A1", "1"),
            ("A2", "2"),
            ("B1", "=A1+SUM(A1:A2)"),
            ("C1", "=B1*2"),
        ]);
        let r = s
            .execute("formulas.trace_precedents", &json!({"cell": "C1", "levels": 2}))
            .unwrap();
        assert_eq!(r, json!(["B1", "A1", "A1:A2"]));
        assert_eq!(s.view.review.arrows.len(), 3);
        let d = s
            .execute("formulas.trace_dependents", &json!({"cell": "A2"}))
            .unwrap();
        assert_eq!(d, json!(["B1"]));
        assert!(
            s.execute("formulas.trace_precedents", &json!({"cell": "A1"}))
                .is_err()
        );
        s.execute("formulas.remove_arrows", &json!({})).unwrap();
        assert!(s.view.review.arrows.is_empty());
    }

    #[test]
    fn evaluate_steps() {
        let mut s = session(&[("A1", "3"), ("A2", "4"), ("B1", "=A1*2+SUM(A1:A2)")]);
        let r = s
            .execute("formulas.evaluate_formula", &json!({"cell": "B1"}))
            .unwrap();
        let steps = r["steps"].as_array().unwrap();
        let first = &steps[0];
        assert_eq!(first["evaluate"], "A1");
        assert_eq!(first["result"], "3");
        assert_eq!(steps[1]["formula"], "=3*2+SUM(A1:A2)");
        assert_eq!(steps.last().unwrap()["result"], "13");
        assert_eq!(r["result"], "13");
    }

    #[test]
    fn errors_watch_and_names() {
        let mut s = session(&[("A1", "=1/0"), ("B2", "'42"), ("C3", "ok")]);
        let r = s.execute("formulas.error_checking", &json!({})).unwrap();
        assert_eq!(r.as_array().unwrap().len(), 2);
        assert_eq!(s.sel.active, CellAddr::new(1, 1));
        let w = s
            .execute("formulas.watch_window", &json!({"action": "add", "range": "A1:B1"}))
            .unwrap();
        assert_eq!(w[0]["value"], "#DIV/0!");
        assert_eq!(w.as_array().unwrap().len(), 2);

        let mut s = session(&[
            ("A1", "Item"),
            ("B1", "Unit Price"),
            ("A2", "x"),
            ("B2", "3"),
            ("A3", "y"),
            ("B3", "4"),
        ]);
        let r = s
            .execute(
                "formulas.create_from_selection",
                &json!({"range": "A1:B3", "top": true}),
            )
            .unwrap();
        assert_eq!(r, json!(["Item", "Unit_Price"]));
        s.execute("cell.set", &json!({"cell": "D1", "input": "=SUM(Unit_Price)"}))
            .unwrap();
        assert_eq!(s.wb.sheets[0].value(CellAddr::new(0, 3)), &Value::Number(7.0));
        assert_eq!(name_from_label("2024 total"), Some("_2024_total".into()));
        assert_eq!(name_from_label("A1"), Some("A1_".into()));
    }
}
