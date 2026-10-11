//! Sheet and workbook protection (`home.cells.protect_sheet`, `review.protect_*`,
//! `file.protect_workbook`, `review.allow_edit_ranges`) and the guard `Session::transact`
//! runs so protected sheets refuse edits to locked cells (`Style.locked`), and protected
//! workbooks refuse structural sheet changes. `Style.hidden_formula` hides formulas on
//! protected sheets (`visible_input`).

use crate::params::{str_opt, str_req};
use crate::{CmdError, CmdResult, CommandSpec, Json, Session};
use serde_json::json;
use sp_core::{CellAddr, CellRange};
use sp_model::{
    AllowEditRange, Cell, PasswordHash, Sheet, SheetProtection, Visibility, Workbook,
    WorkbookProtection,
};
use sp_ops::{History, Txn};
use std::collections::{BTreeMap, BTreeSet};

const LOCKED_MSG: &str = "The cell or chart you're trying to change is on a protected sheet. \
To make a change, unprotect the sheet. You might be requested to enter a password.";

/// Protection state captured before a transaction runs. `None` when nothing is protected
/// (the common case costs one scan of the sheet list).
pub struct Guard {
    sheets: Vec<usize>,
    book: Option<Vec<(String, Visibility)>>,
}

fn sheet_list(wb: &Workbook) -> Vec<(String, Visibility)> {
    wb.sheets
        .iter()
        .map(|s| (s.name.clone(), s.visibility))
        .collect()
}

fn is_locked(wb: &Workbook, cell: Option<&Cell>) -> bool {
    wb.styles.get(cell.map_or_else(Default::default, |c| c.style)).locked
}

/// Is `a` inside an allow-edit range the user may edit (no password, or unlocked by title)?
fn in_allowed_range(sh: &Sheet, unlocked: &[String], a: CellAddr) -> bool {
    sh.allow_edit_ranges.iter().any(|r| {
        r.ranges.iter().any(|x| x.contains(a))
            && (r.password.is_none() || unlocked.iter().any(|t| t.eq_ignore_ascii_case(&r.title)))
    })
}

/// Can the user change cell `a` of sheet `sheet` right now?
pub fn cell_editable(wb: &Workbook, unlocked: &[String], sheet: usize, a: CellAddr) -> bool {
    let Some(sh) = wb.sheet(sheet) else {
        return true;
    };
    sh.protection.is_none()
        || !is_locked(wb, sh.get(a))
        || in_allowed_range(sh, unlocked, a)
}

/// Text for the formula bar: hides formulas of `hidden_formula` cells on protected sheets.
pub fn visible_input(wb: &Workbook, sheet: usize, a: CellAddr) -> String {
    let Some(sh) = wb.sheet(sheet) else {
        return String::new();
    };
    let Some(c) = sh.get(a) else {
        return String::new();
    };
    if sh.protection.is_some() && wb.styles.get(c.style).hidden_formula {
        return String::new();
    }
    c.input_text()
}

/// Everything structural edits can change on a sheet, for before/after comparison.
struct SheetShape {
    cells: BTreeMap<u32, BTreeMap<u32, Cell>>,
    merges: Vec<CellRange>,
    hidden_rows: BTreeSet<u32>,
    hidden_cols: BTreeSet<u32>,
}

impl SheetShape {
    fn of(sh: &Sheet) -> Self {
        SheetShape {
            cells: sh.cells.clone(),
            merges: sh.merges.clone(),
            hidden_rows: sh.hidden_rows.clone(),
            hidden_cols: sh.hidden_cols.clone(),
        }
    }
}

impl Guard {
    pub fn before(wb: &Workbook) -> Option<Guard> {
        let sheets: Vec<usize> = wb
            .sheets
            .iter()
            .enumerate()
            .filter(|(_, s)| s.protection.is_some())
            .map(|(i, _)| i)
            .collect();
        let book = wb
            .protection
            .as_ref()
            .filter(|p| p.structure)
            .map(|_| sheet_list(wb));
        (!sheets.is_empty() || book.is_some()).then_some(Guard { sheets, book })
    }

