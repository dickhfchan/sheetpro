//! Outline: group/ungroup rows or columns, show/hide detail, automatic subtotals
//! (`data.group`, `data.ungroup`, `data.show_detail`, `data.hide_detail`, `data.subtotal`).

use crate::params::{bool_opt, range, str_opt};
use crate::{CmdError, CmdResult, CommandSpec, Json, Session};
use serde_json::json;
use sp_core::{CellAddr, CellRange, col_to_letters};
use sp_formula::shift::Axis;
use sp_model::{Cell, Sheet, features::MAX_OUTLINE_LEVEL};
use std::collections::BTreeMap;

/// Rows/columns one group command may span.
const MAX_SPAN: u32 = 100_000;

/// Rows or columns? Whole columns → columns; otherwise `by`, defaulting to rows.
fn axis_of(p: &Json, r: CellRange) -> Result<bool, CmdError> {
    match str_opt(p, "by") {
        Some("rows") => Ok(true),
        Some("columns") | Some("cols") => Ok(false),
        Some(o) => Err(CmdError::Param(format!("'by' is rows or columns, not '{o}'"))),
        None => Ok(!(r.is_full_cols() && !r.is_full_rows())),
    }
}

fn span(r: CellRange, rows: bool) -> (u32, u32) {
    if rows {
        (r.start.row, r.end.row)
    } else {
        (r.start.col, r.end.col)
    }
}

fn levels(sh: &mut Sheet, rows: bool) -> &mut BTreeMap<u32, u8> {
    if rows {
        &mut sh.outline.rows
    } else {
        &mut sh.outline.cols
    }
}

fn group(s: &mut Session, p: &Json, delta: i8) -> CmdResult {
    let (sheet, r) = range(s, p, "range")?;
    let rows = axis_of(p, r)?;
    let (lo, hi) = span(r, rows);
    if hi - lo >= MAX_SPAN {
        return Err(CmdError::Param("range too large".into()));
    }
    let label = if delta > 0 { "Group" } else { "Ungroup" };
    s.transact(label, |tx, _| {
        tx.touch_sheet(sheet);
        let sh = tx
            .wb
            .sheet_mut(sheet)
            .ok_or_else(|| CmdError::Failed("no sheet".into()))?;
        let lv = levels(sh, rows);
        if delta > 0 && lv.range(lo..=hi).any(|(_, l)| *l >= MAX_OUTLINE_LEVEL) {
            return Err(CmdError::Failed(
                "Cannot group that selection: the outline is at its deepest level.".into(),
            ));
        }
        if delta < 0 && lv.range(lo..=hi).next().is_none() {
            return Err(CmdError::Failed("Cannot ungroup that selection.".into()));
        }
        let mut cleared = Vec::new();
        for i in lo..=hi {
            let cur = lv.get(&i).copied().unwrap_or(0);
            let new = (i16::from(cur) + i16::from(delta)).clamp(0, i16::from(MAX_OUTLINE_LEVEL)) as u8;
            if new == 0 {
                lv.remove(&i);
                if cur > 0 {
                    cleared.push(i);
                }
            } else {
                lv.insert(i, new);
            }
        }
        // Ungrouped rows/columns come back into view.
        let hidden = if rows {
            &mut sh.hidden_rows
        } else {
            &mut sh.hidden_cols
        };
        for i in cleared {
            hidden.remove(&i);
        }
        Ok(())
    })?;
    Ok(json!({ "by": if rows { "rows" } else { "columns" }, "range": r.to_a1() }))
}

/// The group attached to row/col `i`: the detail block it summarises (summary row/col
/// just below/right of the block), else the innermost block containing `i`.
fn group_at(sh: &Sheet, rows: bool, i: u32) -> Option<(u32, u32)> {
    let lv = if rows { &sh.outline.rows } else { &sh.outline.cols };
    let level = |k: u32| lv.get(&k).copied().unwrap_or(0);
    let below = if rows {
        sh.outline.summary_below
    } else {
        sh.outline.summary_right
    };
    let me = level(i);
    let neighbour = if below { i.checked_sub(1) } else { i.checked_add(1) };
    let (seed, min) = match neighbour {
        Some(n) if level(n) > me => (n, me + 1),
        _ if me > 0 => (i, me),
        _ => return None,
    };
    let mut a = seed;
    while a > 0 && level(a - 1) >= min {
        a -= 1;
    }
    let mut b = seed;
    while level(b + 1) >= min && b - a < 2_000_000 {
        b += 1;
    }
    Some((a, b))
}

