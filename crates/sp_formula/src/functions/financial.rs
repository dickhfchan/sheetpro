//! Financial functions (time value of money, cash-flow analysis, depreciation).

use super::{Agg, Registry, arg_array, collect_numbers, ret};
use crate::ast::Expr;
use crate::eval::{Ev, EvalValue};
use sp_core::{ErrorKind, Value};

fn args_n(ev: &mut Ev<'_>, args: &[Expr], defaults: &[Option<f64>]) -> Result<Vec<f64>, ErrorKind> {
    let mut out = Vec::with_capacity(defaults.len());
    for (i, d) in defaults.iter().enumerate() {
        let v = match (args.get(i), d) {
            (None | Some(Expr::Missing), Some(d)) => *d,
            (Some(e), _) => ev.num(e)?,
            (None, None) => return Err(ErrorKind::Value),
        };
        out.push(v);
    }
    Ok(out)
}

/// Like `args_n`, but logicals are `#VALUE!` (analysis-style functions).
fn args_strict(ev: &mut Ev<'_>, args: &[Expr], defaults: &[Option<f64>]) -> Result<Vec<f64>, ErrorKind> {
    for e in args.iter().take(defaults.len()) {
        if !matches!(e, Expr::Missing) && matches!(ev.eval_scalar(e), Value::Bool(_)) {
            return Err(ErrorKind::Value);
        }
    }
    args_n(ev, args, defaults)
}

fn fin(x: f64) -> Result<f64, ErrorKind> {
    if x.is_finite() {
        Ok(x)
    } else {
        Err(ErrorKind::Num)
    }
}

pub fn pmt(rate: f64, nper: f64, pv: f64, fv: f64, typ: f64) -> Result<f64, ErrorKind> {
    if rate <= -1.0 {
        return Err(ErrorKind::Num);
    }
    if nper == 0.0 {
        return Err(ErrorKind::Num);
    }
    if rate == 0.0 {
        return fin(-(pv + fv) / nper);
    }
    let t = if typ != 0.0 { 1.0 } else { 0.0 };
    let f = (1.0 + rate).powf(nper);
    fin(-(rate * (pv * f + fv)) / ((1.0 + rate * t) * (f - 1.0)))
}

pub fn fv(rate: f64, nper: f64, pmt: f64, pv: f64, typ: f64) -> Result<f64, ErrorKind> {
    if rate == -1.0 && nper < 0.0 {
        return Err(ErrorKind::Div0);
    }
    if rate == 0.0 {
        return fin(-(pv + pmt * nper));
    }
    let t = if typ != 0.0 { 1.0 } else { 0.0 };
    let f = (1.0 + rate).powf(nper);
    fin(-(pv * f + pmt * (1.0 + rate * t) * (f - 1.0) / rate))
}

pub fn pv(rate: f64, nper: f64, pmt: f64, fv: f64, typ: f64) -> Result<f64, ErrorKind> {
    if rate == 0.0 {
        return fin(-(fv + pmt * nper));
    }
    let t = if typ != 0.0 { 1.0 } else { 0.0 };
    let f = (1.0 + rate).powf(nper);
    fin(-(fv + pmt * (1.0 + rate * t) * (f - 1.0) / rate) / f)
}

fn nper(rate: f64, pmt: f64, pv: f64, fv: f64, typ: f64) -> Result<f64, ErrorKind> {
    if rate == 0.0 {
        if pmt == 0.0 {
            return Err(ErrorKind::Num);
        }
        return fin(-(pv + fv) / pmt);
    }
    let t = if typ != 0.0 { 1.0 } else { 0.0 };
    let a = pmt * (1.0 + rate * t) / rate;
    let num = a - fv;
    let den = pv + a;
    if num / den <= 0.0 {
        return Err(ErrorKind::Num);
    }
    fin((num / den).ln() / (1.0 + rate).ln())
}

