//! Probability distributions and hypothesis tests (Statistical category) plus their
//! compatibility aliases.
#![allow(clippy::redundant_closure_call)] // `(|| { … })()` enables `?` inside registry closures.

use super::special::{
    beta_reg2, bisect, choose, gamma, gamma_p, gamma_q, ln_beta, ln_choose, ln_gamma, norm_cdf,
    norm_inv, norm_pdf, solve_tail,
};
use super::{Registry, arg_array, ret};
use crate::ast::Expr;
use crate::eval::Ev;
use sp_core::{ErrorKind, Value};

type R = Result<f64, ErrorKind>;
use ErrorKind::{Div0, NA, Num};

// ── argument helpers ────────────────────────────────────────────────────────────────────

fn num(ev: &mut Ev<'_>, a: &[Expr], i: usize) -> R {
    match a.get(i) {
        Some(e) => ev.num(e),
        None => Err(ErrorKind::Value),
    }
}

/// Optional number: absent → `default`; an explicitly empty argument also takes `default`.
fn opt(ev: &mut Ev<'_>, a: &[Expr], i: usize, default: f64) -> R {
    ev.num_or(a, i, default)
}

fn flag(ev: &mut Ev<'_>, a: &[Expr], i: usize) -> Result<bool, ErrorKind> {
    match a.get(i) {
        Some(e) => ev.boolean(e),
        None => Err(ErrorKind::Value),
    }
}

/// Finite check: NaN/∞ → `#NUM!`.
fn fin(x: f64) -> R {
    if x.is_finite() { Ok(x) } else { Err(Num) }
}

fn need(c: bool) -> Result<(), ErrorKind> {
    if c { Ok(()) } else { Err(Num) }
}

fn tr(x: f64) -> f64 {
    x.trunc()
}

// ── normal family ───────────────────────────────────────────────────────────────────────

fn norm_dist(x: f64, m: f64, s: f64, cum: bool) -> R {
    need(s > 0.0)?;
    let z = (x - m) / s;
    fin(if cum { norm_cdf(z) } else { norm_pdf(z) / s })
}

fn norm_quantile(p: f64, m: f64, s: f64) -> R {
    need(p > 0.0 && p < 1.0 && s > 0.0)?;
    fin(m + s * norm_inv(p))
}

fn lognorm_dist(x: f64, m: f64, s: f64, cum: bool) -> R {
    need(x > 0.0 && s > 0.0)?;
    let z = (x.ln() - m) / s;
    fin(if cum {
        norm_cdf(z)
    } else {
        norm_pdf(z) / (s * x)
    })
}

fn lognorm_inv(p: f64, m: f64, s: f64) -> R {
    need(p > 0.0 && p < 1.0 && s > 0.0)?;
    fin((m + s * norm_inv(p)).exp())
}

// ── Student t ───────────────────────────────────────────────────────────────────────────

/// P(T > |t|).
fn t_tail(t: f64, df: f64) -> f64 {
    let t2 = t * t;
    if !t2.is_finite() {
        return 0.0;
    }
    let x = df / (df + t2);
    let y = t2 / (df + t2);
    0.5 * beta_reg2(x, y, df / 2.0, 0.5)
}

fn t_cdf(t: f64, df: f64) -> f64 {
    let tail = t_tail(t, df);
    if t > 0.0 { 1.0 - tail } else { tail }
}

fn t_pdf(t: f64, df: f64) -> f64 {
    let ln = ln_gamma((df + 1.0) / 2.0)
        - ln_gamma(df / 2.0)
        - 0.5 * (df * std::f64::consts::PI).ln()
        - (df + 1.0) / 2.0 * (t * t / df).ln_1p();
    ln.exp()
}

fn t_df(df: f64) -> R {
    let df = tr(df);
    need((1.0..=1e10).contains(&df))?;
    Ok(df)
}

/// t > 0 with P(T > t) = q.
fn t_upper(q: f64, df: f64) -> f64 {
    if q >= 0.5 {
        return 0.0;
    }
    solve_tail(|t| t_tail(t, df), q)
}

fn t_inv(p: f64, df: f64) -> R {
    need(p > 0.0 && p < 1.0)?;
    let df = t_df(df)?;
    fin(if p < 0.5 {
        -t_upper(p, df)
    } else {
        t_upper(1.0 - p, df)
    })
}

fn t_inv_2t(p: f64, df: f64) -> R {
    need(p > 0.0 && p <= 1.0)?;
    let df = t_df(df)?;
    fin(t_upper(p / 2.0, df))
}

// ── chi-squared, F, gamma, beta ─────────────────────────────────────────────────────────

fn chi_df(df: f64) -> R {
    let df = tr(df);
    need((1.0..=1e10).contains(&df))?;
    Ok(df)
}

fn gamma_pdf(x: f64, a: f64, b: f64) -> f64 {
    if x == 0.0 {
        return if a < 1.0 {
            f64::INFINITY
        } else if a == 1.0 {
            1.0 / b
        } else {
            0.0
        };
    }
    ((a - 1.0) * x.ln() - x / b - a * b.ln() - ln_gamma(a)).exp()
}

fn gamma_dist(x: f64, a: f64, b: f64, cum: bool) -> R {
    need(x >= 0.0 && a > 0.0 && b > 0.0)?;
    fin(if cum {
        gamma_p(a, x / b)
    } else {
        gamma_pdf(x, a, b)
    })
}