fn detail(s: &mut Session, p: &Json, show: bool) -> CmdResult {
    let (sheet, r) = range(s, p, "range")?;
    let rows = match str_opt(p, "by") {
        Some(_) => axis_of(p, r)?,
        None => {
            // Prefer whichever axis has an outline at the active cell.
            let sh = s.wb.sheet(sheet);
            let has = |rows: bool| {
                sh.and_then(|x| group_at(x, rows, if rows { r.start.row } else { r.start.col }))
                    .is_some()
            };
            has(true) || !has(false)
        }
    };
    let i = if rows { r.start.row } else { r.start.col };
    let (a, b) = s
        .wb
        .sheet(sheet)
        .and_then(|sh| group_at(sh, rows, i))
        .ok_or_else(|| CmdError::Failed("There is no outline group here.".into()))?;
    s.transact(if show { "Show Detail" } else { "Hide Detail" }, |tx, _| {
        tx.touch_sheet(sheet);
        let sh = tx
            .wb
            .sheet_mut(sheet)
            .ok_or_else(|| CmdError::Failed("no sheet".into()))?;
        let hidden = if rows {
            &mut sh.hidden_rows
        } else {
            &mut sh.hidden_cols
        };
        if show {
            hidden.retain(|x| *x < a || *x > b);
        } else {
            hidden.extend(a..=b);
        }
        Ok(())
    })?;
    let to = |k: u32| {
        if rows {
            (k + 1).to_string()
        } else {
            col_to_letters(k)
        }
    };
    Ok(json!({ "group": format!("{}:{}", to(a), to(b)), "hidden": !show }))
}

/// SUBTOTAL function number for a summary function name.
fn function_number(name: &str) -> Option<u32> {
    Some(match name.to_ascii_lowercase().as_str() {
        "average" => 1,
        "count" => 2,
        "counta" => 3,
        "max" => 4,
        "min" => 5,
        "product" => 6,
        "stdev" => 7,
        "stdevp" => 8,
        "sum" => 9,
        "var" => 10,
        "varp" => 11,
        _ => return None,
    })
}

fn is_subtotal_row(sh: &Sheet, row: u32, r: CellRange) -> bool {
    sh.cells_in(CellRange::new(
        CellAddr::new(row, r.start.col),
        CellAddr::new(row, r.end.col),
    ))
    .any(|(_, c)| {
        c.formula_src()
            .is_some_and(|f| f.to_ascii_uppercase().contains("SUBTOTAL("))
    })
}

fn col_param(v: &Json, r: CellRange) -> Result<u32, CmdError> {
    let t = v
        .as_str()
        .ok_or_else(|| CmdError::Param("columns are letters like \"C\"".into()))?;
    let c = sp_core::letters_to_col(t).ok_or_else(|| CmdError::Param(format!("bad column '{t}'")))?;
    if c < r.start.col || c > r.end.col {
        return Err(CmdError::Param(format!("column {t} is outside the list")));
    }
    Ok(c)
}