fn ipmt(rate: f64, per: f64, nper: f64, pv_: f64, fv_: f64, typ: f64) -> Result<f64, ErrorKind> {
    if per < 1.0 || per >= nper + 1.0 {
        return Err(ErrorKind::Num);
    }
    let p = pmt(rate, nper, pv_, fv_, typ)?;
    let t = typ != 0.0;
    if per == 1.0 && t {
        return Ok(0.0);
    }
    let balance = if t {
        fv(rate, per - 2.0, p, pv_, 1.0)? - p
    } else {
        fv(rate, per - 1.0, p, pv_, 0.0)?
    };
    fin(balance * rate)
}

fn rate(nper: f64, pmt_: f64, pv_: f64, fv_: f64, typ: f64, guess: f64) -> Result<f64, ErrorKind> {
    let f = |r: f64| -> f64 {
        if r.abs() < 1e-12 {
            pv_ + pmt_ * nper + fv_
        } else {
            let t = if typ != 0.0 { 1.0 } else { 0.0 };
            let g = (1.0 + r).powf(nper);
            pv_ * g + pmt_ * (1.0 + r * t) * (g - 1.0) / r + fv_
        }
    };
    let mut r = guess;
    for _ in 0..100 {
        let y = f(r);
        let h = 1e-7_f64.max(r.abs() * 1e-7);
        let dy = (f(r + h) - f(r - h)) / (2.0 * h);
        if dy == 0.0 || !dy.is_finite() {
            return Err(ErrorKind::Num);
        }
        let nr = r - y / dy;
        if !nr.is_finite() || nr <= -1.0 {
            return Err(ErrorKind::Num);
        }
        if (nr - r).abs() < 1e-10 {
            return Ok(nr);
        }
        r = nr;
    }
    Err(ErrorKind::Num)
}

fn npv_of(rate: f64, flows: &[f64]) -> f64 {
    flows
        .iter()
        .enumerate()
        .map(|(i, c)| c / (1.0 + rate).powi(i as i32 + 1))
        .sum()
}

fn irr(flows: &[f64], guess: f64) -> Result<f64, ErrorKind> {
    if !flows.iter().any(|c| *c > 0.0) || !flows.iter().any(|c| *c < 0.0) {
        return Err(ErrorKind::Num);
    }
    if let Ok(r) = irr_newton(flows, guess) {
        return Ok(r);
    }
    let f = |r: f64| -> Result<f64, ErrorKind> {
        fin(flows
            .iter()
            .enumerate()
            .map(|(i, c)| c / (1.0 + r).powi(i as i32))
            .sum::<f64>())
    };
    let r = super::bonds::solve(&f, guess.clamp(-0.99, 10.0), -1.0 + 1e-9, 1e3)?;
    let scale = flows.iter().map(|c| c.abs()).fold(0.0, f64::max).max(1.0);
    if f(r)?.abs() > 1e-7 * scale {
        return Err(ErrorKind::Num);
    }
    Ok(r)
}

fn irr_newton(flows: &[f64], guess: f64) -> Result<f64, ErrorKind> {
    let f = |r: f64| {
        flows
            .iter()
            .enumerate()
            .map(|(i, c)| c / (1.0 + r).powi(i as i32))
            .sum::<f64>()
    };
    let mut r = guess;
    for _ in 0..100 {
        let y = f(r);
        let dy: f64 = flows
            .iter()
            .enumerate()
            .map(|(i, c)| -(i as f64) * c / (1.0 + r).powi(i as i32 + 1))
            .sum();
        if dy == 0.0 || !dy.is_finite() {
            return Err(ErrorKind::Num);
        }
        let nr = r - y / dy;
        if !nr.is_finite() || nr <= -1.0 {
            return Err(ErrorKind::Num);
        }
        if (nr - r).abs() < 1e-10 {
            return Ok(nr);
        }
        r = nr;
    }
    Err(ErrorKind::Num)
}

