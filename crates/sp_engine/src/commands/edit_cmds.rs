//! Undo/redo, clipboard, clear, fill, find/replace, go to (`edit.*`, `home.clipboard.*`,
//! `home.editing.*`).

use crate::params::{bool_opt, range, str_opt, str_req};
use crate::{CmdError, CmdResult, CommandSpec, Json, Session};
use serde_json::json;
use sp_core::{CellAddr, CellRange, Value};
use sp_model::Content;
use sp_ops::clipboard::{self, PasteOpts, PasteWhat};
use sp_ops::edit::{self, ClearWhat, FillDir};

fn has_clip(s: &Session) -> bool {
    s.clipboard.is_some()
}

fn undo(s: &mut Session, _: &Json) -> CmdResult {
    let label = s
        .history
        .undo(&mut s.wb)
        .ok_or_else(|| CmdError::Failed("nothing to undo".into()))?;
    s.dirty = true;
    s.after_change();
    s.status = format!("Undo {label}");
    Ok(json!(label))
}

fn redo(s: &mut Session, _: &Json) -> CmdResult {
    let label = s
        .history
        .redo(&mut s.wb)
        .ok_or_else(|| CmdError::Failed("nothing to redo".into()))?;
    s.dirty = true;
    s.after_change();
    s.status = format!("Redo {label}");
    Ok(json!(label))
}

fn repeat(s: &mut Session, _: &Json) -> CmdResult {
    let (id, p) = s
        .last_command
        .clone()
        .ok_or_else(|| CmdError::Failed("nothing to repeat".into()))?;
    // Repeating a cell entry re-targets the active cell.
    let p = if id == "cell.set" {
        let mut p = p;
        if let Some(o) = p.as_object_mut() {
            o.remove("cell");
        }
        p
    } else {
        p
    };
    s.execute(&id, &p)
}

fn copy(s: &mut Session, p: &Json, cut: bool) -> CmdResult {
    let (sheet, r) = range(s, p, "range")?;
    s.clipboard = Some(clipboard::copy(&s.wb, sheet, r, cut)?);
    s.status = if cut {
        "Select destination and press Enter or choose Paste".into()
    } else {
        "Copied".into()
    };
    Ok(json!({ "range": r.to_a1(), "cells": s.clipboard.as_ref().map_or(0, |c| c.cells.len()) }))
}

/// Paste destination: `range` (tiles when a multiple of the copied block), else `cell`, else
/// the selection.
fn paste_dest(s: &Session, p: &Json) -> Result<(usize, CellRange), CmdError> {
    if p.get("range").is_some() {
        return range(s, p, "range");
    }
    if p.get("cell").is_some() {
        let (sheet, a) = crate::params::cell(s, p, "cell")?;
        return Ok((sheet, CellRange::single(a)));
    }
    Ok((s.sheet_index(), s.sel.range()))
}

/// Paste the clipboard with Paste Special options (shared with the Paste Special dialog).
pub(crate) fn paste_with(s: &mut Session, p: &Json, opts: PasteOpts) -> CmdResult {
    let clip = s
        .clipboard
        .clone()
        .ok_or_else(|| CmdError::Failed("clipboard is empty".into()))?;
    let (sheet, dest) = paste_dest(s, p)?;
    let label = match (opts.what, opts.transpose, opts.op) {
        (_, _, op) if op != sp_ops::PasteOp::None => "Paste Special",
        (PasteWhat::All, true, _) => "Paste Transpose",
        (PasteWhat::All, false, _) => "Paste",
        (PasteWhat::Values, _, _) => "Paste Values",
        (PasteWhat::Formulas, _, _) => "Paste Formulas",
        (PasteWhat::Formats, _, _) => "Paste Formatting",
    };
    let out = s.transact(label, |tx, _| {
        clipboard::paste(tx, &clip, sheet, dest, opts).map_err(CmdError::from)
    })?;
    if clip.cut {
        s.clipboard = None;
    }
    s.wb.active = sheet;
    s.select_range(out, out.start);
    Ok(json!({ "range": out.to_a1() }))
}

fn paste(s: &mut Session, p: &Json, what: PasteWhat, transpose: bool) -> CmdResult {
    paste_with(
        s,
        p,
        PasteOpts {
            what,
            transpose,
            ..Default::default()
        },
    )
}

fn clear(s: &mut Session, p: &Json, what: ClearWhat, label: &str) -> CmdResult {
    let (sheet, r) = range(s, p, "range")?;
    let n = s.transact(label, |tx, _| {
        edit::clear(tx, sheet, r, what).map_err(CmdError::from)
    })?;
    Ok(json!(n))
}