fn subtotal(s: &mut Session, p: &Json) -> CmdResult {
    let (sheet, r) = range(s, p, "range")?;
    let r = if r.is_single() {
        super::nav_cmds::current_region(s, r.start)
    } else {
        r
    };
    if r.rows() < 2 || r.rows() > MAX_SPAN || r.cell_count() > sp_core::MAX_CELLS_PER_OP {
        return Err(CmdError::Param(
            "select a list with a header row (at most 100,000 rows)".into(),
        ));
    }
    let by = col_param(
        p.get("by")
            .or_else(|| p.get("at_each_change_in"))
            .unwrap_or(&Json::String(col_to_letters(r.start.col))),
        r,
    )?;
    let fname = str_opt(p, "function").unwrap_or("sum");
    let fnum = function_number(fname)
        .ok_or_else(|| CmdError::Param(format!("unknown summary function '{fname}'")))?;
    let cols: Vec<u32> = match p.get("columns").or_else(|| p.get("add_to")) {
        Some(Json::Array(a)) => a.iter().map(|v| col_param(v, r)).collect::<Result<_, _>>()?,
        Some(v) => vec![col_param(v, r)?],
        None => vec![r.end.col],
    };
    if cols.is_empty() {
        return Err(CmdError::Param("no columns to subtotal".into()));
    }
    let replace = bool_opt(p, "replace").unwrap_or(true);
    let sh = s.wb.sheet(sheet).ok_or_else(|| CmdError::Failed("no sheet".into()))?;
    // Existing subtotal rows (replaced by default).
    let old: Vec<u32> = if replace {
        (r.start.row + 1..=r.end.row)
            .filter(|row| is_subtotal_row(sh, *row, r))
            .collect()
    } else {
        Vec::new()
    };
    let wb_styles = &s.wb;
    let key = |row: u32| {
        super::filter_cmds::cell_text(wb_styles, sh, CellAddr::new(row, by))
    };
    // Groups of the remaining data rows, in post-removal coordinates.
    let data: Vec<u32> = (r.start.row + 1..=r.end.row)
        .filter(|row| !old.contains(row))
        .collect();
    let mut groups: Vec<(String, u32, u32)> = Vec::new();
    for (k, row) in data.iter().enumerate() {
        let new_row = r.start.row + 1 + k as u32;
        let t = key(*row);
        match groups.last_mut() {
            Some(g) if g.0.eq_ignore_ascii_case(&t) => g.2 = new_row,
            _ => groups.push((t, new_row, new_row)),
        }
    }
    if groups.is_empty() {
        return Err(CmdError::Failed("The list has no data rows.".into()));
    }
    let end = r.start.row + data.len() as u32;
    let n = groups.len() as u32;
    let label = match fnum {
        1 => "Average",
        2 | 3 => "Count",
        4 => "Max",
        5 => "Min",
        6 => "Product",
        7 => "StdDev",
        8 => "StdDevp",
        10 => "Var",
        11 => "Varp",
        _ => "Total",
    };
    let sum_cell = |c: u32, a: u32, b: u32| {
        let l = col_to_letters(c);
        Cell::formula(format!("SUBTOTAL({fnum},{l}{}:{l}{})", a + 1, b + 1))
    };
    let first = r.start.row + 1;
    s.transact("Subtotal", |tx, _| {
        for row in old.iter().rev() {
            sp_ops::edit::insert_delete(tx, sheet, Axis::Rows, *row, -1)?;
        }
        sp_ops::edit::insert_delete(tx, sheet, Axis::Rows, end + 1, 1)?;
        for (_, _, b) in groups.iter().rev() {
            sp_ops::edit::insert_delete(tx, sheet, Axis::Rows, b + 1, 1)?;
        }
        let mut outline = Vec::new();
        for (i, (name, a, b)) in groups.iter().enumerate() {
            let (a, b) = (a + i as u32, b + i as u32);
            let at = b + 1;
            tx.set_cell(
                sheet,
                CellAddr::new(at, by),
                Some(Cell::constant(sp_core::Value::text(format!("{name} {label}").trim()))),
            );
            for c in &cols {
                tx.set_cell(sheet, CellAddr::new(at, *c), Some(sum_cell(*c, a, b)));
            }
            outline.extend((a..=b).map(|x| (x, 2u8)));
            outline.push((at, 1u8));
        }
        let grand = end + n + 1;
        tx.set_cell(
            sheet,
            CellAddr::new(grand, by),
            Some(Cell::constant(sp_core::Value::text(format!("Grand {label}")))),
        );
        for c in &cols {
            tx.set_cell(sheet, CellAddr::new(grand, *c), Some(sum_cell(*c, first, grand - 1)));
        }
        tx.touch_sheet(sheet);
        if let Some(sh) = tx.wb.sheet_mut(sheet) {
            let (lo, hi) = (first, grand);
            sh.outline.rows.retain(|k, _| *k < lo || *k > hi);
            sh.outline.rows.extend(outline);
            sh.outline.summary_below = true;
        }
        Ok(())
    })?;
    Ok(json!({
        "groups": n,
        "range": CellRange::new(r.start, CellAddr::new(end + n + 1, r.end.col)).to_a1(),
    }))
}

