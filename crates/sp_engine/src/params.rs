//! Typed access to JSON command params. Every accessor validates; nothing panics.

use crate::{CmdError, Json, Session};
use sp_core::addr::split_sheet_prefix;
use sp_core::{CellAddr, CellRange};

pub fn str_opt<'a>(p: &'a Json, k: &str) -> Option<&'a str> {
    p.get(k).and_then(Json::as_str)
}

pub fn str_req<'a>(p: &'a Json, k: &str) -> Result<&'a str, CmdError> {
    str_opt(p, k).ok_or_else(|| CmdError::Param(format!("missing string '{k}'")))
}

pub fn f64_opt(p: &Json, k: &str) -> Result<Option<f64>, CmdError> {
    match p.get(k) {
        None | Some(Json::Null) => Ok(None),
        Some(v) => match v
            .as_f64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
        {
            Some(n) if n.is_finite() => Ok(Some(n)),
            _ => Err(CmdError::Param(format!("'{k}' must be a number"))),
        },
    }
}

pub fn f64_req(p: &Json, k: &str) -> Result<f64, CmdError> {
    f64_opt(p, k)?.ok_or_else(|| CmdError::Param(format!("missing number '{k}'")))
}

pub fn u32_opt(p: &Json, k: &str) -> Result<Option<u32>, CmdError> {
    match f64_opt(p, k)? {
        None => Ok(None),
        Some(n) if (0.0..=f64::from(u32::MAX)).contains(&n) => Ok(Some(n as u32)),
        Some(_) => Err(CmdError::Param(format!("'{k}' out of range"))),
    }
}

pub fn bool_opt(p: &Json, k: &str) -> Option<bool> {
    p.get(k).and_then(Json::as_bool)
}

/// Resolve `name` → (sheet index, range). Accepts `A1`, `A1:B2`, `Sheet2!A1`, `'My S'!A:A`,
/// or a defined name. Missing key → the current selection.
pub fn range(s: &Session, p: &Json, k: &str) -> Result<(usize, CellRange), CmdError> {
    match str_opt(p, k) {
        None => Ok((s.sheet_index(), s.sel.range())),
        Some(text) => parse_ref(s, text),
    }
}

/// Like `range` but defaults to the active cell.
pub fn cell(s: &Session, p: &Json, k: &str) -> Result<(usize, CellAddr), CmdError> {
    match str_opt(p, k) {
        None => Ok((s.sheet_index(), s.sel.active)),
        Some(text) => parse_ref(s, text).map(|(i, r)| (i, r.start)),
    }
}

pub fn parse_ref(s: &Session, text: &str) -> Result<(usize, CellRange), CmdError> {
    let (sheet, body) = split_sheet_prefix(text.trim());
    let idx = match sheet {
        Some(name) => {
            s.wb.sheet_index(&name)
                .ok_or_else(|| CmdError::Param(format!("no sheet named '{name}'")))?
        }
        None => s.sheet_index(),
    };
    if let Ok(r) = CellRange::parse(body) {
        return Ok((idx, r));
    }
    if let Some(n) = s.wb.name_lookup(body, idx) {
        let (ns, nb) = split_sheet_prefix(&n.formula);
        let nidx = match ns {
            Some(name) => s.wb.sheet_index(&name).unwrap_or(idx),
            None => idx,
        };
        if let Ok(r) = CellRange::parse(&nb.replace('$', "")) {
            return Ok((nidx, r));
        }
    }
    Err(CmdError::Param(format!("not a reference: '{text}'")))
}
