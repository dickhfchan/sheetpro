//! Structured tables: `insert.table`, `home.styles.format_as_table`, `table.design`
//! (options, totals row, resize, rename, convert to range), and the auto-expansion that runs
//! after typing directly below or right of a table.

use crate::params::{bool_opt, range, str_opt};
use crate::{CmdError, CmdResult, CommandSpec, Json, Session};
use serde_json::json;
use sp_core::{CellAddr, CellRange, MAX_COLS, MAX_ROWS, Value};
use sp_model::table::valid_table_name;
use sp_model::{Cell, Content, Sheet, Table, Workbook};

/// Columns per table (the target's sheet width).
const MAX_TABLE_COLS: u32 = MAX_COLS;

fn bad(m: impl Into<String>) -> CmdError {
    CmdError::Param(m.into())
}

/// Unique, non-blank column names from header texts.
fn header_names(texts: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (i, t) in texts.into_iter().enumerate() {
        let base = if t.trim().is_empty() {
            format!("Column{}", i + 1)
        } else {
            t.trim().to_string()
        };
        let mut name = base.clone();
        let mut n = 2;
        while out.iter().any(|o| o.eq_ignore_ascii_case(&name)) {
            name = format!("{base}{n}");
            n += 1;
        }
        out.push(name);
    }
    out
}

fn overlaps_table(wb: &Workbook, sheet: usize, r: CellRange, except: Option<&str>) -> bool {
    wb.sheet(sheet).is_some_and(|sh| {
        sh.tables
            .iter()
            .filter(|t| except.is_none_or(|e| !t.name.eq_ignore_ascii_case(e)))
            .any(|t| t.range.intersect(&r).is_some())
    })
}

fn check_area(
    wb: &Workbook,
    sheet: usize,
    r: CellRange,
    except: Option<&str>,
) -> Result<(), CmdError> {
    if r.is_full_rows() || r.is_full_cols() || r.cell_count() > sp_core::MAX_CELLS_PER_OP {
        return Err(bad("a table cannot cover whole rows or columns"));
    }
    if overlaps_table(wb, sheet, r, except) {
        return Err(CmdError::Failed(
            "a table cannot overlap another table".into(),
        ));
    }
    if let Some(sh) = wb.sheet(sheet)
        && sh.merges.iter().any(|m| m.intersect(&r).is_some())
    {
        return Err(CmdError::Failed(
            "a table cannot contain merged cells".into(),
        ));
    }
    Ok(())
}