    /// Check a committed transaction (workbook is in the *after* state, and is left there).
    pub fn check(&self, wb: &mut Workbook, unlocked: &[String], t: &Txn) -> Result<(), CmdError> {
        if let Some(before) = &self.book
            && wb.protection.as_ref().is_some_and(|p| p.structure)
            && *before != sheet_list(wb)
        {
            return Err(CmdError::Failed(
                "Workbook is protected and cannot be changed.".into(),
            ));
        }
        // Sheets protected both before and after (protect/unprotect themselves pass).
        let watched: Vec<usize> = self
            .sheets
            .iter()
            .copied()
            .filter(|i| wb.sheet(*i).is_some_and(|s| s.protection.is_some()))
            .collect();
        if watched.is_empty() {
            return Ok(());
        }
        let changed = t.changed_cells();
        // Replay the transaction backwards to inspect the before state, then forwards.
        let mut h = History::new(1);
        h.push(t.clone());
        h.undo(wb);
        let verdict = match &changed {
            Some(cells) => cells
                .iter()
                .filter(|(si, _)| watched.contains(si))
                .all(|(si, a)| cell_editable(wb, unlocked, *si, *a)),
            None => {
                let shapes: Vec<(usize, SheetShape, bool)> = watched
                    .iter()
                    .filter_map(|i| {
                        let sh = wb.sheet(*i)?;
                        let p = sh.protection.as_ref()?;
                        let hide_ok = p.auto_filter || p.format_rows || p.format_columns;
                        Some((*i, SheetShape::of(sh), hide_ok))
                    })
                    .collect();
                h.redo(wb);
                let ok = shapes.iter().all(|(i, before, hide_ok)| {
                    let Some(after) = wb.sheet(*i) else {
                        return true;
                    };
                    let cells_ok = changed_addrs(&before.cells, &after.cells).iter().all(|a| {
                        let locked_before = is_locked(
                            wb,
                            before.cells.get(&a.row).and_then(|r| r.get(&a.col)),
                        );
                        !locked_before || in_allowed_range(after, unlocked, *a)
                    });
                    cells_ok
                        && before.merges == after.merges
                        && (*hide_ok
                            || (before.hidden_rows == after.hidden_rows
                                && before.hidden_cols == after.hidden_cols))
                });
                return if ok {
                    Ok(())
                } else {
                    Err(CmdError::Failed(LOCKED_MSG.into()))
                };
            }
        };
        h.redo(wb);
        if verdict {
            Ok(())
        } else {
            Err(CmdError::Failed(LOCKED_MSG.into()))
        }
    }
}

/// Addresses whose cell differs between two cell maps.
fn changed_addrs(
    a: &BTreeMap<u32, BTreeMap<u32, Cell>>,
    b: &BTreeMap<u32, BTreeMap<u32, Cell>>,
) -> Vec<CellAddr> {
    let mut out = Vec::new();
    let rows: BTreeSet<u32> = a.keys().chain(b.keys()).copied().collect();
    for r in rows {
        let (ra, rb) = (a.get(&r), b.get(&r));
        if ra == rb {
            continue;
        }
        let cols: BTreeSet<u32> = ra
            .into_iter()
            .flat_map(|m| m.keys())
            .chain(rb.into_iter().flat_map(|m| m.keys()))
            .copied()
            .collect();
        for c in cols {
            if ra.and_then(|m| m.get(&c)) != rb.and_then(|m| m.get(&c)) {
                out.push(CellAddr::new(r, c));
            }
        }
    }
    out
}

fn sheet_of(s: &Session, p: &Json) -> Result<usize, CmdError> {
    match p.get("sheet") {
        None | Some(Json::Null) => Ok(s.sheet_index()),
        Some(Json::String(n)) => {
            s.wb.sheet_index(n)
                .ok_or_else(|| CmdError::Param(format!("no sheet named '{n}'")))
        }
        Some(v) => v
            .as_u64()
            .map(|i| i as usize)
            .filter(|i| *i < s.wb.sheets.len())
            .ok_or_else(|| CmdError::Param("bad 'sheet'".into())),
    }
}

fn password_param(p: &Json) -> Option<PasswordHash> {
    str_opt(p, "password")
        .filter(|pw| !pw.is_empty())
        .map(PasswordHash::legacy)
}

fn check_password(stored: Option<&PasswordHash>, p: &Json) -> Result<(), CmdError> {
    match stored {
        None => Ok(()),
        Some(h) if h.verify(str_opt(p, "password").unwrap_or("")) => Ok(()),
        Some(_) => Err(CmdError::Failed(
            "The password you supplied is not correct.".into(),
        )),
    }
}