fn fill(s: &mut Session, p: &Json, dir: FillDir, label: &str) -> CmdResult {
    let (sheet, mut r) = range(s, p, "range")?;
    // A single selected cell fills from its neighbour (above for Down, left for Right).
    if r.is_single() {
        let a = r.start;
        let from = match dir {
            FillDir::Down => a.offset(-1, 0),
            FillDir::Right => a.offset(0, -1),
            FillDir::Up => a.offset(1, 0),
            FillDir::Left => a.offset(0, 1),
        };
        match from {
            Some(f) => r = CellRange::new(f, a),
            None => return Ok(Json::Null),
        }
    }
    s.transact(label, |tx, _| {
        edit::fill(tx, sheet, r, dir).map_err(CmdError::from)
    })?;
    Ok(Json::Null)
}

/// Contiguous numeric block above (or left of) the active cell, for AutoSum.
fn autosum_range(s: &Session) -> Option<CellRange> {
    let sh = s.wb.sheet(s.sheet_index())?;
    let a = s.sel.active;
    let is_num = |r: u32, c: u32| matches!(sh.value(CellAddr::new(r, c)), Value::Number(_));
    if a.row > 0 && is_num(a.row - 1, a.col) {
        let mut top = a.row - 1;
        while top > 0 && is_num(top - 1, a.col) {
            top -= 1;
        }
        return Some(CellRange::new(
            CellAddr::new(top, a.col),
            CellAddr::new(a.row - 1, a.col),
        ));
    }
    if a.col > 0 && is_num(a.row, a.col - 1) {
        let mut left = a.col - 1;
        while left > 0 && is_num(a.row, left - 1) {
            left -= 1;
        }
        return Some(CellRange::new(
            CellAddr::new(a.row, left),
            CellAddr::new(a.row, a.col - 1),
        ));
    }
    None
}

fn autosum(s: &mut Session, func: &str) -> CmdResult {
    let sel = s.sel.range();
    let sheet = s.sheet_index();
    // With a multi-cell numeric selection, put the result below it.
    let (target, src) = if !sel.is_single() {
        let below = CellAddr::new(
            sel.end.row.saturating_add(1).min(sp_core::MAX_ROWS - 1),
            sel.start.col,
        );
        (
            below,
            Some(CellRange::new(
                sel.start,
                CellAddr::new(sel.end.row, sel.start.col),
            )),
        )
    } else {
        (s.sel.active, autosum_range(s))
    };
    let formula = match src {
        Some(r) => format!("={func}({})", r.to_a1()),
        None => format!("={func}()"),
    };
    if src.is_none() {
        // Nothing to sum: the target app opens the formula in edit mode.
        s.view.editing = true;
        return Ok(json!({ "formula": formula }));
    }
    let old = s.wb.sheet(sheet).and_then(|sh| sh.get(target)).cloned();
    let cell = super::cell_cmds::parse_input(&formula, old.as_ref(), &mut s.wb.styles)?;
    s.transact("AutoSum", |tx, _| {
        tx.set_cell(sheet, target, cell);
        Ok(())
    })?;
    s.select(target);
    Ok(json!({ "cell": target.to_a1(), "formula": formula }))
}

fn find_matches(
    s: &Session,
    what: &str,
    in_formulas: bool,
    match_case: bool,
    whole: bool,
) -> Vec<(usize, CellAddr)> {
    let needle = if match_case {
        what.to_string()
    } else {
        what.to_lowercase()
    };
    let mut out = Vec::new();
    for (si, sh) in s.wb.sheets.iter().enumerate() {
        for (a, c) in sh.iter() {
            let hay = if in_formulas {
                c.input_text()
            } else {
                c.value.to_display()
            };
            let hay = if match_case { hay } else { hay.to_lowercase() };
            if (whole && hay == needle) || (!whole && hay.contains(&needle)) {
                out.push((si, a));
            }
        }
    }
    out
}

