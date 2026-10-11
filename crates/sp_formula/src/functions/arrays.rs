//! Dynamic-array functions: FILTER, SORT, SORTBY, UNIQUE, TAKE, DROP, VSTACK, HSTACK, TOCOL,
//! TOROW, WRAPROWS, WRAPCOLS, CHOOSECOLS, CHOOSEROWS, EXPAND, RANDARRAY, TRIMRANGE,
//! ARRAYTOTEXT, VALUETOTEXT and the matrix functions MUNIT, MINVERSE, MDETERM.

use super::{Registry, lift1};
use crate::ast::Expr;
use crate::eval::{Array2, Ev, EvalValue, SheetRange, compare, num_value, to_bool, to_number};
use sp_core::{CellAddr, CellRange, ErrorKind, Value};
use std::cmp::Ordering;

/// Functions whose result can be a multi-cell array for single-valued arguments (used to
/// decide where legacy formulas need implicit intersection).
pub const ARRAY_RESULT: &[&str] = &[
    "FILTER",
    "SORT",
    "SORTBY",
    "UNIQUE",
    "TAKE",
    "DROP",
    "VSTACK",
    "HSTACK",
    "TOCOL",
    "TOROW",
    "WRAPROWS",
    "WRAPCOLS",
    "CHOOSECOLS",
    "CHOOSEROWS",
    "EXPAND",
    "RANDARRAY",
    "TRIMRANGE",
    "MUNIT",
    "MINVERSE",
    "SEQUENCE",
    "TRANSPOSE",
    "MMULT",
    "FREQUENCY",
    "LINEST",
    "LOGEST",
    "TREND",
    "GROWTH",
    "TEXTSPLIT",
    "MAP",
    "SCAN",
    "BYROW",
    "BYCOL",
    "MAKEARRAY",
    "REDUCE",
    "LAMBDA",
    "REGEXEXTRACT",
];

/// Largest array these functions build.
pub const MAX_ARRAY_CELLS: usize = 4_000_000;

type R<T> = Result<T, ErrorKind>;
/// Calls its argument with the (row, col) of every non-empty value.
type Visit<'a> = dyn Fn(&mut dyn FnMut(usize, usize)) + 'a;

fn done(r: R<Array2>) -> EvalValue {
    match r {
        Ok(a) if a.rows == 0 || a.cols == 0 => EvalValue::err(ErrorKind::Calc),
        Ok(a) if a.rows * a.cols == 1 => {
            EvalValue::Scalar(a.data.into_iter().next().unwrap_or(Value::Empty))
        }
        Ok(a) => EvalValue::Array(a),
        Err(e) => EvalValue::err(e),
    }
}

fn arr(ev: &mut Ev<'_>, args: &[Expr], i: usize) -> R<Array2> {
    match args.get(i) {
        Some(Expr::Missing) | None => Err(ErrorKind::Value),
        Some(e) => {
            let v = ev.eval(e);
            if let Some(err) = v.as_error() {
                return Err(err);
            }
            ev.to_array(v)
        }
    }
}

fn int_or(ev: &mut Ev<'_>, args: &[Expr], i: usize, d: i64) -> R<i64> {
    let n = ev.num_or(args, i, d as f64)?;
    if !n.is_finite() || n.abs() > 1e15 {
        return Err(ErrorKind::Value);
    }
    Ok(n.trunc() as i64)
}

fn given(args: &[Expr], i: usize) -> bool {
    !matches!(args.get(i), None | Some(Expr::Missing))
}

fn check_size(rows: usize, cols: usize) -> R<()> {
    if rows.saturating_mul(cols) > MAX_ARRAY_CELLS {
        Err(ErrorKind::Num)
    } else {
        Ok(())
    }
}

fn rows_of(a: &Array2) -> Vec<Vec<Value>> {
    (0..a.rows).map(|r| a.row(r).cloned().collect()).collect()
}

fn from_rows(rows: Vec<Vec<Value>>) -> Array2 {
    let cols = rows.first().map_or(0, Vec::len);
    let n = rows.len();
    Array2::new(n, cols, rows.into_iter().flatten().collect())
}

// ── FILTER ────────────────────────────────────────────────────────────────────────────────