fn flows_from(ev: &mut Ev<'_>, args: &[Expr], i: usize) -> Result<Vec<f64>, ErrorKind> {
    let a = arg_array(ev, args, i)?;
    let mut out = Vec::new();
    for v in &a.data {
        match v {
            Value::Number(n) => out.push(*n),
            Value::Error(e) => return Err(*e),
            _ => {}
        }
    }
    Ok(out)
}

fn ddb(cost: f64, salvage: f64, life: f64, period: f64, factor: f64) -> Result<f64, ErrorKind> {
    if cost < 0.0 || salvage < 0.0 || life <= 0.0 || period <= 0.0 || period > life || factor <= 0.0
    {
        return Err(ErrorKind::Num);
    }
    let mut book = cost;
    let mut dep = 0.0;
    for _ in 0..period.ceil() as u64 {
        dep = (book * factor / life).min((book - salvage).max(0.0));
        book -= dep;
    }
    Ok(dep)
}

fn db(cost: f64, salvage: f64, life: f64, period: f64, month: f64) -> Result<f64, ErrorKind> {
    if cost < 0.0
        || salvage < 0.0
        || life <= 0.0
        || period <= 0.0
        || !(1.0..=12.0).contains(&month)
        || period > life + 1.0
    {
        return Err(ErrorKind::Num);
    }
    if cost == 0.0 {
        return Ok(0.0);
    }
    let rate = ((1.0 - (salvage / cost).powf(1.0 / life)) * 1000.0).round() / 1000.0;
    let mut total = cost * rate * month / 12.0;
    if period == 1.0 {
        return Ok(total);
    }
    let mut dep = 0.0;
    for p in 2..=(period as u64) {
        dep = if p as f64 == life + 1.0 {
            (cost - total) * rate * (12.0 - month) / 12.0
        } else {
            (cost - total) * rate
        };
        total += dep;
    }
    Ok(dep)
}

fn cum(ev: &mut Ev<'_>, args: &[Expr], principal: bool) -> Result<f64, ErrorKind> {
    let v = args_strict(ev, args, &[None, None, None, None, None, None])?;
    let [rate, n, pv_, start, end, typ] = v.as_slice() else {
        return Err(ErrorKind::Value);
    };
    let (start, end, n) = (start.ceil(), end.trunc(), *n);
    if *rate <= 0.0
        || n <= 0.0
        || *pv_ <= 0.0
        || start < 1.0
        || end < start
        || end > n
        || (*typ != 0.0 && *typ != 1.0)
    {
        return Err(ErrorKind::Num);
    }
    let p = pmt(*rate, n, *pv_, 0.0, *typ)?;
    let mut total = 0.0;
    let mut per = start;
    while per <= end {
        let i = ipmt(*rate, per, n, *pv_, 0.0, *typ)?;
        total += if principal { p - i } else { i };
        per += 1.0;
    }
    fin(total)
}

/// Depreciation for whole period `p` (1-based) under declining balance with an optional
/// switch to straight line, given the book value at the start of the period.
fn vdb_schedule(cost: f64, salvage: f64, life: f64, factor: f64, no_switch: bool, periods: usize) -> Vec<f64> {
    let mut out = Vec::with_capacity(periods);
    let mut book = cost;
    let mut sl: Option<f64> = None;
    for p in 0..periods {
        let remaining = life - p as f64;
        let ddb = (book * factor / life).min((book - salvage).max(0.0)).max(0.0);
        let dep = match sl {
            Some(s) => s.min((book - salvage).max(0.0)),
            None if !no_switch && remaining > 0.0 => {
                let s = ((book - salvage) / remaining).max(0.0);
                if s > ddb {
                    sl = Some(s);
                    s
                } else {
                    ddb
                }
            }
            None => ddb,
        };
        out.push(dep);
        book -= dep;
    }
    out
}

