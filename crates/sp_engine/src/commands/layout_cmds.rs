//! Page Layout settings stored on the sheet (`layout.margins`, `layout.orientation`,
//! `layout.size`, `layout.print_area.*`, `layout.page_break.*`, `layout.print_titles`,
//! `layout.scale_to_fit`, `layout.gridlines_print`, `layout.headings_print`).

use crate::params::{bool_opt, cell, f64_opt, range, str_opt, u32_opt};
use crate::{CmdError, CmdResult, CommandSpec, Json, Session};
use serde_json::json;
use sp_core::addr::quote_sheet_name;
use sp_core::{CellRange, col_to_letters};
use sp_model::{DefinedName, Margins, Orientation, PageSetup};

pub const PRINT_AREA: &str = "_xlnm.Print_Area";
pub const PRINT_TITLES: &str = "_xlnm.Print_Titles";

/// Paper sizes by name → file-format code.
const PAPER: &[(&str, u32)] = &[
    ("letter", 1),
    ("tabloid", 3),
    ("ledger", 4),
    ("legal", 5),
    ("statement", 6),
    ("executive", 7),
    ("a3", 8),
    ("a4", 9),
    ("a5", 11),
    ("b4", 12),
    ("b5", 13),
    ("folio", 14),
    ("envelope_10", 20),
    ("envelope_dl", 27),
];

fn page_json(ps: &PageSetup) -> Json {
    json!({
        "margins": {"left": ps.margins.left, "right": ps.margins.right, "top": ps.margins.top,
                     "bottom": ps.margins.bottom, "header": ps.margins.header, "footer": ps.margins.footer},
        "orientation": match ps.orientation { Orientation::Portrait => "portrait", Orientation::Landscape => "landscape" },
        "paper_size": ps.paper_size,
        "scale": ps.scale,
        "fit_width": ps.fit_width,
        "fit_height": ps.fit_height,
        "print_gridlines": ps.print_gridlines,
        "print_headings": ps.print_headings,
        "row_breaks": ps.row_breaks.iter().map(|r| r + 1).collect::<Vec<_>>(),
        "col_breaks": ps.col_breaks.iter().map(|c| col_to_letters(*c)).collect::<Vec<_>>(),
    })
}

/// Change the active sheet's page setup as one undoable step.
fn edit_page(
    s: &mut Session,
    label: &str,
    f: impl FnOnce(&mut PageSetup) -> Result<(), CmdError>,
) -> CmdResult {
    let sheet = s.sheet_index();
    s.transact(label, |tx, _| {
        tx.touch_sheet(sheet);
        let sh = tx
            .wb
            .sheet_mut(sheet)
            .ok_or_else(|| CmdError::Failed("no sheet".into()))?;
        f(&mut sh.page_setup)
    })?;
    Ok(s.wb
        .sheet(sheet)
        .map_or(Json::Null, |sh| page_json(&sh.page_setup)))
}

fn margins(s: &mut Session, p: &Json) -> CmdResult {
    let preset = match str_opt(p, "preset") {
        Some("normal") => Some(Margins::NORMAL),
        Some("wide") => Some(Margins::WIDE),
        Some("narrow") => Some(Margins::NARROW),
        Some(o) => {
            return Err(CmdError::Param(format!(
                "unknown preset '{o}' (normal, wide, narrow)"
            )));
        }
        None => None,
    };
    let mut m = preset.unwrap_or_else(|| {
        s.wb.sheet(s.sheet_index())
            .map_or(Margins::NORMAL, |sh| sh.page_setup.margins)
    });
    let mut any = preset.is_some();
    for (k, slot) in [
        ("left", &mut m.left),
        ("right", &mut m.right),
        ("top", &mut m.top),
        ("bottom", &mut m.bottom),
        ("header", &mut m.header),
        ("footer", &mut m.footer),
    ] {
        if let Some(v) = f64_opt(p, k)? {
            if !(0.0..=49.0).contains(&v) {
                return Err(CmdError::Param(format!("margin '{k}' must be 0-49 inches")));
            }
            *slot = v;
            any = true;
        }
    }
    if !any {
        return Err(CmdError::Param(
            "give 'preset' (normal|wide|narrow) or margins in inches".into(),
        ));
    }
    edit_page(s, "Margins", |ps| {
        ps.margins = m;
        Ok(())
    })
}