/// Permission names accepted in `allow` (snake_case of the protection options).
const ALLOW_KEYS: &[&str] = &[
    "select_locked",
    "select_unlocked",
    "format_cells",
    "format_columns",
    "format_rows",
    "insert_columns",
    "insert_rows",
    "insert_hyperlinks",
    "delete_columns",
    "delete_rows",
    "sort",
    "auto_filter",
    "pivot_tables",
    "objects",
    "scenarios",
];

fn set_allow(p: &mut SheetProtection, key: &str, v: bool) -> bool {
    let slot = match key {
        "select_locked" => &mut p.select_locked,
        "select_unlocked" => &mut p.select_unlocked,
        "format_cells" => &mut p.format_cells,
        "format_columns" => &mut p.format_columns,
        "format_rows" => &mut p.format_rows,
        "insert_columns" => &mut p.insert_columns,
        "insert_rows" => &mut p.insert_rows,
        "insert_hyperlinks" => &mut p.insert_hyperlinks,
        "delete_columns" => &mut p.delete_columns,
        "delete_rows" => &mut p.delete_rows,
        "sort" => &mut p.sort,
        "auto_filter" => &mut p.auto_filter,
        "pivot_tables" => &mut p.pivot_tables,
        "objects" => &mut p.objects,
        "scenarios" => &mut p.scenarios,
        _ => return false,
    };
    *slot = v;
    true
}

/// Toggle (or force with `"protect": bool`) sheet protection.
fn protect_sheet(s: &mut Session, p: &Json) -> CmdResult {
    let sheet = sheet_of(s, p)?;
    let current = s.wb.sheet(sheet).and_then(|x| x.protection.clone());
    let want = p
        .get("protect")
        .and_then(Json::as_bool)
        .unwrap_or(current.is_none());
    if want == current.is_some() {
        return Ok(json!({ "protected": want }));
    }
    let new = if want {
        let mut prot = SheetProtection {
            password: password_param(p),
            ..SheetProtection::default()
        };
        if let Some(list) = p.get("allow").and_then(Json::as_array) {
            for k in list.iter().filter_map(Json::as_str) {
                if !set_allow(&mut prot, k, true) {
                    return Err(CmdError::Param(format!(
                        "unknown permission '{k}' (one of {})",
                        ALLOW_KEYS.join(", ")
                    )));
                }
            }
        }
        Some(prot)
    } else {
        check_password(current.as_ref().and_then(|c| c.password.as_ref()), p)?;
        None
    };
    let label = if want {
        "Protect Sheet"
    } else {
        "Unprotect Sheet"
    };
    s.transact(label, |tx, _| {
        tx.touch_sheet(sheet);
        if let Some(sh) = tx.wb.sheet_mut(sheet) {
            sh.protection = new;
        }
        Ok(())
    })?;
    s.status = if want {
        "Sheet protected".into()
    } else {
        "Sheet unprotected".into()
    };
    Ok(json!({ "protected": want }))
}

/// Toggle (or force with `"protect": bool`) workbook structure protection.
fn protect_workbook(s: &mut Session, p: &Json) -> CmdResult {
    let current = s.wb.protection.clone();
    let want = p
        .get("protect")
        .and_then(Json::as_bool)
        .unwrap_or(current.is_none());
    if want == current.is_some() {
        return Ok(json!({ "protected": want }));
    }
    let new = if want {
        Some(WorkbookProtection {
            password: password_param(p),
            structure: p.get("structure").and_then(Json::as_bool).unwrap_or(true),
            windows: p.get("windows").and_then(Json::as_bool).unwrap_or(false),
        })
    } else {
        check_password(current.as_ref().and_then(|c| c.password.as_ref()), p)?;
        None
    };
    s.transact("Protect Workbook", |tx, _| {
        tx.touch_book();
        tx.wb.protection = new;
        Ok(())
    })?;
    Ok(json!({ "protected": want }))
}