fn create(s: &mut Session, p: &Json, label: &str) -> CmdResult {
    let (sheet, mut r) = range(s, p, "range")?;
    if p.get("range").is_none() && r.is_single() {
        r = super::nav_cmds::current_region(s, s.sel.active);
    }
    let has_headers = bool_opt(p, "has_headers").unwrap_or(true);
    let style = str_opt(p, "style")
        .unwrap_or(sp_model::table::DEFAULT_TABLE_STYLE)
        .to_string();
    if style.len() > 255 {
        return Err(bad("style name too long"));
    }
    if r.cols() > MAX_TABLE_COLS {
        return Err(bad("too many columns"));
    }
    let (id, default_name) = s.wb.next_table_id_and_name();
    let name = str_opt(p, "name").map_or(default_name, str::to_string);
    if !valid_table_name(&name) {
        return Err(bad(format!("'{name}' is not a valid table name")));
    }
    if s.wb.find_table(&name).is_some() || s.wb.name_lookup(&name, sheet).is_some() {
        return Err(CmdError::Failed(format!(
            "the name '{name}' is already in use"
        )));
    }
    // Without headers, a header row is added above the data (the data moves down one row
    // when the row below the range is free).
    let area = if has_headers {
        r
    } else {
        if r.end.row + 1 >= MAX_ROWS {
            return Err(CmdError::Failed("no room for a header row".into()));
        }
        CellRange::new(r.start, CellAddr::new(r.end.row + 1, r.end.col))
    };
    check_area(&s.wb, sheet, area, None)?;
    if !has_headers {
        let below = CellRange::new(CellAddr::new(area.end.row, area.start.col), area.end);
        let busy =
            s.wb.sheet(sheet)
                .is_some_and(|sh| sh.cells_in(below).next().is_some());
        if busy {
            return Err(CmdError::Failed(
                "cannot add a header row: the row below the range is not empty".into(),
            ));
        }
    }
    let out = s.transact(label, |tx, _| {
        tx.touch_sheet(sheet);
        let sh = tx
            .wb
            .sheet_mut(sheet)
            .ok_or_else(|| CmdError::Failed("no such sheet".into()))?;
        if !has_headers {
            // Shift the block down one row, bottom-up.
            for row in (r.start.row..=r.end.row).rev() {
                for col in r.start.col..=r.end.col {
                    if let Some(c) = sh.remove(CellAddr::new(row, col)) {
                        sh.set(CellAddr::new(row + 1, col), c);
                    }
                }
            }
        }
        let texts: Vec<String> = (area.start.col..=area.end.col)
            .map(|c| {
                if has_headers {
                    sh.value(CellAddr::new(area.start.row, c)).to_display()
                } else {
                    String::new()
                }
            })
            .collect();
        let names = header_names(texts);
        // Header cells hold their names as text constants.
        for (i, n) in names.iter().enumerate() {
            let a = CellAddr::new(area.start.row, area.start.col + i as u32);
            let style = sh.get(a).map(|c| c.style).unwrap_or_default();
            sh.set(
                a,
                Cell {
                    style,
                    ..Cell::constant(Value::text(n.as_str()))
                },
            );
        }
        let mut t = Table::new(id, name.clone(), area, names);
        t.style.clone_from(&style);
        sh.tables.push(t);
        Ok(json!({"name": name, "range": area.to_a1(), "style": style}))
    })?;
    if let Some(a) = out
        .get("range")
        .and_then(Json::as_str)
        .and_then(|t| CellRange::parse(t).ok())
    {
        s.select_range(a, a.start);
    }
    Ok(out)
}

fn format_as_table(s: &mut Session, p: &Json) -> CmdResult {
    // Inside an existing table: restyle it.
    let sheet = s.sheet_index();
    let at = s.sel.active;
    let existing =
        s.wb.sheet(sheet)
            .and_then(|sh| sh.table_at(at))
            .map(|t| t.name.clone());
    if p.get("range").is_none()
        && let Some(name) = existing
    {
        let style = str_opt(p, "style").unwrap_or(sp_model::table::DEFAULT_TABLE_STYLE);
        return design(s, &json!({"name": name, "style": style}));
    }
    create(s, p, "Format as Table")
}

/// The table named by `name`, or the one containing the active cell.
fn target(s: &Session, p: &Json) -> Result<(usize, String), CmdError> {
    if let Some(n) = str_opt(p, "name") {
        return s
            .wb
            .find_table(n)
            .map(|(i, t)| (i, t.name.clone()))
            .ok_or_else(|| bad(format!("no table named '{n}'")));
    }
    let sheet = s.sheet_index();
    s.wb.sheet(sheet)
        .and_then(|sh| sh.table_at(s.sel.active))
        .map(|t| (sheet, t.name.clone()))
        .ok_or_else(|| CmdError::Failed("the active cell is not in a table".into()))
}

fn table_mut<'a>(sh: &'a mut Sheet, name: &str) -> Result<&'a mut Table, CmdError> {
    sh.tables
        .iter_mut()
        .find(|t| t.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| CmdError::Failed("table vanished".into()))
}

fn describe(t: &Table) -> Json {
    json!({
        "name": t.name, "range": t.range.to_a1(), "style": t.style,
        "header_row": t.header_row, "total_row": t.totals_row,
        "banded_rows": t.banded_rows, "banded_cols": t.banded_cols,
        "first_col": t.first_col, "last_col": t.last_col, "filter_button": t.autofilter,
        "columns": t.columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
    })
}

