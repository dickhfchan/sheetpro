//! Fill series, Go To Special, visible cells, paste link (`home.editing.fill_series`,
//! `home.editing.goto_special`, `select.visible_cells`, `home.clipboard.paste_link`).

use crate::params::{bool_opt, cell, f64_opt, range, str_opt};
use crate::{CmdError, CmdResult, CommandSpec, Json, Session};
use serde_json::json;
use sp_core::addr::quote_sheet_name;
use sp_core::{CellAddr, CellRange, Value, col_to_letters};
use sp_model::{Cell, Content, Sheet};

/// Cells one fill-series may write.
const MAX_SERIES: u64 = 1_000_000;
/// Cells listed by Go To Special.
const MAX_LIST: usize = 100_000;

fn add_months(serial: f64, months: i64) -> Option<f64> {
    let (y, m, d) = sp_core::date::serial_to_date(serial)?;
    let total = y.checked_mul(12)? + i64::from(m) - 1 + months;
    let (ny, nm) = (total.div_euclid(12), total.rem_euclid(12) + 1);
    let day = i64::from(d).min(i64::from(sp_core::date::days_in_month(ny, nm as u32)));
    Some(sp_core::date::date_to_serial(ny, nm, day)? + serial.fract())
}

fn add_weekdays(serial: f64, n: i64) -> f64 {
    let mut v = serial;
    let mut left = n.abs();
    let step = if n >= 0 { 1.0 } else { -1.0 };
    while left > 0 {
        v += step;
        // Serial 1 = Sunday 1900-01-01 in the 1900 system (with the leap-year quirk).
        let wd = (v.floor() as i64).rem_euclid(7); // 0 = Saturday, 1 = Sunday
        if wd != 0 && wd != 1 {
            left -= 1;
        }
    }
    v
}

fn fill_series(s: &mut Session, p: &Json) -> CmdResult {
    let (sheet, mut r) = range(s, p, "range")?;
    let kind = str_opt(p, "type").unwrap_or("linear");
    let unit = str_opt(p, "date_unit").unwrap_or("day");
    let step = f64_opt(p, "step")?.unwrap_or(1.0);
    let stop = f64_opt(p, "stop")?;
    let by_cols = match str_opt(p, "series_in") {
        Some("rows") => false,
        Some("columns") => true,
        Some(o) => return Err(CmdError::Param(format!("'series_in' is rows or columns, not '{o}'"))),
        None => r.rows() >= r.cols(),
    };
    if !matches!(kind, "linear" | "growth" | "date") {
        return Err(CmdError::Param(format!("unknown type '{kind}' (linear, growth, date)")));
    }
    if !matches!(unit, "day" | "weekday" | "month" | "year") {
        return Err(CmdError::Param(format!("unknown date_unit '{unit}'")));
    }
    // A single cell with a stop value extends until the stop.
    if r.is_single() {
        let Some(stop) = stop else {
            return Err(CmdError::Param("select a range or give 'stop'".into()));
        };
        let start = s.wb.sheet(sheet).and_then(|sh| sh.value(r.start).as_number()).unwrap_or(0.0);
        let n = match kind {
            "linear" if step != 0.0 => ((stop - start) / step).floor() + 1.0,
            "growth" if step > 1.0 && start > 0.0 => ((stop / start).ln() / step.ln()).floor() + 1.0,
            _ => 1.0,
        };
        let n = n.clamp(1.0, MAX_SERIES as f64) as u32;
        let end = if by_cols {
            CellAddr::new(r.start.row.saturating_add(n - 1).min(sp_core::MAX_ROWS - 1), r.start.col)
        } else {
            CellAddr::new(r.start.row, r.start.col.saturating_add(n - 1).min(sp_core::MAX_COLS - 1))
        };
        r = CellRange::new(r.start, end);
    }
    if r.cell_count() > MAX_SERIES {
        return Err(CmdError::Param("range too large".into()));
    }
    let sh = s.wb.sheet(sheet).ok_or_else(|| CmdError::Failed("no sheet".into()))?;
    let mut writes: Vec<(CellAddr, Option<Cell>)> = Vec::new();
    let lines: Vec<(CellAddr, u32)> = if by_cols {
        (r.start.col..=r.end.col).map(|c| (CellAddr::new(r.start.row, c), r.rows())).collect()
    } else {
        (r.start.row..=r.end.row).map(|row| (CellAddr::new(row, r.start.col), r.cols())).collect()
    };
    for (first, len) in lines {
        let Some(Value::Number(start)) = sh.get(first).map(|c| c.value.clone()) else {
            continue;
        };
        let style = sh.get(first).map(|c| c.style).unwrap_or_default();
        for k in 1..len {
            let kf = f64::from(k);
            let v = match (kind, unit) {
                ("linear", _) => Some(start + step * kf),
                ("growth", _) => Some(start * step.powf(kf)),
                (_, "day") => Some(start + step * kf),
                (_, "weekday") => Some(add_weekdays(start, (step * kf) as i64)),
                (_, "month") => add_months(start, (step * kf) as i64),
                _ => add_months(start, (step * kf * 12.0) as i64),
            };
            let Some(v) = v.filter(|v| v.is_finite()) else {
                break;
            };
            if let Some(stop) = stop
                && ((step >= 0.0 && v > stop) || (step < 0.0 && v < stop))
            {
                break;
            }
            let at = if by_cols { first.offset(i64::from(k), 0) } else { first.offset(0, i64::from(k)) };
            if let Some(at) = at {
                let old_style = sh.get(at).map(|c| c.style).filter(|s| *s != Default::default());
                writes.push((
                    at,
                    Some(Cell {
                        style: old_style.unwrap_or(style),
                        ..Cell::constant(Value::Number(v))
                    }),
                ));
            }
        }
    }
    if writes.is_empty() {
        return Err(CmdError::Failed("No starting number to build a series from.".into()));
    }
    let n = writes.len();
    s.transact("Series", |tx, _| {
        for (a, c) in writes {
            tx.set_cell(sheet, a, c);
        }
        Ok(())
    })?;
    Ok(json!({ "range": r.to_a1(), "filled": n }))
}

