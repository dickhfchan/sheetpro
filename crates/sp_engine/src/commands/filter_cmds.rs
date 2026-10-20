//! AutoFilter and Advanced Filter (`data.filter`, `home.editing.filter`, `data.filter_clear`,
//! `data.filter_reapply`, `data.advanced_filter`). Filtering hides the rows that fail.

use crate::params::{bool_opt, cell, f64_opt, parse_ref, range, str_opt};
use crate::{CmdError, CmdResult, CommandSpec, Json, Session};
use serde_json::json;
use sp_core::{CellAddr, CellRange, Value};
use sp_model::{
    AutoFilter, Cell, DefinedName, FilterColumn, FilterCond, FilterCriteria, Sheet, Workbook,
};
use std::collections::HashSet;

/// Rows a filter may span (keeps hidden-row bookkeeping bounded).
const MAX_FILTER_ROWS: u32 = 1_048_576;
const FILTER_DB: &str = "_xlnm._FilterDatabase";

/// Formatted text of a cell, as the filter dropdown lists it.
pub fn cell_text(wb: &Workbook, sh: &Sheet, a: CellAddr) -> String {
    match sh.get(a) {
        None => String::new(),
        Some(c) => {
            sp_render::display_cell(a, &c.value, c.formula_src(), wb.styles.get(c.style), false)
                .text
        }
    }
}

/// `*`/`?` wildcard match, case-insensitive; `~` escapes.
pub fn wildcard_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    // Iterative matcher with single-star backtracking.
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        let pc = p.get(pi).copied();
        let tc = t.get(ti).copied();
        match pc {
            Some('~') if p.get(pi + 1).is_some_and(|x| Some(*x) == tc) => {
                pi += 2;
                ti += 1;
            }
            Some('*') => {
                star = Some((pi, ti));
                pi += 1;
            }
            Some('?') => {
                pi += 1;
                ti += 1;
            }
            Some(c) if Some(c) == tc => {
                pi += 1;
                ti += 1;
            }
            _ => match star {
                Some((sp, st)) => {
                    pi = sp + 1;
                    ti = st + 1;
                    star = Some((sp, st + 1));
                }
                None => return false,
            },
        }
    }
    while p.get(pi) == Some(&'*') {
        pi += 1;
    }
    pi == p.len()
}

/// Does value `v` (display text `text`) satisfy `op val`?
pub fn compare_cond(v: &Value, text: &str, op: &str, val: &str) -> bool {
    let num = val.trim().parse::<f64>().ok();
    if let (Some(x), Some(y)) = (v.as_number().filter(|_| matches!(v, Value::Number(_))), num) {
        return match op {
            "equal" => x == y,
            "notEqual" => x != y,
            "greaterThan" => x > y,
            "greaterThanOrEqual" => x >= y,
            "lessThan" => x < y,
            "lessThanOrEqual" => x <= y,
            _ => false,
        };
    }
    let (a, b) = (text.to_lowercase(), val.to_lowercase());
    match op {
        "equal" => {
            if val.is_empty() {
                text.is_empty()
            } else {
                wildcard_match(val, text)
            }
        }
        "notEqual" => {
            if val.is_empty() {
                !text.is_empty()
            } else {
                !wildcard_match(val, text)
            }
        }
        // Text never compares with numbers.
        _ if num.is_some() || matches!(v, Value::Number(_)) || text.is_empty() => false,
        "greaterThan" => a > b,
        "greaterThanOrEqual" => a >= b,
        "lessThan" => a < b,
        "lessThanOrEqual" => a <= b,
        _ => false,
    }
}

