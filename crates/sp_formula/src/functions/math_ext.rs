//! Math long tail: AGGREGATE, matrix functions (MDETERM, MINVERSE, MUNIT) and SERIESSUM.

use super::{Registry, ret};
use crate::ast::Expr;
use crate::eval::{Array2, Ev, EvalValue};
use sp_core::{ErrorKind, Value};

type R<T> = Result<T, ErrorKind>;

/// Largest matrix side accepted by the matrix functions.
const MAX_MATRIX: usize = 512;

fn square_matrix(ev: &mut Ev<'_>, args: &[Expr]) -> R<(usize, Vec<f64>)> {
    let a = super::arg_array(ev, args, 0)?;
    if a.rows != a.cols || a.rows == 0 || a.rows > MAX_MATRIX {
        return Err(ErrorKind::Value);
    }
    let mut m = Vec::with_capacity(a.data.len());
    for v in &a.data {
        match v {
            Value::Number(n) => m.push(*n),
            Value::Error(e) => return Err(*e),
            _ => return Err(ErrorKind::Value),
        }
    }
    Ok((a.rows, m))
}

/// Gaussian elimination with partial pivoting. Returns the determinant and, when
/// `inverse` is set, the inverse (row-major).
fn eliminate(n: usize, mut a: Vec<f64>, inverse: bool) -> R<(f64, Option<Vec<f64>>)> {
    let mut inv: Vec<f64> = if inverse {
        (0..n * n).map(|i| if i / n == i % n { 1.0 } else { 0.0 }).collect()
    } else {
        Vec::new()
    };
    let at = |r: usize, c: usize| r * n + c;
    let mut det = 1.0;
    for col in 0..n {
        let mut piv = col;
        let mut best = 0.0;
        for r in col..n {
            let v = a.get(at(r, col)).copied().unwrap_or(0.0).abs();
            if v > best {
                best = v;
                piv = r;
            }
        }
        if best == 0.0 {
            return Ok((0.0, None));
        }
        if piv != col {
            for c in 0..n {
                a.swap(at(piv, c), at(col, c));
                if inverse {
                    inv.swap(at(piv, c), at(col, c));
                }
            }
            det = -det;
        }
        let p = a.get(at(col, col)).copied().unwrap_or(0.0);
        det *= p;
        for c in 0..n {
            if let Some(x) = a.get_mut(at(col, c)) {
                *x /= p;
            }
            if inverse && let Some(x) = inv.get_mut(at(col, c)) {
                *x /= p;
            }
        }
        for r in 0..n {
            if r == col {
                continue;
            }
            let f = a.get(at(r, col)).copied().unwrap_or(0.0);
            if f == 0.0 {
                continue;
            }
            for c in 0..n {
                let s = a.get(at(col, c)).copied().unwrap_or(0.0);
                if let Some(x) = a.get_mut(at(r, c)) {
                    *x -= f * s;
                }
                if inverse {
                    let s = inv.get(at(col, c)).copied().unwrap_or(0.0);
                    if let Some(x) = inv.get_mut(at(r, c)) {
                        *x -= f * s;
                    }
                }
            }
        }
    }
    if !det.is_finite() {
        return Err(ErrorKind::Num);
    }
    Ok((det, inverse.then_some(inv)))
}

fn mdeterm(ev: &mut Ev<'_>, args: &[Expr]) -> R<f64> {
    let (n, m) = square_matrix(ev, args)?;
    Ok(eliminate(n, m, false)?.0)
}

fn minverse(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let (n, m) = match square_matrix(ev, args) {
        Ok(x) => x,
        Err(e) => return EvalValue::err(e),
    };
    match eliminate(n, m, true) {
        Ok((d, Some(inv))) if d != 0.0 && inv.iter().all(|x| x.is_finite()) => EvalValue::Array(
            Array2::new(n, n, inv.into_iter().map(crate::eval::num_value).collect()),
        ),
        Ok(_) => EvalValue::err(ErrorKind::Num),
        Err(e) => EvalValue::err(e),
    }
}

fn munit(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let n = match args.first().map(|a| ev.num(a)) {
        Some(Ok(n)) => n.trunc(),
        Some(Err(e)) => return EvalValue::err(e),
        None => return EvalValue::err(ErrorKind::Value),
    };
    if n < 1.0 {
        return EvalValue::err(ErrorKind::Value);
    }
    if n > 4096.0 {
        return EvalValue::err(ErrorKind::Num);
    }
    let n = n as usize;
    let data = (0..n * n)
        .map(|i| Value::Number(if i / n == i % n { 1.0 } else { 0.0 }))
        .collect();
    EvalValue::Array(Array2::new(n, n, data))
}

