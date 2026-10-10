//! More statistical functions: shape statistics, regression (LINEST/LOGEST/TREND/GROWTH,
//! FORECAST, STEYX), FREQUENCY, MODE.MULT, PERCENTRANK, PROB and TRIMMEAN.

use super::stats::pairs;
use super::{Agg, Registry, arg_array, collect_numbers, ret};
use crate::ast::Expr;
use crate::eval::{Array2, Ev, EvalValue, num_value};
use sp_core::{ErrorKind, Value};

type R = Result<f64, ErrorKind>;
use ErrorKind::{Div0, NA, Num};

fn mean(v: &[f64]) -> f64 {
    v.iter().sum::<f64>() / v.len() as f64
}

/// Sum of the k-th powers of standardized deviations, with the given divisor for variance.
fn std_moment(v: &[f64], k: i32, sample: bool) -> R {
    let n = v.len() as f64;
    let m = mean(v);
    let ss: f64 = v.iter().map(|x| (x - m) * (x - m)).sum();
    let var = ss / if sample { n - 1.0 } else { n };
    if var <= 0.0 {
        return Err(Div0);
    }
    let s = var.sqrt();
    Ok(v.iter().map(|x| ((x - m) / s).powi(k)).sum())
}

fn skew(v: Vec<f64>) -> R {
    let n = v.len() as f64;
    if v.len() < 3 {
        return Err(Div0);
    }
    Ok(n / ((n - 1.0) * (n - 2.0)) * std_moment(&v, 3, true)?)
}

fn skew_p(v: Vec<f64>) -> R {
    if v.is_empty() {
        return Err(Div0);
    }
    Ok(std_moment(&v, 3, false)? / v.len() as f64)
}

fn kurt(v: Vec<f64>) -> R {
    let n = v.len() as f64;
    if v.len() < 4 {
        return Err(Div0);
    }
    let s4 = std_moment(&v, 4, true)?;
    Ok(n * (n + 1.0) / ((n - 1.0) * (n - 2.0) * (n - 3.0)) * s4
        - 3.0 * (n - 1.0) * (n - 1.0) / ((n - 2.0) * (n - 3.0)))
}

fn steyx(ys: &[f64], xs: &[f64]) -> R {
    let n = ys.len();
    if n < 3 {
        return Err(Div0);
    }
    let (my, mx) = (mean(ys), mean(xs));
    let syy: f64 = ys.iter().map(|y| (y - my) * (y - my)).sum();
    let sxx: f64 = xs.iter().map(|x| (x - mx) * (x - mx)).sum();
    let sxy: f64 = xs.iter().zip(ys).map(|(x, y)| (x - mx) * (y - my)).sum();
    if sxx == 0.0 {
        return Err(Div0);
    }
    let r = (syy - sxy * sxy / sxx) / (n as f64 - 2.0);
    Ok(r.max(0.0).sqrt())
}

fn forecast(ev: &mut Ev<'_>, a: &[Expr]) -> R {
    let x = match a.first() {
        Some(e) => ev.num(e)?,
        None => return Err(ErrorKind::Value),
    };
    let (ys, xs) = pairs(ev, a.get(1..).unwrap_or(&[]))?;
    if ys.is_empty() {
        return Err(Div0);
    }
    let (my, mx) = (mean(&ys), mean(&xs));
    let sxx: f64 = xs.iter().map(|v| (v - mx) * (v - mx)).sum();
    let sxy: f64 = xs.iter().zip(&ys).map(|(u, v)| (u - mx) * (v - my)).sum();
    if sxx == 0.0 {
        return Err(Div0);
    }
    let b = sxy / sxx;
    Ok(my - b * mx + b * x)
}

// ── least squares ───────────────────────────────────────────────────────────────────────

