//! What-if analysis: `data.what_if.goal_seek`, `data.what_if.data_table` (writes `TABLE()`
//! cells), `data.what_if.scenario` (Scenario Manager). Evaluation under substituted inputs
//! is `sp_formula::whatif`.

use super::pivot_cmds::{abs_ref, parse_ref_text};
use crate::params::{f64_req, str_opt, str_req};
use crate::{CmdError, CmdResult, CommandSpec, Json, Session};
use serde_json::json;
use sp_core::{CellAddr, CellRange, MAX_CELLS_PER_OP, Value};
use sp_model::pivot::{DataTable, MAX_OBJECTS, Scenario};
use sp_model::{Cell, Content, Style};

fn bad(m: impl Into<String>) -> CmdError {
    CmdError::Param(m.into())
}

fn failed(m: impl Into<String>) -> CmdError {
    CmdError::Failed(m.into())
}

fn one_cell(s: &Session, p: &Json, k: &str) -> Result<(usize, CellAddr), CmdError> {
    let t = str_req(p, k)?;
    let (i, r) = parse_ref_text(&s.wb, s.sheet_index(), t)
        .or_else(|| crate::params::parse_ref(s, t).ok())
        .ok_or_else(|| bad(format!("not a reference: '{t}'")))?;
    if !r.is_single() {
        return Err(bad(format!("'{k}' must be a single cell")));
    }
    Ok((i, r.start))
}

fn is_formula(s: &Session, k: (usize, CellAddr)) -> bool {
    s.wb
        .sheet(k.0)
        .and_then(|sh| sh.get(k.1))
        .is_some_and(|c| c.formula_src().is_some())
}

// ---------------------------------------------------------------------------------------
// Goal seek

fn goal_seek(s: &mut Session, p: &Json) -> CmdResult {
    let set = one_cell(s, p, "set_cell")?;
    let target = f64_req(p, "to_value")?;
    let changing = one_cell(s, p, "by_changing")?;
    if !is_formula(s, set) {
        return Err(failed("The 'set_cell' must contain a formula."));
    }
    if is_formula(s, changing) {
        return Err(failed("The 'by_changing' cell must contain a value, not a formula."));
    }
    let g = sp_formula::whatif::goal_seek(&s.wb, set, target, changing);
    if !g.found {
        return Err(failed(format!(
            "Goal Seek may not have found a solution (best input {}, result {}).",
            g.input,
            g.result.to_display()
        )));
    }
    s.transact("Goal Seek", |tx, _| {
        let style = tx
            .wb
            .sheet(changing.0)
            .and_then(|sh| sh.get(changing.1))
            .map(|c| c.style)
            .unwrap_or_default();
        tx.set_cell(
            changing.0,
            changing.1,
            Some(Cell {
                style,
                ..Cell::constant(Value::Number(g.input))
            }),
        );
        Ok(())
    })?;
    let result = s
        .wb
        .sheet(set.0)
        .map_or(Value::Empty, |sh| sh.value(set.1).clone());
    Ok(json!({
        "found": true,
        "value": g.input,
        "result": result.to_display(),
        "evaluations": g.evals,
    }))
}

// ---------------------------------------------------------------------------------------
// Data table

