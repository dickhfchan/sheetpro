//! Lookup & Reference functions.

use super::criteria::wildcard_match;
use super::{Registry, arg_array};
use crate::ast::Expr;
use crate::eval::{Array2, Ev, EvalValue, SheetRange, compare};
use sp_core::addr::{quote_sheet_name, split_sheet_prefix};
use sp_core::{CellAddr, CellRange, ErrorKind, MAX_COLS, MAX_ROWS, Value, col_to_letters};
use std::cmp::Ordering;

fn same_kind(a: &Value, b: &Value) -> bool {
    matches!(
        (a, b),
        (Value::Number(_), Value::Number(_))
            | (Value::Text(_), Value::Text(_))
            | (Value::Bool(_), Value::Bool(_))
    )
}

fn exact_eq(target: &Value, v: &Value, wildcards: bool) -> bool {
    match (target, v) {
        (Value::Text(t), Value::Text(s)) if wildcards && t.contains(['*', '?', '~']) => {
            wildcard_match(t, s)
        }
        _ => same_kind(target, v) && compare(target, v) == Ordering::Equal,
    }
}

/// Position of `target` in `items`. `mode`: 0 exact, 1 largest ≤ (sorted asc),
/// -1 smallest ≥ (sorted desc). Wildcards apply to exact text matches.
pub fn find_pos(items: &[Value], target: &Value, mode: i32, wildcards: bool) -> Option<usize> {
    match mode {
        0 => items.iter().position(|v| exact_eq(target, v, wildcards)),
        1 => {
            // Binary search over same-type values, like the target app's approximate match.
            let (mut lo, mut hi) = (0usize, items.len());
            let mut best = None;
            while lo < hi {
                let mid = lo + (hi - lo) / 2;
                let Some(v) = items.get(mid) else { break };
                if !same_kind(target, v) {
                    // Skip mismatched types by scanning left for a comparable value.
                    let left = (lo..mid)
                        .rev()
                        .find(|i| items.get(*i).is_some_and(|x| same_kind(target, x)));
                    match left {
                        Some(l)
                            if items
                                .get(l)
                                .is_some_and(|x| compare(x, target) != Ordering::Greater) =>
                        {
                            best = Some(l);
                            lo = mid + 1;
                        }
                        _ => hi = mid,
                    }
                    continue;
                }
                if compare(v, target) != Ordering::Greater {
                    best = Some(mid);
                    lo = mid + 1;
                } else {
                    hi = mid;
                }
            }
            best
        }
        _ => {
            let mut best = None;
            for (i, v) in items.iter().enumerate() {
                if !same_kind(target, v) {
                    continue;
                }
                if compare(v, target) != Ordering::Less {
                    best = Some(i);
                } else {
                    break;
                }
            }
            best
        }
    }
}

fn vhlookup(ev: &mut Ev<'_>, args: &[Expr], vertical: bool) -> EvalValue {
    let Some(t) = args.first() else {
        return EvalValue::err(ErrorKind::Value);
    };
    let target = ev.eval_scalar(t);
    if let Value::Error(e) = target {
        return EvalValue::err(e);
    }
    let table = match arg_array(ev, args, 1) {
        Ok(a) => a,
        Err(e) => return EvalValue::err(e),
    };
    let idx = match args.get(2).map(|e| ev.num(e)) {
        Some(Ok(n)) => n.trunc(),
        Some(Err(e)) => return EvalValue::err(e),
        None => return EvalValue::err(ErrorKind::Value),
    };
    let approx = match ev.bool_or(args, 3, true) {
        Ok(b) => b,
        Err(e) => return EvalValue::err(e),
    };
    let width = if vertical { table.cols } else { table.rows };
    if idx < 1.0 {
        return EvalValue::err(ErrorKind::Value);
    }
    if idx as usize > width {
        return EvalValue::err(ErrorKind::Ref);
    }
    let keys: Vec<Value> = if vertical {
        table.col(0).cloned().collect()
    } else {
        table.row(0).cloned().collect()
    };
    let pos = find_pos(&keys, &target, if approx { 1 } else { 0 }, !approx);
    match pos {
        Some(p) => {
            let k = idx as usize - 1;
            let v = if vertical {
                table.get(p, k)
            } else {
                table.get(k, p)
            };
            EvalValue::Scalar(v.clone())
        }
        None => EvalValue::err(ErrorKind::NA),
    }
}