/// Invert a small symmetric positive matrix (Gauss-Jordan with partial pivoting).
fn invert(m: &[Vec<f64>]) -> Option<Vec<Vec<f64>>> {
    let p = m.len();
    let mut a: Vec<Vec<f64>> = m.to_vec();
    let mut inv: Vec<Vec<f64>> = (0..p)
        .map(|i| (0..p).map(|j| if i == j { 1.0 } else { 0.0 }).collect())
        .collect();
    let scale = m
        .iter()
        .flat_map(|r| r.iter())
        .fold(0.0_f64, |s, v| s.max(v.abs()))
        .max(1e-300);
    for col in 0..p {
        let piv = (col..p).max_by(|&i, &j| {
            let x = a.get(i).and_then(|r| r.get(col)).map_or(0.0, |v| v.abs());
            let y = a.get(j).and_then(|r| r.get(col)).map_or(0.0, |v| v.abs());
            x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal)
        })?;
        let pv = *a.get(piv)?.get(col)?;
        if pv.abs() <= 1e-13 * scale {
            return None;
        }
        a.swap(col, piv);
        inv.swap(col, piv);
        let (arow, irow) = (a.get(col)?.clone(), inv.get(col)?.clone());
        let arow: Vec<f64> = arow.iter().map(|v| v / pv).collect();
        let irow: Vec<f64> = irow.iter().map(|v| v / pv).collect();
        for r in 0..p {
            if r == col {
                continue;
            }
            let f = *a.get(r)?.get(col)?;
            if f == 0.0 {
                continue;
            }
            for (x, y) in a.get_mut(r)?.iter_mut().zip(&arow) {
                *x -= f * y;
            }
            for (x, y) in inv.get_mut(r)?.iter_mut().zip(&irow) {
                *x -= f * y;
            }
        }
        *a.get_mut(col)? = arow;
        *inv.get_mut(col)? = irow;
    }
    Some(inv)
}

struct Fit {
    /// Intercept first (0 when forced), then one slope per regressor.
    beta: Vec<f64>,
    se: Vec<f64>,
    r2: f64,
    sey: f64,
    f: f64,
    df: f64,
    ssreg: f64,
    ssresid: f64,
    has_const: bool,
}

fn fit(ys: &[f64], xcols: &[Vec<f64>], has_const: bool) -> Result<Fit, ErrorKind> {
    let n = ys.len();
    let k = xcols.len();
    let p = k + usize::from(has_const);
    if n == 0 || p == 0 {
        return Err(NA);
    }
    let row = |i: usize| -> Vec<f64> {
        let mut r = Vec::with_capacity(p);
        if has_const {
            r.push(1.0);
        }
        for c in xcols {
            r.push(c.get(i).copied().unwrap_or(0.0));
        }
        r
    };
    let mut xtx = vec![vec![0.0; p]; p];
    let mut xty = vec![0.0; p];
    for (i, y) in ys.iter().enumerate() {
        let r = row(i);
        for (a, ra) in r.iter().enumerate() {
            if let Some(v) = xty.get_mut(a) {
                *v += ra * y;
            }
            if let Some(line) = xtx.get_mut(a) {
                for (cell, rb) in line.iter_mut().zip(&r) {
                    *cell += ra * rb;
                }
            }
        }
    }
    let inv = invert(&xtx).ok_or(Num)?;
    let coef: Vec<f64> = inv
        .iter()
        .map(|line| line.iter().zip(&xty).map(|(a, b)| a * b).sum())
        .collect();
    let mut ssresid = 0.0;
    let mut sum_fit2 = 0.0;
    for (i, y) in ys.iter().enumerate() {
        let yhat: f64 = row(i).iter().zip(&coef).map(|(a, b)| a * b).sum();
        ssresid += (y - yhat) * (y - yhat);
        sum_fit2 += yhat * yhat;
    }
    let ybar = mean(ys);
    let sstot = if has_const {
        ys.iter().map(|y| (y - ybar) * (y - ybar)).sum::<f64>()
    } else {
        ys.iter().map(|y| y * y).sum::<f64>()
    };
    let ssreg = if has_const {
        (sstot - ssresid).max(0.0)
    } else {
        sum_fit2
    };
    let df = n as f64 - p as f64;
    let sey = if df > 0.0 {
        (ssresid / df).sqrt()
    } else {
        f64::NAN
    };
    let se: Vec<f64> = (0..p)
        .map(|i| {
            let d = inv.get(i).and_then(|r| r.get(i)).copied().unwrap_or(0.0);
            sey * d.max(0.0).sqrt()
        })
        .collect();
    let mut beta = Vec::with_capacity(k + 1);
    let mut se_full = Vec::with_capacity(k + 1);
    if has_const {
        beta.extend(coef.iter().copied());
        se_full.extend(se.iter().copied());
    } else {
        beta.push(0.0);
        beta.extend(coef.iter().copied());
        se_full.push(f64::NAN);
        se_full.extend(se.iter().copied());
    }
    let r2 = if sstot == 0.0 { 1.0 } else { ssreg / sstot };
    let f = if df > 0.0 && ssresid > 0.0 {
        (ssreg / k as f64) / (ssresid / df)
    } else {
        f64::NAN
    };
    Ok(Fit {
        beta,
        se: se_full,
        r2,
        sey,
        f,
        df,
        ssreg,
        ssresid,
        has_const,
    })
}