fn vdb(ev: &mut Ev<'_>, args: &[Expr]) -> Result<f64, ErrorKind> {
    let v = args_n(ev, args, &[None, None, None, None, None, Some(2.0)])?;
    let [cost, salvage, life, start, end, factor] = v.as_slice() else {
        return Err(ErrorKind::Value);
    };
    let no_switch = ev.bool_or(args, 6, false)?;
    if *cost < 0.0
        || *salvage < 0.0
        || *life <= 0.0
        || *start < 0.0
        || end < start
        || end > life
        || *factor <= 0.0
        || *life > 1e6
    {
        return Err(ErrorKind::Num);
    }
    let periods = life.ceil() as usize;
    let sched = vdb_schedule(*cost, *salvage, *life, *factor, no_switch, periods);
    let mut total = 0.0;
    for (i, d) in sched.iter().enumerate() {
        let (a, b) = (i as f64, i as f64 + 1.0);
        let overlap = end.min(b) - start.max(a);
        if overlap > 0.0 {
            total += d * overlap;
        }
    }
    fin(total)
}

/// Values and dates of XNPV/XIRR: equal counts, numbers only, dates not before the first.
fn x_flows(ev: &mut Ev<'_>, args: &[Expr], vi: usize, di: usize) -> Result<(Vec<f64>, Vec<f64>), ErrorKind> {
    let vals = arg_array(ev, args, vi)?;
    let dates = arg_array(ev, args, di)?;
    if vals.data.len() != dates.data.len() || vals.data.is_empty() {
        return Err(ErrorKind::Num);
    }
    let mut v = Vec::with_capacity(vals.data.len());
    let mut d = Vec::with_capacity(vals.data.len());
    for (x, t) in vals.data.iter().zip(&dates.data) {
        match x {
            Value::Number(n) => v.push(*n),
            Value::Error(e) => return Err(*e),
            _ => return Err(ErrorKind::Value),
        }
        match t {
            Value::Number(n) => d.push(n.trunc()),
            Value::Error(e) => return Err(*e),
            _ => return Err(ErrorKind::Value),
        }
    }
    let d0 = d.first().copied().unwrap_or(0.0);
    if d.iter().any(|t| *t < d0 || *t < 0.0) {
        return Err(ErrorKind::Num);
    }
    Ok((v, d))
}

fn xnpv_of(rate: f64, v: &[f64], d: &[f64]) -> f64 {
    let d0 = d.first().copied().unwrap_or(0.0);
    v.iter()
        .zip(d)
        .map(|(c, t)| c / (1.0 + rate).powf((t - d0) / 365.0))
        .sum()
}

fn xnpv(ev: &mut Ev<'_>, args: &[Expr]) -> Result<f64, ErrorKind> {
    let rate = ev.num(args.first().ok_or(ErrorKind::Value)?)?;
    let (v, d) = x_flows(ev, args, 1, 2)?;
    if rate <= -1.0 {
        return Err(ErrorKind::Num);
    }
    fin(xnpv_of(rate, &v, &d))
}

fn xirr(ev: &mut Ev<'_>, args: &[Expr]) -> Result<f64, ErrorKind> {
    let (v, d) = x_flows(ev, args, 0, 1)?;
    let guess = ev.num_or(args, 2, 0.1)?;
    if guess <= -1.0 {
        return Err(ErrorKind::Num);
    }
    if !v.iter().any(|c| *c > 0.0) || !v.iter().any(|c| *c < 0.0) {
        return Err(ErrorKind::Num);
    }
    let r = super::bonds::solve(&|r| fin(xnpv_of(r, &v, &d)), guess, -1.0 + 1e-12, 1e10)?;
    // Accept only true roots.
    let scale = v.iter().map(|c| c.abs()).fold(0.0, f64::max).max(1.0);
    if xnpv_of(r, &v, &d).abs() > 1e-6 * scale {
        return Err(ErrorKind::Num);
    }
    Ok(r)
}