fn filter(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let r = (|| -> R<Option<Array2>> {
        let a = arr(ev, args, 0)?;
        let inc = arr(ev, args, 1)?;
        let by_rows = if inc.cols == 1 && inc.rows == a.rows {
            true
        } else if inc.rows == 1 && inc.cols == a.cols {
            false
        } else {
            return Err(ErrorKind::Value);
        };
        let mut keep = Vec::with_capacity(inc.data.len());
        for v in &inc.data {
            keep.push(match v {
                Value::Error(e) => return Err(*e),
                Value::Text(_) => return Err(ErrorKind::Value),
                other => to_bool(other)?,
            });
        }
        if !keep.iter().any(|k| *k) {
            return Ok(None);
        }
        let t = if by_rows { a } else { a.transpose() };
        let rows: Vec<Vec<Value>> = rows_of(&t)
            .into_iter()
            .zip(&keep)
            .filter(|(_, k)| **k)
            .map(|(r, _)| r)
            .collect();
        let out = from_rows(rows);
        Ok(Some(if by_rows { out } else { out.transpose() }))
    })();
    match r {
        Ok(Some(a)) => done(Ok(a)),
        Ok(None) if given(args, 2) => match args.get(2) {
            Some(e) => {
                let v = ev.eval(e);
                done(ev.to_array(v))
            }
            None => EvalValue::err(ErrorKind::Calc),
        },
        Ok(None) => EvalValue::err(ErrorKind::Calc),
        Err(e) => EvalValue::err(e),
    }
}

// ── SORT / SORTBY ─────────────────────────────────────────────────────────────────────────

/// Sort order of two values: numbers < text < logicals < errors, blanks always last.
fn sort_cmp(a: &Value, b: &Value, desc: bool) -> Ordering {
    match (a.is_empty(), b.is_empty()) {
        (true, true) => return Ordering::Equal,
        (true, false) => return Ordering::Greater,
        (false, true) => return Ordering::Less,
        _ => {}
    }
    let o = match (a, b) {
        (Value::Error(x), Value::Error(y)) => x.type_code().total_cmp(&y.type_code()),
        _ if a.type_rank() != b.type_rank() => a.type_rank().cmp(&b.type_rank()),
        _ => compare(a, b),
    };
    if desc { o.reverse() } else { o }
}

fn order_arg(ev: &mut Ev<'_>, args: &[Expr], i: usize) -> R<bool> {
    match int_or(ev, args, i, 1)? {
        1 => Ok(false),
        -1 => Ok(true),
        _ => Err(ErrorKind::Value),
    }
}

fn nums_arg(ev: &mut Ev<'_>, args: &[Expr], i: usize, d: f64) -> R<Vec<f64>> {
    if !given(args, i) {
        return Ok(vec![d]);
    }
    let a = arr(ev, args, i)?;
    a.data.iter().map(to_number).collect()
}

fn sort(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    done((|| {
        let a = arr(ev, args, 0)?;
        let idx = nums_arg(ev, args, 1, 1.0)?;
        let ord = nums_arg(ev, args, 2, 1.0)?;
        let by_col = ev.bool_or(args, 3, false)?;
        let t = if by_col { a.transpose() } else { a };
        let mut keys = Vec::new();
        for (k, i) in idx.iter().enumerate() {
            let i = i.trunc();
            if i < 1.0 || i > t.cols as f64 {
                return Err(ErrorKind::Value);
            }
            let o = ord.get(k).or(ord.last()).copied().unwrap_or(1.0).trunc();
            let desc = match o as i64 {
                1 => false,
                -1 => true,
                _ => return Err(ErrorKind::Value),
            };
            keys.push((i as usize - 1, desc));
        }
        let mut rows = rows_of(&t);
        rows.sort_by(|x, y| {
            for &(c, desc) in &keys {
                let o = sort_cmp(
                    x.get(c).unwrap_or(&Value::Empty),
                    y.get(c).unwrap_or(&Value::Empty),
                    desc,
                );
                if o != Ordering::Equal {
                    return o;
                }
            }
            Ordering::Equal
        });
        let out = from_rows(rows);
        Ok(if by_col { out.transpose() } else { out })
    })())
}