fn data_table(s: &mut Session, p: &Json) -> CmdResult {
    let home = s.sheet_index();
    let (sheet, r) = match str_opt(p, "range") {
        Some(t) => parse_ref_text(&s.wb, home, t).ok_or_else(|| bad(format!("not a reference: '{t}'")))?,
        None => (home, s.sel.range()),
    };
    if r.rows() < 2 || r.cols() < 2 {
        return Err(bad("a data table needs at least two rows and two columns"));
    }
    if r.cell_count() > MAX_CELLS_PER_OP {
        return Err(bad("the data table range is too large"));
    }
    let input = |k: &str| -> Result<Option<CellAddr>, CmdError> {
        match str_opt(p, k) {
            None => Ok(None),
            Some(t) if t.trim().is_empty() => Ok(None),
            Some(t) => {
                let (i, c) = parse_ref_text(&s.wb, sheet, t)
                    .ok_or_else(|| bad(format!("not a reference: '{t}'")))?;
                if i != sheet {
                    return Err(failed("Input cell reference is not valid: it must be on the data table's sheet."));
                }
                if !c.is_single() {
                    return Err(bad(format!("'{k}' must be a single cell")));
                }
                Ok(Some(c.start))
            }
        }
    };
    let ri = input("row_input")?;
    let ci = input("col_input")?;
    if ri.is_none() && ci.is_none() {
        return Err(bad("give 'row_input' and/or 'col_input'"));
    }
    let body = CellRange::new(CellAddr::new(r.start.row + 1, r.start.col + 1), r.end);
    if ri.into_iter().chain(ci).any(|a| r.contains(a)) {
        return Err(failed("Input cell reference is not valid: it is inside the table."));
    }
    let sh = s.wb.sheet(sheet).ok_or_else(|| failed("no such sheet"))?;
    if sh.analysis.data_tables.len() >= MAX_OBJECTS {
        return Err(failed("too many data tables on this sheet"));
    }
    let text = |a: Option<CellAddr>| a.map_or(String::new(), |a| a.to_a1());
    let formula = format!("TABLE({},{})", text(ri), text(ci));
    let model = DataTable {
        range: abs_ref(r),
        row_input: ri.map_or(String::new(), |a| abs_ref(CellRange::single(a))),
        col_input: ci.map_or(String::new(), |a| abs_ref(CellRange::single(a))),
    };
    s.transact("Data Table", |tx, _| {
        tx.touch_sheet(sheet);
        let sh = tx.wb.sheet_mut(sheet).ok_or_else(|| failed("no such sheet"))?;
        for a in body.iter() {
            let style = sh.get(a).map(|c| c.style).unwrap_or_default();
            sh.set(
                a,
                Cell {
                    style,
                    ..Cell::formula(formula.as_str())
                },
            );
        }
        // One record per block: a table rewritten over an old one replaces it.
        sh.analysis.data_tables.retain(|d| {
            CellRange::parse(&d.range.replace('$', "")).map_or(true, |x| x.intersect(&r).is_none())
        });
        sh.analysis.data_tables.push(model.clone());
        Ok(())
    })?;
    Ok(json!({"range": r.to_a1(), "formula": format!("={formula}"), "cells": body.cell_count()}))
}

// ---------------------------------------------------------------------------------------
// Scenario manager

/// Most changing cells per scenario (the target's limit).
const MAX_CHANGING: usize = 32;

/// Cells of a changing-cells text (`$B$1,$B$3:$B$4`) on `sheet`.
fn changing_cells(s: &Session, sheet: usize, text: &str) -> Result<Vec<CellAddr>, CmdError> {
    let mut out = Vec::new();
    for part in text.split(',') {
        if part.trim().is_empty() {
            continue;
        }
        let (i, r) = parse_ref_text(&s.wb, sheet, part)
            .ok_or_else(|| bad(format!("not a reference: '{part}'")))?;
        if i != sheet {
            return Err(failed("Changing cells must be on the scenario's sheet."));
        }
        if r.cell_count() > MAX_CHANGING as u64 {
            return Err(failed("Too many changing cells: a scenario holds at most 32."));
        }
        out.extend(r.iter());
        if out.len() > MAX_CHANGING {
            return Err(failed("Too many changing cells: a scenario holds at most 32."));
        }
    }
    if out.is_empty() {
        return Err(bad("missing 'changing' cells"));
    }
    Ok(out)
}

fn json_to_value(v: &Json) -> Value {
    match v {
        Json::Number(n) => n.as_f64().map_or(Value::Empty, Value::Number),
        Json::Bool(b) => Value::Bool(*b),
        Json::String(t) => match sp_core::parse::parse_number(t) {
            Some((n, _)) => Value::Number(n),
            None => Value::text(t.as_str()),
        },
        _ => Value::Empty,
    }
}