/// Turn the totals row on or off (adds/removes a row below the body).
fn set_totals(sh: &mut Sheet, name: &str, on: bool) -> Result<(), CmdError> {
    let t = table_mut(sh, name)?.clone();
    if t.totals_row == on {
        return Ok(());
    }
    if on {
        let row = t.range.end.row + 1;
        if row >= MAX_ROWS {
            return Err(CmdError::Failed("no room for a total row".into()));
        }
        let line = CellRange::new(
            CellAddr::new(row, t.range.start.col),
            CellAddr::new(row, t.range.end.col),
        );
        if sh.cells_in(line).next().is_some() {
            return Err(CmdError::Failed(
                "the row below the table is not empty".into(),
            ));
        }
        if sh
            .tables
            .iter()
            .any(|o| o.name != t.name && o.range.intersect(&line).is_some())
        {
            return Err(CmdError::Failed("another table is below".into()));
        }
        let last = t.columns.len().saturating_sub(1);
        let mut cols = t.columns.clone();
        for (i, c) in cols.iter_mut().enumerate() {
            let a = CellAddr::new(row, t.range.start.col + i as u32);
            if i == 0 && last > 0 {
                c.totals_label = Some("Total".into());
                sh.set(a, Cell::constant(Value::text("Total")));
            } else if i == last {
                c.totals_function = Some("sum".into());
                sh.set(
                    a,
                    Cell::formula(format!("SUBTOTAL(109,{}[{}])", t.name, escape_col(&c.name))),
                );
            }
        }
        let tm = table_mut(sh, name)?;
        tm.columns = cols;
        tm.totals_row = true;
        tm.range = CellRange::new(t.range.start, CellAddr::new(row, t.range.end.col));
    } else {
        let row = t.range.end.row;
        for c in t.range.start.col..=t.range.end.col {
            sh.remove(CellAddr::new(row, c));
        }
        let tm = table_mut(sh, name)?;
        tm.totals_row = false;
        tm.range = CellRange::new(
            t.range.start,
            CellAddr::new(
                row.saturating_sub(1).max(t.range.start.row),
                t.range.end.col,
            ),
        );
        for c in &mut tm.columns {
            c.totals_function = None;
            c.totals_label = None;
            c.totals_formula = None;
        }
    }
    Ok(())
}

fn escape_col(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if matches!(c, '[' | ']' | '#' | '\'') {
            out.push('\'');
        }
        out.push(c);
    }
    out
}

fn design(s: &mut Session, p: &Json) -> CmdResult {
    let (sheet, name) = target(s, p)?;
    let keys = [
        "style",
        "total_row",
        "banded_rows",
        "banded_cols",
        "first_col",
        "last_col",
        "filter_button",
        "rename",
        "resize",
        "convert_to_range",
        "header_row",
    ];
    if !keys.iter().any(|k| p.get(*k).is_some()) {
        let sh = s.wb.sheet(sheet).ok_or_else(|| bad("no sheet"))?;
        let t = sh
            .tables
            .iter()
            .find(|t| t.name == name)
            .ok_or_else(|| bad("no table"))?;
        return Ok(describe(t));
    }
    if let Some(n) = str_opt(p, "rename") {
        if !valid_table_name(n) {
            return Err(bad(format!("'{n}' is not a valid table name")));
        }
        if !n.eq_ignore_ascii_case(&name)
            && (s.wb.find_table(n).is_some() || s.wb.name_lookup(n, sheet).is_some())
        {
            return Err(CmdError::Failed(format!(
                "the name '{n}' is already in use"
            )));
        }
    }
    let resize = match str_opt(p, "resize") {
        Some(t) => {
            let r = CellRange::parse(t).map_err(|_| bad(format!("bad range '{t}'")))?;
            check_area(&s.wb, sheet, r, Some(&name))?;
            Some(r)
        }
        None => None,
    };
    if bool_opt(p, "header_row") == Some(false) {
        return Err(bad("tables always keep their header row in SheetPro"));
    }
    let convert = bool_opt(p, "convert_to_range").unwrap_or(false);
    let out = s.transact("Table Design", |tx, _| {
        tx.touch_book();
        let wb = &mut *tx.wb;
        let sh = wb
            .sheet_mut(sheet)
            .ok_or_else(|| CmdError::Failed("no such sheet".into()))?;
        if convert {
            // The structured table goes away; cells keep their values (and formulas).
            sh.tables.retain(|t| t.name != name);
            return Ok(json!({"converted": name}));
        }
        if let Some(on) = bool_opt(p, "total_row") {
            set_totals(sh, &name, on)?;
        }
        if let Some(r) = resize {
            table_mut(sh, &name)?.range = r;
            sync_headers(sh, &name);
        }
        let t = table_mut(sh, &name)?;
        if let Some(v) = str_opt(p, "style") {
            t.style = v.to_string();
        }
        for (k, f) in [
            ("banded_rows", &mut t.banded_rows),
            ("banded_cols", &mut t.banded_cols),
            ("first_col", &mut t.first_col),
            ("last_col", &mut t.last_col),
            ("filter_button", &mut t.autofilter),
        ] {
            if let Some(b) = bool_opt(p, k) {
                *f = b;
            }
        }
        if let Some(n) = str_opt(p, "rename") {
            let old = t.name.clone();
            t.name = n.to_string();
            rename_in_formulas(wb, &old, n);
        }
        let d = wb
            .sheet(sheet)
            .and_then(|sh| {
                sh.tables.iter().find(|t| {
                    t.name
                        .eq_ignore_ascii_case(str_opt(p, "rename").unwrap_or(&name))
                })
            })
            .map(describe)
            .unwrap_or(Json::Null);
        Ok(d)
    })?;
    Ok(out)
}