/// Known data for a regression: y values, regressor columns, and whether y is a row.
struct Known {
    ys: Vec<f64>,
    xcols: Vec<Vec<f64>>,
    y_is_row: bool,
    /// The x array as given (for TREND's default new_x).
    x_arr: Array2,
}

fn all_numbers(a: &Array2) -> Result<Vec<f64>, ErrorKind> {
    a.data
        .iter()
        .map(|v| match v {
            Value::Number(n) => Ok(*n),
            Value::Error(e) => Err(*e),
            _ => Err(ErrorKind::Value),
        })
        .collect()
}

fn known(ev: &mut Ev<'_>, a: &[Expr], log: bool) -> Result<Known, ErrorKind> {
    let ya = arg_array(ev, a, 0)?;
    let mut ys = all_numbers(&ya)?;
    if log {
        if ys.iter().any(|y| *y <= 0.0) {
            return Err(Num);
        }
        ys = ys.iter().map(|y| y.ln()).collect();
    }
    let n = ys.len();
    let y_is_row = ya.rows == 1 && ya.cols > 1;
    let xa = match a.get(1) {
        None | Some(Expr::Missing) => {
            let seq: Vec<Value> = (1..=n).map(|i| Value::Number(i as f64)).collect();
            Array2::new(ya.rows, ya.cols, seq)
        }
        Some(e) => ev.eval_array(e)?,
    };
    let xv = all_numbers(&xa)?;
    let xcols: Vec<Vec<f64>> = if xv.len() == n {
        vec![xv]
    } else if !y_is_row && ya.cols == 1 && xa.rows == n {
        (0..xa.cols)
            .map(|c| {
                (0..n)
                    .map(|r| xv.get(r * xa.cols + c).copied().unwrap_or(0.0))
                    .collect()
            })
            .collect()
    } else if ya.rows == 1 && xa.cols == n {
        (0..xa.rows)
            .map(|r| {
                (0..n)
                    .map(|c| xv.get(r * xa.cols + c).copied().unwrap_or(0.0))
                    .collect()
            })
            .collect()
    } else {
        return Err(ErrorKind::Ref);
    };
    Ok(Known {
        ys,
        xcols,
        y_is_row,
        x_arr: xa,
    })
}

fn linest(ev: &mut Ev<'_>, a: &[Expr], log: bool) -> Result<Array2, ErrorKind> {
    let kn = known(ev, a, log)?;
    let has_const = ev.bool_or(a, 2, true)?;
    let stats = ev.bool_or(a, 3, false)?;
    let ft = fit(&kn.ys, &kn.xcols, has_const)?;
    let k = kn.xcols.len();
    let cols = k + 1;
    let tf = |x: f64| if log { x.exp() } else { x };
    let nv = |x: f64| {
        if x.is_finite() {
            num_value(x)
        } else {
            Value::Error(NA)
        }
    };
    let mut data = Vec::new();
    // Row 1: m_k … m_1, b.
    for i in (1..=k).rev() {
        data.push(num_value(tf(ft.beta.get(i).copied().unwrap_or(0.0))));
    }
    data.push(num_value(tf(ft.beta.first().copied().unwrap_or(0.0))));
    if !stats {
        return Ok(Array2::new(1, cols, data));
    }
    for i in (1..=k).rev() {
        data.push(nv(ft.se.get(i).copied().unwrap_or(f64::NAN)));
    }
    data.push(if ft.has_const {
        nv(ft.se.first().copied().unwrap_or(f64::NAN))
    } else {
        Value::Error(NA)
    });
    let filler = |data: &mut Vec<Value>| {
        for _ in 2..cols {
            data.push(Value::Error(NA));
        }
    };
    data.push(nv(ft.r2));
    data.push(nv(ft.sey));
    filler(&mut data);
    data.push(nv(ft.f));
    data.push(nv(ft.df));
    filler(&mut data);
    data.push(nv(ft.ssreg));
    data.push(nv(ft.ssresid));
    filler(&mut data);
    Ok(Array2::new(5, cols, data))
}

