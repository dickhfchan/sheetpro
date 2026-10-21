//! View, calculation, names and page-layout display options (`view.*`, `formulas.*`,
//! `layout.*` view toggles).

use crate::params::{f64_req, str_opt, str_req};
use crate::{CmdError, CmdResult, CommandSpec, Json, Session};
use serde_json::json;
use sp_core::CellAddr;
use sp_model::{CalcMode, DefinedName};

fn set_zoom(s: &mut Session, z: f32) -> CmdResult {
    s.view.zoom = z.clamp(0.1, 4.0);
    Ok(json!(s.view.zoom))
}

fn freeze(s: &mut Session, at: Option<CellAddr>, label: &str) -> CmdResult {
    let sheet = s.sheet_index();
    s.transact(label, |tx, _| {
        tx.touch_sheet(sheet);
        if let Some(sh) = tx.wb.sheet_mut(sheet) {
            sh.freeze = at.filter(|a| a.row > 0 || a.col > 0);
        }
        Ok(())
    })?;
    Ok(json!(at.map(|a| a.to_a1())))
}

fn gridlines(s: &mut Session) -> CmdResult {
    let sheet = s.sheet_index();
    let on = !s.wb.sheet(sheet).is_some_and(|sh| sh.show_gridlines);
    s.transact("Gridlines", |tx, _| {
        tx.touch_sheet(sheet);
        if let Some(sh) = tx.wb.sheet_mut(sheet) {
            sh.show_gridlines = on;
        }
        Ok(())
    })?;
    // The setting is the sheet's (saved with it, undone with it); `view.show_gridlines`
    // stays a separate display override, so it isn't touched here.
    Ok(json!(on))
}

fn name_valid(n: &str) -> bool {
    let mut chars = n.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '\\');
    first_ok
        && n.chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == '\\')
        && n.len() <= 255
        && sp_core::CellAddr::parse(n).is_err()
        && !n.eq_ignore_ascii_case("R")
        && !n.eq_ignore_ascii_case("C")
}