/// Visible sub-blocks of `r`: runs of visible rows × runs of visible columns.
pub fn visible_areas(sh: &Sheet, r: CellRange) -> Vec<CellRange> {
    let runs = |lo: u32, hi: u32, hidden: &std::collections::BTreeSet<u32>| {
        let mut out = Vec::new();
        let mut i = lo;
        // Walk hidden entries only, so whole-sheet ranges stay cheap.
        for h in hidden.range(lo..=hi) {
            if *h > i {
                out.push((i, h - 1));
            }
            i = h + 1;
        }
        if i <= hi {
            out.push((i, hi));
        }
        out
    };
    let rows = runs(r.start.row, r.end.row, &sh.hidden_rows);
    let cols = runs(r.start.col, r.end.col, &sh.hidden_cols);
    let mut out = Vec::new();
    for (r0, r1) in &rows {
        for (c0, c1) in &cols {
            out.push(CellRange::new(CellAddr::new(*r0, *c0), CellAddr::new(*r1, *c1)));
            if out.len() >= MAX_LIST {
                return out;
            }
        }
    }
    out
}

fn select_visible(s: &mut Session, p: &Json) -> CmdResult {
    let (sheet, r) = range(s, p, "range")?;
    let sh = s.wb.sheet(sheet).ok_or_else(|| CmdError::Failed("no sheet".into()))?;
    let areas = visible_areas(sh, r);
    if areas.is_empty() {
        return Err(CmdError::Failed("No cells were found.".into()));
    }
    Ok(json!(areas.iter().map(CellRange::to_a1).collect::<Vec<_>>()))
}