fn gamma_inv(p: f64, a: f64, b: f64) -> R {
    need((0.0..=1.0).contains(&p) && a > 0.0 && b > 0.0)?;
    if p == 0.0 {
        return Ok(0.0);
    }
    need(p < 1.0)?;
    let x = if p <= 0.5 {
        // cdf(x) = p ⇔ Q(a, x) = 1 - p: solve on the lower CDF directly.
        bisect_pos(|x| gamma_p(a, x) - p)
    } else {
        solve_tail(|x| gamma_q(a, x), 1.0 - p)
    };
    fin(x * b)
}

/// Root of an increasing function on (0, ∞) that is ≤ 0 at 0.
fn bisect_pos(h: impl Fn(f64) -> f64) -> f64 {
    let mut hi = 1.0_f64;
    for _ in 0..600 {
        let v = h(hi);
        if v.is_nan() {
            return f64::NAN;
        }
        if v >= 0.0 {
            return bisect(h, 0.0, hi);
        }
        hi *= 4.0;
        if !hi.is_finite() {
            break;
        }
    }
    f64::NAN
}

fn chisq_dist(x: f64, df: f64, cum: bool) -> R {
    need(x >= 0.0)?;
    let df = chi_df(df)?;
    gamma_dist(x, df / 2.0, 2.0, cum)
}

fn chisq_rt(x: f64, df: f64) -> R {
    need(x >= 0.0)?;
    let df = chi_df(df)?;
    fin(gamma_q(df / 2.0, x / 2.0))
}

fn chisq_inv(p: f64, df: f64) -> R {
    need((0.0..1.0).contains(&p))?;
    let df = chi_df(df)?;
    gamma_inv(p, df / 2.0, 2.0)
}

fn chisq_inv_rt(p: f64, df: f64) -> R {
    need(p > 0.0 && p <= 1.0)?;
    let df = chi_df(df)?;
    if p == 1.0 {
        return Ok(0.0);
    }
    let x = if p < 0.5 {
        solve_tail(|x| gamma_q(df / 2.0, x), p)
    } else {
        bisect_pos(|x| gamma_p(df / 2.0, x) - (1.0 - p))
    };
    fin(2.0 * x)
}

fn f_dfs(d1: f64, d2: f64) -> Result<(f64, f64), ErrorKind> {
    let (d1, d2) = (tr(d1), tr(d2));
    need((1.0..1e10).contains(&d1) && (1.0..1e10).contains(&d2))?;
    Ok((d1, d2))
}

fn f_cdf(x: f64, d1: f64, d2: f64) -> f64 {
    let den = d1 * x + d2;
    beta_reg2(d1 * x / den, d2 / den, d1 / 2.0, d2 / 2.0)
}

fn f_sf(x: f64, d1: f64, d2: f64) -> f64 {
    let den = d1 * x + d2;
    beta_reg2(d2 / den, d1 * x / den, d2 / 2.0, d1 / 2.0)
}

fn f_dist(x: f64, d1: f64, d2: f64, cum: bool) -> R {
    need(x >= 0.0)?;
    let (d1, d2) = f_dfs(d1, d2)?;
    if cum {
        return fin(f_cdf(x, d1, d2));
    }
    if x == 0.0 {
        return if d1 < 2.0 {
            Err(Num)
        } else if d1 == 2.0 {
            Ok(1.0)
        } else {
            Ok(0.0)
        };
    }
    let ln = 0.5 * (d1 * (d1 * x).ln() + d2 * d2.ln() - (d1 + d2) * (d1 * x + d2).ln())
        - x.ln()
        - ln_beta(d1 / 2.0, d2 / 2.0);
    fin(ln.exp())
}

fn f_rt(x: f64, d1: f64, d2: f64) -> R {
    need(x >= 0.0)?;
    let (d1, d2) = f_dfs(d1, d2)?;
    fin(f_sf(x, d1, d2))
}

fn f_inv(p: f64, d1: f64, d2: f64) -> R {
    need((0.0..=1.0).contains(&p))?;
    let (d1, d2) = f_dfs(d1, d2)?;
    if p == 0.0 {
        return Ok(0.0);
    }
    need(p < 1.0)?;
    fin(if p <= 0.5 {
        bisect_pos(|x| f_cdf(x, d1, d2) - p)
    } else {
        solve_tail(|x| f_sf(x, d1, d2), 1.0 - p)
    })
}

fn f_inv_rt(p: f64, d1: f64, d2: f64) -> R {
    need((0.0..=1.0).contains(&p))?;
    let (d1, d2) = f_dfs(d1, d2)?;
    if p == 1.0 {
        return Ok(0.0);
    }
    need(p > 0.0)?;
    fin(if p < 0.5 {
        solve_tail(|x| f_sf(x, d1, d2), p)
    } else {
        bisect_pos(|x| f_cdf(x, d1, d2) - (1.0 - p))
    })
}

fn beta_dist(x: f64, a: f64, b: f64, cum: bool, lo: f64, hi: f64) -> R {
    need(a > 0.0 && b > 0.0 && lo < hi && x >= lo && x <= hi)?;
    let w = hi - lo;
    let z = (x - lo) / w;
    let y = (hi - x) / w;
    if cum {
        return fin(beta_reg2(z, y, a, b));
    }
    if z == 0.0 || y == 0.0 {
        let edge_exp = if z == 0.0 { a } else { b };
        return if edge_exp < 1.0 {
            Err(Num)
        } else if edge_exp == 1.0 {
            fin((-ln_beta(a, b)).exp() / w)
        } else {
            Ok(0.0)
        };
    }
    fin(((a - 1.0) * z.ln() + (b - 1.0) * y.ln() - ln_beta(a, b)).exp() / w)
}

