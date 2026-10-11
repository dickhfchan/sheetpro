//! Statistical functions.

use super::{Agg, Registry, arg_array, collect_numbers, for_each_value, ret};
use crate::ast::Expr;
use crate::eval::{Ev, EvalValue};
use sp_core::{ErrorKind, Value};

fn nums(ev: &mut Ev<'_>, args: &[Expr], mode: Agg) -> Result<Vec<f64>, ErrorKind> {
    collect_numbers(ev, args, mode)
}

fn mean(v: &[f64]) -> Result<f64, ErrorKind> {
    if v.is_empty() {
        Err(ErrorKind::Div0)
    } else {
        Ok(v.iter().sum::<f64>() / v.len() as f64)
    }
}

fn var(v: &[f64], sample: bool) -> Result<f64, ErrorKind> {
    let n = v.len() as f64;
    if (sample && v.len() < 2) || v.is_empty() {
        return Err(ErrorKind::Div0);
    }
    let m = mean(v)?;
    let ss: f64 = v.iter().map(|x| (x - m) * (x - m)).sum();
    Ok(ss / if sample { n - 1.0 } else { n })
}

/// Inclusive percentile with linear interpolation (PERCENTILE.INC semantics).
pub fn percentile_inc(sorted: &[f64], k: f64) -> Result<f64, ErrorKind> {
    if sorted.is_empty() || !(0.0..=1.0).contains(&k) {
        return Err(ErrorKind::Num);
    }
    let h = (sorted.len() - 1) as f64 * k;
    let lo = h.floor() as usize;
    let frac = h - h.floor();
    let a = sorted.get(lo).copied().ok_or(ErrorKind::Num)?;
    let b = sorted.get(lo + 1).copied().unwrap_or(a);
    Ok(a + frac * (b - a))
}

fn percentile_exc(sorted: &[f64], k: f64) -> Result<f64, ErrorKind> {
    let n = sorted.len() as f64;
    let h = (n + 1.0) * k;
    if h < 1.0 || h > n {
        return Err(ErrorKind::Num);
    }
    let lo = h.floor() as usize;
    let frac = h - h.floor();
    let a = sorted.get(lo - 1).copied().ok_or(ErrorKind::Num)?;
    let b = sorted.get(lo).copied().unwrap_or(a);
    Ok(a + frac * (b - a))
}

fn sorted(mut v: Vec<f64>) -> Vec<f64> {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v
}

fn agg(
    ev: &mut Ev<'_>,
    args: &[Expr],
    mode: Agg,
    f: fn(Vec<f64>) -> Result<f64, ErrorKind>,
) -> EvalValue {
    ret(nums(ev, args, mode).and_then(f))
}

fn count(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let mut n = 0usize;
    let r = for_each_value(ev, args, &mut |v, from_ref| {
        match (v, from_ref) {
            (Value::Number(_), _) => n += 1,
            (Value::Bool(_), false) => n += 1,
            (Value::Text(s), false) if crate::eval::text_to_number(s).is_some() => n += 1,
            _ => {}
        }
        Ok(())
    });
    match r {
        Ok(()) => EvalValue::num(n as f64),
        Err(e) => EvalValue::err(e),
    }
}

fn counta(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let mut n = 0usize;
    for a in args {
        let v = ev.eval(a);
        let _ = super::visit_value(ev, v, &mut |x, from_ref| {
            if !(from_ref && x.is_empty()) {
                n += 1;
            }
            Ok(())
        });
    }
    EvalValue::num(n as f64)
}

fn countblank(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let Some(a) = args.first() else {
        return EvalValue::err(ErrorKind::Value);
    };
    match ev.eval_array(a) {
        Ok(arr) => EvalValue::num(
            arr.data
                .iter()
                .filter(|v| v.is_empty() || matches!(v, Value::Text(s) if s.is_empty()))
                .count() as f64,
        ),
        Err(e) => EvalValue::err(e),
    }
}