fn sortby(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    done((|| {
        let a = arr(ev, args, 0)?;
        let mut keys: Vec<(Vec<Value>, bool)> = Vec::new();
        let mut by_rows: Option<bool> = None;
        let mut i = 1;
        while i < args.len() {
            let by = arr(ev, args, i)?;
            let desc = order_arg(ev, args, i + 1)?;
            let rows_dir = if by.cols == 1 && by.rows == a.rows {
                true
            } else if by.rows == 1 && by.cols == a.cols {
                false
            } else {
                return Err(ErrorKind::Value);
            };
            if by_rows.is_some_and(|d| d != rows_dir) {
                return Err(ErrorKind::Value);
            }
            by_rows = Some(rows_dir);
            keys.push((by.data, desc));
            i += 2;
        }
        let by_rows = by_rows.unwrap_or(true);
        let t = if by_rows { a } else { a.transpose() };
        let rows = rows_of(&t);
        let mut perm: Vec<usize> = (0..rows.len()).collect();
        perm.sort_by(|&x, &y| {
            for (k, desc) in &keys {
                let o = sort_cmp(
                    k.get(x).unwrap_or(&Value::Empty),
                    k.get(y).unwrap_or(&Value::Empty),
                    *desc,
                );
                if o != Ordering::Equal {
                    return o;
                }
            }
            Ordering::Equal
        });
        let sorted: Vec<Vec<Value>> = perm.iter().filter_map(|&p| rows.get(p).cloned()).collect();
        let out = from_rows(sorted);
        Ok(if by_rows { out } else { out.transpose() })
    })())
}

// ── UNIQUE ────────────────────────────────────────────────────────────────────────────────

fn same(a: &[Value], b: &[Value]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| match (x, y) {
            (Value::Text(p), Value::Text(q)) => p.to_lowercase() == q.to_lowercase(),
            _ => x == y,
        })
}

fn unique(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    done((|| {
        let a = arr(ev, args, 0)?;
        let by_col = ev.bool_or(args, 1, false)?;
        let once = ev.bool_or(args, 2, false)?;
        let t = if by_col { a.transpose() } else { a };
        let rows = rows_of(&t);
        let mut groups: Vec<(Vec<Value>, usize)> = Vec::new();
        for r in rows {
            match groups.iter_mut().find(|(g, _)| same(g, &r)) {
                Some(g) => g.1 += 1,
                None => groups.push((r, 1)),
            }
        }
        let out = from_rows(
            groups
                .into_iter()
                .filter(|(_, n)| !once || *n == 1)
                .map(|(r, _)| r)
                .collect(),
        );
        Ok(if by_col { out.transpose() } else { out })
    })())
}

// ── TAKE / DROP / EXPAND / CHOOSE* ────────────────────────────────────────────────────────

fn slice(a: &Array2, r0: usize, r1: usize, c0: usize, c1: usize) -> Array2 {
    let mut data = Vec::with_capacity(r1.saturating_sub(r0) * c1.saturating_sub(c0));
    for r in r0..r1 {
        for c in c0..c1 {
            data.push(a.get(r, c).clone());
        }
    }
    Array2::new(r1.saturating_sub(r0), c1.saturating_sub(c0), data)
}

fn take_range(n: Option<i64>, len: usize, take: bool) -> (usize, usize) {
    let Some(n) = n else { return (0, len) };
    let k = (n.unsigned_abs() as usize).min(len);
    match (take, n >= 0) {
        (true, true) => (0, k),
        (true, false) => (len - k, len),
        (false, true) => (k, len),
        (false, false) => (0, len - k),
    }
}

fn take_drop(ev: &mut Ev<'_>, args: &[Expr], take: bool) -> EvalValue {
    done((|| {
        let a = arr(ev, args, 0)?;
        let rn = if given(args, 1) {
            Some(int_or(ev, args, 1, 0)?)
        } else {
            None
        };
        let cn = if given(args, 2) {
            Some(int_or(ev, args, 2, 0)?)
        } else {
            None
        };
        if take && (rn == Some(0) || cn == Some(0)) {
            return Err(ErrorKind::Calc);
        }
        let (r0, r1) = take_range(rn, a.rows, take);
        let (c0, c1) = take_range(cn, a.cols, take);
        Ok(slice(&a, r0, r1, c0, c1))
    })())
}