pub fn specs() -> Vec<CommandSpec> {
    let g = r#"{"range"?: "A2:A9", "by"?: "rows|columns"}"#;
    vec![
        CommandSpec::new("data.group", g, |s, p| group(s, p, 1)),
        CommandSpec::new("data.ungroup", g, |s, p| group(s, p, -1)),
        CommandSpec::new("data.show_detail", g, |s, p| detail(s, p, true)),
        CommandSpec::new("data.hide_detail", g, |s, p| detail(s, p, false)),
        CommandSpec::new(
            "data.subtotal",
            r#"{"range"?: "A1:D50", "by"?: "A" (at each change in), "function"?: "sum|count|average|max|min|product|counta|stdev|stdevp|var|varp", "columns"?: ["C","D"], "replace"?: true}"#,
            subtotal,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use sp_core::Value;

    fn session(rows: &[[&str; 2]]) -> Session {
        let mut s = Session::new();
        for (r, row) in rows.iter().enumerate() {
            for (c, v) in row.iter().enumerate() {
                let a = CellAddr::new(r as u32, c as u32).to_a1();
                s.execute("cell.set", &json!({"cell": a, "input": v}))
                    .unwrap();
            }
        }
        s
    }

    #[test]
    fn group_hide_show_ungroup() {
        let mut s = Session::new();
        s.execute("data.group", &json!({"range": "A2:A4"})).unwrap();
        s.execute("data.group", &json!({"range": "A3:A3"})).unwrap();
        let sh = &s.wb.sheets[0];
        assert_eq!(sh.outline.rows.get(&1), Some(&1));
        assert_eq!(sh.outline.rows.get(&2), Some(&2));
        // Hide from the summary row (row 5) collapses rows 2-4.
        let r = s.execute("data.hide_detail", &json!({"range": "A5"})).unwrap();
        assert_eq!(r["group"], "2:4");
        assert_eq!(s.wb.sheets[0].hidden_rows.len(), 3);
        s.execute("data.show_detail", &json!({"range": "A5"})).unwrap();
        assert!(s.wb.sheets[0].hidden_rows.is_empty());
        // Inner group from its own row.
        let r = s.execute("data.hide_detail", &json!({"range": "A3"})).unwrap();
        assert_eq!(r["group"], "3:3");
        s.execute("data.ungroup", &json!({"range": "A2:A4"})).unwrap();
        s.execute("data.ungroup", &json!({"range": "A2:A4"})).unwrap();
        assert!(s.wb.sheets[0].outline.rows.is_empty());
        assert!(s.wb.sheets[0].hidden_rows.is_empty());
        assert!(s.execute("data.ungroup", &json!({"range": "A2:A4"})).is_err());
        // Columns.
        s.execute("data.group", &json!({"range": "B:C"})).unwrap();
        assert_eq!(s.wb.sheets[0].outline.cols.len(), 2);
        s.execute("data.hide_detail", &json!({"range": "D1", "by": "columns"}))
            .unwrap();
        assert!(s.wb.sheets[0].hidden_cols.contains(&1));
        for _ in 0..7 {
            s.execute("data.group", &json!({"range": "A10:A11"})).unwrap();
        }
        assert!(s.execute("data.group", &json!({"range": "A10:A11"})).is_err());
    }

    #[test]
    fn subtotals_at_each_change() {
        let mut s = session(&[
            ["Region", "Sales"],
            ["East", "1"],
            ["East", "2"],
            ["West", "4"],
            ["North", "8"],
            ["North", "16"],
        ]);
        let r = s
            .execute("data.subtotal", &json!({"range": "A1:B6", "by": "A", "columns": ["B"]}))
            .unwrap();
        assert_eq!(r["groups"], 3);
        let sh = &s.wb.sheets[0];
        let text = |a: &str| sh.get(CellAddr::parse(a).unwrap()).map(|c| c.input_text()).unwrap_or_default();
        assert_eq!(text("A4"), "East Total");
        assert_eq!(text("B4"), "=SUBTOTAL(9,B2:B3)");
        assert_eq!(text("A6"), "West Total");
        assert_eq!(text("B9"), "=SUBTOTAL(9,B7:B8)");
        assert_eq!(text("A10"), "Grand Total");
        assert_eq!(text("B10"), "=SUBTOTAL(9,B2:B9)");
        assert_eq!(sh.value(CellAddr::parse("B9").unwrap()), &Value::Number(24.0));
        assert_eq!(sh.outline.rows.get(&3), Some(&1));
        assert_eq!(sh.outline.rows.get(&1), Some(&2));
        // Re-running replaces the old subtotal rows instead of nesting them.
        s.execute(
            "data.subtotal",
            &json!({"range": "A1:B10", "by": "A", "columns": ["B"], "function": "count"}),
        )
        .unwrap();
        let sh = &s.wb.sheets[0];
        let text = |a: &str| sh.get(CellAddr::parse(a).unwrap()).map(|c| c.input_text()).unwrap_or_default();
        assert_eq!(text("A4"), "East Count");
        assert_eq!(text("B10"), "=SUBTOTAL(2,B2:B9)");
        assert_eq!(text("A11"), "");
        s.execute("edit.undo", &json!({})).unwrap();
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(s.wb.sheets[0].cell_count(), 12);
    }
}
