//! Rows, columns and sheets (`home.cells.*`, `sheet.*`).

use crate::params::{f64_opt, range, str_opt, str_req, u32_opt};
use crate::{CmdError, CmdResult, CommandSpec, Json, Session};
use serde_json::json;
use sp_core::CellAddr;
use sp_formula::shift::{Axis, invalidate_sheet, rename_sheet};
use sp_model::{Color, Content, Sheet, Visibility};

fn structural(s: &mut Session, p: &Json, axis: Axis, insert: bool) -> CmdResult {
    let (sheet, r) = range(s, p, "range")?;
    let (at, count) = match axis {
        Axis::Rows => (r.start.row, i64::from(r.rows())),
        Axis::Cols => (r.start.col, i64::from(r.cols())),
    };
    let count = if insert { count } else { -count };
    let label = match (axis, insert) {
        (Axis::Rows, true) => "Insert Rows",
        (Axis::Rows, false) => "Delete Rows",
        (Axis::Cols, true) => "Insert Columns",
        (Axis::Cols, false) => "Delete Columns",
    };
    // Inserting would push data off the sheet: refuse, as the target app does.
    if insert && let Some(u) = s.wb.sheet(sheet).and_then(|sh| sh.used_range()) {
        let (end, max) = match axis {
            Axis::Rows => (u.end.row, sp_core::MAX_ROWS),
            Axis::Cols => (u.end.col, sp_core::MAX_COLS),
        };
        if i64::from(end) + count >= i64::from(max) && end >= at {
            return Err(CmdError::Failed(
                "To prevent possible loss of data, cells can't be shifted off the sheet.".into(),
            ));
        }
    }
    s.transact(label, |tx, _| {
        sp_ops::edit::insert_delete(tx, sheet, axis, at, count).map_err(CmdError::from)
    })?;
    Ok(json!({ "at": at, "count": count }))
}

/// Rewrite every formula (and defined name) in the workbook with `f`.
fn rewrite_formulas(wb: &mut sp_model::Workbook, f: &dyn Fn(&mut sp_formula::Expr)) {
    for sh in &mut wb.sheets {
        for row in sh.cells.values_mut() {
            // Unshare (copy-on-write) only rows holding formulas.
            if !row
                .values()
                .any(|c| matches!(c.content, Content::Formula(_)))
            {
                continue;
            }
            for c in std::sync::Arc::make_mut(row).values_mut() {
                if let Content::Formula(src) = &c.content
                    && let Ok(mut e) = sp_formula::parse(src)
                {
                    f(&mut e);
                    c.content = Content::Formula(sp_formula::to_formula(&e));
                }
            }
        }
        // Data-validation sources and in-workbook hyperlink targets refer to sheets too.
        let texts = sh
            .validations
            .iter_mut()
            .flat_map(|dv| [&mut dv.formula1, &mut dv.formula2])
            .chain(sh.hyperlinks.iter_mut().map(|h| &mut h.location));
        for src in texts {
            if let Ok(orig) = sp_formula::parse(src) {
                let mut e = orig.clone();
                f(&mut e);
                if e != orig {
                    *src = sp_formula::to_formula(&e);
                }
            }
        }
        sp_ops::pivot_shift::rewrite_exprs(sh, f);
    }
    for n in &mut wb.names {
        if let Ok(mut e) = sp_formula::parse(&n.formula) {
            f(&mut e);
            n.formula = sp_formula::to_formula(&e);
        }
    }
}