fn expand(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    done((|| {
        let a = arr(ev, args, 0)?;
        let rows = if given(args, 1) {
            int_or(ev, args, 1, 0)?
        } else {
            a.rows as i64
        };
        let cols = if given(args, 2) {
            int_or(ev, args, 2, 0)?
        } else {
            a.cols as i64
        };
        let pad = match args.get(3) {
            Some(e) if given(args, 3) => ev.eval_scalar(e),
            _ => Value::Error(ErrorKind::NA),
        };
        if rows < a.rows as i64 || cols < a.cols as i64 {
            return Err(ErrorKind::Value);
        }
        let (rows, cols) = (rows as usize, cols as usize);
        check_size(rows, cols)?;
        let mut data = Vec::with_capacity(rows * cols);
        for r in 0..rows {
            for c in 0..cols {
                data.push(if r < a.rows && c < a.cols {
                    a.get(r, c).clone()
                } else {
                    pad.clone()
                });
            }
        }
        Ok(Array2::new(rows, cols, data))
    })())
}

fn choose_lines(ev: &mut Ev<'_>, args: &[Expr], cols: bool) -> EvalValue {
    done((|| {
        let a = arr(ev, args, 0)?;
        let t = if cols { a.transpose() } else { a };
        let rows = rows_of(&t);
        let mut out = Vec::new();
        for i in 1..args.len() {
            let ns = arr(ev, args, i)?;
            for v in &ns.data {
                let n = to_number(v)?.trunc();
                let len = rows.len() as f64;
                let idx = if n >= 1.0 && n <= len {
                    n as usize - 1
                } else if n <= -1.0 && -n <= len {
                    (len + n) as usize
                } else {
                    return Err(ErrorKind::Value);
                };
                out.push(rows.get(idx).cloned().unwrap_or_default());
            }
        }
        check_size(out.len(), t.cols)?;
        let res = from_rows(out);
        Ok(if cols { res.transpose() } else { res })
    })())
}

// ── stacking / reshaping ──────────────────────────────────────────────────────────────────

fn stack(ev: &mut Ev<'_>, args: &[Expr], vertical: bool) -> EvalValue {
    done((|| {
        let mut parts = Vec::new();
        for i in 0..args.len() {
            let a = arr(ev, args, i)?;
            parts.push(if vertical { a } else { a.transpose() });
        }
        let width = parts.iter().map(|p| p.cols).max().unwrap_or(0);
        let height: usize = parts.iter().map(|p| p.rows).sum();
        check_size(height, width)?;
        let mut rows = Vec::with_capacity(height);
        for p in &parts {
            for r in 0..p.rows {
                rows.push(
                    (0..width)
                        .map(|c| {
                            if c < p.cols {
                                p.get(r, c).clone()
                            } else {
                                Value::Error(ErrorKind::NA)
                            }
                        })
                        .collect(),
                );
            }
        }
        let out = from_rows(rows);
        Ok(if vertical { out } else { out.transpose() })
    })())
}

fn to_line(ev: &mut Ev<'_>, args: &[Expr], col: bool) -> EvalValue {
    done((|| {
        let a = arr(ev, args, 0)?;
        let ignore = int_or(ev, args, 1, 0)?;
        if !(0..=3).contains(&ignore) {
            return Err(ErrorKind::Value);
        }
        let by_col = ev.bool_or(args, 2, false)?;
        let t = if by_col { a.transpose() } else { a };
        let vals: Vec<Value> = t
            .data
            .into_iter()
            .filter(|v| {
                !((ignore & 1 == 1 && v.is_empty())
                    || (ignore & 2 == 2 && matches!(v, Value::Error(_))))
            })
            .collect();
        let n = vals.len();
        Ok(if col {
            Array2::new(n, 1, vals)
        } else {
            Array2::new(1, n, vals)
        })
    })())
}

