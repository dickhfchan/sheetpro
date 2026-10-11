//! Math & Trig functions.

use super::{Agg, Registry, arg_array, collect_numbers, m1, m2, ret};
use crate::ast::Expr;
use crate::eval::{Array2, Ev, EvalValue, num_value};
use sp_core::{ErrorKind, Value};

fn nz(x: f64) -> Result<f64, ErrorKind> {
    if x.is_finite() {
        Ok(x)
    } else {
        Err(ErrorKind::Num)
    }
}

/// Round half away from zero to `digits` decimals (negative digits round left of the point).
pub fn round_to(x: f64, digits: f64) -> f64 {
    let d = digits.trunc().clamp(-308.0, 308.0) as i32;
    let m = 10f64.powi(d);
    // Pre-round to 15 significant digits to absorb binary noise (2.675 → 2.68).
    let scaled = x * m;
    let scaled = (scaled * 1e9).round() / 1e9;
    let r = scaled.abs().round() * scaled.signum() / m;
    if r.is_finite() { r } else { x }
}

fn round_dir(x: f64, digits: f64, up: bool) -> f64 {
    let d = digits.trunc().clamp(-308.0, 308.0) as i32;
    let m = 10f64.powi(d);
    let raw = (x * m).abs();
    // Absorb binary noise only when the scaled value is large enough to carry it.
    let s = if raw >= 1e-6 {
        (raw * 1e9).round() / 1e9
    } else {
        raw
    };
    let r = if up { s.ceil() } else { s.floor() };
    r * x.signum() / m
}

fn fact(n: f64) -> Result<f64, ErrorKind> {
    if n < 0.0 {
        return Err(ErrorKind::Num);
    }
    let n = n.trunc();
    if n > 170.0 {
        return Err(ErrorKind::Num);
    }
    Ok((1..=n as u64).fold(1.0, |acc, k| acc * k as f64))
}

pub fn combin(n: f64, k: f64) -> Result<f64, ErrorKind> {
    let (n, k) = (n.trunc(), k.trunc());
    if n < 0.0 || k < 0.0 || k > n {
        return Err(ErrorKind::Num);
    }
    let k = k.min(n - k);
    let mut r = 1.0;
    for i in 0..k as u64 {
        r = r * (n - i as f64) / (i as f64 + 1.0);
    }
    nz(r.round())
}