/// Rows of `af` (excluding the header) that pass every column's criteria.
fn passing_rows(wb: &Workbook, sh: &Sheet, af: &AutoFilter) -> Vec<bool> {
    let r = af.range;
    let first = r.start.row + 1;
    if first > r.end.row {
        return Vec::new();
    }
    let n = (r.end.row - first + 1) as usize;
    let mut pass = vec![true; n];
    for fc in &af.columns {
        let col = r.start.col.saturating_add(fc.col);
        if col > r.end.col {
            continue;
        }
        let at = |row: u32| CellAddr::new(row, col);
        match &fc.criteria {
            FilterCriteria::Values { values, blanks } => {
                let set: HashSet<String> = values.iter().map(|v| v.to_lowercase()).collect();
                for (i, row) in (first..=r.end.row).enumerate() {
                    let t = cell_text(wb, sh, at(row));
                    let ok = if t.is_empty() {
                        *blanks
                    } else {
                        set.contains(&t.to_lowercase())
                    };
                    if let Some(p) = pass.get_mut(i) {
                        *p &= ok;
                    }
                }
            }
            FilterCriteria::Custom { and, conds } => {
                for (i, row) in (first..=r.end.row).enumerate() {
                    let v = sh.value(at(row));
                    let t = cell_text(wb, sh, at(row));
                    let mut results = conds.iter().map(|c| compare_cond(v, &t, &c.op, &c.val));
                    let ok = if *and {
                        results.all(|x| x)
                    } else {
                        results.any(|x| x)
                    };
                    if let Some(p) = pass.get_mut(i) {
                        *p &= ok;
                    }
                }
            }
            FilterCriteria::Top10 { top, percent, val } => {
                let mut nums: Vec<f64> = (first..=r.end.row)
                    .filter_map(|row| match sh.value(at(row)) {
                        Value::Number(x) => Some(*x),
                        _ => None,
                    })
                    .collect();
                nums.sort_by(|a, b| a.total_cmp(b));
                if *top {
                    nums.reverse();
                }
                let k = if *percent {
                    ((nums.len() as f64) * val / 100.0).floor().max(1.0) as usize
                } else {
                    val.max(1.0) as usize
                };
                let threshold = nums.get(k.min(nums.len()).saturating_sub(1)).copied();
                for (i, row) in (first..=r.end.row).enumerate() {
                    let ok = match (sh.value(at(row)), threshold) {
                        (Value::Number(x), Some(t)) => {
                            if *top {
                                *x >= t
                            } else {
                                *x <= t
                            }
                        }
                        _ => false,
                    };
                    if let Some(p) = pass.get_mut(i) {
                        *p &= ok;
                    }
                }
            }
        }
    }
    pass
}

/// Hide failing rows / show passing rows of the sheet's AutoFilter.
fn apply(sh: &mut Sheet, wb_styles: &Workbook) -> usize {
    let Some(af) = sh.auto_filter.clone() else {
        return 0;
    };
    let pass = passing_rows(wb_styles, sh, &af);
    let first = af.range.start.row + 1;
    let mut shown = 0;
    for (i, ok) in pass.iter().enumerate() {
        let row = first + i as u32;
        if *ok {
            sh.hidden_rows.remove(&row);
            shown += 1;
        } else {
            sh.hidden_rows.insert(row);
        }
    }
    shown
}

/// Run `f` on the sheet inside a transaction, then re-apply its filter.
fn with_filter(
    s: &mut Session,
    sheet: usize,
    label: &str,
    f: impl FnOnce(&mut Sheet) -> Result<(), CmdError>,
) -> Result<usize, CmdError> {
    s.transact(label, |tx, _| {
        tx.touch_sheet(sheet);
        // Filtering reads styles (formatted text): clone the table, not the workbook.
        let view = Workbook {
            styles: tx.wb.styles.clone(),
            ..Workbook::new()
        };
        let sh = tx
            .wb
            .sheet_mut(sheet)
            .ok_or_else(|| CmdError::Failed("no sheet".into()))?;
        f(sh)?;
        Ok(apply(sh, &view))
    })
}

/// The block a new filter covers: the given range, or the region around the active cell.
fn target_range(s: &Session, p: &Json) -> Result<(usize, CellRange), CmdError> {
    let (sheet, r) = range(s, p, "range")?;
    let r = if r.is_single() {
        super::nav_cmds::current_region(s, r.start)
    } else {
        r
    };
    let used =
        s.wb.sheet(sheet)
            .and_then(Sheet::used_range)
            .unwrap_or(CellRange::single(r.start));
    let r = r.intersect(&used).unwrap_or(r);
    if r.rows() > MAX_FILTER_ROWS || r.cols() > 16_384 {
        return Err(CmdError::Param("range too large".into()));
    }
    Ok((sheet, r))
}