fn match_fn(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let Some(t) = args.first() else {
        return EvalValue::err(ErrorKind::Value);
    };
    let target = ev.eval_scalar(t);
    if let Value::Error(e) = target {
        return EvalValue::err(e);
    }
    let arr = match arg_array(ev, args, 1) {
        Ok(a) => a,
        Err(e) => return EvalValue::err(e),
    };
    if arr.rows != 1 && arr.cols != 1 {
        return EvalValue::err(ErrorKind::NA);
    }
    let mode = match ev.num_or(args, 2, 1.0) {
        Ok(m) => m.trunc().clamp(-1.0, 1.0) as i32,
        Err(e) => return EvalValue::err(e),
    };
    match find_pos(&arr.data, &target, mode, mode == 0) {
        Some(p) => EvalValue::num(p as f64 + 1.0),
        None => EvalValue::err(ErrorKind::NA),
    }
}

/// XMATCH/XLOOKUP search: match_mode 0 exact, -1 exact-or-next-smaller, 1 exact-or-next-larger,
/// 2 wildcard; search_mode 1 first-to-last, -1 last-to-first (binary modes treated linearly).
fn xfind(items: &[Value], target: &Value, match_mode: i32, search_mode: i32) -> Option<usize> {
    let order: Vec<usize> = if search_mode < 0 && search_mode != -2 {
        (0..items.len()).rev().collect()
    } else {
        (0..items.len()).collect()
    };
    let mut best: Option<usize> = None;
    for i in order {
        let Some(v) = items.get(i) else { continue };
        if exact_eq(target, v, match_mode == 2) {
            return Some(i);
        }
        if (match_mode == -1 || match_mode == 1) && same_kind(target, v) {
            let o = compare(v, target);
            let better = |b: usize| {
                items.get(b).is_some_and(|bv| {
                    compare(v, bv)
                        == if match_mode == -1 {
                            Ordering::Greater
                        } else {
                            Ordering::Less
                        }
                })
            };
            let candidate = (match_mode == -1 && o == Ordering::Less)
                || (match_mode == 1 && o == Ordering::Greater);
            if candidate && best.is_none_or(better) {
                best = Some(i);
            }
        }
    }
    best
}

fn xmatch(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let Some(t) = args.first() else {
        return EvalValue::err(ErrorKind::Value);
    };
    let target = ev.eval_scalar(t);
    let arr = match arg_array(ev, args, 1) {
        Ok(a) => a,
        Err(e) => return EvalValue::err(e),
    };
    let (mm, sm) = match (ev.num_or(args, 2, 0.0), ev.num_or(args, 3, 1.0)) {
        (Ok(a), Ok(b)) => (a as i32, b as i32),
        (Err(e), _) | (_, Err(e)) => return EvalValue::err(e),
    };
    if arr.rows != 1 && arr.cols != 1 {
        return EvalValue::err(ErrorKind::Value);
    }
    match xfind(&arr.data, &target, mm, sm) {
        Some(p) => EvalValue::num(p as f64 + 1.0),
        None => EvalValue::err(ErrorKind::NA),
    }
}

fn xlookup(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let Some(t) = args.first() else {
        return EvalValue::err(ErrorKind::Value);
    };
    let target = ev.eval_scalar(t);
    let (look, ret) = match (arg_array(ev, args, 1), arg_array(ev, args, 2)) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => return EvalValue::err(e),
    };
    let (mm, sm) = match (ev.num_or(args, 4, 0.0), ev.num_or(args, 5, 1.0)) {
        (Ok(a), Ok(b)) => (a as i32, b as i32),
        (Err(e), _) | (_, Err(e)) => return EvalValue::err(e),
    };
    let vertical = look.cols == 1;
    if look.rows != 1 && look.cols != 1 {
        return EvalValue::err(ErrorKind::Value);
    }
    if (vertical && ret.rows != look.rows) || (!vertical && ret.cols != look.cols) {
        return EvalValue::err(ErrorKind::Value);
    }
    match xfind(&look.data, &target, mm, sm) {
        Some(p) => {
            if vertical {
                let row: Vec<Value> = ret.row(p).cloned().collect();
                if row.len() == 1 {
                    EvalValue::Scalar(row.into_iter().next().unwrap_or_default())
                } else {
                    EvalValue::Array(Array2::new(1, row.len(), row))
                }
            } else {
                let col: Vec<Value> = ret.col(p).cloned().collect();
                if col.len() == 1 {
                    EvalValue::Scalar(col.into_iter().next().unwrap_or_default())
                } else {
                    EvalValue::Array(Array2::new(col.len(), 1, col))
                }
            }
        }
        None => match args.get(3) {
            Some(Expr::Missing) | None => EvalValue::err(ErrorKind::NA),
            Some(e) => ev.eval(e),
        },
    }
}

