//! Date & Time functions (1900 date system).

use super::{Registry, arg_array, m1, ret};
use crate::ast::Expr;
use crate::eval::{Ev, EvalValue};
use sp_core::date::{date_to_serial, days_in_month, serial_to_date, serial_to_time, weekday};
use sp_core::parse::{parse_date, parse_time};
use sp_core::{ErrorKind, Value};

fn ymd(serial: f64) -> Result<(i64, u32, u32), ErrorKind> {
    serial_to_date(serial).ok_or(ErrorKind::Num)
}

fn date_arg(ev: &mut Ev<'_>, e: &Expr) -> Result<f64, ErrorKind> {
    let v = ev.eval_scalar(e);
    match v {
        Value::Text(s) => parse_date(&s, current_year()).ok_or(ErrorKind::Value),
        other => crate::eval::to_number(&other),
    }
}

fn current_year() -> i64 {
    serial_to_date(sp_core::date::now_serial()).map_or(2000, |d| d.0)
}

fn date(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let v: Vec<Result<f64, ErrorKind>> = args.iter().map(|e| ev.num(e)).collect();
    match v.as_slice() {
        [Ok(y), Ok(m), Ok(d)] => {
            let y = y.trunc();
            if !(0.0..10000.0).contains(&y) {
                return EvalValue::err(ErrorKind::Num);
            }
            ret(date_to_serial(y as i64, m.trunc() as i64, d.trunc() as i64).ok_or(ErrorKind::Num))
        }
        _ => EvalValue::err(v.iter().find_map(|r| r.err()).unwrap_or(ErrorKind::Value)),
    }
}

fn time(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let v: Vec<Result<f64, ErrorKind>> = args.iter().map(|e| ev.num(e)).collect();
    match v.as_slice() {
        [Ok(h), Ok(m), Ok(s)] => {
            let secs = h.trunc() * 3600.0 + m.trunc() * 60.0 + s.trunc();
            if secs < 0.0 || h.trunc() > 32767.0 {
                return EvalValue::err(ErrorKind::Num);
            }
            EvalValue::num((secs / 86_400.0).fract())
        }
        _ => EvalValue::err(v.iter().find_map(|r| r.err()).unwrap_or(ErrorKind::Value)),
    }
}

fn part(ev: &mut Ev<'_>, args: &[Expr], f: fn(f64) -> Result<f64, ErrorKind>) -> EvalValue {
    let Some(a) = args.first() else {
        return EvalValue::err(ErrorKind::Value);
    };
    ret(date_arg(ev, a).and_then(|s| if s < 0.0 { Err(ErrorKind::Num) } else { f(s) }))
}

fn edate(ev: &mut Ev<'_>, args: &[Expr], eom: bool) -> EvalValue {
    let (Some(a), Some(b)) = (args.first(), args.get(1)) else {
        return EvalValue::err(ErrorKind::Value);
    };
    let (s, m) = match (date_arg(ev, a), ev.num(b)) {
        (Ok(s), Ok(m)) => (s, m.trunc() as i64),
        (Err(e), _) | (_, Err(e)) => return EvalValue::err(e),
    };
    let Ok((y, mo, d)) = ymd(s) else {
        return EvalValue::err(ErrorKind::Num);
    };
    let total = y * 12 + i64::from(mo) - 1 + m;
    let (ny, nm) = (total.div_euclid(12), (total.rem_euclid(12) + 1) as u32);
    let dim = days_in_month(ny, nm);
    let nd = if eom { dim } else { d.min(dim) };
    ret(date_to_serial(ny, i64::from(nm), i64::from(nd)).ok_or(ErrorKind::Num))
}

fn weekday_fn(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let Some(a) = args.first() else {
        return EvalValue::err(ErrorKind::Value);
    };
    let s = match date_arg(ev, a) {
        Ok(s) if s >= 0.0 => s,
        Ok(_) => return EvalValue::err(ErrorKind::Num),
        Err(e) => return EvalValue::err(e),
    };
    let ty = match ev.num_or(args, 1, 1.0) {
        Ok(t) => t.trunc() as i64,
        Err(e) => return EvalValue::err(e),
    };
    let w = i64::from(weekday(s)); // 0 = Sunday
    let r = match ty {
        1 | 17 => w + 1,
        2 | 11 => (w + 6) % 7 + 1,
        3 => (w + 6) % 7,
        12..=16 => {
            // 12 = Tuesday start … 16 = Saturday start.
            let start = ty - 10; // Tue = 2
            (w - start).rem_euclid(7) + 1
        }
        _ => return EvalValue::err(ErrorKind::Num),
    };
    EvalValue::num(r as f64)
}