fn op_name(op: &str) -> Option<&'static str> {
    Some(match op.trim() {
        "=" | "eq" | "equal" | "equals" => "equal",
        "<>" | "ne" | "notEqual" | "not_equal" => "notEqual",
        ">" | "gt" | "greaterThan" => "greaterThan",
        ">=" | "ge" | "greaterThanOrEqual" => "greaterThanOrEqual",
        "<" | "lt" | "lessThan" => "lessThan",
        "<=" | "le" | "lessThanOrEqual" => "lessThanOrEqual",
        _ => return None,
    })
}

/// Parse a condition: `{"op": ">", "val": 5}` or text helpers (`contains`, `begins_with`…).
fn cond_of(j: &Json) -> Result<FilterCond, CmdError> {
    let op = j
        .get("op")
        .and_then(Json::as_str)
        .ok_or_else(|| CmdError::Param("each condition needs 'op'".into()))?;
    let val = match j.get("val") {
        Some(Json::String(t)) => t.clone(),
        Some(Json::Number(n)) => n.to_string(),
        Some(Json::Bool(b)) => if *b { "TRUE" } else { "FALSE" }.to_string(),
        _ => String::new(),
    };
    let (op, val) = match op {
        "contains" => ("equal", format!("*{val}*")),
        "not_contains" => ("notEqual", format!("*{val}*")),
        "begins_with" => ("equal", format!("{val}*")),
        "ends_with" => ("equal", format!("*{val}")),
        other => (
            op_name(other).ok_or_else(|| CmdError::Param(format!("unknown op '{other}'")))?,
            val,
        ),
    };
    Ok(FilterCond {
        op: op.to_string(),
        val,
    })
}

/// Criteria for one column from params (`values`, `custom`, `top`/`bottom`).
fn criteria_of(p: &Json) -> Result<Option<FilterCriteria>, CmdError> {
    if let Some(vals) = p.get("values").and_then(Json::as_array) {
        if vals.len() > 100_000 {
            return Err(CmdError::Param("too many values".into()));
        }
        let mut values = Vec::new();
        let mut blanks = bool_opt(p, "blanks").unwrap_or(false);
        for v in vals {
            match v {
                Json::String(t) if t.is_empty() => blanks = true,
                Json::String(t) => values.push(t.clone()),
                Json::Null => blanks = true,
                other => values.push(other.to_string()),
            }
        }
        return Ok(Some(FilterCriteria::Values { values, blanks }));
    }
    if let Some(conds) = p.get("custom").and_then(Json::as_array) {
        if conds.is_empty() || conds.len() > 2 {
            return Err(CmdError::Param(
                "custom filters take 1 or 2 conditions".into(),
            ));
        }
        let conds = conds.iter().map(cond_of).collect::<Result<Vec<_>, _>>()?;
        return Ok(Some(FilterCriteria::Custom {
            and: bool_opt(p, "and").unwrap_or(true),
            conds,
        }));
    }
    for (key, top) in [("top", true), ("bottom", false)] {
        if let Some(n) = f64_opt(p, key)? {
            if !(1.0..=500.0).contains(&n) {
                return Err(CmdError::Param(format!("'{key}' must be 1-500")));
            }
            return Ok(Some(FilterCriteria::Top10 {
                top,
                percent: bool_opt(p, "percent").unwrap_or(false),
                val: n.trunc(),
            }));
        }
    }
    Ok(None)
}

/// Column param: a letter (`"B"`) or a 0-based offset into the filter range.
fn column_of(p: &Json, r: CellRange) -> Result<Option<u32>, CmdError> {
    let off = match p.get("column") {
        None | Some(Json::Null) => return Ok(None),
        Some(Json::String(t)) => {
            let c = sp_core::letters_to_col(t)
                .ok_or_else(|| CmdError::Param(format!("bad column '{t}'")))?;
            c.checked_sub(r.start.col)
                .ok_or_else(|| CmdError::Param("column outside the filter range".into()))?
        }
        Some(v) => v
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| CmdError::Param("bad 'column'".into()))?,
    };
    if off >= r.cols() {
        return Err(CmdError::Param("column outside the filter range".into()));
    }
    Ok(Some(off))
}