fn trend(ev: &mut Ev<'_>, a: &[Expr], log: bool) -> Result<Array2, ErrorKind> {
    let kn = known(ev, a, log)?;
    let new_x = match a.get(2) {
        None | Some(Expr::Missing) => kn.x_arr.clone(),
        Some(e) => ev.eval_array(e)?,
    };
    let has_const = ev.bool_or(a, 3, true)?;
    let ft = fit(&kn.ys, &kn.xcols, has_const)?;
    let nx = all_numbers(&new_x)?;
    let k = kn.xcols.len();
    let b0 = ft.beta.first().copied().unwrap_or(0.0);
    let out = |v: f64| num_value(if log { v.exp() } else { v });
    if k == 1 {
        let m = ft.beta.get(1).copied().unwrap_or(0.0);
        let data = nx.iter().map(|x| out(b0 + m * x)).collect();
        return Ok(Array2::new(new_x.rows, new_x.cols, data));
    }
    // Several regressors: each observation is a row (or column when y is a row).
    let (count, get): (usize, Box<dyn Fn(usize, usize) -> f64>) = if kn.y_is_row {
        if new_x.rows != k {
            return Err(ErrorKind::Ref);
        }
        let c = new_x.cols;
        (
            c,
            Box::new(move |obs, j| nx.get(j * c + obs).copied().unwrap_or(0.0)),
        )
    } else {
        if new_x.cols != k {
            return Err(ErrorKind::Ref);
        }
        (
            new_x.rows,
            Box::new(move |obs, j| nx.get(obs * k + j).copied().unwrap_or(0.0)),
        )
    };
    let data: Vec<Value> = (0..count)
        .map(|obs| {
            let s: f64 = (0..k)
                .map(|j| ft.beta.get(j + 1).copied().unwrap_or(0.0) * get(obs, j))
                .sum();
            out(b0 + s)
        })
        .collect();
    Ok(if kn.y_is_row {
        Array2::new(1, count, data)
    } else {
        Array2::new(count, 1, data)
    })
}

// ── distributions of data ───────────────────────────────────────────────────────────────