fn seriessum(ev: &mut Ev<'_>, args: &[Expr]) -> R<f64> {
    let mut nums = [0.0; 3];
    for (i, slot) in nums.iter_mut().enumerate() {
        let e = args.get(i).ok_or(ErrorKind::Value)?;
        *slot = match ev.eval_scalar(e) {
            Value::Bool(_) => return Err(ErrorKind::Value),
            v => crate::eval::to_number(&v)?,
        };
    }
    let [x, n, m] = nums;
    let coef = super::arg_array(ev, args, 3)?;
    let mut total = 0.0;
    for (i, c) in coef.data.iter().enumerate() {
        let c = match c {
            Value::Number(c) => *c,
            Value::Error(e) => return Err(*e),
            _ => return Err(ErrorKind::Value),
        };
        let p = n + m * i as f64;
        if x == 0.0 && p <= 0.0 {
            return Err(ErrorKind::Num);
        }
        total += c * x.powf(p);
    }
    if total.is_finite() {
        Ok(total)
    } else {
        Err(ErrorKind::Num)
    }
}

// ── AGGREGATE ────────────────────────────────────────────────────────────────────────────

fn mean(v: &[f64]) -> R<f64> {
    if v.is_empty() {
        return Err(ErrorKind::Div0);
    }
    Ok(v.iter().sum::<f64>() / v.len() as f64)
}

fn var(v: &[f64], sample: bool) -> R<f64> {
    let n = v.len() as f64;
    let d = if sample { n - 1.0 } else { n };
    if d <= 0.0 {
        return Err(ErrorKind::Div0);
    }
    let m = mean(v)?;
    Ok(v.iter().map(|x| (x - m) * (x - m)).sum::<f64>() / d)
}

fn sorted(v: &[f64]) -> Vec<f64> {
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    s
}

fn pct_inc(s: &[f64], p: f64) -> R<f64> {
    if s.is_empty() || !(0.0..=1.0).contains(&p) {
        return Err(ErrorKind::Num);
    }
    let h = (s.len() - 1) as f64 * p;
    let lo = h.floor() as usize;
    let a = s.get(lo).copied().ok_or(ErrorKind::Num)?;
    let b = s.get(lo + 1).copied().unwrap_or(a);
    Ok(a + (h - lo as f64) * (b - a))
}

fn pct_exc(s: &[f64], p: f64) -> R<f64> {
    let n = s.len() as f64;
    if s.is_empty() || p <= 0.0 || p >= 1.0 {
        return Err(ErrorKind::Num);
    }
    let h = (n + 1.0) * p;
    if h < 1.0 || h > n {
        return Err(ErrorKind::Num);
    }
    let lo = h.floor() as usize;
    let a = s.get(lo - 1).copied().ok_or(ErrorKind::Num)?;
    let b = s.get(lo).copied().unwrap_or(a);
    Ok(a + (h - lo as f64) * (b - a))
}

fn mode(v: &[f64]) -> R<f64> {
    let mut best: Option<(f64, usize)> = None;
    for x in v {
        let c = v.iter().filter(|y| *y == x).count();
        if c > 1 && best.is_none_or(|(_, bc)| c > bc) {
            best = Some((*x, c));
        }
    }
    best.map(|b| b.0).ok_or(ErrorKind::NA)
}

fn kth(v: &[f64], k: f64, largest: bool) -> R<f64> {
    let s = sorted(v);
    let k = k.ceil();
    if k < 1.0 || k > s.len() as f64 {
        return Err(ErrorKind::Num);
    }
    let i = k as usize - 1;
    let idx = if largest { s.len() - 1 - i } else { i };
    s.get(idx).copied().ok_or(ErrorKind::Num)
}

fn quart(k: f64) -> R<f64> {
    let q = k.trunc();
    if !(0.0..=4.0).contains(&q) {
        return Err(ErrorKind::Num);
    }
    Ok(q / 4.0)
}

/// Collect values of the AGGREGATE references honouring the options.
fn agg_values(ev: &mut Ev<'_>, refs: &[Expr], opt: i64, count_all: bool) -> R<Vec<f64>> {
    let skip_hidden = matches!(opt, 1 | 3 | 5 | 7);
    let skip_errors = matches!(opt, 2 | 3 | 6 | 7);
    let skip_nested = opt <= 3;
    let mut out = Vec::new();
    for e in refs {
        match ev.eval(e) {
            EvalValue::Ref(r) => {
                let rg = ev.clamp_to_used(r);
                let mut cells: Vec<(sp_core::CellAddr, Value)> = Vec::new();
                ev.host.for_each_stored(r.sheet, rg, &mut |a, v| {
                    cells.push((a, v.clone()));
                });
                for (a, v) in cells {
                    if skip_hidden && ev.host.row_hidden(r.sheet, a.row) {
                        continue;
                    }
                    if skip_nested
                        && ev.host.cell_formula(r.sheet, a).is_some_and(|f| {
                            let u = f.to_ascii_uppercase();
                            u.contains("SUBTOTAL(") || u.contains("AGGREGATE(")
                        })
                    {
                        continue;
                    }
                    match v {
                        Value::Number(n) => out.push(n),
                        Value::Error(e) if !skip_errors => return Err(e),
                        Value::Empty => {}
                        _ if count_all => out.push(0.0),
                        _ => {}
                    }
                }
            }
            other => {
                let a = ev.to_array(other)?;
                for v in &a.data {
                    match v {
                        Value::Number(n) => out.push(*n),
                        Value::Error(e) if !skip_errors => return Err(*e),
                        Value::Empty => {}
                        _ if count_all => out.push(0.0),
                        _ => {}
                    }
                }
            }
        }
    }
    Ok(out)
}