fn filter(s: &mut Session, p: &Json) -> CmdResult {
    let sheet = match str_opt(p, "range") {
        Some(t) => parse_ref(s, t)?.0,
        None => s.sheet_index(),
    };
    let existing = s.wb.sheet(sheet).and_then(|sh| sh.auto_filter.clone());
    let has_column = p.get("column").is_some();
    let turn_off = match bool_opt(p, "on") {
        Some(on) => !on,
        None => existing.is_some() && !has_column,
    };
    if turn_off {
        let Some(af) = existing else {
            return Ok(json!({ "filter": null }));
        };
        s.transact("Filter", |tx, _| {
            tx.touch_sheet(sheet);
            if let Some(sh) = tx.wb.sheet_mut(sheet) {
                let (a, b) = (af.range.start.row + 1, af.range.end.row);
                if a <= b {
                    sh.hidden_rows.retain(|r| *r < a || *r > b);
                }
                sh.auto_filter = None;
            }
            Ok(())
        })?;
        return Ok(json!({ "filter": null }));
    }
    let r = match &existing {
        Some(af) if str_opt(p, "range").is_none() => af.range,
        _ => target_range(s, p)?.1,
    };
    let col = column_of(p, r)?;
    let crit = criteria_of(p)?;
    if col.is_some() && crit.is_none() && !bool_opt(p, "clear").unwrap_or(false) {
        return Err(CmdError::Param(
            "give 'values', 'custom', 'top'/'bottom' or 'clear' for the column".into(),
        ));
    }
    let shown = with_filter(s, sheet, "Filter", |sh| {
        let mut af = match sh.auto_filter.take() {
            Some(af) if af.range == r => af,
            _ => AutoFilter {
                range: r,
                columns: Vec::new(),
            },
        };
        if let Some(c) = col {
            af.columns.retain(|fc| fc.col != c);
            if let Some(criteria) = crit {
                af.columns.push(FilterColumn { col: c, criteria });
                af.columns.sort_by_key(|fc| fc.col);
            }
        }
        sh.auto_filter = Some(af);
        Ok(())
    })?;
    s.status = format!("{shown} of {} records found", r.rows().saturating_sub(1));
    Ok(json!({ "filter": r.to_a1(), "shown": shown }))
}

fn has_auto_filter(s: &Session) -> bool {
    s.wb.sheet(s.sheet_index())
        .is_some_and(|sh| sh.auto_filter.is_some())
}

/// An AutoFilter, or an in-place Advanced Filter (Clear Filter handles both).
fn has_any_filter(s: &Session) -> bool {
    let sheet = s.sheet_index();
    has_auto_filter(s)
        || s.wb
            .names
            .iter()
            .any(|n| n.scope == Some(sheet) && n.name.eq_ignore_ascii_case(FILTER_DB))
}

fn filter_clear(s: &mut Session, _: &Json) -> CmdResult {
    let sheet = s.sheet_index();
    if s.wb.sheet(sheet).is_some_and(|sh| sh.auto_filter.is_some()) {
        let shown = with_filter(s, sheet, "Clear Filter", |sh| {
            if let Some(af) = sh.auto_filter.as_mut() {
                af.columns.clear();
            }
            Ok(())
        })?;
        return Ok(json!({ "shown": shown }));
    }
    // An in-place Advanced Filter: show every row of its list range.
    let db =
        s.wb.names
            .iter()
            .find(|n| n.scope == Some(sheet) && n.name.eq_ignore_ascii_case(FILTER_DB))
            .map(|n| n.formula.clone());
    let Some(r) = db
        .and_then(|f| parse_ref(s, &f.replace('$', "")).ok())
        .map(|x| x.1)
    else {
        return Err(CmdError::Failed(
            "This sheet has no filter to clear.".into(),
        ));
    };
    s.transact("Clear Filter", |tx, _| {
        tx.touch_sheet(sheet);
        if let Some(sh) = tx.wb.sheet_mut(sheet) {
            let (a, b) = (r.start.row + 1, r.end.row);
            sh.hidden_rows.retain(|x| *x < a || *x > b);
        }
        Ok(())
    })?;
    Ok(json!({ "shown": r.rows().saturating_sub(1) }))
}