fn orientation(s: &mut Session, p: &Json) -> CmdResult {
    let want = match str_opt(p, "orientation") {
        Some("portrait") => Some(Orientation::Portrait),
        Some("landscape") => Some(Orientation::Landscape),
        Some(o) => return Err(CmdError::Param(format!("unknown orientation '{o}'"))),
        None => None,
    };
    edit_page(s, "Orientation", |ps| {
        ps.orientation = want.unwrap_or(match ps.orientation {
            Orientation::Portrait => Orientation::Landscape,
            Orientation::Landscape => Orientation::Portrait,
        });
        Ok(())
    })
}

fn size(s: &mut Session, p: &Json) -> CmdResult {
    let code = match p.get("size") {
        Some(Json::String(t)) => PAPER
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(t.trim()))
            .map(|(_, c)| *c)
            .ok_or_else(|| {
                CmdError::Param(format!(
                    "unknown paper '{t}' ({})",
                    PAPER.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ")
                ))
            })?,
        Some(_) => match u32_opt(p, "size")? {
            Some(c) if (1..=118).contains(&c) => c,
            _ => return Err(CmdError::Param("paper size code must be 1-118".into())),
        },
        None => {
            return Err(CmdError::Param(
                "missing 'size' (letter, a4, legal…)".into(),
            ));
        }
    };
    edit_page(s, "Size", |ps| {
        ps.paper_size = code;
        Ok(())
    })
}

/// Replace (or remove with `None`) a sheet-scoped built-in name.
fn set_builtin_name(
    s: &mut Session,
    sheet: usize,
    name: &str,
    formula: Option<String>,
) -> CmdResult {
    s.transact(name.trim_start_matches("_xlnm."), |tx, _| {
        tx.touch_book();
        tx.wb
            .names
            .retain(|n| !(n.scope == Some(sheet) && n.name.eq_ignore_ascii_case(name)));
        if let Some(f) = formula.clone() {
            tx.wb.names.push(DefinedName {
                name: name.into(),
                formula: f,
                scope: Some(sheet),
            });
        }
        Ok(())
    })?;
    Ok(formula.map_or(Json::Null, Json::String))
}

fn print_area_set(s: &mut Session, p: &Json) -> CmdResult {
    let (sheet, r) = range(s, p, "range")?;
    let name =
        s.wb.sheet(sheet)
            .map(|sh| sh.name.clone())
            .unwrap_or_default();
    set_builtin_name(
        s,
        sheet,
        PRINT_AREA,
        Some(super::audit_cmds::abs_ref(&name, r)),
    )
}

fn print_area_clear(s: &mut Session, _: &Json) -> CmdResult {
    let sheet = s.sheet_index();
    set_builtin_name(s, sheet, PRINT_AREA, None)
}

fn print_titles(s: &mut Session, p: &Json) -> CmdResult {
    let sheet = s.sheet_index();
    let name =
        s.wb.sheet(sheet)
            .map(|sh| sh.name.clone())
            .unwrap_or_default();
    let q = quote_sheet_name(&name);
    let mut parts = Vec::new();
    if let Some(rows) = str_opt(p, "rows").filter(|t| !t.trim().is_empty()) {
        let r = CellRange::parse(&rows.replace('$', ""))
            .ok()
            .filter(CellRange::is_full_rows)
            .ok_or_else(|| CmdError::Param(format!("'rows' must look like 1:2, not '{rows}'")))?;
        parts.push(format!("{q}!${}:${}", r.start.row + 1, r.end.row + 1));
    }
    if let Some(cols) = str_opt(p, "cols")
        .or_else(|| str_opt(p, "columns"))
        .filter(|t| !t.trim().is_empty())
    {
        let r = CellRange::parse(&cols.replace('$', ""))
            .ok()
            .filter(CellRange::is_full_cols)
            .ok_or_else(|| CmdError::Param(format!("'cols' must look like A:B, not '{cols}'")))?;
        parts.push(format!(
            "{q}!${}:${}",
            col_to_letters(r.start.col),
            col_to_letters(r.end.col)
        ));
    }
    let formula = (!parts.is_empty()).then(|| parts.join(","));
    set_builtin_name(s, sheet, PRINT_TITLES, formula)
}