fn aggregate(ev: &mut Ev<'_>, args: &[Expr]) -> R<f64> {
    let f = ev.num(args.first().ok_or(ErrorKind::Value)?)?.trunc() as i64;
    let opt = ev.num(args.get(1).ok_or(ErrorKind::Value)?)?.trunc() as i64;
    if !(1..=19).contains(&f) || !(0..=7).contains(&opt) {
        return Err(ErrorKind::Value);
    }
    let rest = args.get(2..).unwrap_or(&[]);
    if rest.is_empty() {
        return Err(ErrorKind::Value);
    }
    if f >= 14 {
        // ref, k
        let (Some(re), Some(ke)) = (rest.first(), rest.get(1)) else {
            return Err(ErrorKind::Value);
        };
        let k = ev.num(ke)?;
        let v = agg_values(ev, std::slice::from_ref(re), opt, false)?;
        let s = sorted(&v);
        return match f {
            14 => kth(&v, k, true),
            15 => kth(&v, k, false),
            16 => pct_inc(&s, k),
            17 => pct_inc(&s, quart(k)?),
            18 => pct_exc(&s, k),
            _ => {
                let q = quart(k)?;
                if q == 0.0 || q == 1.0 {
                    return Err(ErrorKind::Num);
                }
                pct_exc(&s, q)
            }
        };
    }
    if f == 3 {
        // COUNTA counts every non-empty value.
        let v = agg_values(ev, rest, opt, true)?;
        return Ok(v.len() as f64);
    }
    let v = agg_values(ev, rest, opt, false)?;
    match f {
        1 => mean(&v),
        2 => Ok(v.len() as f64),
        4 => Ok(v.iter().copied().reduce(f64::max).unwrap_or(0.0)),
        5 => Ok(v.iter().copied().reduce(f64::min).unwrap_or(0.0)),
        6 => Ok(if v.is_empty() { 0.0 } else { v.iter().product() }),
        7 => var(&v, true).map(f64::sqrt),
        8 => var(&v, false).map(f64::sqrt),
        9 => Ok(v.iter().sum()),
        10 => var(&v, true),
        11 => var(&v, false),
        12 => pct_inc(&sorted(&v), 0.5),
        _ => mode(&v),
    }
}

pub fn register(r: &mut Registry) {
    r.add("AGGREGATE", 3, 255, |ev, a| ret(aggregate(ev, a)));
    r.add("MDETERM", 1, 1, |ev, a| ret(mdeterm(ev, a)));
    r.add("MINVERSE", 1, 1, minverse);
    r.add("MUNIT", 1, 1, munit);
    r.add("SERIESSUM", 4, 4, |ev, a| ret(seriessum(ev, a)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn determinant_and_inverse() {
        let (d, _) = eliminate(2, vec![1.0, 2.0, 3.0, 4.0], false).unwrap();
        assert!((d + 2.0).abs() < 1e-12);
        let (_, inv) = eliminate(2, vec![4.0, 7.0, 2.0, 6.0], true).unwrap();
        let inv = inv.unwrap();
        let want = [0.6, -0.7, -0.2, 0.4];
        for (a, b) in inv.iter().zip(want) {
            assert!((a - b).abs() < 1e-12);
        }
        let (d, inv) = eliminate(2, vec![1.0, 2.0, 2.0, 4.0], true).unwrap();
        assert_eq!(d, 0.0);
        assert!(inv.is_none());
    }

    #[test]
    fn stats_helpers() {
        let s = sorted(&[1.0, 2.0, 3.0, 4.0]);
        assert_eq!(pct_inc(&s, 0.5).unwrap(), 2.5);
        assert_eq!(kth(&[5.0, 1.0, 3.0], 1.0, true).unwrap(), 5.0);
        assert_eq!(mode(&[1.0, 2.0, 2.0]).unwrap(), 2.0);
        assert!(pct_exc(&s, 0.1).is_err());
    }
}