fn index(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let Some(a) = args.first() else {
        return EvalValue::err(ErrorKind::Value);
    };
    let base = ev.eval(a);
    let (r, c) = match (ev.num_or(args, 1, 0.0), ev.num_or(args, 2, 0.0)) {
        (Ok(r), Ok(c)) => (r.trunc(), c.trunc()),
        (Err(e), _) | (_, Err(e)) => return EvalValue::err(e),
    };
    if r < 0.0 || c < 0.0 {
        return EvalValue::err(ErrorKind::Value);
    }
    let area = match ev.num_or(args, 3, 1.0) {
        Ok(n) => n.trunc() as usize,
        Err(e) => return EvalValue::err(e),
    };
    let base = match base {
        EvalValue::Refs(rs) => match rs.get(area.saturating_sub(1)) {
            Some(r) => EvalValue::Ref(*r),
            None => return EvalValue::err(ErrorKind::Ref),
        },
        other => other,
    };
    match base {
        EvalValue::Ref(sr) => {
            let rg = sr.range;
            let (rows, cols) = (rg.rows(), rg.cols());
            // With one dimension, a single number indexes along that dimension.
            let (r, c) = if args.len() == 2 && rows == 1 {
                (1.0, r)
            } else {
                (r, c)
            };
            if r as u32 > rows || c as u32 > cols {
                return EvalValue::err(ErrorKind::Ref);
            }
            let (r0, r1) = if r == 0.0 {
                (rg.start.row, rg.end.row)
            } else {
                (rg.start.row + r as u32 - 1, rg.start.row + r as u32 - 1)
            };
            let (c0, c1) = if c == 0.0 {
                (rg.start.col, rg.end.col)
            } else {
                (rg.start.col + c as u32 - 1, rg.start.col + c as u32 - 1)
            };
            EvalValue::Ref(SheetRange {
                sheet: sr.sheet,
                range: CellRange::new(CellAddr::new(r0, c0), CellAddr::new(r1, c1)),
            })
        }
        EvalValue::Array(arr) => {
            let (r, c) = if args.len() == 2 && arr.rows == 1 {
                (1.0, r)
            } else {
                (r, c)
            };
            if r as usize > arr.rows || c as usize > arr.cols {
                return EvalValue::err(ErrorKind::Ref);
            }
            match (r as usize, c as usize) {
                (0, 0) => EvalValue::Array(arr),
                (0, c) => {
                    EvalValue::Array(Array2::new(arr.rows, 1, arr.col(c - 1).cloned().collect()))
                }
                (r, 0) => {
                    EvalValue::Array(Array2::new(1, arr.cols, arr.row(r - 1).cloned().collect()))
                }
                (r, c) => EvalValue::Scalar(arr.get(r - 1, c - 1).clone()),
            }
        }
        EvalValue::Scalar(v) => {
            if r <= 1.0 && c <= 1.0 {
                EvalValue::Scalar(v)
            } else {
                EvalValue::err(ErrorKind::Ref)
            }
        }
        EvalValue::Refs(_) => EvalValue::err(ErrorKind::Ref),
    }
}

fn choose(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let Some(a) = args.first() else {
        return EvalValue::err(ErrorKind::Value);
    };
    match ev.num(a) {
        Ok(n) if n >= 1.0 => match args.get(n.trunc() as usize) {
            Some(e) => ev.eval(e),
            None => EvalValue::err(ErrorKind::Value),
        },
        Ok(_) => EvalValue::err(ErrorKind::Value),
        Err(e) => EvalValue::err(e),
    }
}

fn row_col(ev: &mut Ev<'_>, args: &[Expr], row: bool) -> EvalValue {
    match args.first() {
        None => EvalValue::num(f64::from(if row { ev.cell.row } else { ev.cell.col }) + 1.0),
        Some(e) => match ev.eval(e) {
            EvalValue::Ref(r) => {
                let rg = r.range;
                // A multi-row ref returns a vertical array of row numbers (spills in 365).
                if row && rg.rows() > 1 && !rg.is_full_cols() {
                    let data = (rg.start.row..=rg.end.row)
                        .map(|x| Value::Number(f64::from(x) + 1.0))
                        .collect();
                    return EvalValue::Array(Array2::new(rg.rows() as usize, 1, data));
                }
                if !row && rg.cols() > 1 && !rg.is_full_rows() {
                    let data = (rg.start.col..=rg.end.col)
                        .map(|x| Value::Number(f64::from(x) + 1.0))
                        .collect();
                    return EvalValue::Array(Array2::new(1, rg.cols() as usize, data));
                }
                EvalValue::num(f64::from(if row { rg.start.row } else { rg.start.col }) + 1.0)
            }
            EvalValue::Scalar(Value::Error(e)) => EvalValue::err(e),
            _ => EvalValue::err(ErrorKind::Value),
        },
    }
}