fn find_scenario(s: &Session, sheet: usize, name: &str) -> Option<usize> {
    s.wb.sheet(sheet)?
        .analysis
        .scenarios
        .iter()
        .position(|x| x.name.eq_ignore_ascii_case(name.trim()))
}

fn scenario(s: &mut Session, p: &Json) -> CmdResult {
    let sheet = s.sheet_index();
    let action = str_opt(p, "action").unwrap_or("list").to_ascii_lowercase();
    match action.as_str() {
        "list" => {
            let sh = s.wb.sheet(sheet).ok_or_else(|| failed("no active sheet"))?;
            let v: Vec<Json> = sh
                .analysis
                .scenarios
                .iter()
                .map(|x| {
                    json!({
                        "name": x.name,
                        "changing": x.changing,
                        "values": x.values.iter().map(Value::to_display).collect::<Vec<_>>(),
                        "comment": x.comment,
                    })
                })
                .collect();
            Ok(Json::Array(v))
        }
        "add" | "edit" => {
            let name = str_req(p, "name")?.trim().to_string();
            if name.is_empty() || name.chars().count() > 255 {
                return Err(bad("scenario name must be 1-255 characters"));
            }
            let existing = find_scenario(s, sheet, &name);
            if action == "add" && existing.is_some() {
                return Err(failed(format!("A scenario named '{name}' already exists.")));
            }
            let changing_text = match (str_opt(p, "changing"), existing) {
                (Some(t), _) => t.to_string(),
                (None, Some(i)) => s
                    .wb
                    .sheet(sheet)
                    .and_then(|sh| sh.analysis.scenarios.get(i))
                    .map_or(String::new(), |x| x.changing.clone()),
                (None, None) => return Err(bad("missing string 'changing'")),
            };
            let cells = changing_cells(s, sheet, &changing_text)?;
            let values: Vec<Value> = match p.get("values") {
                Some(Json::Array(a)) => {
                    if a.len() != cells.len() {
                        return Err(bad(format!(
                            "'values' needs one entry per changing cell ({})",
                            cells.len()
                        )));
                    }
                    a.iter().map(json_to_value).collect()
                }
                Some(_) => return Err(bad("'values' must be a list")),
                None => {
                    let sh = s.wb.sheet(sheet).ok_or_else(|| failed("no active sheet"))?;
                    cells.iter().map(|a| sh.value(*a).clone()).collect()
                }
            };
            if s.wb
                .sheet(sheet)
                .is_some_and(|sh| sh.analysis.scenarios.len() >= MAX_OBJECTS)
                && existing.is_none()
            {
                return Err(failed("too many scenarios on this sheet"));
            }
            let changing = cells
                .iter()
                .map(|a| abs_ref(CellRange::single(*a)))
                .collect::<Vec<_>>()
                .join(",");
            let comment: String = str_opt(p, "comment").unwrap_or("").chars().take(255).collect();
            let sc = Scenario {
                name: name.clone(),
                changing,
                values,
                comment,
            };
            s.transact("Scenario", |tx, _| {
                tx.touch_sheet(sheet);
                let sh = tx.wb.sheet_mut(sheet).ok_or_else(|| failed("no such sheet"))?;
                match existing.and_then(|i| sh.analysis.scenarios.get_mut(i)) {
                    Some(slot) => *slot = sc,
                    None => sh.analysis.scenarios.push(sc),
                }
                Ok(())
            })?;
            Ok(json!({"name": name, "cells": cells.len()}))
        }
        "delete" => {
            let name = str_req(p, "name")?;
            let i = find_scenario(s, sheet, name)
                .ok_or_else(|| bad(format!("no scenario named '{name}'")))?;
            s.transact("Delete Scenario", |tx, _| {
                tx.touch_sheet(sheet);
                if let Some(sh) = tx.wb.sheet_mut(sheet)
                    && i < sh.analysis.scenarios.len()
                {
                    sh.analysis.scenarios.remove(i);
                }
                Ok(())
            })?;
            Ok(json!({"deleted": name}))
        }
        "show" => {
            let name = str_req(p, "name")?;
            let i = find_scenario(s, sheet, name)
                .ok_or_else(|| bad(format!("no scenario named '{name}'")))?;
            let sc = s
                .wb
                .sheet(sheet)
                .and_then(|sh| sh.analysis.scenarios.get(i))
                .cloned()
                .ok_or_else(|| failed("no such scenario"))?;
            let cells = changing_cells(s, sheet, &sc.changing)?;
            s.transact("Show Scenario", |tx, _| {
                for (a, v) in cells.iter().zip(&sc.values) {
                    let style = tx
                        .wb
                        .sheet(sheet)
                        .and_then(|sh| sh.get(*a))
                        .map(|c| c.style)
                        .unwrap_or_default();
                    tx.set_cell(
                        sheet,
                        *a,
                        Some(Cell {
                            style,
                            ..Cell::constant(v.clone())
                        }),
                    );
                }
                Ok(())
            })?;
            Ok(json!({"shown": sc.name}))
        }
        "summary" => summary(s, p, sheet),
        other => Err(bad(format!(
            "unknown action '{other}' (add, edit, show, delete, summary, list)"
        ))),
    }
}

