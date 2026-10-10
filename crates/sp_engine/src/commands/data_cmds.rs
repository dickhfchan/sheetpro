//! Data tools: sort, remove duplicates, text to columns, statistics (`data.*`,
//! `home.editing.sort_*`, `review.workbook_statistics`).

use crate::params::{bool_opt, range, str_opt};
use crate::{CmdError, CmdResult, CommandSpec, Json, Session};
use serde_json::json;
use sp_core::{CellAddr, CellRange, Value};
use sp_model::{Cell, Content};
use sp_ops::SortKey;

/// The range a sort applies to: the selection, or the current region around a single cell.
fn sort_target(s: &Session, p: &Json) -> Result<(usize, CellRange), CmdError> {
    let (sheet, r) = range(s, p, "range")?;
    if r.is_single() {
        Ok((sheet, super::nav_cmds::current_region(s, r.start)))
    } else {
        Ok((sheet, r))
    }
}

/// Heuristic header detection: first row is all text while the second row has non-text.
fn looks_like_header(s: &Session, sheet: usize, r: CellRange) -> bool {
    let Some(sh) = s.wb.sheet(sheet) else {
        return false;
    };
    if r.rows() < 2 {
        return false;
    }
    let row_types = |row: u32| {
        (r.start.col..=r.end.col)
            .map(|c| sh.value(CellAddr::new(row, c)).clone())
            .collect::<Vec<_>>()
    };
    let first = row_types(r.start.row);
    let second = row_types(r.start.row + 1);
    first
        .iter()
        .all(|v| matches!(v, Value::Text(_) | Value::Empty))
        && second
            .iter()
            .any(|v| !matches!(v, Value::Text(_) | Value::Empty))
}

fn quick_sort(s: &mut Session, p: &Json, ascending: bool) -> CmdResult {
    let (sheet, r) = sort_target(s, p)?;
    let key_col = s.sel.active.col.clamp(r.start.col, r.end.col) - r.start.col;
    let header = bool_opt(p, "header").unwrap_or_else(|| looks_like_header(s, sheet, r));
    s.transact(
        if ascending {
            "Sort Ascending"
        } else {
            "Sort Descending"
        },
        |tx, _| {
            sp_ops::edit::sort_rows(
                tx,
                sheet,
                r,
                &[SortKey {
                    col: key_col,
                    ascending,
                }],
                header,
            )
            .map_err(CmdError::from)
        },
    )?;
    Ok(json!({ "range": r.to_a1(), "header": header }))
}

fn sort_dialog(s: &mut Session, p: &Json) -> CmdResult {
    let (sheet, r) = sort_target(s, p)?;
    let keys_json = p
        .get("keys")
        .and_then(Json::as_array)
        .ok_or_else(|| CmdError::Param("missing array 'keys'".into()))?;
    let mut keys = Vec::new();
    for k in keys_json {
        let col_name = k
            .get("column")
            .and_then(Json::as_str)
            .ok_or_else(|| CmdError::Param("each key needs 'column' (letter)".into()))?;
        let col = sp_core::letters_to_col(col_name)
            .ok_or_else(|| CmdError::Param(format!("bad column '{col_name}'")))?;
        if col < r.start.col || col > r.end.col {
            return Err(CmdError::Param(format!(
                "column {col_name} is outside the sort range"
            )));
        }
        let ascending = k
            .get("order")
            .and_then(Json::as_str)
            .is_none_or(|o| !o.eq_ignore_ascii_case("desc"));
        keys.push(SortKey {
            col: col - r.start.col,
            ascending,
        });
    }
    if keys.is_empty() || keys.len() > 64 {
        return Err(CmdError::Param("between 1 and 64 sort keys".into()));
    }
    let header = bool_opt(p, "header").unwrap_or_else(|| looks_like_header(s, sheet, r));
    s.transact("Sort", |tx, _| {
        sp_ops::edit::sort_rows(tx, sheet, r, &keys, header).map_err(CmdError::from)
    })?;
    Ok(json!({ "range": r.to_a1(), "header": header }))
}