fn page_break(s: &mut Session, p: &Json, insert: bool) -> CmdResult {
    let (_, at) = cell(s, p, "cell")?;
    edit_page(
        s,
        if insert {
            "Insert Page Break"
        } else {
            "Remove Page Break"
        },
        |ps| {
            if insert {
                if at.row == 0 && at.col == 0 {
                    return Err(CmdError::Failed(
                        "A page break can't go above or left of A1.".into(),
                    ));
                }
                if ps.row_breaks.len() + ps.col_breaks.len() >= 2_052 {
                    return Err(CmdError::Failed("too many page breaks".into()));
                }
                if at.row > 0 {
                    ps.row_breaks.insert(at.row);
                }
                if at.col > 0 {
                    ps.col_breaks.insert(at.col);
                }
                Ok(())
            } else {
                let removed = ps.row_breaks.remove(&at.row) | ps.col_breaks.remove(&at.col);
                if removed {
                    Ok(())
                } else {
                    Err(CmdError::Failed(
                        "There is no manual page break here.".into(),
                    ))
                }
            }
        },
    )
}

fn scale_to_fit(s: &mut Session, p: &Json) -> CmdResult {
    let fit = |k: &str| -> Result<Option<Option<u32>>, CmdError> {
        match p.get(k) {
            None => Ok(None),
            Some(Json::String(t)) if t.eq_ignore_ascii_case("automatic") => Ok(Some(Some(0))),
            Some(_) => match u32_opt(p, k)? {
                Some(n) if n <= 32_767 => Ok(Some(Some(n))),
                _ => Err(CmdError::Param(format!("'{k}' must be 0-32767 pages"))),
            },
        }
    };
    let width = fit("width")?;
    let height = fit("height")?;
    let scale = match u32_opt(p, "scale")? {
        Some(n) if (10..=400).contains(&n) => Some(n),
        Some(_) => return Err(CmdError::Param("'scale' must be 10-400 percent".into())),
        None => None,
    };
    if width.is_none() && height.is_none() && scale.is_none() {
        return Err(CmdError::Param(
            "give 'width'/'height' (pages) or 'scale' (%)".into(),
        ));
    }
    edit_page(s, "Scale to Fit", |ps| {
        if let Some(n) = scale {
            ps.scale = n;
            ps.fit_width = None;
            ps.fit_height = None;
        }
        if let Some(w) = width {
            ps.fit_width = w;
            ps.fit_height = ps.fit_height.or(Some(0));
        }
        if let Some(h) = height {
            ps.fit_height = h;
            ps.fit_width = ps.fit_width.or(Some(0));
        }
        // Both automatic = plain scaling.
        if ps.fit_width == Some(0) && ps.fit_height == Some(0) {
            ps.fit_width = None;
            ps.fit_height = None;
        }
        Ok(())
    })
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new(
            "layout.margins",
            r#"{"preset"?: "normal|wide|narrow", "left"?, "right"?, "top"?, "bottom"?, "header"?, "footer"? (inches)}"#,
            margins,
        ),
        CommandSpec::new(
            "layout.orientation",
            r#"{"orientation"?: "portrait|landscape"} (default toggles)"#,
            orientation,
        ),
        CommandSpec::new(
            "layout.size",
            r#"{"size": "letter|legal|a4|a3|a5|… | 9"}"#,
            size,
        ),
        CommandSpec::new(
            "layout.print_area.set",
            r#"{"range"?: "A1:F40"}"#,
            print_area_set,
        ),
        CommandSpec::new("layout.print_area.clear", "{}", print_area_clear),
        CommandSpec::new("layout.page_break.insert", r#"{"cell"?: "A20"}"#, |s, p| {
            page_break(s, p, true)
        }),
        CommandSpec::new("layout.page_break.remove", r#"{"cell"?: "A20"}"#, |s, p| {
            page_break(s, p, false)
        })
        .when(|s| {
            s.wb.sheet(s.sheet_index()).is_some_and(|sh| {
                !sh.page_setup.row_breaks.is_empty() || !sh.page_setup.col_breaks.is_empty()
            })
        }),
        CommandSpec::new("layout.page_break.reset", "{}", |s, _| {
            edit_page(s, "Reset All Page Breaks", |ps| {
                ps.row_breaks.clear();
                ps.col_breaks.clear();
                Ok(())
            })
        }),
        CommandSpec::new(
            "layout.print_titles",
            r#"{"rows"?: "1:2", "cols"?: "A:A"} (both empty clears)"#,
            print_titles,
        ),
        CommandSpec::new(
            "layout.scale_to_fit",
            r#"{"width"?: pages|"automatic", "height"?: pages|"automatic", "scale"?: 10-400}"#,
            scale_to_fit,
        ),
        CommandSpec::new(
            "layout.gridlines_print",
            r#"{"on"?: bool} (default toggles)"#,
            |s, p| {
                let on = bool_opt(p, "on");
                edit_page(s, "Print Gridlines", |ps| {
                    ps.print_gridlines = on.unwrap_or(!ps.print_gridlines);
                    Ok(())
                })
            },
        ),
        CommandSpec::new(
            "layout.headings_print",
            r#"{"on"?: bool} (default toggles)"#,
            |s, p| {
                let on = bool_opt(p, "on");
                edit_page(s, "Print Headings", |ps| {
                    ps.print_headings = on.unwrap_or(!ps.print_headings);
                    Ok(())
                })
            },
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use sp_core::CellAddr;

    #[test]
    fn page_setup_commands() {
        let mut s = Session::new();
        s.execute("layout.margins", &json!({"preset": "wide"}))
            .unwrap();
        s.execute("layout.margins", &json!({"left": 0.5})).unwrap();
        s.execute("layout.orientation", &json!({})).unwrap();
        s.execute("layout.size", &json!({"size": "A4"})).unwrap();
        s.execute("layout.scale_to_fit", &json!({"width": 1}))
            .unwrap();
        s.execute("layout.gridlines_print", &json!({})).unwrap();
        s.execute("layout.headings_print", &json!({"on": true}))
            .unwrap();
        s.execute("layout.page_break.insert", &json!({"cell": "C10"}))
            .unwrap();
        let ps = &s.wb.sheets[0].page_setup;
        assert_eq!(ps.margins.left, 0.5);
        assert_eq!(ps.margins.top, 1.0);
        assert_eq!(ps.orientation, Orientation::Landscape);
        assert_eq!(ps.paper_size, 9);
        assert_eq!((ps.fit_width, ps.fit_height), (Some(1), Some(0)));
        assert!(ps.print_gridlines && ps.print_headings);
        assert!(ps.row_breaks.contains(&9) && ps.col_breaks.contains(&2));
        s.execute("layout.page_break.remove", &json!({"cell": "A10"}))
            .unwrap();
        assert!(s.wb.sheets[0].page_setup.row_breaks.is_empty());
        assert!(
            s.execute("layout.page_break.remove", &json!({"cell": "A10"}))
                .is_err()
        );
        s.execute("layout.page_break.reset", &json!({})).unwrap();
        assert!(s.wb.sheets[0].page_setup.col_breaks.is_empty());
        assert!(
            s.execute("layout.size", &json!({"size": "napkin"}))
                .is_err()
        );
        s.execute("edit.undo", &json!({})).unwrap();
        assert!(s.wb.sheets[0].page_setup.col_breaks.contains(&2));
    }

    #[test]
    fn print_area_and_titles_are_names() {
        let mut s = Session::new();
        s.execute("layout.print_area.set", &json!({"range": "A1:D20"}))
            .unwrap();
        s.execute(
            "layout.print_titles",
            &json!({"rows": "1:2", "cols": "A:A"}),
        )
        .unwrap();
        let n = s.wb.name_lookup(PRINT_AREA, 0).unwrap();
        assert_eq!(n.formula, "Sheet1!$A$1:$D$20");
        assert_eq!(n.scope, Some(0));
        assert_eq!(
            s.wb.name_lookup(PRINT_TITLES, 0).unwrap().formula,
            "Sheet1!$1:$2,Sheet1!$A:$A"
        );
        s.execute("layout.print_area.clear", &json!({})).unwrap();
        assert!(s.wb.name_lookup(PRINT_AREA, 0).is_none());
        assert!(
            s.execute("layout.print_titles", &json!({"rows": "A1"}))
                .is_err()
        );
        let _ = CellAddr::default();
    }
}