fn allow_edit_ranges(s: &mut Session, p: &Json) -> CmdResult {
    let sheet = sheet_of(s, p)?;
    let action = str_opt(p, "action").unwrap_or(if p.get("range").is_some() {
        "add"
    } else {
        "list"
    });
    let list = |s: &Session| {
        s.wb.sheet(sheet).map_or_else(Vec::new, |sh| {
            sh.allow_edit_ranges
                .iter()
                .map(|r| {
                    json!({
                        "title": r.title,
                        "ranges": r.ranges.iter().map(CellRange::to_a1).collect::<Vec<_>>(),
                        "password": r.password.is_some(),
                    })
                })
                .collect()
        })
    };
    if action == "list" {
        return Ok(Json::Array(list(s)));
    }
    if action == "unlock" {
        let title = str_req(p, "title")?;
        let r = s
            .wb
            .sheet(sheet)
            .and_then(|sh| {
                sh.allow_edit_ranges
                    .iter()
                    .find(|r| r.title.eq_ignore_ascii_case(title))
            })
            .cloned()
            .ok_or_else(|| CmdError::Param(format!("no range titled '{title}'")))?;
        check_password(r.password.as_ref(), p)?;
        if !s.view.review.unlocked.iter().any(|t| t == &r.title) {
            s.view.review.unlocked.push(r.title);
        }
        return Ok(json!({ "unlocked": title }));
    }
    if s.wb.sheet(sheet).is_some_and(|sh| sh.protection.is_some()) {
        return Err(CmdError::Failed(
            "Unprotect the sheet before changing its editable ranges.".into(),
        ));
    }
    match action {
        "add" => {
            let text = str_req(p, "range")?;
            let mut ranges = Vec::new();
            for part in text.split(',').map(str::trim).filter(|t| !t.is_empty()) {
                let (si, r) = crate::params::parse_ref(s, part)?;
                if si != sheet {
                    return Err(CmdError::Param("ranges must be on the sheet".into()));
                }
                ranges.push(r);
            }
            if ranges.is_empty() || ranges.len() > 64 {
                return Err(CmdError::Param("between 1 and 64 ranges".into()));
            }
            let existing = s.wb.sheet(sheet).map_or(0, |sh| sh.allow_edit_ranges.len());
            if existing >= 1024 {
                return Err(CmdError::Failed("too many ranges".into()));
            }
            let title = str_opt(p, "title")
                .map(str::to_string)
                .unwrap_or_else(|| format!("Range{}", existing + 1));
            let item = AllowEditRange {
                title: title.clone(),
                ranges,
                password: password_param(p),
            };
            s.transact("Allow Edit Ranges", |tx, _| {
                tx.touch_sheet(sheet);
                if let Some(sh) = tx.wb.sheet_mut(sheet) {
                    sh.allow_edit_ranges
                        .retain(|r| !r.title.eq_ignore_ascii_case(&title));
                    sh.allow_edit_ranges.push(item);
                }
                Ok(())
            })?;
        }
        "remove" => {
            let title = str_req(p, "title")?.to_string();
            s.transact("Allow Edit Ranges", |tx, _| {
                tx.touch_sheet(sheet);
                if let Some(sh) = tx.wb.sheet_mut(sheet) {
                    sh.allow_edit_ranges
                        .retain(|r| !r.title.eq_ignore_ascii_case(&title));
                }
                Ok(())
            })?;
        }
        other => {
            return Err(CmdError::Param(format!(
                "unknown action '{other}' (list, add, remove, unlock)"
            )));
        }
    }
    Ok(Json::Array(list(s)))
}