fn days360(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let (Some(a), Some(b)) = (args.first(), args.get(1)) else {
        return EvalValue::err(ErrorKind::Value);
    };
    let (s, e) = match (date_arg(ev, a), date_arg(ev, b)) {
        (Ok(s), Ok(e)) => (s, e),
        (Err(x), _) | (_, Err(x)) => return EvalValue::err(x),
    };
    let euro = match ev.bool_or(args, 2, false) {
        Ok(b) => b,
        Err(e) => return EvalValue::err(e),
    };
    let (Ok((y1, m1, d1)), Ok((y2, m2, d2))) = (ymd(s), ymd(e)) else {
        return EvalValue::err(ErrorKind::Num);
    };
    let (mut d1, mut d2) = (i64::from(d1), i64::from(d2));
    if euro {
        d1 = d1.min(30);
        d2 = d2.min(30);
    } else {
        let last_feb = |y: i64, m: u32, d: i64| m == 2 && d == i64::from(days_in_month(y, 2));
        if last_feb(y1, m1, d1) {
            d1 = 30;
        }
        if d1 == 31 {
            d1 = 30;
        }
        if d2 == 31 && d1 >= 30 {
            d2 = 30;
        }
    }
    EvalValue::num(((y2 - y1) * 360 + (i64::from(m2) - i64::from(m1)) * 30 + (d2 - d1)) as f64)
}

fn holidays(ev: &mut Ev<'_>, args: &[Expr], i: usize) -> Result<Vec<f64>, ErrorKind> {
    match args.get(i) {
        None | Some(Expr::Missing) => Ok(Vec::new()),
        Some(_) => Ok(arg_array(ev, args, i)?
            .data
            .iter()
            .filter_map(|v| v.as_number())
            .map(f64::floor)
            .collect()),
    }
}

/// Weekend mask (Mon..Sun) from a NETWORKDAYS.INTL/WORKDAY.INTL weekend argument.
fn weekend_mask(ev: &mut Ev<'_>, args: &[Expr], i: usize) -> Result<[bool; 7], ErrorKind> {
    let v = match args.get(i) {
        None | Some(Expr::Missing) => return Ok([false, false, false, false, false, true, true]),
        Some(e) => ev.eval_scalar(e),
    };
    match v {
        Value::Text(s) if s.len() == 7 && s.chars().all(|c| c == '0' || c == '1') => {
            let mut m = [false; 7];
            for (k, c) in s.chars().enumerate() {
                if let Some(slot) = m.get_mut(k) {
                    *slot = c == '1';
                }
            }
            if m.iter().all(|b| *b) {
                return Err(ErrorKind::Value);
            }
            Ok(m)
        }
        Value::Number(n) => {
            let n = n.trunc() as i64;
            let mut m = [false; 7];
            match n {
                1..=7 => {
                    // 1 = Sat/Sun, 2 = Sun/Mon, … 7 = Fri/Sat.
                    let a = ((n + 4) % 7) as usize;
                    let b = ((n + 5) % 7) as usize;
                    if let Some(s) = m.get_mut(a) {
                        *s = true;
                    }
                    if let Some(s) = m.get_mut(b) {
                        *s = true;
                    }
                }
                11..=17 => {
                    if let Some(s) = m.get_mut(((n - 11 + 6) % 7) as usize) {
                        *s = true;
                    }
                }
                _ => return Err(ErrorKind::Num),
            }
            Ok(m)
        }
        Value::Error(e) => Err(e),
        _ => Err(ErrorKind::Value),
    }
}

fn is_workday(s: f64, mask: &[bool; 7], hol: &[f64]) -> bool {
    let mon0 = (weekday(s) as usize + 6) % 7; // 0 = Monday
    !mask.get(mon0).copied().unwrap_or(false) && !hol.contains(&s.floor())
}