fn filter_reapply(s: &mut Session, _: &Json) -> CmdResult {
    let sheet = s.sheet_index();
    let Some(af) = s.wb.sheet(sheet).and_then(|sh| sh.auto_filter.clone()) else {
        return Err(CmdError::Failed("This sheet has no AutoFilter.".into()));
    };
    // Grow the range over data added below / beside it.
    let region = super::nav_cmds::current_region(s, af.range.start);
    let grown = if region.start == af.range.start && region.rows() <= MAX_FILTER_ROWS {
        CellRange::new(
            af.range.start,
            CellAddr::new(
                region.end.row.max(af.range.end.row),
                region.end.col.max(af.range.end.col),
            ),
        )
    } else {
        af.range
    };
    let shown = with_filter(s, sheet, "Reapply", |sh| {
        if let Some(f) = sh.auto_filter.as_mut() {
            f.range = grown;
        }
        Ok(())
    })?;
    Ok(json!({ "filter": grown.to_a1(), "shown": shown }))
}

/// One criteria cell → condition: `>5`, `<>x`, `=abc` (exact), `abc` (begins with).
fn criteria_cond(v: &Value) -> Option<FilterCond> {
    let text = match v {
        Value::Empty => return None,
        Value::Number(n) => {
            return Some(FilterCond {
                op: "equal".into(),
                val: sp_core::value::format_general(*n),
            });
        }
        other => other.to_display(),
    };
    for (prefix, op) in [
        (">=", "greaterThanOrEqual"),
        ("<=", "lessThanOrEqual"),
        ("<>", "notEqual"),
        (">", "greaterThan"),
        ("<", "lessThan"),
        ("=", "equal"),
    ] {
        if let Some(rest) = text.strip_prefix(prefix) {
            return Some(FilterCond {
                op: op.into(),
                val: rest.to_string(),
            });
        }
    }
    Some(FilterCond {
        op: "equal".into(),
        val: format!("{text}*"),
    })
}