pub fn specs() -> Vec<CommandSpec> {
    let sheet = r#"{"protect"?: bool (default toggles), "password"?: "pw", "allow"?: ["format_cells","sort","auto_filter",…], "sheet"?: "Sheet1"}"#;
    let book = r#"{"protect"?: bool (default toggles), "password"?: "pw", "structure"?: true, "windows"?: false}"#;
    vec![
        CommandSpec::new("home.cells.protect_sheet", sheet, protect_sheet),
        CommandSpec::new("review.protect_sheet", sheet, protect_sheet),
        CommandSpec::new("review.protect_workbook", book, protect_workbook),
        CommandSpec::new("file.protect_workbook", book, protect_workbook),
        CommandSpec::new(
            "review.allow_edit_ranges",
            r#"{"action"?: "list|add|remove|unlock", "title"?: "Range1", "range"?: "A1:B5,D1", "password"?: "pw"}"#,
            allow_edit_ranges,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &Session, a: &str) -> String {
        s.wb.sheets[0]
            .value(CellAddr::parse(a).unwrap())
            .to_display()
    }

    #[test]
    fn protected_sheet_refuses_locked_cells() {
        let mut s = Session::new();
        s.execute("cell.set", &json!({"cell": "A1", "input": "1"}))
            .unwrap();
        // Unlock B1:B3 before protecting.
        s.execute("home.cells.lock_cell", &json!({"range": "B1:B3"}))
            .unwrap();
        assert!(!s.wb.styles.get(s.wb.sheets[0].get(CellAddr::new(0, 1)).unwrap().style).locked);
        s.execute("review.protect_sheet", &json!({"password": "pw"}))
            .unwrap();
        let e = s
            .execute("cell.set", &json!({"cell": "A1", "input": "2"}))
            .unwrap_err();
        assert!(e.to_string().contains("protected"));
        assert_eq!(v(&s, "A1"), "1");
        s.execute("cell.set", &json!({"cell": "B2", "input": "5"}))
            .unwrap();
        assert_eq!(v(&s, "B2"), "5");
        // Structural edits and clears touching locked cells are refused too.
        assert!(
            s.execute("home.cells.insert_rows", &json!({"range": "A1"}))
                .is_err()
        );
        assert!(
            s.execute("home.editing.clear_all", &json!({"range": "A1:B2"}))
                .is_err()
        );
        assert_eq!(v(&s, "A1"), "1");
        // Wrong password keeps it protected.
        assert!(s.execute("review.protect_sheet", &json!({"password": "x"})).is_err());
        s.execute("review.protect_sheet", &json!({"password": "pw"}))
            .unwrap();
        s.execute("cell.set", &json!({"cell": "A1", "input": "2"}))
            .unwrap();
        assert_eq!(v(&s, "A1"), "2");
    }

    #[test]
    fn allow_edit_ranges_and_hidden_formulas() {
        let mut s = Session::new();
        s.execute("cell.set", &json!({"cell": "C1", "input": "=1+1"}))
            .unwrap();
        s.execute(
            "review.allow_edit_ranges",
            &json!({"title": "Inputs", "range": "A1:A5"}),
        )
        .unwrap();
        s.execute(
            "review.allow_edit_ranges",
            &json!({"title": "Secret", "range": "D1", "password": "k"}),
        )
        .unwrap();
        s.wb.sheets[0].entry(CellAddr::new(0, 2)).style =
            s.wb.styles.derive(Default::default(), |st| st.hidden_formula = true);
        s.execute("home.cells.protect_sheet", &json!({})).unwrap();
        assert_eq!(visible_input(&s.wb, 0, CellAddr::new(0, 2)), "");
        s.execute("cell.set", &json!({"cell": "A3", "input": "ok"}))
            .unwrap();
        assert!(s.execute("cell.set", &json!({"cell": "D1", "input": "x"})).is_err());
        s.execute(
            "review.allow_edit_ranges",
            &json!({"action": "unlock", "title": "Secret", "password": "k"}),
        )
        .unwrap();
        s.execute("cell.set", &json!({"cell": "D1", "input": "x"}))
            .unwrap();
        // Ranges can't be changed while protected.
        assert!(
            s.execute("review.allow_edit_ranges", &json!({"range": "E1"}))
                .is_err()
        );
        s.execute("home.cells.protect_sheet", &json!({})).unwrap();
        assert_eq!(visible_input(&s.wb, 0, CellAddr::new(0, 2)), "=1+1");
    }

    #[test]
    fn workbook_structure_protection() {
        let mut s = Session::new();
        s.execute("review.protect_workbook", &json!({"password": "a"}))
            .unwrap();
        assert!(s.execute("home.cells.insert_sheet", &json!({})).is_err());
        assert_eq!(s.wb.sheets.len(), 1);
        // Cell edits are still fine.
        s.execute("cell.set", &json!({"cell": "A1", "input": "1"}))
            .unwrap();
        assert!(s.execute("file.protect_workbook", &json!({})).is_err());
        s.execute("file.protect_workbook", &json!({"password": "a"}))
            .unwrap();
        s.execute("home.cells.insert_sheet", &json!({})).unwrap();
        assert_eq!(s.wb.sheets.len(), 2);
    }
}