fn beta_inv(p: f64, a: f64, b: f64, lo: f64, hi: f64) -> R {
    need(a > 0.0 && b > 0.0 && lo < hi && p > 0.0 && p < 1.0)?;
    let z = if p <= 0.5 {
        bisect(|z| beta_reg2(z, 1.0 - z, a, b) - p, 0.0, 1.0)
    } else {
        // Solve the upper tail in terms of y = 1 - z for precision.
        let q = 1.0 - p;
        1.0 - bisect(|y| beta_reg2(y, 1.0 - y, b, a) - q, 0.0, 1.0)
    };
    fin(lo + z * (hi - lo))
}

// ── discrete distributions ──────────────────────────────────────────────────────────────

fn binom_pmf(k: f64, n: f64, p: f64) -> f64 {
    if p == 0.0 {
        return if k == 0.0 { 1.0 } else { 0.0 };
    }
    if p == 1.0 {
        return if k == n { 1.0 } else { 0.0 };
    }
    let c = choose(n, k);
    if c.is_finite() && c < 1e300 {
        let v = c * p.powf(k) * (1.0 - p).powf(n - k);
        if v.is_finite() && v > 1e-280 {
            return v;
        }
    }
    (ln_choose(n, k) + k * p.ln() + (n - k) * (-p).ln_1p()).exp()
}

fn binom_cdf(k: f64, n: f64, p: f64) -> f64 {
    if k >= n {
        return 1.0;
    }
    if k < 0.0 {
        return 0.0;
    }
    if p == 0.0 {
        return 1.0;
    }
    if p == 1.0 {
        return 0.0;
    }
    if n <= 1000.0 {
        let mut s = 0.0;
        let mut i = 0.0;
        while i <= k {
            s += binom_pmf(i, n, p);
            i += 1.0;
        }
        return s.min(1.0);
    }
    beta_reg2(1.0 - p, p, n - k, k + 1.0)
}

fn binom_dist(k: f64, n: f64, p: f64, cum: bool) -> R {
    let (k, n) = (tr(k), tr(n));
    need(k >= 0.0 && k <= n && (0.0..=1.0).contains(&p) && n < 1e15)?;
    fin(if cum {
        binom_cdf(k, n, p)
    } else {
        binom_pmf(k, n, p)
    })
}

fn binom_range(n: f64, p: f64, s: f64, s2: f64) -> R {
    let (n, s, s2) = (tr(n), tr(s), tr(s2));
    need(
        (0.0..1e15).contains(&n)
            && (0.0..=1.0).contains(&p)
            && s >= 0.0
            && s <= n
            && s2 >= s
            && s2 <= n,
    )?;
    if s2 - s <= 100_000.0 {
        let mut sum = 0.0;
        let mut i = s;
        while i <= s2 {
            sum += binom_pmf(i, n, p);
            i += 1.0;
        }
        return fin(sum);
    }
    fin(binom_cdf(s2, n, p) - binom_cdf(s - 1.0, n, p))
}