fn gcd(a: f64, b: f64) -> f64 {
    let (mut a, mut b) = (a, b);
    while b > 0.5 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

fn sum(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    ret(collect_numbers(ev, args, Agg::Numbers).map(|v| v.iter().sum()))
}

fn product(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    ret(collect_numbers(ev, args, Agg::Numbers).map(|v| {
        if v.is_empty() {
            0.0
        } else {
            v.iter().product()
        }
    }))
}

fn sumsq(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    ret(collect_numbers(ev, args, Agg::Numbers).map(|v| v.iter().map(|x| x * x).sum()))
}

fn sumproduct(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let mut arrays: Vec<Array2> = Vec::new();
    for i in 0..args.len() {
        match arg_array(ev, args, i) {
            Ok(a) => arrays.push(a),
            Err(e) => return EvalValue::err(e),
        }
    }
    let Some(first) = arrays.first() else {
        return EvalValue::err(ErrorKind::Value);
    };
    let (rows, cols) = (first.rows, first.cols);
    if arrays.iter().any(|a| a.rows != rows || a.cols != cols) {
        return EvalValue::err(ErrorKind::Value);
    }
    let mut total = 0.0;
    for k in 0..rows * cols {
        let mut p = 1.0;
        for a in &arrays {
            match a.data.get(k) {
                Some(Value::Number(n)) => p *= n,
                Some(Value::Error(e)) => return EvalValue::err(*e),
                _ => p = 0.0,
            }
        }
        total += p;
    }
    EvalValue::num(total)
}

/// Pairs of (criteria range, criterion) → mask (see `criteria::criteria_mask`).
pub fn criteria_mask(
    ev: &mut Ev<'_>,
    pairs: &[Expr],
    shape: Option<(usize, usize)>,
) -> Result<Vec<bool>, ErrorKind> {
    super::criteria::criteria_mask(ev, pairs, shape)
}

fn subtotal(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let code = match args.first().map(|a| ev.num(a)) {
        Some(Ok(c)) => c.trunc() as i64,
        Some(Err(e)) => return EvalValue::err(e),
        None => return EvalValue::err(ErrorKind::Value),
    };
    let f = match code % 100 {
        1 => "AVERAGE",
        2 => "COUNT",
        3 => "COUNTA",
        4 => "MAX",
        5 => "MIN",
        6 => "PRODUCT",
        7 => "STDEV.S",
        8 => "STDEV.P",
        9 => "SUM",
        10 => "VAR.S",
        11 => "VAR.P",
        _ => return EvalValue::err(ErrorKind::Value),
    };
    if !(1..=11).contains(&code) && !(101..=111).contains(&code) {
        return EvalValue::err(ErrorKind::Value);
    }
    match ev.registry.get(f) {
        Some(spec) => (spec.imp)(ev, args.get(1..).unwrap_or(&[])),
        None => EvalValue::err(ErrorKind::Name),
    }
}

fn mround(x: f64, m: f64) -> Result<f64, ErrorKind> {
    if m == 0.0 {
        return Ok(0.0);
    }
    if x * m < 0.0 {
        return Err(ErrorKind::Num);
    }
    Ok(round_to(x / m, 0.0) * m)
}

fn ceiling(x: f64, sig: f64) -> Result<f64, ErrorKind> {
    if sig == 0.0 {
        return Ok(0.0);
    }
    if x > 0.0 && sig < 0.0 {
        return Err(ErrorKind::Num);
    }
    let q = x / sig;
    Ok(((q * 1e9).round() / 1e9).ceil() * sig)
}

fn floor_fn(x: f64, sig: f64) -> Result<f64, ErrorKind> {
    if sig == 0.0 {
        return if x == 0.0 {
            Ok(0.0)
        } else {
            Err(ErrorKind::Div0)
        };
    }
    if x > 0.0 && sig < 0.0 {
        return Err(ErrorKind::Num);
    }
    let q = x / sig;
    Ok(((q * 1e9).round() / 1e9).floor() * sig)
}

fn ceiling_math(ev: &mut Ev<'_>, args: &[Expr], up: bool) -> EvalValue {
    let x = match args.first().map(|a| ev.num(a)) {
        Some(Ok(x)) => x,
        Some(Err(e)) => return EvalValue::err(e),
        None => return EvalValue::err(ErrorKind::Value),
    };
    let sig = match ev.num_or(args, 1, 1.0) {
        Ok(s) => s.abs(),
        Err(e) => return EvalValue::err(e),
    };
    let mode = match ev.num_or(args, 2, 0.0) {
        Ok(m) => m,
        Err(e) => return EvalValue::err(e),
    };
    if sig == 0.0 {
        return EvalValue::num(0.0);
    }
    let q = ((x / sig) * 1e9).round() / 1e9;
    // Negative numbers round toward zero by default; mode≠0 rounds away from zero.
    let r = if x < 0.0 && mode != 0.0 {
        if up { q.floor() } else { q.ceil() }
    } else if up {
        q.ceil()
    } else {
        q.floor()
    };
    EvalValue::num(r * sig)
}

fn rand(ev: &mut Ev<'_>, _args: &[Expr]) -> EvalValue {
    EvalValue::num(ev.host.random())
}

fn randbetween(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let r = ev.host.random();
    m2(ev, args, None, move |lo, hi| {
        let (lo, hi) = (lo.ceil(), hi.floor());
        if lo > hi {
            return Err(ErrorKind::Num);
        }
        Ok(lo + (r * (hi - lo + 1.0)).floor())
    })
}

/// Numbers for GCD/LCM/MULTINOMIAL: logicals are `#VALUE!`; numeric text counts (in
/// ranges only when `text_in_refs`); blanks skip, but only blank direct arguments is `#VALUE!`.
fn strict_numbers(
    ev: &mut Ev<'_>,
    args: &[Expr],
    text_in_refs: bool,
) -> Result<Vec<f64>, ErrorKind> {
    let mut out = Vec::new();
    let mut direct_blank = false;
    for a in args {
        let v = ev.eval(a);
        let single = match &v {
            crate::eval::EvalValue::Ref(r) => r.range.is_single(),
            crate::eval::EvalValue::Scalar(_) => true,
            _ => false,
        };
        let arr = ev.to_array(v)?;
        for x in &arr.data {
            match x {
                Value::Number(n) => out.push(*n),
                Value::Error(e) => return Err(*e),
                Value::Bool(_) => return Err(ErrorKind::Value),
                Value::Empty => direct_blank |= single,
                Value::Text(t) if single || text_in_refs => {
                    out.push(crate::eval::text_to_number(t).ok_or(ErrorKind::Value)?)
                }
                Value::Text(_) => return Err(ErrorKind::Value),
            }
        }
    }
    if out.is_empty() && direct_blank {
        return Err(ErrorKind::Value);
    }
    Ok(out)
}

fn gcd_lcm(ev: &mut Ev<'_>, args: &[Expr], lcm: bool) -> EvalValue {
    let nums = match strict_numbers(ev, args, true) {
        Ok(v) => v,
        Err(e) => return EvalValue::err(e),
    };
    if nums
        .iter()
        .any(|n| *n < 0.0 || *n >= 9.007_199_254_740_992e15)
    {
        return EvalValue::err(ErrorKind::Num);
    }
    if nums.is_empty() {
        return EvalValue::num(0.0);
    }
    let mut acc = if lcm { 1.0 } else { 0.0 };
    for n in nums.iter().map(|n| n.trunc()) {
        if lcm {
            if n == 0.0 {
                return EvalValue::num(0.0);
            }
            acc = acc / gcd(acc, n) * n;
        } else {
            acc = gcd(acc.max(n), acc.min(n));
        }
    }
    EvalValue::num(acc)
}

fn sequence(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let get = |ev: &mut Ev<'_>, i: usize, d: f64| ev.num_or(args, i, d);
    let (rows, cols, start, step) = match (
        get(ev, 0, 1.0),
        get(ev, 1, 1.0),
        get(ev, 2, 1.0),
        get(ev, 3, 1.0),
    ) {
        (Ok(a), Ok(b), Ok(c), Ok(d)) => (a.trunc(), b.trunc(), c, d),
        (Err(e), ..) | (_, Err(e), ..) | (_, _, Err(e), _) | (.., Err(e)) => {
            return EvalValue::err(e);
        }
    };
    if rows < 1.0 || cols < 1.0 {
        return EvalValue::err(ErrorKind::Calc);
    }
    if rows * cols > sp_core::MAX_CELLS_PER_OP as f64 {
        return EvalValue::err(ErrorKind::Num);
    }
    let (r, c) = (rows as usize, cols as usize);
    let data = (0..r * c)
        .map(|k| num_value(start + step * k as f64))
        .collect();
    EvalValue::Array(Array2::new(r, c, data))
}

fn base(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let (n, radix) = match (
        args.first().map(|a| ev.num(a)),
        args.get(1).map(|a| ev.num(a)),
    ) {
        (Some(Ok(n)), Some(Ok(r))) => (n.trunc(), r.trunc()),
        (Some(Err(e)), _) | (_, Some(Err(e))) => return EvalValue::err(e),
        _ => return EvalValue::err(ErrorKind::Value),
    };
    let min_len = match ev.num_or(args, 2, 0.0) {
        Ok(m) => m.trunc().max(0.0) as usize,
        Err(e) => return EvalValue::err(e),
    };
    if !(2.0..=36.0).contains(&radix) || n < 0.0 || n >= 2f64.powi(53) || min_len > 255 {
        return EvalValue::err(ErrorKind::Num);
    }
    let (mut v, r) = (n as u64, radix as u64);
    let mut s = Vec::new();
    loop {
        let d = (v % r) as u32;
        s.push(
            char::from_digit(d, r as u32)
                .unwrap_or('0')
                .to_ascii_uppercase(),
        );
        v /= r;
        if v == 0 {
            break;
        }
    }
    while s.len() < min_len {
        s.push('0');
    }
    s.reverse();
    EvalValue::text(s.into_iter().collect::<String>())
}

fn decimal(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let (Some(a), Some(b)) = (args.first(), args.get(1)) else {
        return EvalValue::err(ErrorKind::Value);
    };
    let s = match ev.text(a) {
        Ok(s) => s,
        Err(e) => return EvalValue::err(e),
    };
    let radix = match ev.num(b) {
        Ok(r) => r.trunc(),
        Err(e) => return EvalValue::err(e),
    };
    if !(2.0..=36.0).contains(&radix) || s.len() > 255 {
        return EvalValue::err(ErrorKind::Num);
    }
    let mut acc = 0.0f64;
    for c in s.trim().chars() {
        match c.to_digit(radix as u32) {
            Some(d) => acc = acc * radix + f64::from(d),
            None => return EvalValue::err(ErrorKind::Num),
        }
    }
    EvalValue::num(acc)
}

const ROMAN: [(u32, &str); 13] = [
    (1000, "M"),
    (900, "CM"),
    (500, "D"),
    (400, "CD"),
    (100, "C"),
    (90, "XC"),
    (50, "L"),
    (40, "XL"),
    (10, "X"),
    (9, "IX"),
    (5, "V"),
    (4, "IV"),
    (1, "I"),
];

fn roman(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let n = match args.first().map(|a| ev.num(a)) {
        Some(Ok(n)) => n.trunc(),
        Some(Err(e)) => return EvalValue::err(e),
        None => return EvalValue::err(ErrorKind::Value),
    };
    if !(0.0..=3999.0).contains(&n) {
        return EvalValue::err(ErrorKind::Value);
    }
    let mut v = n as u32;
    let mut s = String::new();
    for (k, sym) in ROMAN {
        while v >= k {
            s.push_str(sym);
            v -= k;
        }
    }
    EvalValue::text(s)
}

fn arabic(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let s = match args.first().map(|a| ev.text(a)) {
        Some(Ok(s)) => s.trim().to_ascii_uppercase(),
        Some(Err(e)) => return EvalValue::err(e),
        None => return EvalValue::err(ErrorKind::Value),
    };
    let (neg, body) = match s.strip_prefix('-') {
        Some(b) => (true, b.to_string()),
        None => (false, s),
    };
    let val = |c: char| match c {
        'I' => Some(1),
        'V' => Some(5),
        'X' => Some(10),
        'L' => Some(50),
        'C' => Some(100),
        'D' => Some(500),
        'M' => Some(1000),
        _ => None,
    };
    let mut total: i64 = 0;
    let chars: Vec<char> = body.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        let Some(v) = val(*c) else {
            return EvalValue::err(ErrorKind::Value);
        };
        let next = chars.get(i + 1).and_then(|n| val(*n)).unwrap_or(0);
        total += if v < next { -v } else { v };
    }
    EvalValue::num(if neg { -total as f64 } else { total as f64 })
}