fn dollar_conv(ev: &mut Ev<'_>, args: &[Expr], to_decimal: bool) -> Result<f64, ErrorKind> {
    let v = args_strict(ev, args, &[None, None])?;
    let [x, frac] = v.as_slice() else {
        return Err(ErrorKind::Value);
    };
    let f = frac.trunc();
    if f < 0.0 {
        return Err(ErrorKind::Num);
    }
    if f == 0.0 {
        return Err(ErrorKind::Div0);
    }
    let int = x.trunc();
    let rest = x - int;
    let scale = 10f64.powf(f.log10().ceil());
    if to_decimal {
        fin(int + rest * scale / f)
    } else {
        fin(int + rest * f / scale)
    }
}

pub fn register(r: &mut Registry) {
    r.add("CUMIPMT", 6, 6, |ev, a| ret(cum(ev, a, false)));
    r.add("CUMPRINC", 6, 6, |ev, a| ret(cum(ev, a, true)));
    r.add("VDB", 5, 7, |ev, a| ret(vdb(ev, a)));
    r.add("XNPV", 3, 3, |ev, a| ret(xnpv(ev, a)));
    r.add("XIRR", 2, 3, |ev, a| ret(xirr(ev, a)));
    r.add("DOLLARDE", 2, 2, |ev, a| ret(dollar_conv(ev, a, true)));
    r.add("DOLLARFR", 2, 2, |ev, a| ret(dollar_conv(ev, a, false)));
    r.add("PMT", 3, 5, |ev, a| {
        ret(
            args_n(ev, a, &[None, None, None, Some(0.0), Some(0.0)]).and_then(|v| {
                match v.as_slice() {
                    [r, n, p, f, t] => pmt(*r, *n, *p, *f, *t),
                    _ => Err(ErrorKind::Value),
                }
            }),
        )
    });
    r.add("FV", 3, 5, |ev, a| {
        ret(
            args_n(ev, a, &[None, None, None, Some(0.0), Some(0.0)]).and_then(|v| {
                match v.as_slice() {
                    [r, n, p, pv_, t] => fv(*r, *n, *p, *pv_, *t),
                    _ => Err(ErrorKind::Value),
                }
            }),
        )
    });
    r.add("PV", 3, 5, |ev, a| {
        ret(
            args_n(ev, a, &[None, None, None, Some(0.0), Some(0.0)]).and_then(|v| {
                match v.as_slice() {
                    [r, n, p, f, t] => pv(*r, *n, *p, *f, *t),
                    _ => Err(ErrorKind::Value),
                }
            }),
        )
    });
    r.add("NPER", 3, 5, |ev, a| {
        ret(
            args_n(ev, a, &[None, None, None, Some(0.0), Some(0.0)]).and_then(|v| {
                match v.as_slice() {
                    [r, p, pv_, f, t] => nper(*r, *p, *pv_, *f, *t),
                    _ => Err(ErrorKind::Value),
                }
            }),
        )
    });
    r.add("IPMT", 4, 6, |ev, a| {
        ret(
            args_n(ev, a, &[None, None, None, None, Some(0.0), Some(0.0)]).and_then(|v| {
                match v.as_slice() {
                    [r, per, n, p, f, t] => ipmt(*r, *per, *n, *p, *f, *t),
                    _ => Err(ErrorKind::Value),
                }
            }),
        )
    });
    r.add("PPMT", 4, 6, |ev, a| {
        ret(
            args_n(ev, a, &[None, None, None, None, Some(0.0), Some(0.0)]).and_then(|v| {
                match v.as_slice() {
                    [r, per, n, p, f, t] => {
                        Ok(pmt(*r, *n, *p, *f, *t)? - ipmt(*r, *per, *n, *p, *f, *t)?)
                    }
                    _ => Err(ErrorKind::Value),
                }
            }),
        )
    });
    r.add("RATE", 3, 6, |ev, a| {
        ret(
            args_n(ev, a, &[None, None, None, Some(0.0), Some(0.0), Some(0.1)]).and_then(
                |v| match v.as_slice() {
                    [n, p, pv_, f, t, g] => rate(*n, *p, *pv_, *f, *t, *g),
                    _ => Err(ErrorKind::Value),
                },
            ),
        )
    });
    r.add("NPV", 2, 255, |ev, a| {
        let Some(re) = a.first() else {
            return EvalValue::err(ErrorKind::Value);
        };
        let rate = match ev.num(re) {
            Ok(r) => r,
            Err(e) => return EvalValue::err(e),
        };
        if rate == -1.0 {
            return EvalValue::err(ErrorKind::Div0);
        }
        ret(collect_numbers(ev, a.get(1..).unwrap_or(&[]), Agg::Numbers).map(|f| npv_of(rate, &f)))
    });
    r.add("IRR", 1, 2, |ev, a| {
        let flows = match flows_from(ev, a, 0) {
            Ok(f) => f,
            Err(e) => return EvalValue::err(e),
        };
        let g = match ev.num_or(a, 1, 0.1) {
            Ok(g) if g <= -1.0 => return EvalValue::err(ErrorKind::Value),
            Ok(g) => g,
            Err(e) => return EvalValue::err(e),
        };
        ret(irr(&flows, g))
    });
    r.add("MIRR", 3, 3, |ev, a| {
        let flows = match flows_from(ev, a, 0) {
            Ok(f) => f,
            Err(e) => return EvalValue::err(e),
        };
        let (fr, rr) = match (a.get(1).map(|e| ev.num(e)), a.get(2).map(|e| ev.num(e))) {
            (Some(Ok(x)), Some(Ok(y))) => (x, y),
            (Some(Err(e)), _) | (_, Some(Err(e))) => return EvalValue::err(e),
            _ => return EvalValue::err(ErrorKind::Value),
        };
        let n = flows.len() as f64;
        let pos: f64 = flows
            .iter()
            .enumerate()
            .filter(|(_, c)| **c > 0.0)
            .map(|(i, c)| c * (1.0 + rr).powf(n - 1.0 - i as f64))
            .sum();
        let neg: f64 = flows
            .iter()
            .enumerate()
            .filter(|(_, c)| **c < 0.0)
            .map(|(i, c)| c / (1.0 + fr).powi(i as i32))
            .sum();
        if neg == 0.0 {
            return EvalValue::err(ErrorKind::Div0);
        }
        if pos == 0.0 {
            return EvalValue::num(-1.0);
        }
        ret(fin((-pos / neg).powf(1.0 / (n - 1.0)) - 1.0))
    });
    r.add("SLN", 3, 3, |ev, a| {
        ret(
            args_n(ev, a, &[None, None, None]).and_then(|v| match v.as_slice() {
                [c, s, l] if *l != 0.0 => Ok((c - s) / l),
                [_, _, _] => Err(ErrorKind::Div0),
                _ => Err(ErrorKind::Value),
            }),
        )
    });
    r.add("SYD", 4, 4, |ev, a| {
        ret(
            args_n(ev, a, &[None, None, None, None]).and_then(|v| match v.as_slice() {
                [c, s, l, p] if *l > 0.0 && *p > 0.0 && p <= l => {
                    Ok((c - s) * (l - p + 1.0) * 2.0 / (l * (l + 1.0)))
                }
                [_, _, _, _] => Err(ErrorKind::Num),
                _ => Err(ErrorKind::Value),
            }),
        )
    });
    r.add("DDB", 4, 5, |ev, a| {
        ret(
            args_n(ev, a, &[None, None, None, None, Some(2.0)]).and_then(|v| match v.as_slice() {
                [c, s, l, p, f] => ddb(*c, *s, *l, *p, *f),
                _ => Err(ErrorKind::Value),
            }),
        )
    });
    r.add("DB", 4, 5, |ev, a| {
        ret(
            args_n(ev, a, &[None, None, None, None, Some(12.0)]).and_then(|v| match v.as_slice() {
                [c, s, l, p, m] => db(*c, *s, *l, *p, *m),
                _ => Err(ErrorKind::Value),
            }),
        )
    });
    r.add("EFFECT", 2, 2, |ev, a| {
        ret(
            args_strict(ev, a, &[None, None]).and_then(|v| match v.as_slice() {
                [r, n] if *r > 0.0 && n.trunc() >= 1.0 => {
                    fin((1.0 + r / n.trunc()).powf(n.trunc()) - 1.0)
                }
                [_, _] => Err(ErrorKind::Num),
                _ => Err(ErrorKind::Value),
            }),
        )
    });
    r.add("NOMINAL", 2, 2, |ev, a| {
        ret(
            args_strict(ev, a, &[None, None]).and_then(|v| match v.as_slice() {
                [r, n] if *r > 0.0 && n.trunc() >= 1.0 => {
                    fin(((1.0 + r).powf(1.0 / n.trunc()) - 1.0) * n.trunc())
                }
                [_, _] => Err(ErrorKind::Num),
                _ => Err(ErrorKind::Value),
            }),
        )
    });
    r.add("PDURATION", 3, 3, |ev, a| {
        ret(
            args_n(ev, a, &[None, None, None]).and_then(|v| match v.as_slice() {
                [r, p, f] if *r > 0.0 && *p > 0.0 && *f > 0.0 => {
                    fin((f.ln() - p.ln()) / (1.0 + r).ln())
                }
                [_, _, _] => Err(ErrorKind::Num),
                _ => Err(ErrorKind::Value),
            }),
        )
    });
    r.add("RRI", 3, 3, |ev, a| {
        ret(
            args_n(ev, a, &[None, None, None]).and_then(|v| match v.as_slice() {
                [n, p, f] if *n > 0.0 && *p != 0.0 => fin((f / p).powf(1.0 / n) - 1.0),
                [_, _, _] => Err(ErrorKind::Num),
                _ => Err(ErrorKind::Value),
            }),
        )
    });
    r.add("ISPMT", 4, 4, |ev, a| {
        ret(
            args_n(ev, a, &[None, None, None, None]).and_then(|v| match v.as_slice() {
                [r, per, n, p] if *n != 0.0 => Ok(p * r * (per / n - 1.0)),
                [_, _, _, _] => Err(ErrorKind::Div0),
                _ => Err(ErrorKind::Value),
            }),
        )
    });
    r.add("FVSCHEDULE", 2, 2, |ev, a| {
        let Some(p) = a.first() else {
            return EvalValue::err(ErrorKind::Value);
        };
        let principal = match ev.num(p) {
            Ok(x) => x,
            Err(e) => return EvalValue::err(e),
        };
        match arg_array(ev, a, 1) {
            Ok(arr) => {
                let mut acc = principal;
                for v in &arr.data {
                    match v {
                        Value::Number(r) => acc *= 1.0 + r,
                        Value::Empty => {}
                        Value::Error(e) => return EvalValue::err(*e),
                        _ => return EvalValue::err(ErrorKind::Value),
                    }
                }
                EvalValue::num(acc)
            }
            Err(e) => EvalValue::err(e),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tvm() {
        let p = pmt(0.08 / 12.0, 10.0, 10_000.0, 0.0, 0.0).unwrap();
        assert!((p - -1_037.032_089).abs() < 1e-5, "{p}");
        let f = fv(0.06 / 12.0, 10.0, -200.0, -500.0, 1.0).unwrap();
        assert!((f - 2_581.403_374).abs() < 1e-5, "{f}");
        let r = irr(
            &[-70000.0, 12000.0, 15000.0, 18000.0, 21000.0, 26000.0],
            0.1,
        )
        .unwrap();
        assert!((r - 0.086_630_948).abs() < 1e-8, "{r}");
    }
}