fn rank(ev: &mut Ev<'_>, args: &[Expr], avg: bool) -> EvalValue {
    let Some(a) = args.first() else {
        return EvalValue::err(ErrorKind::Value);
    };
    let x = match ev.num(a) {
        Ok(x) => x,
        Err(e) => return EvalValue::err(e),
    };
    let refv = match args.get(1).map(|e| ev.eval_array(e)) {
        Some(Ok(arr)) => arr
            .data
            .iter()
            .filter_map(|v| v.as_number())
            .collect::<Vec<_>>(),
        Some(Err(e)) => return EvalValue::err(e),
        None => return EvalValue::err(ErrorKind::Value),
    };
    let asc = match ev.num_or(args, 2, 0.0) {
        Ok(o) => o != 0.0,
        Err(e) => return EvalValue::err(e),
    };
    let better = refv
        .iter()
        .filter(|v| if asc { **v < x } else { **v > x })
        .count();
    let ties = refv.iter().filter(|v| **v == x).count();
    if ties == 0 {
        return EvalValue::err(ErrorKind::NA);
    }
    if avg {
        EvalValue::num(better as f64 + (ties as f64 + 1.0) / 2.0)
    } else {
        EvalValue::num(better as f64 + 1.0)
    }
}

fn kth(ev: &mut Ev<'_>, args: &[Expr], large: bool) -> EvalValue {
    // A direct scalar argument (not a reference or array) is coerced like a number.
    let first = args.first().map(|e| ev.eval(e));
    let arr = match first {
        Some(EvalValue::Scalar(Value::Error(e))) => return EvalValue::err(e),
        Some(EvalValue::Scalar(v)) => match crate::eval::to_number(&v) {
            Ok(n) => Ok(crate::eval::Array2::scalar(Value::Number(n))),
            Err(e) => return EvalValue::err(e),
        },
        Some(v) => ev.to_array(v),
        None => Err(ErrorKind::Value),
    };
    let v = match arr {
        Ok(a) => {
            if let Some(Value::Error(e)) = a.data.iter().find(|v| matches!(v, Value::Error(_))) {
                return EvalValue::err(*e);
            }
            sorted(a.data.iter().filter_map(|v| v.as_number()).collect())
        }
        Err(e) => return EvalValue::err(e),
    };
    let k = match args.get(1).map(|e| ev.num(e)) {
        Some(Ok(k)) => k.ceil(),
        Some(Err(e)) => return EvalValue::err(e),
        None => return EvalValue::err(ErrorKind::Value),
    };
    if k < 1.0 || k as usize > v.len() {
        return EvalValue::err(ErrorKind::Num);
    }
    let idx = if large {
        v.len() - k as usize
    } else {
        k as usize - 1
    };
    ret(v.get(idx).copied().ok_or(ErrorKind::Num))
}

fn pct(ev: &mut Ev<'_>, args: &[Expr], quart: bool, exc: bool) -> EvalValue {
    let v = match arg_array(ev, args, 0) {
        Ok(a) => sorted(a.data.iter().filter_map(|v| v.as_number()).collect()),
        Err(e) => return EvalValue::err(e),
    };
    let k = match args.get(1).map(|e| ev.num(e)) {
        Some(Ok(k)) => k,
        Some(Err(e)) => return EvalValue::err(e),
        None => return EvalValue::err(ErrorKind::Value),
    };
    let k = if quart {
        let q = k.trunc();
        if !(0.0..=4.0).contains(&q) || (exc && (q == 0.0 || q == 4.0)) {
            return EvalValue::err(ErrorKind::Num);
        }
        q / 4.0
    } else {
        k
    };
    ret(if exc {
        percentile_exc(&v, k)
    } else {
        percentile_inc(&v, k)
    })
}

pub(crate) fn pairs(ev: &mut Ev<'_>, args: &[Expr]) -> Result<(Vec<f64>, Vec<f64>), ErrorKind> {
    let a = arg_array(ev, args, 0)?;
    let b = arg_array(ev, args, 1)?;
    if a.data.len() != b.data.len() {
        return Err(ErrorKind::NA);
    }
    let mut xs = Vec::new();
    let mut ys = Vec::new();
    for (x, y) in a.data.iter().zip(&b.data) {
        if let Value::Error(e) = x {
            return Err(*e);
        }
        if let Value::Error(e) = y {
            return Err(*e);
        }
        if let (Value::Number(x), Value::Number(y)) = (x, y) {
            xs.push(*x);
            ys.push(*y);
        }
    }
    Ok((xs, ys))
}