fn mmult(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let (a, b) = match (arg_array(ev, args, 0), arg_array(ev, args, 1)) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => return EvalValue::err(e),
    };
    if a.cols != b.rows {
        return EvalValue::err(ErrorKind::Value);
    }
    let mut data = Vec::with_capacity(a.rows * b.cols);
    for i in 0..a.rows {
        for j in 0..b.cols {
            let mut s = 0.0;
            for k in 0..a.cols {
                match (a.get(i, k), b.get(k, j)) {
                    (Value::Number(x), Value::Number(y)) => s += x * y,
                    (Value::Error(e), _) | (_, Value::Error(e)) => return EvalValue::err(*e),
                    _ => return EvalValue::err(ErrorKind::Value),
                }
            }
            data.push(num_value(s));
        }
    }
    EvalValue::Array(Array2::new(a.rows, b.cols, data))
}

fn sum_x_y(ev: &mut Ev<'_>, args: &[Expr], f: fn(f64, f64) -> f64) -> EvalValue {
    let (a, b) = match (arg_array(ev, args, 0), arg_array(ev, args, 1)) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => return EvalValue::err(e),
    };
    if a.data.len() != b.data.len() {
        return EvalValue::err(ErrorKind::NA);
    }
    let mut s = 0.0;
    for (x, y) in a.data.iter().zip(&b.data) {
        if let (Value::Number(x), Value::Number(y)) = (x, y) {
            s += f(*x, *y);
        }
    }
    EvalValue::num(s)
}