fn find(s: &mut Session, p: &Json, next: bool) -> CmdResult {
    let what = match str_opt(p, "what") {
        Some(w) => w.to_string(),
        None if next => s
            .last_find
            .clone()
            .ok_or_else(|| CmdError::Failed("nothing to find".into()))?,
        None => return Err(CmdError::Param("missing string 'what'".into())),
    };
    if what.is_empty() {
        return Err(CmdError::Param("search text is empty".into()));
    }
    s.last_find = Some(what.clone());
    let hits = find_matches(
        s,
        &what,
        bool_opt(p, "formulas").unwrap_or(true),
        bool_opt(p, "match_case").unwrap_or(false),
        bool_opt(p, "whole").unwrap_or(false),
    );
    let cur = (s.sheet_index(), s.sel.active);
    // Next match after the current cell in sheet/row-major order, wrapping around.
    let hit = hits
        .iter()
        .find(|h| (h.0, h.1.row, h.1.col) > (cur.0, cur.1.row, cur.1.col))
        .or_else(|| hits.first())
        .copied();
    match hit {
        Some((si, a)) => {
            s.wb.active = si;
            s.select(a);
            Ok(
                json!({ "cell": a.to_a1(), "sheet": s.wb.sheets.get(si).map(|x| x.name.clone()), "count": hits.len() }),
            )
        }
        None => Err(CmdError::Failed(format!(
            "We couldn't find what you were looking for: '{what}'"
        ))),
    }
}

fn replace(s: &mut Session, p: &Json) -> CmdResult {
    let what = str_req(p, "what")?.to_string();
    let with = str_opt(p, "with").unwrap_or("").to_string();
    if what.is_empty() {
        return Err(CmdError::Param("search text is empty".into()));
    }
    let match_case = bool_opt(p, "match_case").unwrap_or(false);
    let all = bool_opt(p, "all").unwrap_or(true);
    let hits = find_matches(s, &what, true, match_case, false);
    let hits: Vec<_> = if all {
        hits
    } else {
        hits.into_iter().take(1).collect()
    };
    let mut edits = Vec::new();
    for (si, a) in &hits {
        let Some(c) = s.wb.sheet(*si).and_then(|sh| sh.get(*a)) else {
            continue;
        };
        let text = c.input_text();
        let new_text = if match_case {
            text.replace(&what, &with)
        } else {
            // Case-insensitive replace preserving the rest of the text.
            let lower = text.to_lowercase();
            let wl = what.to_lowercase();
            let mut out = String::new();
            let mut i = 0;
            while let Some(pos) = lower.get(i..).and_then(|t| t.find(&wl)) {
                out.push_str(text.get(i..i + pos).unwrap_or(""));
                out.push_str(&with);
                i += pos + wl.len();
            }
            out.push_str(text.get(i..).unwrap_or(""));
            out
        };
        let old = c.clone();
        let new = super::cell_cmds::parse_input(&new_text, Some(&old), &mut s.wb.styles)?;
        edits.push((*si, *a, new));
    }
    let n = edits.len();
    s.transact("Replace", |tx, _| {
        for (si, a, c) in edits {
            tx.set_cell(si, a, c);
        }
        Ok(())
    })?;
    s.status = format!("All done. We made {n} replacements.");
    Ok(json!(n))
}

fn goto(s: &mut Session, p: &Json) -> CmdResult {
    let target = str_req(p, "reference")?;
    let (si, r) = crate::params::parse_ref(s, target)?;
    s.wb.active = si;
    s.select_range(r, r.start);
    Ok(json!(r.to_a1()))
}