fn cov(xs: &[f64], ys: &[f64], sample: bool) -> Result<f64, ErrorKind> {
    let n = xs.len() as f64;
    if xs.is_empty() || (sample && xs.len() < 2) {
        return Err(ErrorKind::Div0);
    }
    let (mx, my) = (mean(xs)?, mean(ys)?);
    let s: f64 = xs.iter().zip(ys).map(|(x, y)| (x - mx) * (y - my)).sum();
    Ok(s / if sample { n - 1.0 } else { n })
}

fn correl(xs: &[f64], ys: &[f64]) -> Result<f64, ErrorKind> {
    let c = cov(xs, ys, false)?;
    let (sx, sy) = (var(xs, false)?.sqrt(), var(ys, false)?.sqrt());
    if sx == 0.0 || sy == 0.0 {
        return Err(ErrorKind::Div0);
    }
    Ok(c / (sx * sy))
}

fn slope(ys: &[f64], xs: &[f64]) -> Result<f64, ErrorKind> {
    let vx = var(xs, false)?;
    if vx == 0.0 {
        return Err(ErrorKind::Div0);
    }
    Ok(cov(xs, ys, false)? / vx)
}

fn pair_fn(
    ev: &mut Ev<'_>,
    args: &[Expr],
    f: fn(&[f64], &[f64]) -> Result<f64, ErrorKind>,
) -> EvalValue {
    ret(pairs(ev, args).and_then(|(a, b)| f(&a, &b)))
}

fn mode_sngl(v: Vec<f64>) -> Result<f64, ErrorKind> {
    let mut best: Option<(f64, usize)> = None;
    for x in &v {
        let c = v.iter().filter(|y| *y == x).count();
        if c > 1 && best.is_none_or(|(_, bc)| c > bc) {
            best = Some((*x, c));
        }
    }
    best.map(|b| b.0).ok_or(ErrorKind::NA)
}