/// Rewrite `Old[` → `New[` in every formula (structured references to a renamed table).
fn rename_in_formulas(wb: &mut Workbook, old: &str, new: &str) {
    for sh in &mut wb.sheets {
        let addrs: Vec<CellAddr> = sh
            .iter()
            .filter(|(_, c)| c.formula_src().is_some_and(|f| f.contains('[')))
            .map(|(a, _)| a)
            .collect();
        for a in addrs {
            let Some(c) = sh.get_mut(a) else { continue };
            let Content::Formula(f) = &c.content else {
                continue;
            };
            let Ok(mut e) = sp_formula::parse(f) else {
                continue;
            };
            let mut hit = false;
            e.walk_mut(&mut |x| {
                if let sp_formula::Expr::Table(t) = x
                    && t.table
                        .as_deref()
                        .is_some_and(|n| n.eq_ignore_ascii_case(old))
                {
                    t.table = Some(new.to_string());
                    hit = true;
                }
            });
            if hit {
                c.content = Content::Formula(sp_formula::to_formula(&e));
            }
        }
    }
}

/// Re-read column names from the header row after a resize or header edit.
fn sync_headers(sh: &mut Sheet, name: &str) {
    let Some(t) = sh.tables.iter().find(|t| t.name == name).cloned() else {
        return;
    };
    let row = t.range.start.row;
    let texts: Vec<String> = (t.range.start.col..=t.range.end.col)
        .enumerate()
        .map(|(i, c)| {
            if t.header_row {
                sh.value(CellAddr::new(row, c)).to_display()
            } else {
                t.columns.get(i).map(|c| c.name.clone()).unwrap_or_default()
            }
        })
        .collect();
    let names = header_names(texts);
    if t.header_row {
        // Blank header cells get their generated names.
        for (i, n) in names.iter().enumerate() {
            let a = CellAddr::new(row, t.range.start.col + i as u32);
            if sh.value(a).is_empty() {
                let style = sh.get(a).map(|c| c.style).unwrap_or_default();
                sh.set(
                    a,
                    Cell {
                        style,
                        ..Cell::constant(Value::text(n.as_str()))
                    },
                );
            }
        }
    }
    if let Some(tm) = sh.tables.iter_mut().find(|x| x.name == name) {
        let old = std::mem::take(&mut tm.columns);
        tm.columns = names
            .into_iter()
            .enumerate()
            .map(|(i, n)| {
                let mut c = old.get(i).cloned().unwrap_or_default();
                c.name = n;
                c
            })
            .collect();
    }
}