fn networkdays(ev: &mut Ev<'_>, args: &[Expr], intl: bool) -> EvalValue {
    let (Some(a), Some(b)) = (args.first(), args.get(1)) else {
        return EvalValue::err(ErrorKind::Value);
    };
    let (s, e) = match (date_arg(ev, a), date_arg(ev, b)) {
        (Ok(s), Ok(e)) => (s.floor(), e.floor()),
        (Err(x), _) | (_, Err(x)) => return EvalValue::err(x),
    };
    let (mask, hi) = if intl {
        (weekend_mask(ev, args, 2), 3)
    } else {
        (Ok([false, false, false, false, false, true, true]), 2)
    };
    let mask = match mask {
        Ok(m) => m,
        Err(e) => return EvalValue::err(e),
    };
    let hol = match holidays(ev, args, hi) {
        Ok(h) => h,
        Err(e) => return EvalValue::err(e),
    };
    let (lo, hi_d, sign) = if s <= e { (s, e, 1.0) } else { (e, s, -1.0) };
    if hi_d - lo > 3_000_000.0 {
        return EvalValue::err(ErrorKind::Num);
    }
    let mut n = 0.0;
    let mut d = lo;
    while d <= hi_d {
        if is_workday(d, &mask, &hol) {
            n += 1.0;
        }
        d += 1.0;
    }
    EvalValue::num(sign * n)
}

fn workday(ev: &mut Ev<'_>, args: &[Expr], intl: bool) -> EvalValue {
    let (Some(a), Some(b)) = (args.first(), args.get(1)) else {
        return EvalValue::err(ErrorKind::Value);
    };
    let (s, days) = match (date_arg(ev, a), ev.num(b)) {
        (Ok(s), Ok(d)) => (s.floor(), d.trunc()),
        (Err(x), _) | (_, Err(x)) => return EvalValue::err(x),
    };
    let (mask, hi) = if intl {
        (weekend_mask(ev, args, 2), 3)
    } else {
        (Ok([false, false, false, false, false, true, true]), 2)
    };
    let mask = match mask {
        Ok(m) => m,
        Err(e) => return EvalValue::err(e),
    };
    let hol = match holidays(ev, args, hi) {
        Ok(h) => h,
        Err(e) => return EvalValue::err(e),
    };
    if days.abs() > 3_000_000.0 {
        return EvalValue::err(ErrorKind::Num);
    }
    let step = if days >= 0.0 { 1.0 } else { -1.0 };
    let mut left = days.abs();
    let mut d = s;
    while left > 0.0 {
        d += step;
        if d < 0.0 || d > 2_958_465.0 {
            return EvalValue::err(ErrorKind::Num);
        }
        if is_workday(d, &mask, &hol) {
            left -= 1.0;
        }
    }
    EvalValue::num(d)
}

fn weeknum(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let Some(a) = args.first() else {
        return EvalValue::err(ErrorKind::Value);
    };
    let s = match date_arg(ev, a) {
        Ok(s) => s.floor(),
        Err(e) => return EvalValue::err(e),
    };
    let ty = match ev.num_or(args, 1, 1.0) {
        Ok(t) => t.trunc() as i64,
        Err(e) => return EvalValue::err(e),
    };
    if ty == 21 {
        return ret(iso_week(s));
    }
    let start_dow: i64 = match ty {
        1 | 17 => 0,
        2 | 11 => 1,
        12 => 2,
        13 => 3,
        14 => 4,
        15 => 5,
        16 => 6,
        _ => return EvalValue::err(ErrorKind::Num),
    };
    let Ok((y, _, _)) = ymd(s) else {
        return EvalValue::err(ErrorKind::Num);
    };
    let Some(jan1) = date_to_serial(y, 1, 1) else {
        return EvalValue::err(ErrorKind::Num);
    };
    let offset = (i64::from(weekday(jan1)) - start_dow).rem_euclid(7);
    EvalValue::num((((s - jan1) as i64 + offset) / 7 + 1) as f64)
}

fn iso_week(s: f64) -> Result<f64, ErrorKind> {
    let mon0 = |x: f64| (i64::from(weekday(x)) + 6) % 7;
    let thursday = s - mon0(s) as f64 + 3.0;
    let (ty, _, _) = ymd(thursday).map_err(|_| ErrorKind::Num)?;
    let jan1 = date_to_serial(ty, 1, 1).ok_or(ErrorKind::Num)?;
    Ok(((thursday - jan1) / 7.0).floor() + 1.0)
}