fn advanced_filter(s: &mut Session, p: &Json) -> CmdResult {
    let (sheet, list) = match str_opt(p, "range") {
        Some(_) => range(s, p, "range")?,
        None => {
            let a = s.sel.active;
            (s.sheet_index(), super::nav_cmds::current_region(s, a))
        }
    };
    let crit_txt =
        str_opt(p, "criteria").ok_or_else(|| CmdError::Param("missing 'criteria' range".into()))?;
    let (csheet, crit) = parse_ref(s, crit_txt)?;
    if list.rows() < 1
        || list.cell_count() > sp_core::MAX_CELLS_PER_OP
        || crit.cell_count() > 100_000
    {
        return Err(CmdError::Param("range too large".into()));
    }
    let unique = bool_opt(p, "unique").unwrap_or(false);
    let copy_to = match str_opt(p, "copy_to") {
        Some(_) => Some(cell(s, p, "copy_to")?),
        None => None,
    };
    let wb = &s.wb;
    let sh = wb
        .sheet(sheet)
        .ok_or_else(|| CmdError::Failed("no sheet".into()))?;
    let csh = wb
        .sheet(csheet)
        .ok_or_else(|| CmdError::Failed("no sheet".into()))?;
    // Map criteria headers to list columns.
    let header = |sh: &Sheet, row: u32, col: u32| sh.value(CellAddr::new(row, col)).to_display();
    let mut crit_cols = Vec::new();
    for c in crit.start.col..=crit.end.col {
        let h = header(csh, crit.start.row, c);
        if h.trim().is_empty() {
            continue;
        }
        let lc = (list.start.col..=list.end.col)
            .find(|lc| header(sh, list.start.row, *lc).eq_ignore_ascii_case(h.trim()))
            .ok_or_else(|| CmdError::Param(format!("criteria label '{h}' is not a list column")))?;
        crit_cols.push((c, lc));
    }
    // Criteria rows: OR of rows, AND within a row.
    let mut rows: Vec<Vec<(u32, FilterCond)>> = Vec::new();
    for row in crit.start.row + 1..=crit.end.row {
        let conds: Vec<(u32, FilterCond)> = crit_cols
            .iter()
            .filter_map(|(cc, lc)| {
                criteria_cond(csh.value(CellAddr::new(row, *cc))).map(|f| (*lc, f))
            })
            .collect();
        rows.push(conds);
    }
    let matches = |row: u32| {
        rows.is_empty()
            || rows.iter().any(|conds| {
                conds.iter().all(|(lc, f)| {
                    let a = CellAddr::new(row, *lc);
                    compare_cond(sh.value(a), &cell_text(wb, sh, a), &f.op, &f.val)
                })
            })
    };
    let mut seen: HashSet<Vec<String>> = HashSet::new();
    let mut keep: Vec<u32> = Vec::new();
    let mut hide: Vec<u32> = Vec::new();
    for row in list.start.row + 1..=list.end.row {
        let mut ok = matches(row);
        if ok && unique {
            let key: Vec<String> = (list.start.col..=list.end.col)
                .map(|c| cell_text(wb, sh, CellAddr::new(row, c)).to_lowercase())
                .collect();
            ok = seen.insert(key);
        }
        if ok {
            keep.push(row);
        } else {
            hide.push(row);
        }
    }
    let found = keep.len();
    match copy_to {
        Some((dsheet, dest)) => {
            let mut out: Vec<(CellAddr, Option<Cell>)> = Vec::new();
            for (i, row) in std::iter::once(list.start.row).chain(keep).enumerate() {
                for (j, c) in (list.start.col..=list.end.col).enumerate() {
                    let Some(at) = dest.offset(i as i64, j as i64) else {
                        return Err(CmdError::Failed(
                            "The extract range doesn't fit on the sheet.".into(),
                        ));
                    };
                    let src = sh.get(CellAddr::new(row, c));
                    out.push((
                        at,
                        src.map(|c| Cell {
                            style: c.style,
                            ..Cell::constant(c.value.clone())
                        }),
                    ));
                }
            }
            let dest_rect = CellRange::new(dest, out.last().map_or(dest, |(a, _)| *a));
            if dsheet == sheet && dest_rect.intersect(&list).is_some() {
                return Err(CmdError::Param(
                    "the copy-to range overlaps the list".into(),
                ));
            }
            s.transact("Advanced Filter", |tx, _| {
                for (a, c) in out {
                    tx.set_cell(dsheet, a, c);
                }
                Ok(())
            })?;
            Ok(json!({ "found": found, "copied_to": dest_rect.to_a1() }))
        }
        None => {
            let sheet_name = sh.name.clone();
            s.transact("Advanced Filter", |tx, _| {
                tx.touch_book();
                tx.wb.names.retain(|n| {
                    !(n.scope == Some(sheet) && n.name.eq_ignore_ascii_case(FILTER_DB))
                });
                tx.wb.names.push(DefinedName {
                    name: FILTER_DB.into(),
                    formula: super::audit_cmds::abs_ref(&sheet_name, list),
                    scope: Some(sheet),
                });
                if let Some(sh) = tx.wb.sheet_mut(sheet) {
                    let (a, b) = (list.start.row + 1, list.end.row);
                    sh.hidden_rows.retain(|x| *x < a || *x > b);
                    sh.hidden_rows.extend(hide);
                }
                Ok(())
            })?;
            s.status = format!("{found} of {} records found", list.rows().saturating_sub(1));
            Ok(json!({ "found": found }))
        }
    }
}