fn binom_inv(n: f64, p: f64, alpha: f64) -> R {
    let n = tr(n);
    need((0.0..1e15).contains(&n) && p > 0.0 && p < 1.0 && alpha > 0.0 && alpha < 1.0)?;
    // Smallest k with CDF(k) ≥ alpha.
    let (mut lo, mut hi) = (0.0_f64, n);
    if binom_cdf(0.0, n, p) >= alpha {
        return Ok(0.0);
    }
    for _ in 0..200 {
        if hi - lo <= 1.0 {
            break;
        }
        let mid = ((lo + hi) / 2.0).floor();
        if binom_cdf(mid, n, p) >= alpha {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    Ok(hi)
}

fn poisson(x: f64, m: f64, cum: bool) -> R {
    need(x >= 0.0 && m >= 0.0)?;
    let x = tr(x);
    if m == 0.0 {
        return Ok(if cum || x == 0.0 { 1.0 } else { 0.0 });
    }
    if cum {
        if x <= 1000.0 {
            let mut s = 0.0;
            let mut i = 0.0;
            while i <= x {
                s += poisson_pmf(i, m);
                i += 1.0;
            }
            return fin(s.min(1.0));
        }
        return fin(gamma_q(x + 1.0, m));
    }
    fin(poisson_pmf(x, m))
}

fn poisson_pmf(x: f64, m: f64) -> f64 {
    if x <= 170.0 && m < 700.0 {
        let v = m.powf(x) * (-m).exp() / gamma(x + 1.0);
        if v.is_finite() && v > 1e-280 {
            return v;
        }
    }
    (x * m.ln() - m - ln_gamma(x + 1.0)).exp()
}

fn negbinom(f: f64, s: f64, p: f64, cum: bool) -> R {
    let (f, s) = (tr(f), tr(s));
    need(f >= 0.0 && s >= 1.0 && p > 0.0 && p < 1.0)?;
    if cum {
        if p == 0.0 {
            return Ok(0.0);
        }
        if p == 1.0 {
            return Ok(1.0);
        }
        return fin(beta_reg2(p, 1.0 - p, s, f + 1.0));
    }
    fin(negbinom_pmf(f, s, p))
}

fn negbinom_pmf(f: f64, s: f64, p: f64) -> f64 {
    if p == 0.0 {
        return 0.0;
    }
    if p == 1.0 {
        return if f == 0.0 { 1.0 } else { 0.0 };
    }
    let c = choose(f + s - 1.0, s - 1.0);
    if c.is_finite() && c < 1e300 {
        let v = c * p.powf(s) * (1.0 - p).powf(f);
        if v.is_finite() && v > 1e-280 {
            return v;
        }
    }
    (ln_choose(f + s - 1.0, s - 1.0) + s * p.ln() + f * (-p).ln_1p()).exp()
}

fn hypgeom_pmf(k: f64, n: f64, big_m: f64, big_n: f64) -> f64 {
    let c = choose(big_m, k) * choose(big_n - big_m, n - k) / choose(big_n, n);
    if c.is_finite() && c > 1e-280 {
        return c;
    }
    (ln_choose(big_m, k) + ln_choose(big_n - big_m, n - k) - ln_choose(big_n, n)).exp()
}

fn hypgeom(k: f64, n: f64, m: f64, big_n: f64, cum: bool) -> R {
    let (k, n, m, big_n) = (tr(k), tr(n), tr(m), tr(big_n));
    need(
        k >= 0.0
            && n > 0.0
            && m > 0.0
            && big_n > 0.0
            && n <= big_n
            && m <= big_n
            && k <= n.min(m)
            && k >= (n - big_n + m).max(0.0)
            && big_n < 1e15,
    )?;
    if !cum {
        return fin(hypgeom_pmf(k, n, m, big_n));
    }
    let lo = (n - big_n + m).max(0.0);
    need(k - lo <= 10_000_000.0)?;
    let mut s = 0.0;
    let mut i = lo;
    while i <= k {
        s += hypgeom_pmf(i, n, m, big_n);
        i += 1.0;
    }
    fin(s.min(1.0))
}

// ── tests on data ───────────────────────────────────────────────────────────────────────

fn numbers_of(ev: &mut Ev<'_>, a: &[Expr], i: usize) -> Result<Vec<f64>, ErrorKind> {
    let arr = arg_array(ev, a, i)?;
    let mut out = Vec::new();
    for v in &arr.data {
        match v {
            Value::Number(n) => out.push(*n),
            Value::Error(e) => return Err(*e),
            _ => {}
        }
    }
    Ok(out)
}

fn mean_var(v: &[f64]) -> (f64, f64) {
    let n = v.len() as f64;
    let m = v.iter().sum::<f64>() / n;
    let ss: f64 = v.iter().map(|x| (x - m) * (x - m)).sum();
    (m, ss / (n - 1.0))
}

fn z_test(ev: &mut Ev<'_>, a: &[Expr]) -> R {
    let v = numbers_of(ev, a, 0)?;
    let x = num(ev, a, 1)?;
    let sigma = match a.get(2) {
        None | Some(Expr::Missing) => None,
        Some(e) => Some(ev.num(e)?),
    };
    if v.is_empty() {
        return Err(NA);
    }
    let n = v.len() as f64;
    let (m, var) = mean_var(&v);
    let s = match sigma {
        Some(s) => s,
        None => {
            if v.len() < 2 {
                return Err(Div0);
            }
            var.sqrt()
        }
    };
    if s == 0.0 {
        return Err(Div0);
    }
    fin(1.0 - norm_cdf((m - x) / (s / n.sqrt())))
}

fn t_test(ev: &mut Ev<'_>, a: &[Expr]) -> R {
    let x = arg_array(ev, a, 0)?;
    let y = arg_array(ev, a, 1)?;
    let tails = tr(num(ev, a, 2)?);
    let kind = tr(num(ev, a, 3)?);
    need(tails == 1.0 || tails == 2.0)?;
    need((1.0..=3.0).contains(&kind))?;
    let (t, df) = if kind == 1.0 {
        if x.data.len() != y.data.len() {
            return Err(NA);
        }
        let mut d = Vec::new();
        for (p, q) in x.data.iter().zip(&y.data) {
            if let Value::Error(e) = p {
                return Err(*e);
            }
            if let Value::Error(e) = q {
                return Err(*e);
            }
            if let (Value::Number(p), Value::Number(q)) = (p, q) {
                d.push(p - q);
            }
        }
        if d.len() < 2 {
            return Err(Div0);
        }
        let n = d.len() as f64;
        let (m, var) = mean_var(&d);
        if var == 0.0 {
            return Err(Div0);
        }
        (m / (var / n).sqrt(), n - 1.0)
    } else {
        let collect = |arr: &crate::eval::Array2| -> Result<Vec<f64>, ErrorKind> {
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
        let (xs, ys) = (collect(&x)?, collect(&y)?);
        if xs.len() < 2 || ys.len() < 2 {
            return Err(Div0);
        }
        let (n1, n2) = (xs.len() as f64, ys.len() as f64);
        let (m1, v1) = mean_var(&xs);
        let (m2, v2) = mean_var(&ys);
        if kind == 2.0 {
            let sp = ((n1 - 1.0) * v1 + (n2 - 1.0) * v2) / (n1 + n2 - 2.0);
            let se = (sp * (1.0 / n1 + 1.0 / n2)).sqrt();
            if se == 0.0 {
                return Err(Div0);
            }
            ((m1 - m2) / se, n1 + n2 - 2.0)
        } else {
            let (a1, a2) = (v1 / n1, v2 / n2);
            let se = (a1 + a2).sqrt();
            if se == 0.0 {
                return Err(Div0);
            }
            let df = (a1 + a2).powi(2) / (a1 * a1 / (n1 - 1.0) + a2 * a2 / (n2 - 1.0));
            ((m1 - m2) / se, df)
        }
    };
    fin(tails * t_tail(t, df))
}

fn chisq_test(ev: &mut Ev<'_>, a: &[Expr]) -> R {
    let obs = arg_array(ev, a, 0)?;
    let exp = arg_array(ev, a, 1)?;
    if obs.rows != exp.rows || obs.cols != exp.cols {
        return Err(NA);
    }
    let mut chi = 0.0;
    for (o, e) in obs.data.iter().zip(&exp.data) {
        if let Value::Error(x) = o {
            return Err(*x);
        }
        if let Value::Error(x) = e {
            return Err(*x);
        }
        if let (Value::Number(o), Value::Number(e)) = (o, e) {
            if *e == 0.0 {
                return Err(Div0);
            }
            chi += (o - e) * (o - e) / e;
        }
    }
    let df = if obs.rows > 1 && obs.cols > 1 {
        (obs.rows - 1) * (obs.cols - 1)
    } else {
        obs.rows * obs.cols - 1
    };
    if df == 0 {
        return Err(NA);
    }
    fin(gamma_q(df as f64 / 2.0, chi / 2.0))
}

fn f_test(ev: &mut Ev<'_>, a: &[Expr]) -> R {
    let x = numbers_of(ev, a, 0)?;
    let y = numbers_of(ev, a, 1)?;
    if x.len() < 2 || y.len() < 2 {
        return Err(Div0);
    }
    let (_, v1) = mean_var(&x);
    let (_, v2) = mean_var(&y);
    if v1 == 0.0 || v2 == 0.0 {
        return Err(Div0);
    }
    let f = v1 / v2;
    let (d1, d2) = ((x.len() - 1) as f64, (y.len() - 1) as f64);
    let c = f_cdf(f, d1, d2);
    let s = f_sf(f, d1, d2);
    fin((2.0 * c.min(s)).min(1.0))
}

// ── registration ────────────────────────────────────────────────────────────────────────

macro_rules! reg {
    ($r:expr, $name:literal, $min:expr, $max:expr, |$ev:ident, $a:ident| $body:expr) => {
        $r.add($name, $min, $max, |$ev, $a| ret((|| -> R { $body })()));
    };
}

pub fn register(r: &mut Registry) {
    // Normal.
    reg!(r, "NORM.DIST", 4, 4, |ev, a| {
        let (x, m, s) = (num(ev, a, 0)?, num(ev, a, 1)?, num(ev, a, 2)?);
        let c = flag(ev, a, 3)?;
        norm_dist(x, m, s, c)
    });
    reg!(r, "NORMDIST", 4, 4, |ev, a| {
        let (x, m, s) = (num(ev, a, 0)?, num(ev, a, 1)?, num(ev, a, 2)?);
        let c = flag(ev, a, 3)?;
        norm_dist(x, m, s, c)
    });
    reg!(r, "NORM.INV", 3, 3, |ev, a| {
        let (p, m, s) = (num(ev, a, 0)?, num(ev, a, 1)?, num(ev, a, 2)?);
        norm_quantile(p, m, s)
    });
    r.alias("NORMINV", "NORM.INV");
    reg!(r, "NORM.S.DIST", 2, 2, |ev, a| {
        let z = num(ev, a, 0)?;
        let c = flag(ev, a, 1)?;
        norm_dist(z, 0.0, 1.0, c)
    });
    reg!(r, "NORMSDIST", 1, 1, |ev, a| {
        norm_dist(num(ev, a, 0)?, 0.0, 1.0, true)
    });
    reg!(r, "NORM.S.INV", 1, 1, |ev, a| {
        norm_quantile(num(ev, a, 0)?, 0.0, 1.0)
    });
    r.alias("NORMSINV", "NORM.S.INV");
    reg!(r, "PHI", 1, 1, |ev, a| fin(norm_pdf(num(ev, a, 0)?)));
    reg!(r, "GAUSS", 1, 1, |ev, a| {
        let z = num(ev, a, 0)?;
        // Φ(z) - 1/2 without cancellation near 0.
        fin(0.5 * super::special::erf(z / std::f64::consts::SQRT_2))
    });
    reg!(r, "LOGNORM.DIST", 4, 4, |ev, a| {
        let (x, m, s) = (num(ev, a, 0)?, num(ev, a, 1)?, num(ev, a, 2)?);
        let c = flag(ev, a, 3)?;
        lognorm_dist(x, m, s, c)
    });
    reg!(r, "LOGNORMDIST", 3, 3, |ev, a| {
        let (x, m, s) = (num(ev, a, 0)?, num(ev, a, 1)?, num(ev, a, 2)?);
        lognorm_dist(x, m, s, true)
    });
    reg!(r, "LOGNORM.INV", 3, 3, |ev, a| {
        let (p, m, s) = (num(ev, a, 0)?, num(ev, a, 1)?, num(ev, a, 2)?);
        lognorm_inv(p, m, s)
    });
    r.alias("LOGINV", "LOGNORM.INV");
    reg!(r, "CONFIDENCE.NORM", 3, 3, |ev, a| {
        let (al, sd, n) = (num(ev, a, 0)?, num(ev, a, 1)?, tr(num(ev, a, 2)?));
        need(al > 0.0 && al < 1.0 && sd > 0.0 && n >= 1.0)?;
        fin(norm_inv(1.0 - al / 2.0) * sd / n.sqrt())
    });
    r.alias("CONFIDENCE", "CONFIDENCE.NORM");
    reg!(r, "CONFIDENCE.T", 3, 3, |ev, a| {
        let (al, sd, n) = (num(ev, a, 0)?, num(ev, a, 1)?, tr(num(ev, a, 2)?));
        need(al > 0.0 && al < 1.0 && sd > 0.0 && n >= 1.0)?;
        if n == 1.0 {
            return Err(Div0);
        }
        fin(t_inv_2t(al, n - 1.0)? * sd / n.sqrt())
    });
    reg!(r, "Z.TEST", 2, 3, |ev, a| z_test(ev, a));
    r.alias("ZTEST", "Z.TEST");

    // Student t.
    reg!(r, "T.DIST", 3, 3, |ev, a| {
        let (x, df) = (num(ev, a, 0)?, num(ev, a, 1)?);
        let c = flag(ev, a, 2)?;
        let df = t_df(df)?;
        fin(if c { t_cdf(x, df) } else { t_pdf(x, df) })
    });
    reg!(r, "T.DIST.RT", 2, 2, |ev, a| {
        let (x, df) = (num(ev, a, 0)?, num(ev, a, 1)?);
        let df = t_df(df)?;
        fin(t_cdf(-x, df))
    });
    reg!(r, "T.DIST.2T", 2, 2, |ev, a| {
        let (x, df) = (num(ev, a, 0)?, num(ev, a, 1)?);
        need(x >= 0.0)?;
        let df = t_df(df)?;
        fin(2.0 * t_tail(x, df))
    });
    reg!(r, "TDIST", 3, 3, |ev, a| {
        let (x, df, tails) = (num(ev, a, 0)?, num(ev, a, 1)?, tr(num(ev, a, 2)?));
        need(x >= 0.0 && (tails == 1.0 || tails == 2.0))?;
        let df = t_df(df)?;
        fin(tails * t_tail(x, df))
    });
    reg!(r, "T.INV", 2, 2, |ev, a| {
        let (p, df) = (num(ev, a, 0)?, num(ev, a, 1)?);
        t_inv(p, df)
    });
    reg!(r, "T.INV.2T", 2, 2, |ev, a| {
        let (p, df) = (num(ev, a, 0)?, num(ev, a, 1)?);
        t_inv_2t(p, df)
    });
    r.alias("TINV", "T.INV.2T");
    reg!(r, "T.TEST", 4, 4, |ev, a| t_test(ev, a));
    r.alias("TTEST", "T.TEST");

    // Chi-squared.
    reg!(r, "CHISQ.DIST", 3, 3, |ev, a| {
        let (x, df) = (num(ev, a, 0)?, num(ev, a, 1)?);
        let c = flag(ev, a, 2)?;
        chisq_dist(x, df, c)
    });
    reg!(r, "CHISQ.DIST.RT", 2, 2, |ev, a| {
        let (x, df) = (num(ev, a, 0)?, num(ev, a, 1)?);
        chisq_rt(x, df)
    });
    r.alias("CHIDIST", "CHISQ.DIST.RT");
    reg!(r, "CHISQ.INV", 2, 2, |ev, a| {
        let (p, df) = (num(ev, a, 0)?, num(ev, a, 1)?);
        chisq_inv(p, df)
    });
    reg!(r, "CHISQ.INV.RT", 2, 2, |ev, a| {
        let (p, df) = (num(ev, a, 0)?, num(ev, a, 1)?);
        chisq_inv_rt(p, df)
    });
    r.alias("CHIINV", "CHISQ.INV.RT");
    reg!(r, "CHISQ.TEST", 2, 2, |ev, a| chisq_test(ev, a));
    r.alias("CHITEST", "CHISQ.TEST");

    // F.
    reg!(r, "F.DIST", 4, 4, |ev, a| {
        let (x, d1, d2) = (num(ev, a, 0)?, num(ev, a, 1)?, num(ev, a, 2)?);
        let c = flag(ev, a, 3)?;
        f_dist(x, d1, d2, c)
    });
    reg!(r, "F.DIST.RT", 3, 3, |ev, a| {
        let (x, d1, d2) = (num(ev, a, 0)?, num(ev, a, 1)?, num(ev, a, 2)?);
        f_rt(x, d1, d2)
    });
    r.alias("FDIST", "F.DIST.RT");
    reg!(r, "F.INV", 3, 3, |ev, a| {
        let (p, d1, d2) = (num(ev, a, 0)?, num(ev, a, 1)?, num(ev, a, 2)?);
        f_inv(p, d1, d2)
    });
    reg!(r, "F.INV.RT", 3, 3, |ev, a| {
        let (p, d1, d2) = (num(ev, a, 0)?, num(ev, a, 1)?, num(ev, a, 2)?);
        f_inv_rt(p, d1, d2)
    });
    r.alias("FINV", "F.INV.RT");
    reg!(r, "F.TEST", 2, 2, |ev, a| f_test(ev, a));
    r.alias("FTEST", "F.TEST");

    // Gamma and beta.
    reg!(r, "GAMMA", 1, 1, |ev, a| {
        let x = num(ev, a, 0)?;
        need(!(x <= 0.0 && x == x.floor()))?;
        fin(gamma(x))
    });
    reg!(r, "GAMMALN", 1, 1, |ev, a| {
        let x = num(ev, a, 0)?;
        need(x > 0.0)?;
        fin(ln_gamma(x))
    });
    r.alias("GAMMALN.PRECISE", "GAMMALN");
    reg!(r, "GAMMA.DIST", 4, 4, |ev, a| {
        let (x, al, b) = (num(ev, a, 0)?, num(ev, a, 1)?, num(ev, a, 2)?);
        let c = flag(ev, a, 3)?;
        gamma_dist(x, al, b, c)
    });
    r.alias("GAMMADIST", "GAMMA.DIST");
    reg!(r, "GAMMA.INV", 3, 3, |ev, a| {
        let (p, al, b) = (num(ev, a, 0)?, num(ev, a, 1)?, num(ev, a, 2)?);
        gamma_inv(p, al, b)
    });
    r.alias("GAMMAINV", "GAMMA.INV");
    reg!(r, "BETA.DIST", 4, 6, |ev, a| {
        let (x, al, b) = (num(ev, a, 0)?, num(ev, a, 1)?, num(ev, a, 2)?);
        let c = flag(ev, a, 3)?;
        let lo = opt(ev, a, 4, 0.0)?;
        let hi = opt(ev, a, 5, 1.0)?;
        beta_dist(x, al, b, c, lo, hi)
    });
    reg!(r, "BETADIST", 3, 5, |ev, a| {
        let (x, al, b) = (num(ev, a, 0)?, num(ev, a, 1)?, num(ev, a, 2)?);
        let lo = opt(ev, a, 3, 0.0)?;
        let hi = opt(ev, a, 4, 1.0)?;
        beta_dist(x, al, b, true, lo, hi)
    });
    reg!(r, "BETA.INV", 3, 5, |ev, a| {
        let (p, al, b) = (num(ev, a, 0)?, num(ev, a, 1)?, num(ev, a, 2)?);
        let lo = opt(ev, a, 3, 0.0)?;
        let hi = opt(ev, a, 4, 1.0)?;
        beta_inv(p, al, b, lo, hi)
    });
    r.alias("BETAINV", "BETA.INV");

    // Exponential and Weibull.
    reg!(r, "EXPON.DIST", 3, 3, |ev, a| {
        let (x, l) = (num(ev, a, 0)?, num(ev, a, 1)?);
        let c = flag(ev, a, 2)?;
        need(x >= 0.0 && l > 0.0)?;
        fin(if c {
            -(-l * x).exp_m1()
        } else {
            l * (-l * x).exp()
        })
    });
    r.alias("EXPONDIST", "EXPON.DIST");
    reg!(r, "WEIBULL.DIST", 4, 4, |ev, a| {
        let (x, al, b) = (num(ev, a, 0)?, num(ev, a, 1)?, num(ev, a, 2)?);
        let c = flag(ev, a, 3)?;
        need(x >= 0.0 && al > 0.0 && b > 0.0)?;
        let u = (x / b).powf(al);
        fin(if c {
            -(-u).exp_m1()
        } else {
            al / b * (x / b).powf(al - 1.0) * (-u).exp()
        })
    });
    r.alias("WEIBULL", "WEIBULL.DIST");

    // Discrete.
    reg!(r, "BINOM.DIST", 4, 4, |ev, a| {
        let (k, n, p) = (num(ev, a, 0)?, num(ev, a, 1)?, num(ev, a, 2)?);
        let c = flag(ev, a, 3)?;
        binom_dist(k, n, p, c)
    });
    r.alias("BINOMDIST", "BINOM.DIST");
    reg!(r, "BINOM.DIST.RANGE", 3, 4, |ev, a| {
        let (n, p, s) = (num(ev, a, 0)?, num(ev, a, 1)?, num(ev, a, 2)?);
        let s2 = match a.get(3) {
            None => s,
            Some(_) => num(ev, a, 3)?,
        };
        binom_range(n, p, s, s2)
    });
    reg!(r, "BINOM.INV", 3, 3, |ev, a| {
        let (n, p, al) = (num(ev, a, 0)?, num(ev, a, 1)?, num(ev, a, 2)?);
        binom_inv(n, p, al)
    });
    r.alias("CRITBINOM", "BINOM.INV");
    reg!(r, "POISSON.DIST", 3, 3, |ev, a| {
        let (x, m) = (num(ev, a, 0)?, num(ev, a, 1)?);
        let c = flag(ev, a, 2)?;
        poisson(x, m, c)
    });
    r.alias("POISSON", "POISSON.DIST");
    reg!(r, "NEGBINOM.DIST", 4, 4, |ev, a| {
        let (f, s, p) = (num(ev, a, 0)?, num(ev, a, 1)?, num(ev, a, 2)?);
        let c = flag(ev, a, 3)?;
        negbinom(f, s, p, c)
    });
    reg!(r, "NEGBINOMDIST", 3, 3, |ev, a| {
        let (f, s, p) = (num(ev, a, 0)?, num(ev, a, 1)?, num(ev, a, 2)?);
        negbinom(f, s, p, false)
    });
    reg!(r, "HYPGEOM.DIST", 5, 5, |ev, a| {
        let (k, n, m, nn) = (
            num(ev, a, 0)?,
            num(ev, a, 1)?,
            num(ev, a, 2)?,
            num(ev, a, 3)?,
        );
        let c = flag(ev, a, 4)?;
        hypgeom(k, n, m, nn, c)
    });
    reg!(r, "HYPGEOMDIST", 4, 4, |ev, a| {
        let (k, n, m, nn) = (
            num(ev, a, 0)?,
            num(ev, a, 1)?,
            num(ev, a, 2)?,
            num(ev, a, 3)?,
        );
        hypgeom(k, n, m, nn, false)
    });

    // Fisher transformation.
    reg!(r, "FISHER", 1, 1, |ev, a| {
        let x = num(ev, a, 0)?;
        need(x > -1.0 && x < 1.0)?;
        fin(x.atanh())
    });
    reg!(r, "FISHERINV", 1, 1, |ev, a| fin(num(ev, a, 0)?.tanh()));
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

    fn err(src: &str, e: ErrorKind) {
        assert_eq!(ev(src), Value::Error(e), "{src}");
    }

    #[test]
    fn normal_family() {
        approx("NORM.DIST(42,40,1.5,TRUE)", 0.908_788_780_274_132);
        approx("NORM.DIST(42,40,1.5,FALSE)", 0.109_340_049_783_996);
        approx("NORM.INV(0.908789,40,1.5)", 42.000_002_009_566);
        approx("NORM.S.DIST(1.333333,TRUE)", 0.908_788_725_604_095);
        approx("NORM.S.INV(0.908789)", 1.333_334_673_044_108);
        approx("NORMSDIST(1)", 0.841_344_746_068_543);
        approx("PHI(0.75)", 0.301_137_432_154_804);
        approx("GAUSS(2)", 0.477_249_868_051_821);
        approx("LOGNORM.DIST(4,3.5,1.2,TRUE)", 0.039_083_555_706_800);
        approx("LOGNORM.INV(0.039084,3.5,1.2)", 4.000_025_218_680_635);
        approx("CONFIDENCE.NORM(0.05,2.5,50)", 0.692_951_912_174_839);
        approx("CONFIDENCE.T(0.05,1,50)", 0.284_196_855_495_730);
        err("NORM.DIST(1,0,0,TRUE)", ErrorKind::Num);
        err("NORM.INV(1,0,1)", ErrorKind::Num);
        err("CONFIDENCE.T(0.05,1,1)", ErrorKind::Div0);
    }

    #[test]
    fn t_chi_f() {
        approx("T.DIST(60,1,TRUE)", 0.994_695_326_367_377);
        approx("T.DIST(8,3,FALSE)", 0.000_736_906_520_946_926);
        approx("T.DIST.2T(1.959999998,60)", 0.054_644_929_975_920_9);
        approx("T.DIST.RT(1.959999998,60)", 0.027_322_464_987_960_4);
        approx("T.INV(0.75,2)", 0.816_496_580_927_726);
        approx("T.INV.2T(0.546449,60)", 0.606_533_075_825_755);
        approx("TDIST(1.959999998,60,2)", 0.054_644_929_975_920_9);
        approx("CHISQ.DIST(0.5,1,TRUE)", 0.520_499_877_813_047);
        approx("CHISQ.DIST(2,3,FALSE)", 0.207_553_748_710_297);
        approx("CHISQ.DIST.RT(18.307,10)", 0.050_000_589_091_398);
        approx("CHISQ.INV(0.93,1)", 3.283_020_286_759_54);
        approx("CHISQ.INV.RT(0.050001,10)", 18.306_973_456_961_05);
        approx("F.DIST(15.2069,6,4,TRUE)", 0.990_000_043_002_763);
        approx("F.DIST(15.2069,6,4,FALSE)", 0.001_223_791_708_783_18);
        approx("F.DIST.RT(15.2068649,6,4)", 0.009_999_999_952_464_6);
        approx("F.INV(0.01,6,4)", 0.109_309_914_124_578_5);
        approx("F.INV.RT(0.01,6,4)", 15.206_864_861_157_52);
        err("T.DIST(1,0,TRUE)", ErrorKind::Num);
        err("CHISQ.DIST(-1,2,TRUE)", ErrorKind::Num);
    }

    #[test]
    fn gamma_beta() {
        approx("GAMMA(2.5)", 1.329_340_388_179_14);
        approx("GAMMALN(4.5)", 2.453_736_570_842_44);
        approx(
            "GAMMA.DIST(10.00001131,9,2,FALSE)",
            0.032_639_130_418_293_96,
        );
        approx("GAMMA.DIST(10.00001131,9,2,TRUE)", 0.068_094_003_869_787_2);
        approx("GAMMA.INV(0.068094,9,2)", 10.000_011_191_437_18);
        approx("BETA.DIST(2,8,10,TRUE,1,3)", 0.685_470_581_054_687);
        approx("BETA.DIST(2,8,10,FALSE,1,3)", 1.483_764_648_437_5);
        approx("BETA.INV(0.685470581,8,10,1,3)", 2.0);
        err("GAMMA(0)", ErrorKind::Num);
        err("GAMMA(-2)", ErrorKind::Num);
        err("BETA.DIST(0.5,0,1,TRUE)", ErrorKind::Num);
    }

    #[test]
    fn discrete() {
        approx("BINOM.DIST(6,10,0.5,FALSE)", 0.205_078_125);
        approx("BINOM.DIST(6,10,0.5,TRUE)", 0.828_125);
        approx("BINOM.DIST.RANGE(60,0.75,45,50)", 0.523_629_793_486_118);
        approx("BINOM.INV(6,0.5,0.75)", 4.0);
        approx("POISSON.DIST(2,5,TRUE)", 0.124_652_019_483_081);
        approx("POISSON.DIST(2,5,FALSE)", 0.084_224_337_488_568_32);
        approx("NEGBINOM.DIST(10,5,0.25,TRUE)", 0.313_514_058_478_176_6);
        approx("NEGBINOM.DIST(10,5,0.25,FALSE)", 0.055_048_660_375_177_88);
        approx("HYPGEOM.DIST(1,4,8,20,TRUE)", 0.465_428_276_573_787_4);
        approx("HYPGEOM.DIST(1,4,8,20,FALSE)", 0.363_261_093_911_249);
        approx("EXPON.DIST(0.2,10,TRUE)", 0.864_664_716_763_387);
        approx("WEIBULL.DIST(105,20,100,TRUE)", 0.929_581_390_069_83);
        approx("WEIBULL.DIST(105,20,100,FALSE)", 0.035_588_864_024_504_33);
        approx("FISHER(0.75)", 0.972_955_074_527_657);
        approx("FISHERINV(0.972955)", 0.749_999_967_394_148_4);
        err("BINOM.DIST(11,10,0.5,TRUE)", ErrorKind::Num);
        err("FISHER(1)", ErrorKind::Num);
    }

    #[test]
    fn hostile_inputs_do_not_hang_or_panic() {
        for src in [
            "GAMMA.INV(0.5,1E300,1E300)",
            "GAMMA.DIST(1E300,1E300,1,TRUE)",
            "BETA.INV(0.5,1E-300,1E300)",
            "T.INV(1E-300,1)",
            "BINOM.INV(1E14,0.5,0.5)",
            "BINOM.DIST.RANGE(1E14,0.5,0,1E14)",
            "HYPGEOM.DIST(1E9,1E10,1E10,1E14,TRUE)",
            "POISSON.DIST(1E12,1E12,TRUE)",
            "F.INV(0.5,1E9,1)",
            "CHISQ.INV.RT(1E-300,1E10)",
        ] {
            let _ = ev(src);
        }
    }
}