fn rows_cols(ev: &mut Ev<'_>, args: &[Expr], rows: bool) -> EvalValue {
    let Some(a) = args.first() else {
        return EvalValue::err(ErrorKind::Value);
    };
    match ev.eval(a) {
        EvalValue::Ref(r) => EvalValue::num(f64::from(if rows {
            r.range.rows()
        } else {
            r.range.cols()
        })),
        EvalValue::Array(arr) => EvalValue::num((if rows { arr.rows } else { arr.cols }) as f64),
        EvalValue::Scalar(Value::Error(e)) => EvalValue::err(e),
        EvalValue::Scalar(_) => EvalValue::num(1.0),
        EvalValue::Refs(_) => EvalValue::err(ErrorKind::Ref),
    }
}

fn address(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let (r, c) = match (
        args.first().map(|e| ev.num(e)),
        args.get(1).map(|e| ev.num(e)),
    ) {
        (Some(Ok(r)), Some(Ok(c))) => (r.trunc(), c.trunc()),
        (Some(Err(e)), _) | (_, Some(Err(e))) => return EvalValue::err(e),
        _ => return EvalValue::err(ErrorKind::Value),
    };
    let abs = match ev.num_or(args, 2, 1.0) {
        Ok(a) => a.trunc() as i32,
        Err(e) => return EvalValue::err(e),
    };
    let a1 = match ev.bool_or(args, 3, true) {
        Ok(b) => b,
        Err(e) => return EvalValue::err(e),
    };
    if r < 1.0
        || c < 1.0
        || r > f64::from(MAX_ROWS)
        || c > f64::from(MAX_COLS)
        || !(1..=4).contains(&abs)
    {
        return EvalValue::err(ErrorKind::Value);
    }
    let (ra, ca) = (abs == 1 || abs == 2, abs == 1 || abs == 3);
    let body = if a1 {
        format!(
            "{}{}{}{}",
            if ca { "$" } else { "" },
            col_to_letters(c as u32 - 1),
            if ra { "$" } else { "" },
            r as u32
        )
    } else {
        format!(
            "R{}C{}",
            if ra {
                format!("{}", r as u32)
            } else {
                format!("[{}]", r as u32)
            },
            if ca {
                format!("{}", c as u32)
            } else {
                format!("[{}]", c as u32)
            }
        )
    };
    match args.get(4) {
        Some(e) if !matches!(e, Expr::Missing) => match ev.text(e) {
            Ok(s) => EvalValue::text(format!("{}!{}", quote_sheet_name(&s), body)),
            Err(e) => EvalValue::err(e),
        },
        _ => EvalValue::text(body),
    }
}

fn indirect(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let Some(a) = args.first() else {
        return EvalValue::err(ErrorKind::Value);
    };
    let s = match ev.text(a) {
        Ok(s) => s,
        Err(e) => return EvalValue::err(e),
    };
    let a1 = match ev.bool_or(args, 1, true) {
        Ok(b) => b,
        Err(e) => return EvalValue::err(e),
    };
    if !a1 {
        return EvalValue::err(ErrorKind::Ref);
    }
    let (sheet, body) = split_sheet_prefix(s.trim());
    let sheet_idx = match sheet {
        Some(n) => match ev.host.sheet_index(&n) {
            Some(i) => i,
            None => return EvalValue::err(ErrorKind::Ref),
        },
        None => ev.sheet,
    };
    match CellRange::parse(body) {
        Ok(rg) => EvalValue::Ref(SheetRange {
            sheet: sheet_idx,
            range: rg,
        }),
        Err(_) => match ev.host.defined_name(body, ev.sheet) {
            Some(expr) => ev.eval(&expr),
            None => EvalValue::err(ErrorKind::Ref),
        },
    }
}

fn offset(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let Some(a) = args.first() else {
        return EvalValue::err(ErrorKind::Value);
    };
    let EvalValue::Ref(base) = ev.eval(a) else {
        return EvalValue::err(ErrorKind::Value);
    };
    let (dr, dc) = match (ev.num_or(args, 1, 0.0), ev.num_or(args, 2, 0.0)) {
        (Ok(r), Ok(c)) => (r.trunc() as i64, c.trunc() as i64),
        (Err(e), _) | (_, Err(e)) => return EvalValue::err(e),
    };
    let (h, w) = match (
        ev.num_or(args, 3, f64::from(base.range.rows())),
        ev.num_or(args, 4, f64::from(base.range.cols())),
    ) {
        (Ok(h), Ok(w)) => (h.trunc() as i64, w.trunc() as i64),
        (Err(e), _) | (_, Err(e)) => return EvalValue::err(e),
    };
    if h == 0 || w == 0 {
        return EvalValue::err(ErrorKind::Ref);
    }
    let Some(start) = base.range.start.offset(dr, dc) else {
        return EvalValue::err(ErrorKind::Ref);
    };
    let Some(end) = start.offset(h - h.signum(), w - w.signum()) else {
        return EvalValue::err(ErrorKind::Ref);
    };
    EvalValue::Ref(SheetRange {
        sheet: base.sheet,
        range: CellRange::new(start, end),
    })
}