pub fn specs() -> Vec<CommandSpec> {
    let f = r#"{"range"?: "A1:D50", "on"?: bool (default toggles), "column"?: "B" | 1, "values"?: ["x",""], "custom"?: [{"op": ">|>=|<|<=|=|<>|contains|begins_with|ends_with", "val": 5}], "and"?: true, "top"?: 10, "bottom"?: 10, "percent"?: bool, "clear"?: bool}"#;
    vec![
        CommandSpec::new("data.filter", f, filter),
        CommandSpec::new("home.editing.filter", f, filter),
        CommandSpec::new("data.filter_clear", "{}", filter_clear).when(has_any_filter),
        CommandSpec::new("data.filter_reapply", "{}", filter_reapply).when(has_auto_filter),
        CommandSpec::new(
            "data.advanced_filter",
            r#"{"range"?: "A1:D50", "criteria": "F1:G3", "unique"?: bool, "copy_to"?: "I1"}"#,
            advanced_filter,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        let mut s = Session::new();
        let data = [
            ["Name", "Dept", "Pay"],
            ["Ann", "Ops", "50"],
            ["Bob", "Dev", "70"],
            ["Cid", "Ops", "40"],
            ["Dee", "Dev", "90"],
            ["Ann", "Ops", "50"],
        ];
        for (r, row) in data.iter().enumerate() {
            for (c, v) in row.iter().enumerate() {
                let a = CellAddr::new(r as u32, c as u32).to_a1();
                s.execute("cell.set", &json!({"cell": a, "input": v}))
                    .unwrap();
            }
        }
        s
    }

    fn hidden(s: &Session) -> Vec<u32> {
        s.wb.sheets[0].hidden_rows.iter().map(|r| r + 1).collect()
    }

    #[test]
    fn autofilter_values_custom_top() {
        let mut s = session();
        s.execute("data.filter", &json!({})).unwrap();
        assert_eq!(
            s.wb.sheets[0].auto_filter.as_ref().unwrap().range.to_a1(),
            "A1:C6"
        );
        s.execute("data.filter", &json!({"column": "B", "values": ["ops"]}))
            .unwrap();
        assert_eq!(hidden(&s), vec![3, 5]);
        s.execute(
            "data.filter",
            &json!({"column": "C", "custom": [{"op": ">", "val": 45}]}),
        )
        .unwrap();
        assert_eq!(hidden(&s), vec![3, 4, 5]);
        s.execute("data.filter_clear", &json!({})).unwrap();
        assert!(hidden(&s).is_empty());
        s.execute("data.filter", &json!({"column": "C", "top": 2}))
            .unwrap();
        assert_eq!(hidden(&s), vec![2, 4, 6]);
        s.execute(
            "data.filter",
            &json!({"column": 0, "custom": [{"op": "begins_with", "val": "a"}]}),
        )
        .unwrap();
        assert_eq!(hidden(&s), vec![2, 3, 4, 5, 6]);
        // Data changes: reapply.
        s.execute("cell.set", &json!({"cell": "C3", "input": "95"}))
            .unwrap();
        s.execute("data.filter", &json!({"column": 0, "clear": true}))
            .unwrap();
        s.execute("data.filter_reapply", &json!({})).unwrap();
        assert_eq!(hidden(&s), vec![2, 4, 6]);
        s.execute("edit.undo", &json!({})).unwrap();
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(hidden(&s), vec![2, 3, 4, 5, 6]);
        // Toggle off shows everything.
        s.execute("home.editing.filter", &json!({})).unwrap();
        assert!(hidden(&s).is_empty());
        assert!(s.wb.sheets[0].auto_filter.is_none());
    }

    #[test]
    fn advanced_filter_in_place_and_copy() {
        let mut s = session();
        s.execute("cell.set", &json!({"cell": "E1", "input": "Dept"}))
            .unwrap();
        s.execute("cell.set", &json!({"cell": "F1", "input": "Pay"}))
            .unwrap();
        s.execute("cell.set", &json!({"cell": "E2", "input": "Ops"}))
            .unwrap();
        s.execute("cell.set", &json!({"cell": "F2", "input": ">=50"}))
            .unwrap();
        let r = s
            .execute(
                "data.advanced_filter",
                &json!({"range": "A1:C6", "criteria": "E1:F2"}),
            )
            .unwrap();
        assert_eq!(r["found"], 2);
        assert_eq!(hidden(&s), vec![3, 4, 5]);
        s.execute("data.filter_clear", &json!({})).unwrap();
        assert!(hidden(&s).is_empty());
        let r = s
            .execute(
                "data.advanced_filter",
                &json!({"range": "A1:C6", "criteria": "E1:F2", "unique": true, "copy_to": "H1"}),
            )
            .unwrap();
        assert_eq!(r["found"], 1);
        assert_eq!(r["copied_to"], "H1:J2");
        assert_eq!(
            s.wb.sheets[0]
                .value(CellAddr::parse("H2").unwrap())
                .to_display(),
            "Ann"
        );
        assert!(
            s.execute(
                "data.advanced_filter",
                &json!({"range": "A1:C6", "criteria": "E1:F2", "copy_to": "B2"})
            )
            .is_err()
        );
    }

    #[test]
    fn wildcards() {
        assert!(wildcard_match("a*c", "abbbc"));
        assert!(wildcard_match("*b*", "abc"));
        assert!(wildcard_match("a?c", "ABC"));
        assert!(!wildcard_match("a?c", "ac"));
        assert!(wildcard_match("~*x", "*x"));
        assert!(!wildcard_match("ab", "abc"));
    }
}