fn select_kind(s: &mut Session, formulas: bool) -> CmdResult {
    let sheet = s.sheet_index();
    let Some(sh) = s.wb.sheet(sheet) else {
        return Err(CmdError::Failed("no sheet".into()));
    };
    let hits: Vec<CellAddr> = sh
        .iter()
        .filter(|(_, c)| match &c.content {
            Content::Formula(_) => formulas,
            Content::Value(v) => !formulas && !v.is_empty(),
        })
        .map(|(a, _)| a)
        .collect();
    let (Some(first), Some(last)) = (hits.first(), hits.last()) else {
        return Err(CmdError::Failed("No cells were found.".into()));
    };
    let (c0, c1) = hits
        .iter()
        .fold((u32::MAX, 0), |(lo, hi), a| (lo.min(a.col), hi.max(a.col)));
    s.select_range(
        CellRange::new(CellAddr::new(first.row, c0), CellAddr::new(last.row, c1)),
        *first,
    );
    Ok(json!(hits.iter().map(|a| a.to_a1()).collect::<Vec<_>>()))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("edit.undo", "{}", undo).when(|s| s.history.can_undo()),
        CommandSpec::new("edit.redo", "{}", redo).when(|s| s.history.can_redo()),
        CommandSpec::new("edit.repeat", "{}", repeat).when(|s| s.last_command.is_some()),
        CommandSpec::new("home.clipboard.copy", r#"{"range"?: "A1:B2"}"#, |s, p| {
            copy(s, p, false)
        }),
        CommandSpec::new("home.clipboard.cut", r#"{"range"?: "A1:B2"}"#, |s, p| {
            copy(s, p, true)
        }),
        CommandSpec::new(
            "home.clipboard.paste",
            r#"{"cell"?: "D1" | "range"?: "D1:F6" (default: selection; tiles when a multiple of the copy)}"#,
            |s, p| paste(s, p, PasteWhat::All, false),
        )
        .when(has_clip),
        CommandSpec::new(
            "home.clipboard.paste_values",
            r#"{"cell"?: "D1" | "range"?: "D1:F6"}"#,
            |s, p| paste(s, p, PasteWhat::Values, false),
        )
        .when(has_clip),
        CommandSpec::new(
            "home.clipboard.paste_formulas",
            r#"{"cell"?: "D1" | "range"?: "D1:F6"}"#,
            |s, p| paste(s, p, PasteWhat::Formulas, false),
        )
        .when(has_clip),
        CommandSpec::new(
            "home.clipboard.paste_formats",
            r#"{"cell"?: "D1" | "range"?: "D1:F6"}"#,
            |s, p| paste(s, p, PasteWhat::Formats, false),
        )
        .when(has_clip),
        CommandSpec::new(
            "home.clipboard.paste_transpose",
            r#"{"cell"?: "D1" | "range"?: "D1:F6"}"#,
            |s, p| paste(s, p, PasteWhat::All, true),
        )
        .when(has_clip),
        CommandSpec::new(
            "home.editing.clear_all",
            r#"{"range"?: "A1:B2"}"#,
            |s, p| clear(s, p, ClearWhat::All, "Clear"),
        ),
        CommandSpec::new(
            "home.editing.clear_formats",
            r#"{"range"?: "A1:B2"}"#,
            |s, p| clear(s, p, ClearWhat::Formats, "Clear Formats"),
        ),
        CommandSpec::new(
            "home.editing.clear_contents",
            r#"{"range"?: "A1:B2"}"#,
            |s, p| clear(s, p, ClearWhat::Contents, "Clear"),
        ),
        CommandSpec::new(
            "home.editing.fill_down",
            r#"{"range"?: "A1:A9"}"#,
            |s, p| fill(s, p, FillDir::Down, "Fill Down"),
        ),
        CommandSpec::new(
            "home.editing.fill_right",
            r#"{"range"?: "A1:F1"}"#,
            |s, p| fill(s, p, FillDir::Right, "Fill Right"),
        ),
        CommandSpec::new("home.editing.fill_up", r#"{"range"?: "A1:A9"}"#, |s, p| {
            fill(s, p, FillDir::Up, "Fill Up")
        }),
        CommandSpec::new(
            "home.editing.fill_left",
            r#"{"range"?: "A1:F1"}"#,
            |s, p| fill(s, p, FillDir::Left, "Fill Left"),
        ),
        CommandSpec::new("home.editing.autosum", "{}", |s, _| autosum(s, "SUM")),
        CommandSpec::new("home.editing.autosum_average", "{}", |s, _| {
            autosum(s, "AVERAGE")
        }),
        CommandSpec::new("home.editing.autosum_count", "{}", |s, _| {
            autosum(s, "COUNT")
        }),
        CommandSpec::new("home.editing.autosum_max", "{}", |s, _| autosum(s, "MAX")),
        CommandSpec::new("home.editing.autosum_min", "{}", |s, _| autosum(s, "MIN")),
        CommandSpec::new(
            "home.editing.find",
            r#"{"what": "text", "formulas"?: true, "match_case"?: false, "whole"?: false}"#,
            |s, p| find(s, p, false),
        ),
        CommandSpec::new("home.editing.find_next", r#"{"what"?: "text"}"#, |s, p| {
            find(s, p, true)
        }),
        CommandSpec::new(
            "home.editing.replace",
            r#"{"what": "a", "with": "b", "match_case"?: false, "all"?: true}"#,
            replace,
        ),
        CommandSpec::new(
            "home.editing.goto",
            r#"{"reference": "Sheet2!B5 | name"}"#,
            goto,
        ),
        CommandSpec::new("nav.goto_cell", r#"{"reference": "B5"}"#, goto),
        CommandSpec::new("home.editing.select_formulas", "{}", |s, _| {
            select_kind(s, true)
        }),
        CommandSpec::new("home.editing.select_constants", "{}", |s, _| {
            select_kind(s, false)
        }),
    ]
}