fn yearfrac(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let (Some(a), Some(b)) = (args.first(), args.get(1)) else {
        return EvalValue::err(ErrorKind::Value);
    };
    let (mut s, mut e) = match (date_arg(ev, a), date_arg(ev, b)) {
        (Ok(s), Ok(e)) => (s.floor(), e.floor()),
        (Err(x), _) | (_, Err(x)) => return EvalValue::err(x),
    };
    let basis = match ev.num_or(args, 2, 0.0) {
        Ok(b) => b.trunc() as i64,
        Err(x) => return EvalValue::err(x),
    };
    if s > e {
        std::mem::swap(&mut s, &mut e);
    }
    let (Ok((y1, m1, d1)), Ok((y2, m2, d2))) = (ymd(s), ymd(e)) else {
        return EvalValue::err(ErrorKind::Num);
    };
    let r = match basis {
        0 => {
            let (mut d1, mut d2) = (i64::from(d1), i64::from(d2));
            let feb_end = |y: i64, m: u32, d: i64| m == 2 && d == i64::from(days_in_month(y, 2));
            if feb_end(y1, m1, d1) && feb_end(y2, m2, d2) {
                d2 = 30;
            }
            if feb_end(y1, m1, d1) {
                d1 = 30;
            }
            if d2 == 31 && d1 >= 30 {
                d2 = 30;
            }
            if d1 == 31 {
                d1 = 30;
            }
            (((y2 - y1) * 360 + (i64::from(m2) - i64::from(m1)) * 30 + (d2 - d1)) as f64) / 360.0
        }
        1 => {
            let days = e - s;
            let year_len = if y1 == y2 {
                if sp_core::date::is_leap_year(y1) {
                    366.0
                } else {
                    365.0
                }
            } else if y2 == y1 + 1 && (m1 > m2 || (m1 == m2 && d1 >= d2)) {
                let leap = (sp_core::date::is_leap_year(y1) && (m1 < 3))
                    || (sp_core::date::is_leap_year(y2) && (m2 > 2 || (m2 == 2 && d2 == 29)));
                if leap { 366.0 } else { 365.0 }
            } else {
                let total: f64 = (y1..=y2)
                    .map(|y| {
                        if sp_core::date::is_leap_year(y) {
                            366.0
                        } else {
                            365.0
                        }
                    })
                    .sum();
                total / (y2 - y1 + 1) as f64
            };
            days / year_len
        }
        2 => (e - s) / 360.0,
        3 => (e - s) / 365.0,
        4 => {
            let (d1, d2) = (i64::from(d1).min(30), i64::from(d2).min(30));
            (((y2 - y1) * 360 + (i64::from(m2) - i64::from(m1)) * 30 + (d2 - d1)) as f64) / 360.0
        }
        _ => return EvalValue::err(ErrorKind::Num),
    };
    EvalValue::num(r)
}

fn datedif(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let (Some(a), Some(b), Some(c)) = (args.first(), args.get(1), args.get(2)) else {
        return EvalValue::err(ErrorKind::Value);
    };
    let (s, e, unit) = match (date_arg(ev, a), date_arg(ev, b), ev.text(c)) {
        (Ok(s), Ok(e), Ok(u)) => (s.floor(), e.floor(), u.to_ascii_uppercase()),
        (Err(x), ..) | (_, Err(x), _) | (.., Err(x)) => return EvalValue::err(x),
    };
    if s > e {
        return EvalValue::err(ErrorKind::Num);
    }
    let (Ok((y1, m1, d1)), Ok((y2, m2, d2))) = (ymd(s), ymd(e)) else {
        return EvalValue::err(ErrorKind::Num);
    };
    let months = (y2 - y1) * 12 + i64::from(m2) - i64::from(m1) - i64::from(d2 < d1);
    let r = match unit.as_str() {
        "D" => e - s,
        "M" => months as f64,
        "Y" => (months / 12) as f64,
        "YM" => (months % 12) as f64,
        "MD" => {
            if d2 >= d1 {
                f64::from(d2 - d1)
            } else {
                let (py, pm) = if m2 == 1 { (y2 - 1, 12) } else { (y2, m2 - 1) };
                f64::from(days_in_month(py, pm)) - f64::from(d1) + f64::from(d2)
            }
        }
        "YD" => {
            let ny = if (m2, d2) >= (m1, d1) { y2 } else { y2 - 1 };
            match date_to_serial(ny, i64::from(m1), i64::from(d1)) {
                Some(anchor) => e - anchor,
                None => return EvalValue::err(ErrorKind::Num),
            }
        }
        _ => return EvalValue::err(ErrorKind::Num),
    };
    EvalValue::num(r)
}