/// After typing: grow tables whose next row/column received a value, copy calculated-column
/// formulas into the new row, and keep column names in step with header edits. Runs as its
/// own undo step ("Table AutoExpansion"), like the target app.
pub fn auto_expand(s: &mut Session, cells: &[(usize, CellAddr)]) {
    let mut plan: Vec<(usize, String, Option<CellRange>, bool)> = Vec::new();
    for (si, a) in cells {
        let Some(sh) = s.wb.sheet(*si) else { continue };
        if sh.tables.is_empty() || sh.value(*a).is_empty() {
            continue;
        }
        for t in &sh.tables {
            let r = t.range;
            let header_edit = t.header_row && a.row == r.start.row && r.contains(*a);
            let below = !t.totals_row
                && a.row == r.end.row + 1
                && a.col >= r.start.col
                && a.col <= r.end.col;
            let right = a.col == r.end.col + 1 && a.row >= r.start.row && a.row <= r.end.row;
            let grown = if below {
                Some(CellRange::new(r.start, CellAddr::new(a.row, r.end.col)))
            } else if right {
                Some(CellRange::new(r.start, CellAddr::new(r.end.row, a.col)))
            } else {
                None
            };
            if let Some(g) = grown
                && (overlaps_table(&s.wb, *si, g, Some(&t.name))
                    || sh.merges.iter().any(|m| m.intersect(&g).is_some()))
            {
                continue;
            }
            if grown.is_some() || header_edit {
                plan.push((*si, t.name.clone(), grown, below));
            }
        }
    }
    if plan.is_empty() {
        return;
    }
    let _ = s.transact("Table AutoExpansion", |tx, _| {
        for (si, name, grown, below) in &plan {
            tx.touch_sheet(*si);
            let Some(sh) = tx.wb.sheet_mut(*si) else {
                continue;
            };
            let Ok(t) = table_mut(sh, name) else { continue };
            let old = t.range;
            if let Some(g) = grown {
                t.range = *g;
            }
            let new_range = t.range;
            if *below {
                // Calculated columns: carry the formula of the last body row down.
                for c in old.start.col..=old.end.col {
                    let src = CellAddr::new(old.end.row, c);
                    let dst = CellAddr::new(new_range.end.row, c);
                    if sh
                        .get(dst)
                        .is_some_and(|x| !x.value.is_empty() || x.formula_src().is_some())
                    {
                        continue;
                    }
                    let Some(f) = sh
                        .get(src)
                        .and_then(|x| x.formula_src())
                        .map(str::to_string)
                    else {
                        continue;
                    };
                    let Ok(mut e) = sp_formula::parse(&f) else {
                        continue;
                    };
                    sp_formula::shift::shift_for_copy(&mut e, 1, 0);
                    let style = sh.get(src).map(|x| x.style).unwrap_or_default();
                    sh.set(
                        dst,
                        Cell {
                            style,
                            ..Cell::formula(sp_formula::to_formula(&e))
                        },
                    );
                }
            }
            sync_headers(sh, name);
        }
        Ok(())
    });
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new(
            "insert.table",
            r#"{"range"?: "A1:D20" (default: selection, or the current region around a single active cell), "has_headers"?: true, "name"?: "Table1", "style"?: "TableStyleMedium2"} → {"name", "range", "style"}"#,
            |s, p| create(s, p, "Create Table"),
        ),
        CommandSpec::new(
            "home.styles.format_as_table",
            r#"{"style"?: "TableStyleLight1..21|TableStyleMedium1..28|TableStyleDark1..11", "range"?, "has_headers"?, "name"?} — restyles the table at the active cell, else creates one"#,
            format_as_table,
        ),
        CommandSpec::new(
            "table.design",
            r#"{"name"?: "Table1" (default: table at the active cell), "style"?, "total_row"?: bool, "banded_rows"?, "banded_cols"?, "first_col"?, "last_col"?, "filter_button"?, "rename"?: "Sales", "resize"?: "A1:E30", "convert_to_range"?: true} → table description (no options: just describe)"#,
            design,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(s: &mut Session, a: &str, v: &str) {
        s.execute("cell.set", &json!({"cell": a, "input": v}))
            .unwrap();
    }

    fn val(s: &Session, a: &str) -> Value {
        s.wb.sheets[0].value(CellAddr::parse(a).unwrap()).clone()
    }

    fn session() -> Session {
        let mut s = Session::new();
        for (a, v) in [
            ("A1", "Item"),
            ("B1", "Qty"),
            ("C1", "Price"),
            ("A2", "Pen"),
            ("B2", "2"),
            ("C2", "1.5"),
            ("A3", "Pad"),
            ("B3", "3"),
            ("C3", "4"),
        ] {
            set(&mut s, a, v);
        }
        s
    }

    #[test]
    fn create_reference_grow_totals_rename() {
        let mut s = session();
        s.select(CellAddr::new(1, 1));
        let out = s.execute("insert.table", &json!({})).unwrap();
        assert_eq!(out["range"], "A1:C3");
        assert_eq!(out["name"], "Table1");
        // Calculated column to the right grows the table and names the column.
        set(&mut s, "D1", "Total");
        let t = &s.wb.sheets[0].tables[0];
        assert_eq!(t.range.to_a1(), "A1:D3");
        assert_eq!(t.columns[3].name, "Total");
        set(&mut s, "D2", "=[@Qty]*[@Price]");
        set(&mut s, "D3", "=[@Qty]*[@Price]");
        set(&mut s, "F1", "=SUM(Table1[Total])");
        assert_eq!(val(&s, "F1"), Value::Number(15.0));
        // Typing below grows the table and fills the calculated column.
        set(&mut s, "A4", "Ink");
        set(&mut s, "B4", "1");
        set(&mut s, "C4", "10");
        assert_eq!(s.wb.sheets[0].tables[0].range.to_a1(), "A1:D4");
        assert_eq!(val(&s, "D4"), Value::Number(10.0));
        assert_eq!(val(&s, "F1"), Value::Number(25.0));
        // Undo/redo: typing into the grown row is an ordinary step; undoing the step
        // that typed A4 also undoes the expansion (its own step) only when reached.
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(s.wb.sheets[0].tables[0].range.to_a1(), "A1:D4");
        assert_eq!(val(&s, "F1"), Value::Number(15.0));
        s.execute("edit.redo", &json!({})).unwrap();
        assert_eq!(val(&s, "F1"), Value::Number(25.0));
        // Totals row.
        let d = s
            .execute(
                "table.design",
                &json!({"total_row": true, "name": "table1"}),
            )
            .unwrap();
        assert_eq!(d["range"], "A1:D5");
        assert_eq!(val(&s, "D5"), val(&s, "F1"));
        // Rename rewrites formulas.
        s.execute(
            "table.design",
            &json!({"rename": "Sales", "name": "Table1"}),
        )
        .unwrap();
        let f = s.wb.sheets[0]
            .get(CellAddr::parse("F1").unwrap())
            .and_then(|c| c.formula_src())
            .unwrap()
            .to_string();
        assert_eq!(f, "SUM(Sales[Total])");
        assert_eq!(val(&s, "F1"), Value::Number(25.0));
        // Header edit renames the column.
        set(&mut s, "B1", "Count");
        assert_eq!(s.wb.sheets[0].tables[0].columns[1].name, "Count");
        // Restyle via format_as_table inside the table.
        s.select(CellAddr::new(1, 1));
        s.execute(
            "home.styles.format_as_table",
            &json!({"style": "TableStyleLight9"}),
        )
        .unwrap();
        assert_eq!(s.wb.sheets[0].tables[0].style, "TableStyleLight9");
        s.execute("table.design", &json!({"convert_to_range": true}))
            .unwrap();
        assert!(s.wb.sheets[0].tables.is_empty());
    }

    #[test]
    fn no_headers_and_errors() {
        let mut s = Session::new();
        set(&mut s, "A1", "1");
        set(&mut s, "A2", "2");
        let out = s
            .execute(
                "insert.table",
                &json!({"range": "A1:A2", "has_headers": false, "name": "Nums"}),
            )
            .unwrap();
        assert_eq!(out["range"], "A1:A3");
        assert_eq!(val(&s, "A1"), Value::text("Column1"));
        assert_eq!(val(&s, "A3"), Value::Number(2.0));
        for p in [
            json!({"range": "A1:B2"}),
            json!({"range": "C1:D2", "name": "A1"}),
            json!({"range": "C1:D2", "name": "nums"}),
            json!({"range": "A:A"}),
        ] {
            assert!(s.execute("insert.table", &p).is_err(), "{p}");
        }
        assert!(
            s.execute("table.design", &json!({"name": "Nope", "style": "x"}))
                .is_err()
        );
        assert!(
            s.execute("table.design", &json!({"name": "Nums", "rename": "1bad"}))
                .is_err()
        );
    }
}