fn lookup(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let Some(t) = args.first() else {
        return EvalValue::err(ErrorKind::Value);
    };
    let target = ev.eval_scalar(t);
    let look = match arg_array(ev, args, 1) {
        Ok(a) => a,
        Err(e) => return EvalValue::err(e),
    };
    let (keys, result): (Vec<Value>, Vec<Value>) = match args.get(2) {
        Some(_) => {
            let res = match arg_array(ev, args, 2) {
                Ok(a) => a,
                Err(e) => return EvalValue::err(e),
            };
            (look.data.clone(), res.data)
        }
        None => {
            // Array form: search first row/col of the larger dimension, return last row/col.
            if look.cols > look.rows {
                (
                    look.row(0).cloned().collect(),
                    look.row(look.rows.saturating_sub(1)).cloned().collect(),
                )
            } else {
                (
                    look.col(0).cloned().collect(),
                    look.col(look.cols.saturating_sub(1)).cloned().collect(),
                )
            }
        }
    };
    match find_pos(&keys, &target, 1, false) {
        Some(p) => EvalValue::Scalar(
            result
                .get(p)
                .cloned()
                .unwrap_or(Value::Error(ErrorKind::NA)),
        ),
        None => EvalValue::err(ErrorKind::NA),
    }
}

pub fn register(r: &mut Registry) {
    r.add("VLOOKUP", 3, 4, |ev, a| vhlookup(ev, a, true));
    r.add("HLOOKUP", 3, 4, |ev, a| vhlookup(ev, a, false));
    r.add("MATCH", 2, 3, match_fn);
    r.add("XMATCH", 2, 4, xmatch);
    r.add("XLOOKUP", 3, 6, xlookup);
    r.add("INDEX", 1, 4, index);
    r.add("CHOOSE", 2, 255, choose);
    r.add("ROW", 0, 1, |ev, a| row_col(ev, a, true));
    r.add("COLUMN", 0, 1, |ev, a| row_col(ev, a, false));
    r.add("ROWS", 1, 1, |ev, a| rows_cols(ev, a, true));
    r.add("COLUMNS", 1, 1, |ev, a| rows_cols(ev, a, false));
    r.add("ADDRESS", 2, 5, address);
    r.add("INDIRECT", 1, 2, indirect);
    r.add("OFFSET", 3, 5, offset);
    r.add("LOOKUP", 2, 3, lookup);
    r.add("TRANSPOSE", 1, 1, |ev, a| match arg_array(ev, a, 0) {
        Ok(arr) => EvalValue::Array(arr.transpose()),
        Err(e) => EvalValue::err(e),
    });
    r.add("FORMULATEXT", 1, 1, |ev, a| {
        match a.first().map(|e| ev.eval(e)) {
            Some(EvalValue::Ref(r)) => match ev.host.cell_formula(r.sheet, r.range.start) {
                Some(f) => EvalValue::text(format!("={f}")),
                None => EvalValue::err(ErrorKind::NA),
            },
            _ => EvalValue::err(ErrorKind::NA),
        }
    });
    r.add("AREAS", 1, 1, |ev, a| match a.first().map(|e| ev.eval(e)) {
        Some(EvalValue::Ref(_)) => EvalValue::num(1.0),
        Some(EvalValue::Refs(rs)) => EvalValue::num(rs.len() as f64),
        _ => EvalValue::err(ErrorKind::Value),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approximate_match() {
        let items: Vec<Value> = [1.0, 3.0, 5.0, 7.0]
            .iter()
            .map(|n| Value::Number(*n))
            .collect();
        assert_eq!(find_pos(&items, &Value::Number(4.0), 1, false), Some(1));
        assert_eq!(find_pos(&items, &Value::Number(0.5), 1, false), None);
        assert_eq!(find_pos(&items, &Value::Number(9.0), 1, false), Some(3));
        assert_eq!(find_pos(&items, &Value::Number(5.0), 0, false), Some(2));
    }
}