fn wrap(ev: &mut Ev<'_>, args: &[Expr], rows: bool) -> EvalValue {
    done((|| {
        let a = arr(ev, args, 0)?;
        if a.rows != 1 && a.cols != 1 {
            return Err(ErrorKind::Value);
        }
        let n = int_or(ev, args, 1, 0)?;
        if n < 1 {
            return Err(ErrorKind::Num);
        }
        let pad = match args.get(2) {
            Some(e) if given(args, 2) => ev.eval_scalar(e),
            _ => Value::Error(ErrorKind::NA),
        };
        let n = (n as usize).min(MAX_ARRAY_CELLS);
        let len = a.data.len();
        let lines = len.div_ceil(n);
        let mut data = Vec::with_capacity(lines * n);
        for i in 0..lines * n {
            data.push(a.data.get(i).cloned().unwrap_or_else(|| pad.clone()));
        }
        let out = Array2::new(lines, n, data);
        Ok(if rows { out } else { out.transpose() })
    })())
}

// ── RANDARRAY / TRIMRANGE ─────────────────────────────────────────────────────────────────

fn randarray(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    done((|| {
        let rows = int_or(ev, args, 0, 1)?;
        let cols = int_or(ev, args, 1, 1)?;
        let lo = ev.num_or(args, 2, 0.0)?;
        let hi = ev.num_or(args, 3, 1.0)?;
        let int = ev.bool_or(args, 4, false)?;
        if rows < 0 || cols < 0 || lo > hi || (int && (lo.fract() != 0.0 || hi.fract() != 0.0)) {
            return Err(ErrorKind::Value);
        }
        if rows == 0 || cols == 0 {
            return Err(ErrorKind::Calc);
        }
        let (rows, cols) = (rows as usize, cols as usize);
        check_size(rows, cols)?;
        let data = (0..rows * cols)
            .map(|_| {
                let r = ev.host.random();
                num_value(if int {
                    (lo + (r * (hi - lo + 1.0)).floor()).min(hi)
                } else {
                    lo + r * (hi - lo)
                })
            })
            .collect();
        Ok(Array2::new(rows, cols, data))
    })())
}

fn trimrange(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let Some(e) = args.first() else {
        return EvalValue::err(ErrorKind::Value);
    };
    let v = ev.eval(e);
    let (rows_mode, cols_mode) = match (int_or(ev, args, 1, 3), int_or(ev, args, 2, 3)) {
        (Ok(a), Ok(b)) if (0..=3).contains(&a) && (0..=3).contains(&b) => (a, b),
        (Err(e), _) | (_, Err(e)) => return EvalValue::err(e),
        _ => return EvalValue::err(ErrorKind::Value),
    };
    // Bounding box of non-empty cells, relative to the area.
    let bbox = |rows: usize, cols: usize, nonempty: &Visit<'_>| {
        let (mut r0, mut r1, mut c0, mut c1) = (usize::MAX, 0, usize::MAX, 0);
        nonempty(&mut |r, c| {
            r0 = r0.min(r);
            r1 = r1.max(r);
            c0 = c0.min(c);
            c1 = c1.max(c);
        });
        if r0 == usize::MAX {
            return None;
        }
        let (r0, r1) = (
            if rows_mode & 1 == 1 { r0 } else { 0 },
            if rows_mode & 2 == 2 { r1 } else { rows - 1 },
        );
        let (c0, c1) = (
            if cols_mode & 1 == 1 { c0 } else { 0 },
            if cols_mode & 2 == 2 { c1 } else { cols - 1 },
        );
        Some((r0, r1, c0, c1))
    };
    match v {
        EvalValue::Ref(r) => {
            let rg = ev.clamp_to_used(r);
            let host = ev.host;
            let b = bbox(rg.rows() as usize, rg.cols() as usize, &|f| {
                host.for_each_stored(r.sheet, rg, &mut |a, v| {
                    if !v.is_empty() {
                        f(
                            (a.row - rg.start.row) as usize,
                            (a.col - rg.start.col) as usize,
                        );
                    }
                })
            });
            match b {
                None => EvalValue::err(ErrorKind::Ref),
                Some((r0, r1, c0, c1)) => EvalValue::Ref(SheetRange {
                    sheet: r.sheet,
                    range: CellRange::new(
                        CellAddr::new(rg.start.row + r0 as u32, rg.start.col + c0 as u32),
                        CellAddr::new(rg.start.row + r1 as u32, rg.start.col + c1 as u32),
                    ),
                }),
            }
        }
        other => {
            let a = match ev.to_array(other) {
                Ok(a) => a,
                Err(e) => return EvalValue::err(e),
            };
            let b = bbox(a.rows, a.cols, &|f| {
                for r in 0..a.rows {
                    for c in 0..a.cols {
                        if !a.get(r, c).is_empty() {
                            f(r, c);
                        }
                    }
                }
            });
            match b {
                None => EvalValue::err(ErrorKind::Ref),
                Some((r0, r1, c0, c1)) => done(Ok(slice(&a, r0, r1 + 1, c0, c1 + 1))),
            }
        }
    }
}