fn goto_special(s: &mut Session, p: &Json) -> CmdResult {
    let kind = str_opt(p, "kind").unwrap_or("blanks");
    let sheet = s.sheet_index();
    let sh = s.wb.sheet(sheet).ok_or_else(|| CmdError::Failed("no sheet".into()))?;
    // Scope: the selection when it spans more than one cell, else the used range.
    let used = sh.used_range();
    let scope = match str_opt(p, "range") {
        Some(_) => range(s, p, "range")?.1,
        None if !s.sel.range().is_single() => s.sel.range(),
        None => used.unwrap_or(CellRange::single(CellAddr::default())),
    };
    let scope = match used {
        Some(u) if kind != "visible" => scope.intersect(&u).unwrap_or(CellRange::single(u.start)),
        _ => scope,
    };
    let types: Vec<String> = p
        .get("types")
        .and_then(Json::as_array)
        .map(|a| a.iter().filter_map(Json::as_str).map(str::to_string).collect())
        .unwrap_or_default();
    let type_ok = |v: &Value| {
        types.is_empty()
            || types.iter().any(|t| match t.as_str() {
                "numbers" => matches!(v, Value::Number(_)),
                "text" => matches!(v, Value::Text(_)),
                "logicals" => matches!(v, Value::Bool(_)),
                "errors" => matches!(v, Value::Error(_)),
                _ => false,
            })
    };
    let mut hits: Vec<CellAddr> = Vec::new();
    match kind {
        "visible" => {
            let areas = visible_areas(sh, scope);
            return Ok(json!(areas.iter().map(CellRange::to_a1).collect::<Vec<_>>()));
        }
        "last_cell" => {
            let u = used.ok_or_else(|| CmdError::Failed("No cells were found.".into()))?;
            s.select(u.end);
            return Ok(json!([u.end.to_a1()]));
        }
        "current_region" => {
            let r = super::nav_cmds::current_region(s, s.sel.active);
            s.select_range(r, s.sel.active);
            return Ok(json!([r.to_a1()]));
        }
        "blanks" => {
            if scope.cell_count() > sp_core::MAX_CELLS_PER_OP {
                return Err(CmdError::Param("range too large".into()));
            }
            for a in scope.iter() {
                if sh.get(a).is_none_or(|c| c.value.is_empty() && c.formula_src().is_none()) {
                    hits.push(a);
                    if hits.len() >= MAX_LIST {
                        break;
                    }
                }
            }
        }
        "constants" | "formulas" | "errors" | "comments" | "notes" | "data_validation" => {
            for (a, c) in sh.cells_in(scope) {
                let ok = match kind {
                    "constants" => {
                        matches!(&c.content, Content::Value(v) if !v.is_empty() && type_ok(v))
                    }
                    "formulas" => c.formula_src().is_some() && type_ok(&c.value),
                    "errors" => matches!(c.value, Value::Error(_)),
                    _ => false,
                };
                if ok {
                    hits.push(a);
                }
                if hits.len() >= MAX_LIST {
                    break;
                }
            }
            if matches!(kind, "comments" | "notes") {
                hits = sh.comments.iter().map(|c| c.addr).filter(|a| scope.contains(*a)).collect();
            }
            if kind == "data_validation" {
                for v in &sh.validations {
                    for r in &v.ranges {
                        if r.cell_count() <= MAX_LIST as u64 {
                            hits.extend(r.iter());
                        }
                    }
                }
                hits.sort();
                hits.dedup();
                hits.truncate(MAX_LIST);
            }
        }
        other => {
            return Err(CmdError::Param(format!(
                "unknown kind '{other}' (blanks, constants, formulas, errors, visible, comments, notes, data_validation, current_region, last_cell)"
            )));
        }
    }
    let (Some(first), Some(_)) = (hits.first().copied(), hits.last()) else {
        return Err(CmdError::Failed("No cells were found.".into()));
    };
    let (r0, r1, c0, c1) = hits.iter().fold((u32::MAX, 0, u32::MAX, 0), |(a, b, c, d), x| {
        (a.min(x.row), b.max(x.row), c.min(x.col), d.max(x.col))
    });
    s.select_range(CellRange::new(CellAddr::new(r0, c0), CellAddr::new(r1, c1)), first);
    Ok(json!(hits.iter().map(|a| a.to_a1()).collect::<Vec<_>>()))
}