fn frequency(ev: &mut Ev<'_>, a: &[Expr]) -> Result<Array2, ErrorKind> {
    let nums = |arr: &Array2| -> Result<Vec<f64>, ErrorKind> {
        let mut out = Vec::new();
        for v in &arr.data {
            match v {
                Value::Number(n) => out.push(*n),
                Value::Error(e) => return Err(*e),
                _ => {}
            }
        }
        Ok(out)
    };
    let data = nums(&arg_array(ev, a, 0)?)?;
    let bins = nums(&arg_array(ev, a, 1)?)?;
    let mut order: Vec<usize> = (0..bins.len()).collect();
    order.sort_by(|&i, &j| {
        let (x, y) = (bins.get(i), bins.get(j));
        x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut counts = vec![0usize; bins.len() + 1];
    for d in &data {
        // First bin (in ascending order) with d ≤ bin.
        let slot = order
            .iter()
            .find(|&&i| bins.get(i).is_some_and(|b| *d <= *b))
            .copied()
            .unwrap_or(bins.len());
        if let Some(c) = counts.get_mut(slot) {
            *c += 1;
        }
    }
    let n = counts.len();
    Ok(Array2::new(
        n,
        1,
        counts
            .into_iter()
            .map(|c| Value::Number(c as f64))
            .collect(),
    ))
}

fn mode_mult(ev: &mut Ev<'_>, a: &[Expr]) -> Result<Array2, ErrorKind> {
    let v = collect_numbers(ev, a, Agg::Numbers)?;
    let mut seen: Vec<(f64, usize)> = Vec::new();
    for x in &v {
        match seen.iter_mut().find(|(y, _)| y == x) {
            Some(e) => e.1 += 1,
            None => seen.push((*x, 1)),
        }
    }
    let best = seen.iter().map(|e| e.1).max().unwrap_or(0);
    if best < 2 {
        return Err(NA);
    }
    let modes: Vec<Value> = seen
        .iter()
        .filter(|e| e.1 == best)
        .map(|e| Value::Number(e.0))
        .collect();
    let n = modes.len();
    Ok(Array2::new(n, 1, modes))
}

fn percentrank(ev: &mut Ev<'_>, a: &[Expr], exc: bool) -> R {
    let arr = arg_array(ev, a, 0)?;
    let mut v = Vec::new();
    for x in &arr.data {
        match x {
            Value::Number(n) => v.push(*n),
            Value::Error(e) => return Err(*e),
            _ => {}
        }
    }
    let x = match a.get(1) {
        Some(e) => ev.num(e)?,
        None => return Err(ErrorKind::Value),
    };
    let sig = ev.num_or(a, 2, 3.0)?.trunc();
    if sig < 1.0 {
        return Err(Num);
    }
    if v.is_empty() {
        return Err(Num);
    }
    v.sort_by(|p, q| p.partial_cmp(q).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len() as f64;
    let (Some(&lo), Some(&hi)) = (v.first(), v.last()) else {
        return Err(Num);
    };
    if x < lo || x > hi {
        return Err(NA);
    }
    let less = v.iter().filter(|y| **y < x).count();
    let rank = if v.contains(&x) {
        less as f64
    } else {
        // Interpolate between neighbours v[less-1] < x < v[less].
        let a0 = v.get(less.wrapping_sub(1)).copied().ok_or(NA)?;
        let a1 = v.get(less).copied().ok_or(NA)?;
        (less as f64 - 1.0) + (x - a0) / (a1 - a0)
    };
    let r = if exc {
        (rank + 1.0) / (n + 1.0)
    } else {
        if v.len() == 1 {
            return Ok(1.0);
        }
        rank / (n - 1.0)
    };
    let f = 10f64.powi(sig.min(15.0) as i32);
    // Truncate toward zero; nudge by a few ulps so exact decimals do not drop a digit.
    let scaled = r * f;
    let nudged = (scaled * (1.0 + 4.0 * f64::EPSILON)).floor();
    Ok(nudged / f)
}

fn prob(ev: &mut Ev<'_>, a: &[Expr]) -> R {
    let xs = arg_array(ev, a, 0)?;
    let ps = arg_array(ev, a, 1)?;
    let lower = match a.get(2) {
        Some(e) => ev.num(e)?,
        None => return Err(ErrorKind::Value),
    };
    let upper = match a.get(3) {
        None | Some(Expr::Missing) => lower,
        Some(e) => ev.num(e)?,
    };
    if xs.data.len() != ps.data.len() {
        return Err(NA);
    }
    let mut total = 0.0;
    let mut hit = 0.0;
    let mut any = false;
    for (x, p) in xs.data.iter().zip(&ps.data) {
        if let Value::Error(e) = x {
            return Err(*e);
        }
        if let Value::Error(e) = p {
            return Err(*e);
        }
        if let Value::Number(p) = p {
            if !(0.0..=1.0).contains(p) {
                return Err(Num);
            }
            any = true;
            total += p;
            if let Value::Number(x) = x
                && *x >= lower
                && *x <= upper
            {
                hit += p;
            }
        }
    }
    if !any {
        return Err(Div0);
    }
    if (total - 1.0).abs() > 1e-9 {
        return Err(Num);
    }
    Ok(hit)
}

fn trimmean(ev: &mut Ev<'_>, a: &[Expr]) -> R {
    let arr = arg_array(ev, a, 0)?;
    let mut v = Vec::new();
    for x in &arr.data {
        match x {
            Value::Number(n) => v.push(*n),
            Value::Error(e) => return Err(*e),
            _ => {}
        }
    }
    let p = match a.get(1) {
        Some(e) => ev.num(e)?,
        None => return Err(ErrorKind::Value),
    };
    if !(0.0..1.0).contains(&p) {
        return Err(Num);
    }
    if v.is_empty() {
        return Err(Num);
    }
    v.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
    let drop = ((v.len() as f64 * p) / 2.0).floor() as usize;
    let kept = v.get(drop..v.len().saturating_sub(drop)).unwrap_or(&[]);
    if kept.is_empty() {
        return Err(Num);
    }
    Ok(mean(kept))
}

fn arr_ret(r: Result<Array2, ErrorKind>) -> EvalValue {
    match r {
        Ok(a) => EvalValue::Array(a),
        Err(e) => EvalValue::err(e),
    }
}

pub fn register(r: &mut Registry) {
    r.add("SKEW", 1, 255, |ev, a| {
        ret(collect_numbers(ev, a, Agg::Numbers).and_then(skew))
    });
    r.add("SKEW.P", 1, 255, |ev, a| {
        ret(collect_numbers(ev, a, Agg::Numbers).and_then(skew_p))
    });
    r.add("KURT", 1, 255, |ev, a| {
        ret(collect_numbers(ev, a, Agg::Numbers).and_then(kurt))
    });
    r.add("STEYX", 2, 2, |ev, a| {
        ret(pairs(ev, a).and_then(|(y, x)| steyx(&y, &x)))
    });
    r.add("FORECAST", 3, 3, |ev, a| ret(forecast(ev, a)));
    r.alias("FORECAST.LINEAR", "FORECAST");
    r.add("LINEST", 1, 4, |ev, a| arr_ret(linest(ev, a, false)));
    r.add("LOGEST", 1, 4, |ev, a| arr_ret(linest(ev, a, true)));
    r.add("TREND", 1, 4, |ev, a| arr_ret(trend(ev, a, false)));
    r.add("GROWTH", 1, 4, |ev, a| arr_ret(trend(ev, a, true)));
    r.add("FREQUENCY", 2, 2, |ev, a| arr_ret(frequency(ev, a)));
    r.add("MODE.MULT", 1, 255, |ev, a| arr_ret(mode_mult(ev, a)));
    r.add("PERCENTRANK.INC", 2, 3, |ev, a| {
        ret(percentrank(ev, a, false))
    });
    r.add("PERCENTRANK.EXC", 2, 3, |ev, a| {
        ret(percentrank(ev, a, true))
    });
    r.alias("PERCENTRANK", "PERCENTRANK.INC");
    r.add("PROB", 3, 4, |ev, a| ret(prob(ev, a)));
    r.add("TRIMMEAN", 2, 2, |ev, a| ret(trimmean(ev, a)));
}

#[cfg(test)]
mod tests {
    use crate::recalc::eval_formula;
    use sp_core::{CellAddr, ErrorKind, Value};
    use sp_model::Workbook;

    fn ev(src: &str) -> Value {
        eval_formula(&Workbook::new(), 0, CellAddr::new(0, 0), src)
    }

    fn approx(src: &str, want: f64) {
        match ev(src) {
            Value::Number(n) => assert!(
                (n - want).abs() <= 1e-9 * want.abs().max(1.0),
                "{src}: got {n}, want {want}"
            ),
            other => panic!("{src}: got {other:?}, want {want}"),
        }
    }

    #[test]
    fn shape_and_regression() {
        approx("SKEW({3,4,5,2,3,4,5,6,4,7})", 0.359_543_071_407_298_7);
        approx("SKEW.P({3,4,5,2,3,4,5,6,4,7})", 0.303_193_339_354_144);
        approx("KURT({3,4,5,2,3,4,5,6,4,7,4})", 0.187_484_782_079_378_1);
        approx(
            "STEYX({2,3,9,1,8,7,5},{6,5,11,7,5,4,4})",
            3.305_718_950_210_041,
        );
        approx(
            "FORECAST(30,{6,7,9,15,21},{20,28,31,38,40})",
            10.607_253_086_419_75,
        );
        approx("LINEST({1,9,5,7},{0,4,2,3})", 2.0);
        approx("INDEX(LINEST({1,9,5,7},{0,4,2,3}),1,2)", 1.0);
        approx("TREND({1,9,5,7},{0,4,2,3},{5})", 11.0);
        approx("LOGEST({2,4,8,16})", 2.0);
        approx("GROWTH({2,4,8,16},,{5})", 32.0);
        approx(
            "TRIMMEAN({4,5,6,7,2,3,4,5,1,2,3},0.2)",
            3.777_777_777_777_778,
        );
        approx("PERCENTRANK.INC({13,12,11,8,4,3,2,1,1,1},2)", 0.333);
        approx("PERCENTRANK.EXC({1,2,3,4,5,6,7,8,9,10},9,2)", 0.81);
        approx("PROB({0,1,2,3},{0.2,0.3,0.1,0.4},2)", 0.1);
        approx("PROB({0,1,2,3},{0.2,0.3,0.1,0.4},1,3)", 0.8);
        approx(
            "INDEX(FREQUENCY({79,85,78,85,50,81,95,88,97},{70,79,89}),3)",
            4.0,
        );
        approx("INDEX(MODE.MULT({1,2,3,4,3,2,1,2,3}),2)", 3.0);
        assert_eq!(ev("MODE.MULT({1,2,3})"), Value::Error(ErrorKind::NA));
        assert_eq!(ev("TRIMMEAN({1,2},1)"), Value::Error(ErrorKind::Num));
        assert_eq!(ev("KURT({1,2,3})"), Value::Error(ErrorKind::Div0));
        assert_eq!(ev("LINEST({1,2,3},{1,1,1})"), Value::Error(ErrorKind::Num));
    }
}