fn multinomial(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let nums = match strict_numbers(ev, args, false) {
        Ok(v) => v,
        Err(e) => return EvalValue::err(e),
    };
    let mut total = 0.0;
    let mut denom = 1.0;
    for n in nums {
        if n < 0.0 {
            return EvalValue::err(ErrorKind::Num);
        }
        total += n.trunc();
        match fact(n) {
            Ok(f) => denom *= f,
            Err(e) => return EvalValue::err(e),
        }
    }
    ret(fact(total).map(|t| t / denom))
}

pub fn register(r: &mut Registry) {
    r.add("ABS", 1, 1, |ev, a| m1(ev, a, |x| Ok(x.abs())));
    r.add("SIGN", 1, 1, |ev, a| {
        m1(ev, a, |x| {
            Ok(if x > 0.0 {
                1.0
            } else if x < 0.0 {
                -1.0
            } else {
                0.0
            })
        })
    });
    r.add("SQRT", 1, 1, |ev, a| {
        m1(ev, a, |x| {
            if x < 0.0 {
                Err(ErrorKind::Num)
            } else {
                Ok(x.sqrt())
            }
        })
    });
    r.add("SQRTPI", 1, 1, |ev, a| {
        m1(ev, a, |x| {
            if x < 0.0 {
                Err(ErrorKind::Num)
            } else {
                Ok((x * std::f64::consts::PI).sqrt())
            }
        })
    });
    r.add("EXP", 1, 1, |ev, a| m1(ev, a, |x| nz(x.exp())));
    r.add("LN", 1, 1, |ev, a| {
        m1(ev, a, |x| {
            if x <= 0.0 {
                Err(ErrorKind::Num)
            } else {
                Ok(x.ln())
            }
        })
    });
    r.add("LOG10", 1, 1, |ev, a| {
        m1(ev, a, |x| {
            if x <= 0.0 {
                Err(ErrorKind::Num)
            } else {
                Ok(x.log10())
            }
        })
    });
    r.add("LOG", 1, 2, |ev, a| {
        m2(ev, a, Some(10.0), |x, b| {
            if x <= 0.0 || b <= 0.0 {
                Err(ErrorKind::Num)
            } else if b == 1.0 {
                Err(ErrorKind::Div0)
            } else {
                Ok(x.ln() / b.ln())
            }
        })
    });
    r.add("POWER", 2, 2, |ev, a| {
        m2(ev, a, None, |x, y| {
            if x == 0.0 && y == 0.0 {
                Err(ErrorKind::Num)
            } else if x == 0.0 && y < 0.0 {
                Err(ErrorKind::Div0)
            } else {
                nz(x.powf(y))
            }
        })
    });
    r.add("PI", 0, 0, |_, _| EvalValue::num(std::f64::consts::PI));
    r.add("SIN", 1, 1, |ev, a| m1(ev, a, |x| Ok(x.sin())));
    r.add("COS", 1, 1, |ev, a| m1(ev, a, |x| Ok(x.cos())));
    r.add("TAN", 1, 1, |ev, a| m1(ev, a, |x| Ok(x.tan())));
    r.add("ASIN", 1, 1, |ev, a| {
        m1(ev, a, |x| {
            if x.abs() > 1.0 {
                Err(ErrorKind::Num)
            } else {
                Ok(x.asin())
            }
        })
    });
    r.add("ACOS", 1, 1, |ev, a| {
        m1(ev, a, |x| {
            if x.abs() > 1.0 {
                Err(ErrorKind::Num)
            } else {
                Ok(x.acos())
            }
        })
    });
    r.add("ATAN", 1, 1, |ev, a| m1(ev, a, |x| Ok(x.atan())));
    r.add("ATAN2", 2, 2, |ev, a| {
        m2(ev, a, None, |x, y| {
            if x == 0.0 && y == 0.0 {
                Err(ErrorKind::Div0)
            } else {
                Ok(y.atan2(x))
            }
        })
    });
    r.add("SINH", 1, 1, |ev, a| m1(ev, a, |x| nz(x.sinh())));
    r.add("COSH", 1, 1, |ev, a| m1(ev, a, |x| nz(x.cosh())));
    r.add("TANH", 1, 1, |ev, a| m1(ev, a, |x| Ok(x.tanh())));
    r.add("ASINH", 1, 1, |ev, a| m1(ev, a, |x| Ok(x.asinh())));
    r.add("ACOSH", 1, 1, |ev, a| {
        m1(ev, a, |x| {
            if x < 1.0 {
                Err(ErrorKind::Num)
            } else {
                Ok(x.acosh())
            }
        })
    });
    r.add("ATANH", 1, 1, |ev, a| {
        m1(ev, a, |x| {
            if x.abs() >= 1.0 {
                Err(ErrorKind::Num)
            } else {
                Ok(x.atanh())
            }
        })
    });
    r.add("COT", 1, 1, |ev, a| {
        m1(ev, a, |x| {
            if x == 0.0 {
                Err(ErrorKind::Div0)
            } else {
                Ok(1.0 / x.tan())
            }
        })
    });
    r.add("COTH", 1, 1, |ev, a| {
        m1(ev, a, |x| {
            if x == 0.0 {
                Err(ErrorKind::Div0)
            } else {
                Ok(1.0 / x.tanh())
            }
        })
    });
    r.add("CSC", 1, 1, |ev, a| {
        m1(ev, a, |x| {
            if x == 0.0 {
                Err(ErrorKind::Div0)
            } else {
                Ok(1.0 / x.sin())
            }
        })
    });
    r.add("CSCH", 1, 1, |ev, a| {
        m1(ev, a, |x| {
            if x == 0.0 {
                Err(ErrorKind::Div0)
            } else {
                Ok(1.0 / x.sinh())
            }
        })
    });
    r.add("SEC", 1, 1, |ev, a| m1(ev, a, |x| Ok(1.0 / x.cos())));
    r.add("SECH", 1, 1, |ev, a| m1(ev, a, |x| Ok(1.0 / x.cosh())));
    r.add("ACOT", 1, 1, |ev, a| {
        m1(ev, a, |x| Ok(std::f64::consts::FRAC_PI_2 - x.atan()))
    });
    r.add("ACOTH", 1, 1, |ev, a| {
        m1(ev, a, |x| {
            if x.abs() <= 1.0 {
                Err(ErrorKind::Num)
            } else {
                Ok(0.5 * ((x + 1.0) / (x - 1.0)).ln())
            }
        })
    });
    r.add("DEGREES", 1, 1, |ev, a| m1(ev, a, |x| Ok(x.to_degrees())));
    r.add("RADIANS", 1, 1, |ev, a| m1(ev, a, |x| Ok(x.to_radians())));
    r.add("INT", 1, 1, |ev, a| m1(ev, a, |x| Ok(x.floor())));
    r.add("TRUNC", 1, 2, |ev, a| {
        m2(ev, a, Some(0.0), |x, d| Ok(round_dir(x, d, false)))
    });
    r.add("ROUND", 2, 2, |ev, a| {
        m2(ev, a, None, |x, d| Ok(round_to(x, d)))
    });
    r.add("ROUNDUP", 2, 2, |ev, a| {
        m2(ev, a, None, |x, d| Ok(round_dir(x, d, true)))
    });
    r.add("ROUNDDOWN", 2, 2, |ev, a| {
        m2(ev, a, None, |x, d| Ok(round_dir(x, d, false)))
    });
    r.add("MROUND", 2, 2, |ev, a| m2(ev, a, None, mround));
    r.add("CEILING", 2, 2, |ev, a| m2(ev, a, None, ceiling));
    r.add("FLOOR", 2, 2, |ev, a| m2(ev, a, None, floor_fn));
    r.add("CEILING.MATH", 1, 3, |ev, a| ceiling_math(ev, a, true));
    r.add("FLOOR.MATH", 1, 3, |ev, a| ceiling_math(ev, a, false));
    r.add("CEILING.PRECISE", 1, 2, |ev, a| {
        m2(ev, a, Some(1.0), |x, s| ceiling(x, s.abs()).or(Ok(0.0)))
    });
    r.alias("ISO.CEILING", "CEILING.PRECISE");
    r.add("FLOOR.PRECISE", 1, 2, |ev, a| {
        m2(ev, a, Some(1.0), |x, s| {
            if s == 0.0 {
                Ok(0.0)
            } else {
                floor_fn(x, s.abs())
            }
        })
    });
    r.add("MOD", 2, 2, |ev, a| {
        m2(ev, a, None, |x, y| {
            if y == 0.0 {
                Err(ErrorKind::Div0)
            } else {
                Ok(x - y * (x / y).floor())
            }
        })
    });
    r.add("QUOTIENT", 2, 2, |ev, a| {
        m2(ev, a, None, |x, y| {
            if y == 0.0 {
                Err(ErrorKind::Div0)
            } else {
                Ok((x / y).trunc())
            }
        })
    });
    r.add("EVEN", 1, 1, |ev, a| {
        m1(ev, a, |x| Ok(((x.abs() / 2.0).ceil() * 2.0).copysign(x)))
    });
    r.add("ODD", 1, 1, |ev, a| {
        m1(ev, a, |x| {
            let v = x.abs().ceil();
            let v = if v % 2.0 == 0.0 { v + 1.0 } else { v };
            Ok(if x < 0.0 { -v } else { v })
        })
    });
    r.add("FACT", 1, 1, |ev, a| m1(ev, a, fact));
    r.add("FACTDOUBLE", 1, 1, |ev, a| {
        m1(ev, a, |x| {
            if x < -1.0 {
                return Err(ErrorKind::Num);
            }
            let mut n = x.trunc();
            let mut r = 1.0;
            while n > 1.0 {
                r *= n;
                n -= 2.0;
            }
            nz(r)
        })
    });
    r.add("COMBIN", 2, 2, |ev, a| m2(ev, a, None, combin));
    r.add("COMBINA", 2, 2, |ev, a| {
        m2(ev, a, None, |n, k| {
            if n < 0.0 || k < 0.0 {
                Err(ErrorKind::Num)
            } else if n.trunc() == 0.0 && k.trunc() == 0.0 {
                Ok(1.0)
            } else {
                combin(n.trunc() + k.trunc() - 1.0, k)
            }
        })
    });
    r.add("PERMUT", 2, 2, |ev, a| {
        m2(ev, a, None, |n, k| {
            let (n, k) = (n.trunc(), k.trunc());
            if n < 0.0 || k < 0.0 || k > n {
                return Err(ErrorKind::Num);
            }
            nz((0..k as u64).fold(1.0, |acc, i| acc * (n - i as f64)))
        })
    });
    r.add("PERMUTATIONA", 2, 2, |ev, a| {
        m2(ev, a, None, |n, k| {
            if n < 0.0 || k < 0.0 {
                Err(ErrorKind::Num)
            } else {
                nz(n.trunc().powf(k.trunc()))
            }
        })
    });
    r.add("GCD", 1, 255, |ev, a| gcd_lcm(ev, a, false));
    r.add("LCM", 1, 255, |ev, a| gcd_lcm(ev, a, true));
    r.add("SUM", 1, 255, sum);
    r.add("PRODUCT", 1, 255, product);
    r.add("SUMSQ", 1, 255, sumsq);
    r.add("SUMPRODUCT", 1, 255, sumproduct);
    r.add("SUBTOTAL", 2, 255, subtotal);
    r.add("RAND", 0, 0, rand);
    r.add("RANDBETWEEN", 2, 2, randbetween);
    r.add("SEQUENCE", 1, 4, sequence);
    r.add("BASE", 2, 3, base);
    r.add("DECIMAL", 2, 2, decimal);
    r.add("ROMAN", 1, 2, roman);
    r.add("ARABIC", 1, 1, arabic);
    r.add("MMULT", 2, 2, mmult);
    r.add("MULTINOMIAL", 1, 255, multinomial);
    r.add("SUMX2MY2", 2, 2, |ev, a| {
        sum_x_y(ev, a, |x, y| x * x - y * y)
    });
    r.add("SUMX2PY2", 2, 2, |ev, a| {
        sum_x_y(ev, a, |x, y| x * x + y * y)
    });
    r.add("SUMXMY2", 2, 2, |ev, a| {
        sum_x_y(ev, a, |x, y| (x - y) * (x - y))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounding() {
        assert_eq!(round_to(2.675, 2.0), 2.68);
        assert_eq!(round_to(-2.5, 0.0), -3.0);
        assert_eq!(round_to(1234.5, -2.0), 1200.0);
        assert_eq!(round_dir(1.23451, 3.0, true), 1.235);
        assert_eq!(round_dir(-1.23451, 1.0, false), -1.2);
        assert_eq!(combin(5.0, 2.0), Ok(10.0));
    }
}