fn insert_sheet(s: &mut Session, p: &Json) -> CmdResult {
    let name = match str_opt(p, "name") {
        Some(n) => n.to_string(),
        None => s.wb.next_sheet_name(),
    };
    s.wb.check_sheet_name(&name, None)
        .map_err(CmdError::Param)?;
    let at = s.sheet_index();
    s.transact("Insert Sheet", |tx, _| {
        tx.touch_book();
        tx.wb.sheets.insert(at, Sheet::new(name.clone()));
        // Sheet-scoped names shift with their sheets.
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
    Ok(json!(name))
}

fn delete_sheet(s: &mut Session, p: &Json) -> CmdResult {
    let idx = sheet_param(s, p)?;
    if s.wb.visible_sheet_count() <= 1
        && s.wb
            .sheets
            .get(idx)
            .is_some_and(|x| x.visibility == Visibility::Visible)
    {
        return Err(CmdError::Failed(
            "A workbook must contain at least one visible worksheet.".into(),
        ));
    }
    let name =
        s.wb.sheets
            .get(idx)
            .map(|x| x.name.clone())
            .unwrap_or_default();
    s.transact("Delete Sheet", |tx, _| {
        tx.touch_book();
        tx.wb.sheets.remove(idx);
        tx.wb.names.retain(|n| n.scope != Some(idx));
        for n in &mut tx.wb.names {
            if let Some(sc) = n.scope.as_mut()
                && *sc > idx
            {
                *sc -= 1;
            }
        }
        rewrite_formulas(tx.wb, &|e| invalidate_sheet(e, &name));
        tx.wb.active = idx.min(tx.wb.sheets.len().saturating_sub(1));
        Ok(())
    })?;
    Ok(json!(name))
}

fn sheet_param(s: &Session, p: &Json) -> Result<usize, CmdError> {
    match p.get("sheet") {
        None | Some(Json::Null) => Ok(s.sheet_index()),
        Some(Json::String(n)) => {
            s.wb.sheet_index(n)
                .ok_or_else(|| CmdError::Param(format!("no sheet named '{n}'")))
        }
        Some(v) => match v.as_u64() {
            Some(i) if (i as usize) < s.wb.sheets.len() => Ok(i as usize),
            _ => Err(CmdError::Param(
                "sheet must be a name or a valid index".into(),
            )),
        },
    }
}

fn rename(s: &mut Session, p: &Json) -> CmdResult {
    let idx = sheet_param(s, p)?;
    let new = str_req(p, "name")?.to_string();
    s.wb.check_sheet_name(&new, Some(idx))
        .map_err(CmdError::Param)?;
    let old =
        s.wb.sheets
            .get(idx)
            .map(|x| x.name.clone())
            .unwrap_or_default();
    s.transact("Rename Sheet", |tx, _| {
        tx.touch_book();
        if let Some(sh) = tx.wb.sheet_mut(idx) {
            sh.name = new.clone();
        }
        rewrite_formulas(tx.wb, &|e| rename_sheet(e, &old, &new));
        Ok(())
    })?;
    Ok(json!(new))
}

fn move_copy(s: &mut Session, p: &Json, copy: bool) -> CmdResult {
    let idx = sheet_param(s, p)?;
    let n = s.wb.sheets.len();
    let to = match u32_opt(p, "to")? {
        Some(t) => (t as usize).min(if copy { n } else { n - 1 }),
        None => {
            if copy {
                idx + 1
            } else {
                idx
            }
        }
    };
    let copy = copy || crate::params::bool_opt(p, "copy").unwrap_or(false);
    let new_name = if copy {
        let base =
            s.wb.sheets
                .get(idx)
                .map(|x| x.name.clone())
                .unwrap_or_default();
        let base: String = base.chars().take(26).collect();
        (2..)
            .map(|k| format!("{base} ({k})"))
            .find(|c| s.wb.sheet_index(c).is_none())
            .unwrap_or(base)
    } else {
        String::new()
    };
    s.transact(if copy { "Copy Sheet" } else { "Move Sheet" }, |tx, _| {
        tx.touch_book();
        if copy {
            let mut sh = tx
                .wb
                .sheets
                .get(idx)
                .cloned()
                .ok_or_else(|| CmdError::Failed("no sheet".into()))?;
            sh.name = new_name.clone();
            tx.wb.sheets.insert(to.min(tx.wb.sheets.len()), sh);
            tx.wb.active = to.min(tx.wb.sheets.len() - 1);
        } else {
            let sh = tx.wb.sheets.remove(idx);
            tx.wb.sheets.insert(to.min(tx.wb.sheets.len()), sh);
            tx.wb.active = to;
            // Sheet-scoped names keep pointing at their sheet.
            for nm in &mut tx.wb.names {
                if let Some(sc) = nm.scope.as_mut() {
                    *sc = if *sc == idx {
                        to
                    } else if idx < to && *sc > idx && *sc <= to {
                        *sc - 1
                    } else if to < idx && *sc >= to && *sc < idx {
                        *sc + 1
                    } else {
                        *sc
                    };
                }
            }
        }
        Ok(())
    })?;
    Ok(json!({ "index": to, "name": if copy { new_name } else { String::new() } }))
}

fn sizes(s: &mut Session, p: &Json, rows: bool, value: Option<f64>, label: &str) -> CmdResult {
    let (sheet, r) = range(s, p, "range")?;
    let (lo, hi) = if rows {
        (r.start.row, r.end.row)
    } else {
        (r.start.col, r.end.col)
    };
    if hi - lo > 100_000 {
        return Err(CmdError::Param("too many rows/columns".into()));
    }
    s.transact(label, |tx, _| {
        tx.touch_sheet(sheet);
        let sh = tx
            .wb
            .sheet_mut(sheet)
            .ok_or_else(|| CmdError::Failed("no sheet".into()))?;
        for i in lo..=hi {
            let map = if rows {
                &mut sh.row_heights
            } else {
                &mut sh.col_widths
            };
            match value {
                Some(v) => {
                    map.insert(i, v);
                }
                None => {
                    map.remove(&i);
                }
            }
        }
        Ok(())
    })?;
    Ok(Json::Null)
}

/// Approximate column auto-fit from display text length.
fn autofit_cols(s: &mut Session, p: &Json) -> CmdResult {
    let (sheet, r) = range(s, p, "range")?;
    let Some(sh) = s.wb.sheet(sheet) else {
        return Err(CmdError::Failed("no sheet".into()));
    };
    let mut widths: std::collections::BTreeMap<u32, f64> = std::collections::BTreeMap::new();
    let scan = sp_core::CellRange::new(
        CellAddr::new(0, r.start.col),
        CellAddr::new(sp_core::MAX_ROWS - 1, r.end.col),
    );
    for (a, c) in sh.cells_in(scan) {
        let st = s.wb.styles.get(c.style);
        let text = sp_core::numfmt::format_value(&c.value, &st.num_fmt).text;
        let w = (text.chars().count() as f64 * st.font_size() / 11.0 + 1.0).min(255.0);
        let e = widths.entry(a.col).or_insert(0.0);
        *e = e.max(w);
    }
    s.transact("AutoFit Column Width", |tx, _| {
        tx.touch_sheet(sheet);
        let sh = tx
            .wb
            .sheet_mut(sheet)
            .ok_or_else(|| CmdError::Failed("no sheet".into()))?;
        for c in r.start.col..=r.end.col.min(r.start.col + 16_383) {
            match widths.get(&c) {
                Some(w) => {
                    sh.col_widths.insert(c, *w);
                }
                None => {
                    sh.col_widths.remove(&c);
                }
            }
        }
        Ok(())
    })?;
    Ok(json!(widths.len()))
}

fn hide(s: &mut Session, p: &Json, rows: bool, hide: bool) -> CmdResult {
    let (sheet, r) = range(s, p, "range")?;
    let (lo, hi) = if rows {
        (r.start.row, r.end.row)
    } else {
        (r.start.col, r.end.col)
    };
    let label = match (rows, hide) {
        (true, true) => "Hide Rows",
        (true, false) => "Unhide Rows",
        (false, true) => "Hide Columns",
        (false, false) => "Unhide Columns",
    };
    s.transact(label, |tx, _| {
        tx.touch_sheet(sheet);
        let sh = tx
            .wb
            .sheet_mut(sheet)
            .ok_or_else(|| CmdError::Failed("no sheet".into()))?;
        let set = if rows {
            &mut sh.hidden_rows
        } else {
            &mut sh.hidden_cols
        };
        if hide {
            if hi - lo > 1_100_000 {
                return Err(CmdError::Param("range too large".into()));
            }
            set.extend(lo..=hi);
        } else {
            set.retain(|i| *i < lo || *i > hi);
        }
        Ok(())
    })?;
    Ok(Json::Null)
}

fn set_visibility(s: &mut Session, p: &Json, vis: Visibility) -> CmdResult {
    let idx = sheet_param(s, p)?;
    if vis != Visibility::Visible && s.wb.visible_sheet_count() <= 1 {
        return Err(CmdError::Failed(
            "A workbook must contain at least one visible worksheet.".into(),
        ));
    }
    s.transact(
        if vis == Visibility::Visible {
            "Unhide Sheet"
        } else {
            "Hide Sheet"
        },
        |tx, _| {
            tx.touch_book();
            if let Some(sh) = tx.wb.sheet_mut(idx) {
                sh.visibility = vis;
            }
            if vis != Visibility::Visible && tx.wb.active == idx {
                tx.wb.active = tx
                    .wb
                    .sheets
                    .iter()
                    .position(|x| x.visibility == Visibility::Visible)
                    .unwrap_or(0);
            }
            Ok(())
        },
    )?;
    Ok(Json::Null)
}

fn activate(s: &mut Session, idx: usize) -> CmdResult {
    if idx >= s.wb.sheets.len() {
        return Err(CmdError::Param("no such sheet".into()));
    }
    s.wb.active = idx;
    s.sel = crate::Selection::at(CellAddr::default());
    s.view.scroll = CellAddr::default();
    Ok(json!(s.wb.sheets.get(idx).map(|x| x.name.clone())))
}

fn cycle_sheet(s: &mut Session, forward: bool) -> CmdResult {
    let n = s.wb.sheets.len();
    let mut i = s.sheet_index();
    for _ in 0..n {
        i = if forward {
            (i + 1) % n
        } else {
            (i + n - 1) % n
        };
        if s.wb
            .sheets
            .get(i)
            .is_some_and(|x| x.visibility == Visibility::Visible)
        {
            return activate(s, i);
        }
    }
    Ok(Json::Null)
}

pub fn specs() -> Vec<CommandSpec> {
    let r = r#"{"range"?: "A1:B2"}"#;
    vec![
        CommandSpec::new("home.cells.insert_rows", r, |s, p| {
            structural(s, p, Axis::Rows, true)
        }),
        CommandSpec::new("home.cells.insert_columns", r, |s, p| {
            structural(s, p, Axis::Cols, true)
        }),
        CommandSpec::new("home.cells.delete_rows", r, |s, p| {
            structural(s, p, Axis::Rows, false)
        }),
        CommandSpec::new("home.cells.delete_columns", r, |s, p| {
            structural(s, p, Axis::Cols, false)
        }),
        CommandSpec::new(
            "home.cells.insert_sheet",
            r#"{"name"?: "Data"}"#,
            insert_sheet,
        ),
        CommandSpec::new(
            "home.cells.delete_sheet",
            r#"{"sheet"?: "Sheet2" | 1}"#,
            delete_sheet,
        ),
        CommandSpec::new(
            "home.cells.rename_sheet",
            r#"{"sheet"?: "Sheet1", "name": "Data"}"#,
            rename,
        ),
        CommandSpec::new(
            "home.cells.move_copy_sheet",
            r#"{"sheet"?: "Sheet1", "to": 2, "copy"?: false}"#,
            |s, p| move_copy(s, p, false),
        ),
        CommandSpec::new("sheet.duplicate", r#"{"sheet"?: "Sheet1"}"#, |s, p| {
            move_copy(s, p, true)
        }),
        CommandSpec::new(
            "home.cells.row_height",
            r#"{"range"?: "A1", "height": 20}"#,
            |s, p| {
                let h = f64_opt(p, "height")?
                    .ok_or_else(|| CmdError::Param("missing number 'height'".into()))?;
                if !(0.0..=409.0).contains(&h) {
                    return Err(CmdError::Param(
                        "row height must be between 0 and 409".into(),
                    ));
                }
                sizes(s, p, true, Some(h), "Row Height")
            },
        ),
        CommandSpec::new(
            "home.cells.column_width",
            r#"{"range"?: "A1", "width": 12}"#,
            |s, p| {
                let w = f64_opt(p, "width")?
                    .ok_or_else(|| CmdError::Param("missing number 'width'".into()))?;
                if !(0.0..=255.0).contains(&w) {
                    return Err(CmdError::Param(
                        "column width must be between 0 and 255".into(),
                    ));
                }
                sizes(s, p, false, Some(w), "Column Width")
            },
        ),
        CommandSpec::new("home.cells.autofit_row", r, |s, p| {
            sizes(s, p, true, None, "AutoFit Row Height")
        }),
        CommandSpec::new("home.cells.autofit_column", r, autofit_cols),
        CommandSpec::new("home.cells.default_width", r#"{"width": 10}"#, |s, p| {
            let w = crate::params::f64_req(p, "width")?;
            if !(0.0..=255.0).contains(&w) {
                return Err(CmdError::Param("width must be between 0 and 255".into()));
            }
            let sheet = s.sheet_index();
            s.transact("Default Width", |tx, _| {
                tx.touch_sheet(sheet);
                if let Some(sh) = tx.wb.sheet_mut(sheet) {
                    sh.default_col_width = w;
                }
                Ok(())
            })?;
            Ok(Json::Null)
        }),
        CommandSpec::new("home.cells.hide_rows", r, |s, p| hide(s, p, true, true)),
        CommandSpec::new("home.cells.hide_columns", r, |s, p| hide(s, p, false, true)),
        CommandSpec::new("home.cells.unhide_rows", r, |s, p| hide(s, p, true, false)),
        CommandSpec::new("home.cells.unhide_columns", r, |s, p| {
            hide(s, p, false, false)
        }),
        CommandSpec::new(
            "home.cells.hide_sheet",
            r#"{"sheet"?: "Sheet2"}"#,
            |s, p| set_visibility(s, p, Visibility::Hidden),
        ),
        CommandSpec::new(
            "home.cells.unhide_sheet",
            r#"{"sheet": "Sheet2"}"#,
            |s, p| set_visibility(s, p, Visibility::Visible),
        ),
        CommandSpec::new(
            "home.cells.tab_color",
            r#"{"sheet"?: "Sheet1", "color": "FF0000" | null}"#,
            |s, p| {
                let idx = sheet_param(s, p)?;
                let c = match p.get("color") {
                    None | Some(Json::Null) => None,
                    Some(v) => Some(
                        v.as_str()
                            .and_then(Color::from_hex)
                            .ok_or_else(|| CmdError::Param("bad color".into()))?,
                    ),
                };
                s.transact("Tab Color", |tx, _| {
                    tx.touch_sheet(idx);
                    if let Some(sh) = tx.wb.sheet_mut(idx) {
                        sh.tab_color = c;
                    }
                    Ok(())
                })?;
                Ok(Json::Null)
            },
        ),
        CommandSpec::new("sheet.next", "{}", |s, _| cycle_sheet(s, true))
            .when(|s| s.wb.sheets.len() > 1),
        CommandSpec::new("sheet.previous", "{}", |s, _| cycle_sheet(s, false))
            .when(|s| s.wb.sheets.len() > 1),
        CommandSpec::new("sheet.activate", r#"{"sheet": "Sheet2" | 1}"#, |s, p| {
            let i = sheet_param(s, p)?;
            activate(s, i)
        }),
        CommandSpec::new(
            "sheet.select_all_sheets",
            "{} → names of all sheets",
            |s, _| {
                Ok(json!(
                    s.wb.sheets
                        .iter()
                        .map(|x| x.name.clone())
                        .collect::<Vec<_>>()
                ))
            },
        ),
        CommandSpec::new(
            "home.cells.insert_cells",
            r#"{"range"?: "A1:B2", "shift"?: "row|column"}  (partial-range shifts: not yet)"#,
            |s, p| match str_opt(p, "shift").unwrap_or("row") {
                "row" => structural(s, p, Axis::Rows, true),
                "column" => structural(s, p, Axis::Cols, true),
                "down" | "right" => Err(CmdError::Failed(
                    "shifting a partial range of cells is not supported yet".into(),
                )),
                other => Err(CmdError::Param(format!("unknown shift '{other}'"))),
            },
        ),
        CommandSpec::new(
            "home.cells.delete_cells",
            r#"{"range"?: "A1:B2", "shift"?: "row|column"}  (partial-range shifts: not yet)"#,
            |s, p| match str_opt(p, "shift").unwrap_or("row") {
                "row" => structural(s, p, Axis::Rows, false),
                "column" => structural(s, p, Axis::Cols, false),
                "up" | "left" => Err(CmdError::Failed(
                    "shifting a partial range of cells is not supported yet".into(),
                )),
                other => Err(CmdError::Param(format!("unknown shift '{other}'"))),
            },
        ),
    ]
}