fn paste_link(s: &mut Session, p: &Json) -> CmdResult {
    let clip = s
        .clipboard
        .clone()
        .ok_or_else(|| CmdError::Failed("clipboard is empty".into()))?;
    let (sheet, dest) = cell(s, p, "cell")?;
    if clip.range.cell_count() > 1_000_000 {
        return Err(CmdError::Param("range too large".into()));
    }
    let src_name = s.wb.sheet(clip.sheet).map(|sh| sh.name.clone()).unwrap_or_default();
    let prefix = if clip.sheet == sheet {
        String::new()
    } else {
        format!("{}!", quote_sheet_name(&src_name))
    };
    let absolute = bool_opt(p, "absolute").unwrap_or(true);
    let mut writes = Vec::new();
    for a in clip.range.iter() {
        let (dr, dc) = (a.row - clip.range.start.row, a.col - clip.range.start.col);
        let Some(at) = dest.offset(i64::from(dr), i64::from(dc)) else {
            return Err(CmdError::Failed("The paste area doesn't fit on the sheet.".into()));
        };
        let reference = if absolute {
            format!("${}${}", col_to_letters(a.col), a.row + 1)
        } else {
            a.to_a1()
        };
        let style = s.wb.sheet(sheet).and_then(|sh| sh.get(at)).map(|c| c.style).unwrap_or_default();
        writes.push((at, Cell { style, ..Cell::formula(format!("{prefix}{reference}")) }));
    }
    let out = CellRange::new(dest, writes.last().map_or(dest, |(a, _)| *a));
    s.transact("Paste Link", |tx, _| {
        for (a, c) in writes {
            tx.set_cell(sheet, a, Some(c));
        }
        Ok(())
    })?;
    s.select_range(out, out.start);
    Ok(json!({ "range": out.to_a1() }))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new(
            "home.editing.fill_series",
            r#"{"range"?: "A1:A10", "series_in"?: "rows|columns", "type"?: "linear|growth|date", "date_unit"?: "day|weekday|month|year", "step"?: 1, "stop"?: 100}"#,
            fill_series,
        ),
        CommandSpec::new(
            "home.editing.goto_special",
            r#"{"kind": "blanks|constants|formulas|errors|visible|comments|notes|data_validation|current_region|last_cell", "types"?: ["numbers","text","logicals","errors"], "range"?: "A1:D9"}"#,
            goto_special,
        ),
        CommandSpec::new("select.visible_cells", r#"{"range"?: "A1:D9"} → visible areas"#, select_visible),
        CommandSpec::new(
            "home.clipboard.paste_link",
            r#"{"cell"?: "D1", "absolute"?: true}"#,
            paste_link,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &Session, a: &str) -> String {
        s.wb.sheets[0].value(CellAddr::parse(a).unwrap()).to_display()
    }

    #[test]
    fn series() {
        let mut s = Session::new();
        s.execute("cell.set", &json!({"cell": "A1", "input": "2"})).unwrap();
        s.execute("home.editing.fill_series", &json!({"range": "A1:A5", "step": 3})).unwrap();
        assert_eq!(v(&s, "A5"), "14");
        s.execute("home.editing.fill_series", &json!({"range": "A1", "type": "growth", "step": 2, "stop": 40}))
            .unwrap();
        assert_eq!(v(&s, "A5"), "32");
        assert_eq!(v(&s, "A6"), "");
        s.execute("cell.set", &json!({"cell": "C1", "input": "2024-01-31"})).unwrap();
        s.execute(
            "home.editing.fill_series",
            &json!({"range": "C1:C3", "type": "date", "date_unit": "month"}),
        )
        .unwrap();
        let feb = sp_core::date::date_to_serial(2024, 2, 29).unwrap();
        assert_eq!(s.wb.sheets[0].value(CellAddr::parse("C2").unwrap()), &Value::Number(feb));
        // Weekdays skip the weekend: Fri 2024-01-05 → Mon 2024-01-08.
        s.execute("cell.set", &json!({"cell": "D1", "input": "2024-01-05"})).unwrap();
        s.execute(
            "home.editing.fill_series",
            &json!({"range": "D1:D2", "type": "date", "date_unit": "weekday"}),
        )
        .unwrap();
        let mon = sp_core::date::date_to_serial(2024, 1, 8).unwrap();
        assert_eq!(s.wb.sheets[0].value(CellAddr::parse("D2").unwrap()), &Value::Number(mon));
    }

    #[test]
    fn goto_special_visible_and_paste_link() {
        let mut s = Session::new();
        for (a, i) in [("A1", "1"), ("A3", "=A1"), ("B2", "x"), ("B3", "=1/0")] {
            s.execute("cell.set", &json!({"cell": a, "input": i})).unwrap();
        }
        let r = s.execute("home.editing.goto_special", &json!({"kind": "blanks"})).unwrap();
        assert_eq!(r, json!(["B1", "A2"]));
        let r = s
            .execute("home.editing.goto_special", &json!({"kind": "constants", "types": ["text"]}))
            .unwrap();
        assert_eq!(r, json!(["B2"]));
        let r = s.execute("home.editing.goto_special", &json!({"kind": "errors"})).unwrap();
        assert_eq!(r, json!(["B3"]));
        s.execute("home.cells.hide_rows", &json!({"range": "A2"})).unwrap();
        let r = s.execute("select.visible_cells", &json!({"range": "A1:B4"})).unwrap();
        assert_eq!(r, json!(["A1:B1", "A3:B4"]));
        s.execute("home.clipboard.copy", &json!({"range": "A1:B1"})).unwrap();
        s.execute("sheet.insert", &json!({})).ok();
        s.execute("home.clipboard.paste_link", &json!({"cell": "D5"})).unwrap();
        let c = s.wb.sheets[0].get(CellAddr::parse("D5").unwrap()).unwrap();
        assert_eq!(c.input_text(), "=$A$1");
        assert_eq!(v(&s, "D5"), "1");
    }
}