pub fn register(r: &mut Registry) {
    r.add("AVERAGE", 1, 255, |ev, a| {
        agg(ev, a, Agg::Numbers, |v| mean(&v))
    });
    r.add("AVERAGEA", 1, 255, |ev, a| {
        agg(ev, a, Agg::All, |v| mean(&v))
    });
    r.add("COUNT", 1, 255, count);
    r.add("COUNTA", 1, 255, counta);
    r.add("COUNTBLANK", 1, 1, countblank);
    r.add("MAX", 1, 255, |ev, a| {
        agg(ev, a, Agg::Numbers, |v| {
            Ok(v.into_iter().reduce(f64::max).unwrap_or(0.0))
        })
    });
    r.add("MIN", 1, 255, |ev, a| {
        agg(ev, a, Agg::Numbers, |v| {
            Ok(v.into_iter().reduce(f64::min).unwrap_or(0.0))
        })
    });
    r.add("MAXA", 1, 255, |ev, a| {
        agg(ev, a, Agg::All, |v| {
            Ok(v.into_iter().reduce(f64::max).unwrap_or(0.0))
        })
    });
    r.add("MINA", 1, 255, |ev, a| {
        agg(ev, a, Agg::All, |v| {
            Ok(v.into_iter().reduce(f64::min).unwrap_or(0.0))
        })
    });
    r.add("MEDIAN", 1, 255, |ev, a| {
        agg(ev, a, Agg::Numbers, |v| percentile_inc(&sorted(v), 0.5))
    });
    r.add("MODE.SNGL", 1, 255, |ev, a| {
        agg(ev, a, Agg::Numbers, mode_sngl)
    });
    r.alias("MODE", "MODE.SNGL");
    r.add("LARGE", 2, 2, |ev, a| kth(ev, a, true));
    r.add("SMALL", 2, 2, |ev, a| kth(ev, a, false));
    r.add("RANK.EQ", 2, 3, |ev, a| rank(ev, a, false));
    r.alias("RANK", "RANK.EQ");
    r.add("RANK.AVG", 2, 3, |ev, a| rank(ev, a, true));
    r.add("VAR.S", 1, 255, |ev, a| {
        agg(ev, a, Agg::Numbers, |v| var(&v, true))
    });
    r.add("VAR.P", 1, 255, |ev, a| {
        agg(ev, a, Agg::Numbers, |v| var(&v, false))
    });
    r.add("VARA", 1, 255, |ev, a| {
        agg(ev, a, Agg::All, |v| var(&v, true))
    });
    r.add("VARPA", 1, 255, |ev, a| {
        agg(ev, a, Agg::All, |v| var(&v, false))
    });
    r.alias("VAR", "VAR.S");
    r.alias("VARP", "VAR.P");
    r.add("STDEV.S", 1, 255, |ev, a| {
        agg(ev, a, Agg::Numbers, |v| var(&v, true).map(f64::sqrt))
    });
    r.add("STDEV.P", 1, 255, |ev, a| {
        agg(ev, a, Agg::Numbers, |v| var(&v, false).map(f64::sqrt))
    });
    r.add("STDEVA", 1, 255, |ev, a| {
        agg(ev, a, Agg::All, |v| var(&v, true).map(f64::sqrt))
    });
    r.add("STDEVPA", 1, 255, |ev, a| {
        agg(ev, a, Agg::All, |v| var(&v, false).map(f64::sqrt))
    });
    r.alias("STDEV", "STDEV.S");
    r.alias("STDEVP", "STDEV.P");
    r.add("AVEDEV", 1, 255, |ev, a| {
        agg(ev, a, Agg::Numbers, |v| {
            let m = mean(&v).map_err(|_| ErrorKind::Num)?;
            Ok(v.iter().map(|x| (x - m).abs()).sum::<f64>() / v.len() as f64)
        })
    });
    r.add("DEVSQ", 1, 255, |ev, a| {
        agg(ev, a, Agg::Numbers, |v| {
            let m = mean(&v).map_err(|_| ErrorKind::Num)?;
            Ok(v.iter().map(|x| (x - m) * (x - m)).sum())
        })
    });
    r.add("GEOMEAN", 1, 255, |ev, a| {
        agg(ev, a, Agg::Numbers, |v| {
            if v.is_empty() || v.iter().any(|x| *x <= 0.0) {
                return Err(ErrorKind::Num);
            }
            Ok((v.iter().map(|x| x.ln()).sum::<f64>() / v.len() as f64).exp())
        })
    });
    r.add("HARMEAN", 1, 255, |ev, a| {
        agg(ev, a, Agg::Numbers, |v| {
            if v.is_empty() || v.iter().any(|x| *x <= 0.0) {
                return Err(ErrorKind::Num);
            }
            Ok(v.len() as f64 / v.iter().map(|x| 1.0 / x).sum::<f64>())
        })
    });
    r.add("PERCENTILE.INC", 2, 2, |ev, a| pct(ev, a, false, false));
    r.add("PERCENTILE.EXC", 2, 2, |ev, a| pct(ev, a, false, true));
    r.add("QUARTILE.INC", 2, 2, |ev, a| pct(ev, a, true, false));
    r.add("QUARTILE.EXC", 2, 2, |ev, a| pct(ev, a, true, true));
    r.alias("PERCENTILE", "PERCENTILE.INC");
    r.alias("QUARTILE", "QUARTILE.INC");
    r.add("CORREL", 2, 2, |ev, a| pair_fn(ev, a, correl));
    r.alias("PEARSON", "CORREL");
    r.add("RSQ", 2, 2, |ev, a| {
        pair_fn(ev, a, |y, x| correl(x, y).map(|r| r * r))
    });
    r.add("COVARIANCE.P", 2, 2, |ev, a| {
        pair_fn(ev, a, |x, y| cov(x, y, false))
    });
    r.add("COVARIANCE.S", 2, 2, |ev, a| {
        pair_fn(ev, a, |x, y| cov(x, y, true))
    });
    r.alias("COVAR", "COVARIANCE.P");
    r.add("SLOPE", 2, 2, |ev, a| pair_fn(ev, a, slope));
    r.add("INTERCEPT", 2, 2, |ev, a| {
        pair_fn(ev, a, |ys, xs| {
            let b = slope(ys, xs)?;
            Ok(mean(ys)? - b * mean(xs)?)
        })
    });
    r.add("STANDARDIZE", 3, 3, |ev, a| {
        let vals: Vec<Result<f64, ErrorKind>> = a.iter().map(|e| ev.num(e)).collect();
        match vals.as_slice() {
            [Ok(x), Ok(m), Ok(s)] if *s > 0.0 => EvalValue::num((x - m) / s),
            [Ok(_), Ok(_), Ok(_)] => EvalValue::err(ErrorKind::Num),
            _ => EvalValue::err(
                vals.iter()
                    .find_map(|r| r.err())
                    .unwrap_or(ErrorKind::Value),
            ),
        }
    });
}