fn remove_duplicates(s: &mut Session, p: &Json) -> CmdResult {
    let (sheet, r) = sort_target(s, p)?;
    if r.cell_count() > sp_core::MAX_CELLS_PER_OP {
        return Err(CmdError::Param("range too large".into()));
    }
    let header = bool_opt(p, "header").unwrap_or_else(|| looks_like_header(s, sheet, r));
    let cols: Vec<u32> = match p.get("columns").and_then(Json::as_array) {
        Some(a) => a
            .iter()
            .filter_map(Json::as_str)
            .filter_map(sp_core::letters_to_col)
            .filter(|c| *c >= r.start.col && *c <= r.end.col)
            .collect(),
        None => (r.start.col..=r.end.col).collect(),
    };
    let sh =
        s.wb.sheet(sheet)
            .ok_or_else(|| CmdError::Failed("no sheet".into()))?;
    let first = if header { r.start.row + 1 } else { r.start.row };
    let mut seen: Vec<Vec<String>> = Vec::new();
    let mut keep: Vec<u32> = Vec::new();
    for row in first..=r.end.row {
        let key: Vec<String> = cols
            .iter()
            .map(|c| sh.value(CellAddr::new(row, *c)).to_display().to_lowercase())
            .collect();
        if !seen.contains(&key) {
            seen.push(key);
            keep.push(row);
        }
    }
    let removed = (r.end.row + 1 - first) as usize - keep.len();
    let rows: Vec<Vec<Option<Cell>>> = keep
        .iter()
        .map(|row| {
            (r.start.col..=r.end.col)
                .map(|c| sh.get(CellAddr::new(*row, c)).cloned())
                .collect()
        })
        .collect();
    s.transact("Remove Duplicates", |tx, _| {
        for (i, row) in (first..=r.end.row).enumerate() {
            for (j, c) in (r.start.col..=r.end.col).enumerate() {
                let cell = rows.get(i).and_then(|r| r.get(j)).cloned().flatten();
                tx.set_cell(sheet, CellAddr::new(row, c), cell);
            }
        }
        Ok(())
    })?;
    s.status = format!(
        "{removed} duplicate values found and removed; {} unique values remain.",
        keep.len()
    );
    Ok(json!({ "removed": removed, "remaining": keep.len() }))
}

fn text_to_columns(s: &mut Session, p: &Json) -> CmdResult {
    let (sheet, r) = range(s, p, "range")?;
    if r.cols() != 1 {
        return Err(CmdError::Param(
            "Text to Columns works on one column at a time".into(),
        ));
    }
    let delim = str_opt(p, "delimiter").unwrap_or(",").to_string();
    if delim.is_empty() {
        return Err(CmdError::Param("delimiter is empty".into()));
    }
    let sh =
        s.wb.sheet(sheet)
            .ok_or_else(|| CmdError::Failed("no sheet".into()))?;
    let mut writes = Vec::new();
    for (a, c) in sh.cells_in(r) {
        let Content::Value(Value::Text(t)) = &c.content else {
            continue;
        };
        for (k, part) in t.split(delim.as_str()).enumerate() {
            if let Some(at) = a.offset(0, k as i64) {
                let v = sp_io::csv_io::field_value(part.trim());
                writes.push((
                    at,
                    if v.is_empty() {
                        None
                    } else {
                        Some(Cell::constant(v))
                    },
                ));
            }
        }
    }
    let n = writes.len();
    s.transact("Text to Columns", |tx, _| {
        for (a, c) in writes {
            tx.set_cell(sheet, a, c);
        }
        Ok(())
    })?;
    Ok(json!(n))
}

fn statistics(s: &mut Session, _: &Json) -> CmdResult {
    let mut cells = 0;
    let mut formulas = 0;
    for sh in &s.wb.sheets {
        for (_, c) in sh.iter() {
            if !c.value.is_empty() || c.formula_src().is_some() {
                cells += 1;
            }
            if c.formula_src().is_some() {
                formulas += 1;
            }
        }
    }
    let sheet = s.wb.sheet(s.sheet_index());
    Ok(json!({
        "sheets": s.wb.sheets.len(),
        "cells_with_data": cells,
        "formulas": formulas,
        "end_of_sheet": sheet.and_then(|x| x.used_range()).map(|r| r.end.to_a1()),
        "names": s.wb.names.len(),
        "merged_ranges": s.wb.sheets.iter().map(|x| x.merges.len()).sum::<usize>(),
    }))
}

pub fn specs() -> Vec<CommandSpec> {
    let sort = r#"{"range"?: "A1:D20", "header"?: bool}"#;
    vec![
        CommandSpec::new("home.editing.sort_az", sort, |s, p| quick_sort(s, p, true)),
        CommandSpec::new("home.editing.sort_za", sort, |s, p| quick_sort(s, p, false)),
        CommandSpec::new("data.sort_az", sort, |s, p| quick_sort(s, p, true)),
        CommandSpec::new("data.sort_za", sort, |s, p| quick_sort(s, p, false)),
        CommandSpec::new(
            "data.sort",
            r#"{"range"?: "A1:D20", "keys": [{"column": "B", "order": "asc|desc"}], "header"?: bool}"#,
            sort_dialog,
        ),
        CommandSpec::new(
            "home.editing.sort_custom",
            r#"{"range"?: "A1:D20", "keys": [{"column": "B", "order": "asc|desc"}]}"#,
            sort_dialog,
        ),
        CommandSpec::new(
            "data.remove_duplicates",
            r#"{"range"?: "A1:C50", "columns"?: ["A","C"], "header"?: bool}"#,
            remove_duplicates,
        ),
        CommandSpec::new(
            "data.text_to_columns",
            r#"{"range"?: "A1:A50", "delimiter"?: ","}"#,
            text_to_columns,
        ),
        CommandSpec::new("review.workbook_statistics", "{}", statistics),
    ]
}