/// A new "Scenario Summary" sheet: changing cells and result cells, current values and the
/// values under each scenario (result cells recomputed with the scenario's inputs).
fn summary(s: &mut Session, p: &Json, sheet: usize) -> CmdResult {
    let scenarios = s
        .wb
        .sheet(sheet)
        .map(|sh| sh.analysis.scenarios.clone())
        .unwrap_or_default();
    if scenarios.is_empty() {
        return Err(failed("There are no scenarios on this sheet."));
    }
    let results: Vec<CellAddr> = match str_opt(p, "result_cells") {
        Some(t) if !t.trim().is_empty() => changing_cells(s, sheet, t)?,
        _ => Vec::new(),
    };
    let mut changing: Vec<CellAddr> = Vec::new();
    let mut per: Vec<Vec<(CellAddr, Value)>> = Vec::new();
    for sc in &scenarios {
        let cells = changing_cells(s, sheet, &sc.changing)?;
        for a in &cells {
            if !changing.contains(a) {
                changing.push(*a);
            }
        }
        per.push(cells.into_iter().zip(sc.values.iter().cloned()).collect());
    }
    let home = s.wb.sheet(sheet).ok_or_else(|| failed("no active sheet"))?;
    let current = |a: &CellAddr| home.value(*a).clone();
    // Rows: title, header, "Changing Cells:", changing rows, "Result Cells:", result rows.
    let mut grid: Vec<Vec<Value>> = Vec::new();
    grid.push(vec![Value::text("Scenario Summary")]);
    let mut header = vec![Value::Empty, Value::text("Current Values:")];
    header.extend(scenarios.iter().map(|x| Value::text(x.name.as_str())));
    grid.push(header);
    grid.push(vec![Value::text("Changing Cells:")]);
    for a in &changing {
        let mut row = vec![Value::text(abs_ref(CellRange::single(*a))), current(a)];
        for vals in &per {
            row.push(
                vals.iter()
                    .find(|(x, _)| x == a)
                    .map_or_else(|| current(a), |(_, v)| v.clone()),
            );
        }
        grid.push(row);
    }
    if !results.is_empty() {
        grid.push(vec![Value::text("Result Cells:")]);
        let keys: Vec<(usize, CellAddr)> = results.iter().map(|a| (sheet, *a)).collect();
        let under: Vec<Vec<Value>> = per
            .iter()
            .map(|vals| {
                let ov = vals.iter().map(|(a, v)| ((sheet, *a), v.clone())).collect();
                sp_formula::whatif::eval_substituted(&s.wb, ov, &keys)
            })
            .collect();
        for (k, a) in results.iter().enumerate() {
            let mut row = vec![Value::text(abs_ref(CellRange::single(*a))), current(a)];
            for u in &under {
                row.push(u.get(k).cloned().unwrap_or(Value::Empty));
            }
            grid.push(row);
        }
    }
    let base = "Scenario Summary";
    let name = (1u32..=1000)
        .map(|n| if n == 1 { base.to_string() } else { format!("{base} {n}") })
        .find(|n| s.wb.check_sheet_name(n, None).is_ok())
        .ok_or_else(|| failed("no free sheet name"))?;
    let at = (sheet + 1).min(s.wb.sheets.len());
    let sheet_name = name.clone();
    s.transact("Scenario Summary", move |tx, _| {
        tx.touch_book();
        let bold = tx.wb.styles.intern(Style {
            bold: true,
            ..Style::default()
        });
        let mut sh = sp_model::Sheet::new(sheet_name);
        for (r, row) in grid.iter().enumerate() {
            for (c, v) in row.iter().enumerate() {
                if v.is_empty() {
                    continue;
                }
                let mut cell = Cell::constant(v.clone());
                if r <= 1 || matches!(&cell.content, Content::Value(Value::Text(t)) if t.ends_with(':')) {
                    cell.style = bold;
                }
                sh.set(CellAddr::new(r as u32, c as u32), cell);
            }
        }
        tx.wb.sheets.insert(at, sh);
        for n in &mut tx.wb.names {
            if let Some(sc) = n.scope.as_mut()
                && *sc >= at
            {
                *sc += 1;
            }
        }
        tx.wb.active = at;
        Ok(())
    })?;
    s.select(CellAddr::default());
    Ok(json!({"sheet": name, "scenarios": scenarios.len()}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new(
            "data.what_if.goal_seek",
            r#"{"set_cell": "B4", "to_value": 1000, "by_changing": "B1"}"#,
            goal_seek,
        ),
        CommandSpec::new(
            "data.what_if.data_table",
            r#"{"range"?: "D2:F8" (default: selection; inputs on its top row / left column, formula(s) on the other edge), "row_input"?: "B1", "col_input"?: "B2"}"#,
            data_table,
        ),
        CommandSpec::new(
            "data.what_if.scenario",
            r#"{"action": "add|edit|show|delete|summary|list", "name"?: "Best case", "changing"?: "B1,B2:B3", "values"?: [1, 2, 3] (default: current values), "comment"?: "...", "result_cells"?: "B10,B11" (summary)}"#,
            scenario,
        ),
    ]
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use crate::Session;
    use serde_json::json;
    use sp_core::{CellAddr, Value};

    fn set(s: &mut Session, a: &str, v: &str) {
        s.execute("cell.set", &json!({"cell": a, "input": v})).unwrap();
    }

    fn num(s: &Session, sheet: usize, a: &str) -> f64 {
        match s.wb.sheets[sheet].value(CellAddr::parse(a).unwrap()) {
            Value::Number(n) => *n,
            other => panic!("{a}: {other:?}"),
        }
    }

    #[test]
    fn goal_seek_writes_input() {
        let mut s = Session::new();
        set(&mut s, "A1", "2");
        set(&mut s, "A2", "=A1*A1+1");
        let r = s
            .execute(
                "data.what_if.goal_seek",
                &json!({"set_cell": "A2", "to_value": 10, "by_changing": "A1"}),
            )
            .unwrap();
        assert_eq!(r["found"], true);
        assert!((num(&s, 0, "A1") - 3.0).abs() < 1e-6);
        assert!((num(&s, 0, "A2") - 10.0).abs() < 1e-3);
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(num(&s, 0, "A1"), 2.0);
        // Refusals.
        for bad in [
            json!({"set_cell": "A1", "to_value": 1, "by_changing": "A1"}),
            json!({"set_cell": "A2", "to_value": 1, "by_changing": "A2"}),
            json!({"set_cell": "A2", "to_value": -5, "by_changing": "A1"}),
            json!({"set_cell": "A2:A3", "to_value": 1, "by_changing": "A1"}),
            json!({"set_cell": "A2", "by_changing": "A1"}),
        ] {
            assert!(s.execute("data.what_if.goal_seek", &bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn one_and_two_variable_data_tables() {
        let mut s = Session::new();
        set(&mut s, "A1", "2");
        set(&mut s, "A2", "3");
        // One-variable, column-oriented: inputs C2:C4, formula D1.
        set(&mut s, "D1", "=A1*10");
        for (a, v) in [("C2", "1"), ("C3", "2"), ("C4", "3")] {
            set(&mut s, a, v);
        }
        let r = s
            .execute("data.what_if.data_table", &json!({"range": "C1:D4", "col_input": "A1"}))
            .unwrap();
        assert_eq!(r["formula"], "=TABLE(,A1)");
        assert_eq!(num(&s, 0, "D2"), 10.0);
        assert_eq!(num(&s, 0, "D4"), 30.0);
        // Two-variable: F1 formula, G1:H1 row inputs, F2:F3 col inputs.
        set(&mut s, "F1", "=A1*A2");
        for (a, v) in [("G1", "10"), ("H1", "20"), ("F2", "1"), ("F3", "2")] {
            set(&mut s, a, v);
        }
        s.execute(
            "data.what_if.data_table",
            &json!({"range": "F1:H3", "row_input": "A1", "col_input": "A2"}),
        )
        .unwrap();
        assert_eq!(num(&s, 0, "H3"), 40.0);
        assert_eq!(s.wb.sheets[0].analysis.data_tables.len(), 2);
        // Model change flows through.
        set(&mut s, "A1", "5");
        assert_eq!(num(&s, 0, "D1"), 50.0);
        assert_eq!(num(&s, 0, "D3"), 20.0);
        for bad in [
            json!({"range": "C1:D4"}),
            json!({"range": "C1:C4", "col_input": "A1"}),
            json!({"range": "C1:D4", "col_input": "D2"}),
            json!({"range": "C1:D4", "col_input": "A1:A2"}),
        ] {
            assert!(s.execute("data.what_if.data_table", &bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn scenarios_add_show_summary_delete() {
        let mut s = Session::new();
        set(&mut s, "B1", "100");
        set(&mut s, "B2", "0.25");
        set(&mut s, "B3", "=B1*(1+B2)");
        s.execute(
            "data.what_if.scenario",
            &json!({"action": "add", "name": "Best", "changing": "B1:B2", "values": [200, 0.5]}),
        )
        .unwrap();
        s.execute(
            "data.what_if.scenario",
            &json!({"action": "add", "name": "Worst", "changing": "B1,B2", "values": [50, "0"]}),
        )
        .unwrap();
        assert!(
            s.execute(
                "data.what_if.scenario",
                &json!({"action": "add", "name": "best", "changing": "B1"})
            )
            .is_err()
        );
        let list = s.execute("data.what_if.scenario", &json!({})).unwrap();
        assert_eq!(list.as_array().unwrap().len(), 2);
        s.execute("data.what_if.scenario", &json!({"action": "show", "name": "Best"}))
            .unwrap();
        assert_eq!(num(&s, 0, "B3"), 300.0);
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(num(&s, 0, "B3"), 125.0);
        let r = s
            .execute(
                "data.what_if.scenario",
                &json!({"action": "summary", "result_cells": "B3"}),
            )
            .unwrap();
        assert_eq!(r["sheet"], "Scenario Summary");
        assert_eq!(s.wb.active, 1);
        // Rows: title, header, Changing Cells:, $B$1, $B$2, Result Cells:, $B$3.
        assert_eq!(num(&s, 1, "C4"), 200.0);
        assert_eq!(num(&s, 1, "B7"), 125.0);
        assert_eq!(num(&s, 1, "C7"), 300.0);
        assert_eq!(num(&s, 1, "D7"), 50.0);
        s.wb.active = 0;
        s.execute("data.what_if.scenario", &json!({"action": "delete", "name": "Worst"}))
            .unwrap();
        assert_eq!(s.wb.sheets[0].analysis.scenarios.len(), 1);
        assert!(s.execute("data.what_if.scenario", &json!({"action": "bogus"})).is_err());
        assert!(
            s.execute(
                "data.what_if.scenario",
                &json!({"action": "add", "name": "Big", "changing": "A1:A40"})
            )
            .is_err()
        );
    }
}