// ── text ──────────────────────────────────────────────────────────────────────────────────

fn value_text(v: &Value, strict: bool) -> String {
    match v {
        Value::Text(s) if strict => format!("\"{}\"", s.replace('"', "\"\"")),
        other => other.to_display(),
    }
}

fn format_arg(ev: &mut Ev<'_>, args: &[Expr]) -> R<bool> {
    match int_or(ev, args, 1, 0)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(ErrorKind::Value),
    }
}

fn arraytotext(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let r = (|| -> R<String> {
        let a = arr(ev, args, 0)?;
        let strict = format_arg(ev, args)?;
        let s = if strict {
            let rows: Vec<String> = (0..a.rows)
                .map(|r| {
                    a.row(r)
                        .map(|v| value_text(v, true))
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .collect();
            format!("{{{}}}", rows.join(";"))
        } else {
            a.data
                .iter()
                .map(|v| value_text(v, false))
                .collect::<Vec<_>>()
                .join(", ")
        };
        if s.chars().count() > sp_core::MAX_TEXT_LEN {
            return Err(ErrorKind::Value);
        }
        Ok(s)
    })();
    match r {
        Ok(s) => EvalValue::text(s),
        Err(e) => EvalValue::err(e),
    }
}

fn valuetotext(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let strict = match format_arg(ev, args) {
        Ok(s) => s,
        Err(e) => return EvalValue::err(e),
    };
    let Some(e) = args.first() else {
        return EvalValue::err(ErrorKind::Value);
    };
    let v = ev.eval(e);
    lift1(ev, v, |x| Value::text(value_text(x, strict)))
}

// ── matrices ──────────────────────────────────────────────────────────────────────────────

fn square(ev: &mut Ev<'_>, args: &[Expr]) -> R<(usize, Vec<f64>)> {
    let a = arr(ev, args, 0)?;
    if a.rows != a.cols {
        return Err(ErrorKind::Value);
    }
    let m = a
        .data
        .iter()
        .map(|v| match v {
            Value::Number(n) => Ok(*n),
            Value::Error(e) => Err(*e),
            _ => Err(ErrorKind::Value),
        })
        .collect::<R<Vec<f64>>>()?;
    Ok((a.rows, m))
}

/// LU decomposition with partial pivoting in place; returns the permutation sign, or `None`
/// for a singular matrix.
fn lu(n: usize, m: &mut [f64], perm: &mut [usize]) -> Option<f64> {
    let mut sign = 1.0;
    for k in 0..n {
        let mut p = k;
        let mut best = 0.0;
        for i in k..n {
            let v = m.get(i * n + k).copied().unwrap_or(0.0).abs();
            if v > best {
                best = v;
                p = i;
            }
        }
        if best == 0.0 {
            return None;
        }
        if p != k {
            for j in 0..n {
                m.swap(k * n + j, p * n + j);
            }
            perm.swap(k, p);
            sign = -sign;
        }
        let piv = m.get(k * n + k).copied().unwrap_or(1.0);
        for i in k + 1..n {
            let f = m.get(i * n + k).copied().unwrap_or(0.0) / piv;
            if let Some(x) = m.get_mut(i * n + k) {
                *x = f;
            }
            for j in k + 1..n {
                let u = m.get(k * n + j).copied().unwrap_or(0.0);
                if let Some(x) = m.get_mut(i * n + j) {
                    *x -= f * u;
                }
            }
        }
    }
    Some(sign)
}

fn mdeterm(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    match square(ev, args) {
        Err(e) => EvalValue::err(e),
        Ok((n, mut m)) => {
            let mut perm: Vec<usize> = (0..n).collect();
            match lu(n, &mut m, &mut perm) {
                None => EvalValue::num(0.0),
                Some(sign) => EvalValue::num(
                    (0..n).fold(sign, |d, i| d * m.get(i * n + i).copied().unwrap_or(0.0)),
                ),
            }
        }
    }
}

fn minverse(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    done((|| {
        let (n, mut m) = square(ev, args)?;
        let mut perm: Vec<usize> = (0..n).collect();
        lu(n, &mut m, &mut perm).ok_or(ErrorKind::Num)?;
        let g = |m: &[f64], i: usize, j: usize| m.get(i * n + j).copied().unwrap_or(0.0);
        let mut inv = vec![0.0; n * n];
        for col in 0..n {
            // Solve L U x = P e_col.
            let mut y: Vec<f64> = perm
                .iter()
                .map(|&p| if p == col { 1.0 } else { 0.0 })
                .collect();
            for i in 0..n {
                let mut s = y.get(i).copied().unwrap_or(0.0);
                for k in 0..i {
                    s -= g(&m, i, k) * y.get(k).copied().unwrap_or(0.0);
                }
                if let Some(v) = y.get_mut(i) {
                    *v = s;
                }
            }
            for i in (0..n).rev() {
                let mut s = y.get(i).copied().unwrap_or(0.0);
                for k in i + 1..n {
                    s -= g(&m, i, k) * y.get(k).copied().unwrap_or(0.0);
                }
                let d = g(&m, i, i);
                if let Some(v) = y.get_mut(i) {
                    *v = s / d;
                }
            }
            for (i, v) in y.iter().enumerate() {
                if let Some(x) = inv.get_mut(i * n + col) {
                    *x = *v;
                }
            }
        }
        Ok(Array2::new(n, n, inv.into_iter().map(num_value).collect()))
    })())
}

fn munit(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    done((|| {
        let n = int_or(ev, args, 0, 1)?;
        if n < 1 {
            return Err(ErrorKind::Value);
        }
        let n = n as usize;
        check_size(n, n)?;
        let data = (0..n * n)
            .map(|k| Value::Number(if k / n == k % n { 1.0 } else { 0.0 }))
            .collect();
        Ok(Array2::new(n, n, data))
    })())
}

pub fn register(r: &mut Registry) {
    r.add("FILTER", 2, 3, filter);
    r.add("SORT", 1, 4, sort);
    r.add("SORTBY", 2, 255, sortby);
    r.add("UNIQUE", 1, 3, unique);
    r.add("TAKE", 2, 3, |ev, a| take_drop(ev, a, true));
    r.add("DROP", 2, 3, |ev, a| take_drop(ev, a, false));
    r.add("EXPAND", 2, 4, expand);
    r.add("CHOOSECOLS", 2, 255, |ev, a| choose_lines(ev, a, true));
    r.add("CHOOSEROWS", 2, 255, |ev, a| choose_lines(ev, a, false));
    r.add("VSTACK", 1, 254, |ev, a| stack(ev, a, true));
    r.add("HSTACK", 1, 254, |ev, a| stack(ev, a, false));
    r.add("TOCOL", 1, 3, |ev, a| to_line(ev, a, true));
    r.add("TOROW", 1, 3, |ev, a| to_line(ev, a, false));
    r.add("WRAPROWS", 2, 3, |ev, a| wrap(ev, a, true));
    r.add("WRAPCOLS", 2, 3, |ev, a| wrap(ev, a, false));
    r.add("RANDARRAY", 0, 5, randarray);
    r.add("TRIMRANGE", 1, 3, trimrange);
    r.add("ARRAYTOTEXT", 1, 2, arraytotext);
    r.add("VALUETOTEXT", 1, 2, valuetotext);
    r.add("MUNIT", 1, 1, munit);
    r.add("MINVERSE", 1, 1, minverse);
    r.add("MDETERM", 1, 1, mdeterm);
}