pub fn register(r: &mut Registry) {
    r.add("DATE", 3, 3, date);
    r.add("TIME", 3, 3, time);
    r.add("YEAR", 1, 1, |ev, a| {
        part(ev, a, |s| ymd(s).map(|d| d.0 as f64))
    });
    r.add("MONTH", 1, 1, |ev, a| {
        part(ev, a, |s| ymd(s).map(|d| f64::from(d.1)))
    });
    r.add("DAY", 1, 1, |ev, a| {
        part(ev, a, |s| ymd(s).map(|d| f64::from(d.2)))
    });
    r.add("HOUR", 1, 1, |ev, a| {
        part(ev, a, |s| Ok(f64::from(serial_to_time(s).0)))
    });
    r.add("MINUTE", 1, 1, |ev, a| {
        part(ev, a, |s| Ok(f64::from(serial_to_time(s).1)))
    });
    r.add("SECOND", 1, 1, |ev, a| {
        part(ev, a, |s| Ok(f64::from(serial_to_time(s).2)))
    });
    r.add("TODAY", 0, 0, |ev, _| EvalValue::num(ev.host.now().floor()));
    r.add("NOW", 0, 0, |ev, _| EvalValue::num(ev.host.now()));
    r.add("WEEKDAY", 1, 2, weekday_fn);
    r.add("EDATE", 2, 2, |ev, a| edate(ev, a, false));
    r.add("EOMONTH", 2, 2, |ev, a| edate(ev, a, true));
    r.add("DAYS", 2, 2, |ev, a| {
        match (
            a.first().map(|e| date_arg(ev, e)),
            a.get(1).map(|e| date_arg(ev, e)),
        ) {
            (Some(Ok(e)), Some(Ok(s))) => EvalValue::num(e.floor() - s.floor()),
            (Some(Err(x)), _) | (_, Some(Err(x))) => EvalValue::err(x),
            _ => EvalValue::err(ErrorKind::Value),
        }
    });
    r.add("DAYS360", 2, 3, days360);
    r.add("DATEVALUE", 1, 1, |ev, a| {
        match a.first().map(|e| ev.text(e)) {
            Some(Ok(s)) => {
                let date_part = s
                    .split_once(' ')
                    .filter(|(_, t)| parse_time(t).is_some())
                    .map_or(s.as_str(), |(d, _)| d);
                ret(parse_date(date_part, current_year()).ok_or(ErrorKind::Value))
            }
            Some(Err(e)) => EvalValue::err(e),
            None => EvalValue::err(ErrorKind::Value),
        }
    });
    r.add("TIMEVALUE", 1, 1, |ev, a| {
        match a.first().map(|e| ev.text(e)) {
            Some(Ok(s)) => {
                let t = s
                    .split_once(' ')
                    .and_then(|(d, t)| parse_date(d, 2000).and(parse_time(t)))
                    .or_else(|| parse_time(&s));
                ret(t.map(f64::fract).ok_or(ErrorKind::Value))
            }
            Some(Err(e)) => EvalValue::err(e),
            None => EvalValue::err(ErrorKind::Value),
        }
    });
    r.add("NETWORKDAYS", 2, 3, |ev, a| networkdays(ev, a, false));
    r.add("NETWORKDAYS.INTL", 2, 4, |ev, a| networkdays(ev, a, true));
    r.add("WORKDAY", 2, 3, |ev, a| workday(ev, a, false));
    r.add("WORKDAY.INTL", 2, 4, |ev, a| workday(ev, a, true));
    r.add("WEEKNUM", 1, 2, weeknum);
    r.add("ISOWEEKNUM", 1, 1, |ev, a| {
        m1(ev, a, |s| iso_week(s.floor()))
    });
    r.add("YEARFRAC", 2, 3, yearfrac);
    r.add("DATEDIF", 3, 3, datedif);
}