fn define_name(s: &mut Session, p: &Json) -> CmdResult {
    let name = str_req(p, "name")?.to_string();
    if !name_valid(&name) {
        return Err(CmdError::Param(format!("'{name}' is not a valid name")));
    }
    let refers = match str_opt(p, "refers_to") {
        Some(r) => r.trim_start_matches('=').to_string(),
        None => {
            let sheet =
                s.wb.sheets
                    .get(s.sheet_index())
                    .map(|x| x.name.clone())
                    .unwrap_or_default();
            let r = s.sel.range();
            let abs = |a: CellAddr| format!("${}${}", sp_core::col_to_letters(a.col), a.row + 1);
            let body = if r.is_single() {
                abs(r.start)
            } else {
                format!("{}:{}", abs(r.start), abs(r.end))
            };
            format!("{}!{}", sp_core::addr::quote_sheet_name(&sheet), body)
        }
    };
    sp_formula::parse(&refers).map_err(|e| CmdError::Param(e.to_string()))?;
    let scope = match str_opt(p, "scope") {
        None | Some("workbook") => None,
        Some(sh) => Some(
            s.wb.sheet_index(sh)
                .ok_or_else(|| CmdError::Param(format!("no sheet named '{sh}'")))?,
        ),
    };
    s.transact("Define Name", |tx, _| {
        tx.touch_book();
        tx.wb
            .names
            .retain(|n| !(n.name.eq_ignore_ascii_case(&name) && n.scope == scope));
        tx.wb.names.push(DefinedName {
            name: name.clone(),
            formula: refers.clone(),
            scope,
        });
        Ok(())
    })?;
    Ok(json!({ "name": name, "refers_to": format!("={refers}") }))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("view.zoom", r#"{"percent": 125}"#, |s, p| {
            let pct = f64_req(p, "percent")?;
            if !(10.0..=400.0).contains(&pct) {
                return Err(CmdError::Param("zoom must be between 10% and 400%".into()));
            }
            set_zoom(s, (pct / 100.0) as f32)
        }),
        CommandSpec::new("view.zoom_100", "{}", |s, _| set_zoom(s, 1.0)),
        CommandSpec::new("view.zoom_in", "{}", |s, _| set_zoom(s, s.view.zoom + 0.1)),
        CommandSpec::new("view.zoom_out", "{}", |s, _| set_zoom(s, s.view.zoom - 0.1)),
        CommandSpec::new("view.zoom_selection", "{}", |s, _| {
            let r = s.sel.range();
            let fit_r = s.view.page_rows as f32 * s.view.zoom / r.rows().max(1) as f32;
            let fit_c = s.view.page_cols as f32 * s.view.zoom / r.cols().max(1) as f32;
            s.view.scroll = r.start;
            set_zoom(s, fit_r.min(fit_c))
        }),
        CommandSpec::new("view.gridlines", "{}", |s, _| gridlines(s)),
        CommandSpec::new("layout.gridlines_view", "{}", |s, _| gridlines(s)),
        CommandSpec::new("view.headings", "{}", |s, _| {
            s.view.show_headings = !s.view.show_headings;
            Ok(json!(s.view.show_headings))
        }),
        CommandSpec::new("layout.headings_view", "{}", |s, _| {
            s.view.show_headings = !s.view.show_headings;
            Ok(json!(s.view.show_headings))
        }),
        CommandSpec::new("view.formula_bar", "{}", |s, _| {
            s.view.show_formula_bar = !s.view.show_formula_bar;
            Ok(json!(s.view.show_formula_bar))
        }),
        CommandSpec::new("view.full_screen", "{}", |s, _| {
            s.view.full_screen = !s.view.full_screen;
            Ok(json!(s.view.full_screen))
        }),
        CommandSpec::new("view.normal", "{}", |s, _| {
            s.view.full_screen = false;
            s.view.print.mode = crate::commands::print_cmds::ViewMode::Normal;
            Ok(Json::Null)
        }),
        CommandSpec::new("view.freeze_panes", r#"{"cell"?: "B2"}"#, |s, p| {
            let (_, at) = crate::params::cell(s, p, "cell")?;
            freeze(s, Some(at), "Freeze Panes")
        }),
        CommandSpec::new("view.freeze_top_row", "{}", |s, _| {
            freeze(s, Some(CellAddr::new(1, 0)), "Freeze Top Row")
        }),
        CommandSpec::new("view.freeze_first_column", "{}", |s, _| {
            freeze(s, Some(CellAddr::new(0, 1)), "Freeze First Column")
        }),
        CommandSpec::new("view.unfreeze_panes", "{}", |s, _| {
            freeze(s, None, "Unfreeze Panes")
        })
        .when(|s| {
            s.wb.sheet(s.sheet_index())
                .is_some_and(|sh| sh.freeze.is_some())
        }),
        CommandSpec::new("formulas.show_formulas", "{}", |s, _| {
            s.view.show_formulas = !s.view.show_formulas;
            Ok(json!(s.view.show_formulas))
        }),
        CommandSpec::new("formulas.calc_automatic", "{}", |s, _| {
            s.wb.calc_mode = CalcMode::Automatic;
            s.recalc();
            Ok(Json::Null)
        }),
        CommandSpec::new("formulas.calc_manual", "{}", |s, _| {
            s.wb.calc_mode = CalcMode::Manual;
            Ok(Json::Null)
        }),
        CommandSpec::new("formulas.calculate_now", "{}", |s, _| {
            s.recalc();
            Ok(json!({ "formulas": s.last_recalc.formulas, "cycles": s.last_recalc.cycles }))
        }),
        CommandSpec::new("formulas.calculate_sheet", "{}", |s, _| {
            s.recalc();
            Ok(json!({ "formulas": s.last_recalc.formulas }))
        }),
        CommandSpec::new("formulas.calculate_full", "{}", |s, _| {
            s.cache.clear();
            s.recalc();
            Ok(
                json!({ "formulas": s.last_recalc.formulas, "evaluations": s.last_recalc.evaluations }),
            )
        }),
        CommandSpec::new(
            "formulas.define_name",
            r#"{"name": "Rate", "refers_to"?: "=Sheet1!$B$1", "scope"?: "workbook|<sheet>"}"#,
            define_name,
        ),
        CommandSpec::new(
            "formulas.name_manager",
            r#"{"delete"?: "Rate"} → list of names"#,
            |s, p| {
                if let Some(del) = str_opt(p, "delete") {
                    let del = del.to_string();
                    s.transact("Delete Name", |tx, _| {
                        tx.touch_book();
                        tx.wb.names.retain(|n| !n.name.eq_ignore_ascii_case(&del));
                        Ok(())
                    })?;
                }
                Ok(json!(s.wb.names.iter().map(|n| json!({
                "name": n.name,
                "refers_to": format!("={}", n.formula),
                "scope": n.scope.and_then(|i| s.wb.sheets.get(i)).map_or("Workbook".to_string(), |sh| sh.name.clone()),
            })).collect::<Vec<_>>()))
            },
        ),
        CommandSpec::new(
            "formulas.insert_function",
            r#"{"category"?: "Math & Trig"} → function names"#,
            |_, p| {
                let cat = str_opt(p, "category");
                let live = sp_formula::function_names();
                let names: Vec<String> = crate::catalog::functions()
                    .iter()
                    .filter(|g| cat.is_none_or(|c| g.category.eq_ignore_ascii_case(c)))
                    .flat_map(|g| g.names.iter().cloned())
                    .filter(|n| live.contains(&n.as_str()))
                    .collect();
                Ok(json!(names))
            },
        ),
    ]
}

#[cfg(test)]
mod tests {
    use crate::Session;
    use serde_json::json;

    /// Gridlines are per sheet: undo brings them back, other sheets keep theirs. (The
    /// command also cleared the view-wide flag, which undo left off, so the grid stayed
    /// without gridlines after undo, and on every other sheet.)
    #[test]
    fn gridlines_toggle_is_per_sheet_and_undoes() {
        let mut s = Session::new();
        s.execute("home.cells.insert_sheet", &json!({"name": "Two"}))
            .unwrap();
        s.execute("sheet.activate", &json!({"sheet": "Sheet1"}))
            .unwrap();
        let shown = |s: &Session, name: &str| {
            s.view.show_gridlines
                && s.wb
                    .sheets
                    .iter()
                    .any(|x| x.name == name && x.show_gridlines)
        };
        s.execute("view.gridlines", &json!({})).unwrap();
        assert!(!shown(&s, "Sheet1"));
        assert!(shown(&s, "Two"), "the other sheet keeps its gridlines");
        s.execute("edit.undo", &json!({})).unwrap();
        assert!(shown(&s, "Sheet1"), "undo shows them again");
    }
}
