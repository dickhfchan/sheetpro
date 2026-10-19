//! Flash Fill (`home.editing.flash_fill`, Ctrl+E): the column of the active cell holds a
//! few typed examples next to source columns; infer the transformation
//! (`sp_ops::flash_fill`) and fill the column's empty cells in the current region.

use crate::params::{bool_opt, cell};
use crate::{CmdError, CmdResult, CommandSpec, Json, Session};
use serde_json::json;
use sp_core::{CellAddr, CellRange, Value};
use sp_model::Cell;
use sp_ops::flash_fill::{FlashError, MAX_COLUMNS, flash_fill};

fn failed(m: impl Into<String>) -> CmdError {
    CmdError::Failed(m.into())
}

/// Text of a cell as displayed (numbers and dates formatted).
fn shown(s: &Session, sheet: usize, a: CellAddr) -> String {
    let Some(c) = s.wb.sheet(sheet).and_then(|sh| sh.get(a)) else {
        return String::new();
    };
    if c.value.is_empty() {
        return String::new();
    }
    sp_render::display_cell(a, &c.value, None, s.wb.styles.get(c.style), false).text
}

/// A filled value: numbers that print back exactly become numbers, the rest stays text
/// (so `00123` keeps its zeros).
fn value_of(text: &str) -> Value {
    match text.parse::<f64>() {
        Ok(n) if n.is_finite() && sp_core::value::format_general(n) == text => Value::Number(n),
        _ => Value::text(text),
    }
}

struct Block {
    rows: Vec<u32>,
    src_cols: Vec<u32>,
    sources: Vec<Vec<String>>,
    outputs: Vec<Option<String>>,
}

fn block(s: &Session, sheet: usize, region: CellRange, col: u32, skip_first: bool) -> Block {
    // Source columns: the others of the region, nearest to the target first.
    let mut src_cols: Vec<u32> = (region.start.col..=region.end.col)
        .filter(|c| *c != col)
        .collect();
    src_cols.sort_by_key(|c| (c.abs_diff(col), *c > col));
    src_cols.truncate(MAX_COLUMNS);
    src_cols.sort_unstable();
    let first = region.start.row + u32::from(skip_first);
    let rows: Vec<u32> = (first..=region.end.row).collect();
    let sources = rows
        .iter()
        .map(|r| {
            src_cols
                .iter()
                .map(|c| shown(s, sheet, CellAddr::new(*r, *c)))
                .collect()
        })
        .collect();
    let outputs = rows
        .iter()
        .map(|r| {
            let t = shown(s, sheet, CellAddr::new(*r, col));
            (!t.is_empty()).then_some(t)
        })
        .collect();
    Block {
        rows,
        src_cols,
        sources,
        outputs,
    }
}

fn run(s: &mut Session, p: &Json) -> CmdResult {
    let (sheet, at) = cell(s, p, "cell")?;
    if sheet != s.sheet_index() {
        return Err(failed("Flash Fill works on the active sheet."));
    }
    let region = super::nav_cmds::current_region(s, at);
    if region.cols() < 2 {
        return Err(failed(
            "Flash Fill needs data in a column next to the examples.",
        ));
    }
    let header = bool_opt(p, "header");
    // With no explicit header flag, try the whole block first, then without its first row
    // (a header rarely fits the pattern).
    let attempts: Vec<bool> = match header {
        Some(h) => vec![h],
        None if region.rows() > 1 => vec![false, true],
        None => vec![false],
    };
    let mut last_err = FlashError::NoExamples;
    for skip in attempts {
        let b = block(s, sheet, region, at.col, skip);
        match flash_fill(&b.sources, &b.outputs) {
            Ok((program, filled)) => {
                let headers: Vec<String> = if skip {
                    b.src_cols
                        .iter()
                        .map(|c| shown(s, sheet, CellAddr::new(region.start.row, *c)))
                        .collect()
                } else {
                    Vec::new()
                };
                let desc = program.describe(&headers);
                let writes: Vec<(CellAddr, Value)> = b
                    .rows
                    .iter()
                    .zip(filled)
                    .filter_map(|(r, v)| v.map(|v| (CellAddr::new(*r, at.col), value_of(&v))))
                    .collect();
                if writes.is_empty() {
                    return Err(failed(FlashError::NothingToFill.to_string()));
                }
                let n = writes.len();
                s.transact("Flash Fill", |tx, _| {
                    for (a, v) in writes {
                        let style = tx
                            .wb
                            .sheet(sheet)
                            .and_then(|sh| sh.get(a))
                            .map(|c| c.style)
                            .unwrap_or_default();
                        tx.set_cell(
                            sheet,
                            a,
                            Some(Cell {
                                style,
                                ..Cell::constant(v)
                            }),
                        );
                    }
                    Ok(())
                })?;
                s.status = format!("Flash Fill changed {n} cells");
                return Ok(json!({"filled": n, "program": desc, "header": skip}));
            }
            Err(e @ FlashError::Ambiguous(_)) => return Err(failed(e.to_string())),
            Err(e) => last_err = e,
        }
    }
    Err(failed(last_err.to_string()))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec::new(
        "home.editing.flash_fill",
        r#"{"cell"?: "C2" (default: active cell; its column holds the examples), "header"?: bool (first row of the region is a header; default: detected)} → {"filled": n, "program": "1st word of Name + …"}"#,
        run,
    )]
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

    fn val(s: &Session, a: &str) -> Value {
        s.wb.sheets[0].value(CellAddr::parse(a).unwrap()).clone()
    }

    #[test]
    fn fills_first_names_below_a_header_and_undoes() {
        let mut s = Session::new();
        for (i, n) in ["Full name", "Ada Lovelace", "Alan Turing", "Grace Hopper"]
            .iter()
            .enumerate()
        {
            set(&mut s, &format!("A{}", i + 1), n);
        }
        set(&mut s, "B1", "First");
        set(&mut s, "B2", "Ada");
        s.select(CellAddr::parse("B3").unwrap());
        let r = s.execute("home.editing.flash_fill", &json!({})).unwrap();
        assert_eq!(r["filled"], 2);
        assert_eq!(r["header"], true);
        assert_eq!(r["program"], "1st word of Full name");
        assert_eq!(val(&s, "B3"), Value::text("Alan"));
        assert_eq!(val(&s, "B4"), Value::text("Grace"));
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(val(&s, "B3"), Value::Empty);
    }

    #[test]
    fn numbers_stay_numbers_and_codes_keep_zeros() {
        let mut s = Session::new();
        for (i, n) in ["SKU-00123-2024", "SKU-04410-2023", "SKU-07001-2022"]
            .iter()
            .enumerate()
        {
            set(&mut s, &format!("A{}", i + 1), n);
        }
        set(&mut s, "B1", "'00123");
        set(&mut s, "C1", "2024");
        s.execute("home.editing.flash_fill", &json!({"cell": "B1"})).unwrap();
        assert_eq!(val(&s, "B2"), Value::text("04410"));
        s.execute("home.editing.flash_fill", &json!({"cell": "C1"})).unwrap();
        assert_eq!(val(&s, "C3"), Value::Number(2022.0));
    }

    #[test]
    fn refuses_with_clear_errors() {
        let mut s = Session::new();
        // No data next to the column.
        set(&mut s, "D1", "x");
        assert!(s.execute("home.editing.flash_fill", &json!({"cell": "D1"})).is_err());
        // No examples.
        set(&mut s, "A1", "Ada Lovelace");
        set(&mut s, "A2", "Alan Turing");
        set(&mut s, "B1", "");
        let e = s
            .execute("home.editing.flash_fill", &json!({"cell": "B1"}))
            .unwrap_err()
            .to_string();
        assert!(e.contains("example"), "{e}");
        // Ambiguous: 2nd word or last word?
        set(&mut s, "A3", "Mary Ann Lee");
        set(&mut s, "B1", "Lovelace");
        let e = s
            .execute("home.editing.flash_fill", &json!({"cell": "B1", "header": false}))
            .unwrap_err()
            .to_string();
        assert!(e.contains("more than one pattern"), "{e}");
        assert_eq!(val(&s, "B2"), Value::Empty);
        // Bad params never panic.
        assert!(s.execute("home.editing.flash_fill", &json!({"cell": "nope"})).is_err());
    }
}
