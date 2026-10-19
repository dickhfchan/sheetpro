//! Securities functions: coupon schedules, accrued interest, bond price/yield/duration,
//! discount securities, Treasury bills, odd-period bonds and French depreciation.
//!
//! Day-count bases: 0 = US 30/360, 1 = actual/actual, 2 = actual/360, 3 = actual/365,
//! 4 = European 30/360. Coupon dates are generated backwards (or forwards) from an anchor
//! date in steps of 12/frequency months; an anchor on the last day of a month keeps every
//! coupon on a month end.

use super::Registry;
use crate::ast::Expr;
use crate::eval::{Ev, EvalValue};
use sp_core::date::{date_to_serial, days_in_month, is_leap_year, serial_to_date};
use sp_core::parse::{parse_date_time_text, parse_number};
use sp_core::{ErrorKind, Value};

type R<T> = Result<T, ErrorKind>;

/// Iteration cap for the yield solvers.
const MAX_ITER: usize = 200;

// ── arguments ────────────────────────────────────────────────────────────────────────────

fn num(ev: &mut Ev<'_>, args: &[Expr], i: usize) -> R<f64> {
    let e = args.get(i).ok_or(ErrorKind::Value)?;
    if !matches!(e, Expr::Missing) && matches!(ev.eval_scalar(e), Value::Bool(_)) {
        return Err(ErrorKind::Value);
    }
    let n = ev.num(e)?;
    if n.is_finite() {
        Ok(n)
    } else {
        Err(ErrorKind::Num)
    }
}

fn current_year() -> i64 {
    serial_to_date(sp_core::date::now_serial()).map_or(2000, |d| d.0)
}

/// A date argument: serial numbers or date text, truncated to whole days.
fn date(ev: &mut Ev<'_>, args: &[Expr], i: usize) -> R<f64> {
    let e = args.get(i).ok_or(ErrorKind::Value)?;
    let n = match ev.eval_scalar(e) {
        Value::Number(n) => n,
        Value::Empty => 0.0,
        Value::Error(e) => return Err(e),
        Value::Bool(_) => return Err(ErrorKind::Value),
        Value::Text(s) => parse_number(&s)
            .map(|(n, _)| n)
            .or_else(|| parse_date_time_text(&s, current_year()))
            .ok_or(ErrorKind::Value)?,
    };
    let d = n.trunc();
    if !(0.0..=2_958_465.0).contains(&d) {
        return Err(ErrorKind::Num);
    }
    Ok(d)
}

fn basis(ev: &mut Ev<'_>, args: &[Expr], i: usize) -> R<i32> {
    let b = match args.get(i) {
        None | Some(Expr::Missing) => 0.0,
        Some(_) => num(ev, args, i)?.trunc(),
    };
    if (0.0..=4.0).contains(&b) {
        Ok(b as i32)
    } else {
        Err(ErrorKind::Num)
    }
}

fn frequency(ev: &mut Ev<'_>, args: &[Expr], i: usize) -> R<f64> {
    let f = num(ev, args, i)?.trunc();
    if f == 1.0 || f == 2.0 || f == 4.0 {
        Ok(f)
    } else {
        Err(ErrorKind::Num)
    }
}

// ── day counts ───────────────────────────────────────────────────────────────────────────

fn ymd(s: f64) -> R<(i64, u32, u32)> {
    serial_to_date(s).ok_or(ErrorKind::Num)
}

fn feb_end(y: i64, m: u32, d: i64) -> bool {
    m == 2 && d == i64::from(days_in_month(y, 2))
}

/// 30/360 day count between two serials (US/NASD rules or European).
fn days_360(s: f64, e: f64, european: bool) -> R<f64> {
    let (y1, m1, d1) = ymd(s)?;
    let (y2, m2, d2) = ymd(e)?;
    let (mut d1, mut d2) = (i64::from(d1), i64::from(d2));
    if european {
        d1 = d1.min(30);
        d2 = d2.min(30);
    } else {
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
    }
    Ok(((y2 - y1) * 360 + (i64::from(m2) - i64::from(m1)) * 30 + (d2 - d1)) as f64)
}

/// Days between two dates under a basis.
fn day_diff(s: f64, e: f64, basis: i32) -> R<f64> {
    match basis {
        0 => days_360(s, e, false),
        4 => days_360(s, e, true),
        _ => Ok(e - s),
    }
}

/// Fraction of a year between two dates (as YEARFRAC).
fn year_frac(s: f64, e: f64, basis: i32) -> R<f64> {
    let (s, e) = if s <= e { (s, e) } else { (e, s) };
    match basis {
        0 | 4 => Ok(day_diff(s, e, basis)? / 360.0),
        2 => Ok((e - s) / 360.0),
        3 => Ok((e - s) / 365.0),
        _ => {
            let (y1, m1, d1) = ymd(s)?;
            let (y2, m2, d2) = ymd(e)?;
            let year_len = if y1 == y2 {
                if is_leap_year(y1) { 366.0 } else { 365.0 }
            } else if y2 == y1 + 1 && (m1 > m2 || (m1 == m2 && d1 >= d2)) {
                let leap = (is_leap_year(y1) && m1 < 3)
                    || (is_leap_year(y2) && (m2 > 2 || (m2 == 2 && d2 == 29)));
                if leap { 366.0 } else { 365.0 }
            } else {
                let years = (y2 - y1 + 1) as f64;
                let total: f64 = (y1..=y2)
                    .map(|y| if is_leap_year(y) { 366.0 } else { 365.0 })
                    .sum();
                total / years
            };
            Ok((e - s) / year_len)
        }
    }
}

// ── coupon schedule ──────────────────────────────────────────────────────────────────────

/// A coupon-date generator anchored at one coupon date.
#[derive(Clone, Copy)]
struct Schedule {
    y: i64,
    m: u32,
    d: u32,
    eom: bool,
    step: i64,
}

impl Schedule {
    fn new(anchor: f64, freq: f64) -> R<Schedule> {
        let (y, m, d) = ymd(anchor)?;
        Ok(Schedule {
            y,
            m,
            d,
            eom: d == days_in_month(y, m),
            step: 12 / (freq as i64).max(1),
        })
    }

    /// The coupon date `k` periods after the anchor (negative = before).
    fn date(&self, k: i64) -> R<f64> {
        let total = self.y * 12 + i64::from(self.m) - 1 + k * self.step;
        let (y, m) = (total.div_euclid(12), total.rem_euclid(12) as u32 + 1);
        let dim = days_in_month(y, m);
        let d = if self.eom { dim } else { self.d.min(dim) };
        date_to_serial(y, i64::from(m), i64::from(d)).ok_or(ErrorKind::Num)
    }

    /// Index `k` of the last coupon date on or before `t`.
    fn index_at_or_before(&self, t: f64) -> R<i64> {
        let (ty, tm, _) = ymd(t)?;
        let months = (ty * 12 + i64::from(tm)) - (self.y * 12 + i64::from(self.m));
        let mut k = months.div_euclid(self.step);
        for _ in 0..8 {
            if self.date(k)? > t {
                k -= 1;
            } else if self.date(k + 1)? <= t {
                k += 1;
            } else {
                return Ok(k);
            }
        }
        Err(ErrorKind::Num)
    }
}

/// Coupon facts for a settlement date (anchored at maturity).
struct Coupon {
    pcd: f64,
    ncd: f64,
    /// Coupons payable between settlement and maturity.
    num: f64,
}

fn coupon(settle: f64, mat: f64, freq: f64) -> R<Coupon> {
    if settle >= mat {
        return Err(ErrorKind::Num);
    }
    let s = Schedule::new(mat, freq)?;
    let k = s.index_at_or_before(settle)?;
    Ok(Coupon {
        pcd: s.date(k)?,
        ncd: s.date(k + 1)?,
        num: (-k) as f64,
    })
}

/// Length of the coupon period containing settlement, in basis days.
fn coup_days(c: &Coupon, freq: f64, basis: i32) -> f64 {
    match basis {
        1 => c.ncd - c.pcd,
        3 => 365.0 / freq,
        _ => 360.0 / freq,
    }
}

fn coup_daybs(c: &Coupon, settle: f64, basis: i32) -> R<f64> {
    day_diff(c.pcd, settle, basis)
}

fn coup_daysnc(c: &Coupon, settle: f64, freq: f64, basis: i32) -> R<f64> {
    match basis {
        0 | 4 => Ok(coup_days(c, freq, basis) - coup_daybs(c, settle, basis)?),
        _ => Ok(c.ncd - settle),
    }
}

/// Normal length of the quasi-coupon period `[a, b]` in basis days.
fn period_len(a: f64, b: f64, freq: f64, basis: i32) -> f64 {
    match basis {
        1 => b - a,
        3 => 365.0 / freq,
        _ => 360.0 / freq,
    }
}

/// Σ (days of `[from, to]` inside each quasi-coupon period) / (period length), with
/// periods anchored at `anchor`. Negative when `to < from`.
fn accrual_periods(from: f64, to: f64, anchor: f64, freq: f64, basis: i32) -> R<f64> {
    accrual_core(from, to, anchor, freq, basis, false, false)
}

/// Like [`accrual_periods`], but a quasi-coupon period never contributes more than 1.
fn accrual_capped(from: f64, to: f64, anchor: f64, freq: f64, basis: i32) -> R<f64> {
    let s = Schedule::new(anchor, freq)?;
    let k0 = s.index_at_or_before(from.min(to))?;
    let (lo_t, hi_t) = (from.min(to), from.max(to));
    let mut total = 0.0;
    for k in (k0..).take(20_000) {
        let (a, b) = (s.date(k)?, s.date(k + 1)?);
        if a >= hi_t {
            let sign = if to < from { -1.0 } else { 1.0 };
            return Ok(sign * total);
        }
        let (lo, hi) = (lo_t.max(a), hi_t.min(b));
        if hi > lo {
            total += (day_diff(lo, hi, basis)? / period_len(a, b, freq, basis)).min(1.0);
        }
    }
    Err(ErrorKind::Num)
}

/// Accrual as ACCRINT counts it: periods wholly covered before the one holding `to` count
/// exactly 1, and actual/actual uses the length of the period ending at the anchor.
fn accrual_accrint(from: f64, to: f64, anchor: f64, freq: f64, basis: i32) -> R<f64> {
    accrual_core(from, to, anchor, freq, basis, true, true)
}

/// Like [`accrual_periods`], but actual/actual divides every period by the length of the
/// one ending at the anchor (ACCRINT with calc_method FALSE).
fn accrual_nl1(from: f64, to: f64, anchor: f64, freq: f64, basis: i32) -> R<f64> {
    accrual_core(from, to, anchor, freq, basis, false, true)
}

fn accrual_core(
    from: f64,
    to: f64,
    anchor: f64,
    freq: f64,
    basis: i32,
    whole: bool,
    use_nl1: bool,
) -> R<f64> {
    if to < from {
        return accrual_core(to, from, anchor, freq, basis, whole, use_nl1).map(|x| -x);
    }
    let s = Schedule::new(anchor, freq)?;
    let nl1 = s.date(0)? - s.date(-1)?;
    let k0 = s.index_at_or_before(from)?;
    let mut total = 0.0;
    for k in (k0..).take(20_000) {
        let (a, b) = (s.date(k)?, s.date(k + 1)?);
        if a >= to {
            return Ok(total);
        }
        let lo = from.max(a);
        let hi = to.min(b);
        if whole && from <= a && b < to {
            total += 1.0;
        } else if hi > lo {
            let nl = if use_nl1 && basis == 1 {
                nl1
            } else {
                period_len(a, b, freq, basis)
            };
            total += day_diff(lo, hi, basis)? / nl;
        }
    }
    Err(ErrorKind::Num)
}

// ── solvers ──────────────────────────────────────────────────────────────────────────────

/// Root of `f` near `guess`: Newton steps, then bisection over a scanned bracket.
pub(super) fn solve(f: &dyn Fn(f64) -> R<f64>, guess: f64, lo: f64, hi: f64) -> R<f64> {
    let mut x = guess;
    for _ in 0..MAX_ITER {
        let Ok(y) = f(x) else { break };
        if y.abs() < 1e-12 {
            return Ok(x);
        }
        let h = 1e-7_f64.max(x.abs() * 1e-7);
        let (Ok(a), Ok(b)) = (f(x + h), f(x - h)) else {
            break;
        };
        let dy = (a - b) / (2.0 * h);
        if dy == 0.0 || !dy.is_finite() {
            break;
        }
        let nx = x - y / dy;
        if !nx.is_finite() || nx <= lo || nx >= hi {
            break;
        }
        if (nx - x).abs() <= 1e-13 * nx.abs().max(1.0) {
            return Ok(nx);
        }
        x = nx;
    }
    // Bisection on the first sign change of a coarse scan.
    let n = 400;
    let mut prev: Option<(f64, f64)> = None;
    for i in 0..=n {
        let x = lo + (hi - lo) * (i as f64 / n as f64);
        let Ok(y) = f(x) else { continue };
        if let Some((px, py)) = prev
            && py.signum() != y.signum()
        {
            let (mut a, mut b, mut fa) = (px, x, py);
            for _ in 0..MAX_ITER {
                let m = 0.5 * (a + b);
                let fm = f(m)?;
                if fm == 0.0 || (b - a).abs() < 1e-15 {
                    return Ok(m);
                }
                if fm.signum() == fa.signum() {
                    a = m;
                    fa = fm;
                } else {
                    b = m;
                }
            }
            return Ok(0.5 * (a + b));
        }
        prev = Some((x, y));
    }
    Err(ErrorKind::Num)
}

fn fin(x: f64) -> R<f64> {
    if x.is_finite() {
        Ok(x)
    } else {
        Err(ErrorKind::Num)
    }
}

// ── bond price / yield / duration ────────────────────────────────────────────────────────

struct Bond {
    settle: f64,
    mat: f64,
    rate: f64,
    freq: f64,
    basis: i32,
}

impl Bond {
    fn parts(&self) -> R<(f64, f64, f64, f64)> {
        let c = coupon(self.settle, self.mat, self.freq)?;
        let e = coup_days(&c, self.freq, self.basis);
        let a = coup_daybs(&c, self.settle, self.basis)?;
        // Pricing measures settlement→next coupon as E − A in every basis.
        Ok((e, a, e - a, c.num))
    }

    fn price(&self, yld: f64, red: f64) -> R<f64> {
        let (e, a, dsc, n) = self.parts()?;
        let f = self.freq;
        let cpn = 100.0 * self.rate / f;
        if n <= 1.0 {
            return fin((red + cpn) / (1.0 + dsc / e * yld / f) - a / e * cpn);
        }
        let base = 1.0 + yld / f;
        let x = dsc / e;
        let mut p = red / base.powf(n - 1.0 + x);
        for k in 1..=(n as i64) {
            p += cpn / base.powf(k as f64 - 1.0 + x);
        }
        fin(p - cpn * a / e)
    }

    fn yld(&self, pr: f64, red: f64) -> R<f64> {
        let (e, a, dsc, n) = self.parts()?;
        let f = self.freq;
        if n <= 1.0 {
            let ai = a / e * self.rate / f;
            let num = (red / 100.0 + self.rate / f) - (pr / 100.0 + ai);
            return fin(num / (pr / 100.0 + ai) * f * e / dsc);
        }
        solve(&|y| Ok(self.price(y, red)? - pr), 0.1, -f + 1e-9, 1e4)
    }

    /// Macaulay duration in years.
    fn duration(&self, yld: f64) -> R<f64> {
        let (e, _, dsc, n) = self.parts()?;
        let f = self.freq;
        let cpn = 100.0 * self.rate / f;
        let base = 1.0 + yld / f;
        let x = dsc / e;
        let (mut w, mut pv) = (0.0, 0.0);
        for k in 1..=(n as i64) {
            let t = k as f64 - 1.0 + x;
            let cf = if k as f64 == n { cpn + 100.0 } else { cpn };
            let v = cf / base.powf(t);
            w += t / f * v;
            pv += v;
        }
        if pv == 0.0 {
            return Err(ErrorKind::Div0);
        }
        fin(w / pv)
    }
}

fn bond_args(ev: &mut Ev<'_>, args: &[Expr], fi: usize, bi: usize) -> R<Bond> {
    let settle = date(ev, args, 0)?;
    let mat = date(ev, args, 1)?;
    let rate = num(ev, args, 2)?;
    let freq = frequency(ev, args, fi)?;
    let basis = basis(ev, args, bi)?;
    if settle >= mat {
        return Err(ErrorKind::Num);
    }
    Ok(Bond {
        settle,
        mat,
        rate,
        freq,
        basis,
    })
}

fn price_fn(ev: &mut Ev<'_>, args: &[Expr]) -> R<f64> {
    let b = bond_args(ev, args, 5, 6)?;
    let yld = num(ev, args, 3)?;
    let red = num(ev, args, 4)?;
    if b.rate < 0.0 || yld < 0.0 || red <= 0.0 {
        return Err(ErrorKind::Num);
    }
    b.price(yld, red)
}

fn yield_fn(ev: &mut Ev<'_>, args: &[Expr]) -> R<f64> {
    let b = bond_args(ev, args, 5, 6)?;
    let pr = num(ev, args, 3)?;
    let red = num(ev, args, 4)?;
    if b.rate < 0.0 || pr <= 0.0 || red <= 0.0 {
        return Err(ErrorKind::Num);
    }
    b.yld(pr, red)
}

fn duration_fn(ev: &mut Ev<'_>, args: &[Expr], modified: bool) -> R<f64> {
    let b = bond_args(ev, args, 4, 5)?;
    let yld = num(ev, args, 3)?;
    if b.rate < 0.0 || yld < 0.0 {
        return Err(ErrorKind::Num);
    }
    let d = b.duration(yld)?;
    if modified {
        fin(d / (1.0 + yld / b.freq))
    } else {
        Ok(d)
    }
}

fn coup_fn(ev: &mut Ev<'_>, args: &[Expr], which: u8) -> R<f64> {
    let settle = date(ev, args, 0)?;
    let mat = date(ev, args, 1)?;
    let freq = frequency(ev, args, 2)?;
    let basis = basis(ev, args, 3)?;
    let c = coupon(settle, mat, freq)?;
    match which {
        0 => Ok(coup_days(&c, freq, basis)),
        1 => coup_daybs(&c, settle, basis),
        2 => coup_daysnc(&c, settle, freq, basis),
        3 => Ok(c.ncd),
        4 => Ok(c.num),
        _ => Ok(c.pcd),
    }
}

// ── accrued interest ─────────────────────────────────────────────────────────────────────

fn accrint(ev: &mut Ev<'_>, args: &[Expr]) -> R<f64> {
    let issue = date(ev, args, 0)?;
    let first = date(ev, args, 1)?;
    let settle = date(ev, args, 2)?;
    let rate = num(ev, args, 3)?;
    let par = match args.get(4) {
        None | Some(Expr::Missing) => 1000.0,
        Some(_) => num(ev, args, 4)?,
    };
    let freq = frequency(ev, args, 5)?;
    let basis = basis(ev, args, 6)?;
    let from_issue = match args.get(7) {
        None | Some(Expr::Missing) => true,
        Some(e) => ev.boolean(e)?,
    };
    if rate <= 0.0 || par <= 0.0 || issue >= settle {
        return Err(ErrorKind::Num);
    }
    // Quasi-coupon periods are anchored at the first interest date. Before the period
    // that ends there, accrual counts the issue's partial period plus whole periods; the
    // settlement's offset from the start of that last period is measured in plain days.
    let sch = Schedule::new(first, freq)?;
    let p = sch.date(-1)?;
    let fwd = |a: f64, b: f64| -> R<f64> {
        if from_issue {
            accrual_accrint(a, b, first, freq, basis)
        } else {
            accrual_nl1(a, b, first, freq, basis)
        }
    };
    let periods = if issue >= p {
        fwd(issue, settle)?
    } else {
        let k = sch.index_at_or_before(issue)?;
        let b = if sch.date(k)? == issue { k } else { k + 1 };
        let issue_frac = accrual_periods(issue, sch.date(b)?, first, freq, basis)?;
        let whole = (-1 - b).max(0) as f64;
        let g = if settle >= p {
            fwd(p, settle)?
        } else {
            // Counted backwards from the period start, as the target does (30/360 day
            // adjustments then apply to `p`, not to settlement).
            day_diff(p, settle, basis)? / period_len(p, first, freq, basis)
        };
        issue_frac + g + if from_issue { whole } else { 0.0 }
    };
    fin(par * rate / freq * periods)
}

fn accrintm(ev: &mut Ev<'_>, args: &[Expr]) -> R<f64> {
    let issue = date(ev, args, 0)?;
    let settle = date(ev, args, 1)?;
    let rate = num(ev, args, 2)?;
    let par = match args.get(3) {
        None | Some(Expr::Missing) => 1000.0,
        Some(_) => num(ev, args, 3)?,
    };
    let basis = basis(ev, args, 4)?;
    if rate <= 0.0 || par <= 0.0 || issue >= settle {
        return Err(ErrorKind::Num);
    }
    fin(par * rate * year_frac(issue, settle, basis)?)
}

// ── discount securities ──────────────────────────────────────────────────────────────────

/// (settlement, maturity, x, y, basis) with settlement < maturity; returns the year
/// fraction settlement→maturity.
fn disc_args(ev: &mut Ev<'_>, args: &[Expr]) -> R<(f64, f64, f64, f64, f64, i32)> {
    let settle = date(ev, args, 0)?;
    let mat = date(ev, args, 1)?;
    let x = num(ev, args, 2)?;
    let y = num(ev, args, 3)?;
    let basis = basis(ev, args, 4)?;
    if settle >= mat {
        return Err(ErrorKind::Num);
    }
    Ok((settle, mat, x, y, year_frac(settle, mat, basis)?, basis))
}

fn disc(ev: &mut Ev<'_>, args: &[Expr]) -> R<f64> {
    let (_, _, pr, red, t, _) = disc_args(ev, args)?;
    if pr <= 0.0 || red <= 0.0 {
        return Err(ErrorKind::Num);
    }
    fin((red - pr) / red / t)
}

fn intrate(ev: &mut Ev<'_>, args: &[Expr]) -> R<f64> {
    let (_, _, inv, red, t, _) = disc_args(ev, args)?;
    if inv <= 0.0 || red <= 0.0 {
        return Err(ErrorKind::Num);
    }
    fin((red - inv) / inv / t)
}

fn received(ev: &mut Ev<'_>, args: &[Expr]) -> R<f64> {
    let (_, _, inv, dsc, t, _) = disc_args(ev, args)?;
    if inv <= 0.0 || dsc <= 0.0 {
        return Err(ErrorKind::Num);
    }
    let d = 1.0 - dsc * t;
    if d <= 0.0 {
        return Err(ErrorKind::Num);
    }
    fin(inv / d)
}

fn pricedisc(ev: &mut Ev<'_>, args: &[Expr]) -> R<f64> {
    let (_, _, dsc, red, t, _) = disc_args(ev, args)?;
    if dsc <= 0.0 || red <= 0.0 {
        return Err(ErrorKind::Num);
    }
    fin(red * (1.0 - dsc * t))
}

fn yielddisc(ev: &mut Ev<'_>, args: &[Expr]) -> R<f64> {
    let (_, _, pr, red, t, _) = disc_args(ev, args)?;
    if pr <= 0.0 || red <= 0.0 {
        return Err(ErrorKind::Num);
    }
    fin((red / pr - 1.0) / t)
}

/// PRICEMAT / YIELDMAT arguments: (settle, maturity, issue, rate, x, basis) →
/// (fractions issue→maturity, issue→settle, settle→maturity, rate, x).
fn mat_args(ev: &mut Ev<'_>, args: &[Expr]) -> R<(f64, f64, f64, f64, f64)> {
    let settle = date(ev, args, 0)?;
    let mat = date(ev, args, 1)?;
    let issue = date(ev, args, 2)?;
    let rate = num(ev, args, 3)?;
    let x = num(ev, args, 4)?;
    let basis = basis(ev, args, 5)?;
    if settle >= mat || issue > settle || rate < 0.0 || x < 0.0 {
        return Err(ErrorKind::Num);
    }
    Ok((
        year_frac(issue, mat, basis)?,
        year_frac(issue, settle, basis)?,
        year_frac(settle, mat, basis)?,
        rate,
        x,
    ))
}

fn pricemat(ev: &mut Ev<'_>, args: &[Expr]) -> R<f64> {
    let (dim, a, dsm, rate, yld) = mat_args(ev, args)?;
    fin((100.0 + dim * rate * 100.0) / (1.0 + dsm * yld) - a * rate * 100.0)
}

fn yieldmat(ev: &mut Ev<'_>, args: &[Expr]) -> R<f64> {
    let (dim, a, dsm, rate, pr) = mat_args(ev, args)?;
    if pr <= 0.0 {
        return Err(ErrorKind::Num);
    }
    let paid = pr / 100.0 + a * rate;
    fin(((1.0 + dim * rate) - paid) / paid / dsm)
}

// ── Treasury bills ───────────────────────────────────────────────────────────────────────

fn tbill_args(ev: &mut Ev<'_>, args: &[Expr]) -> R<(f64, f64)> {
    let settle = date(ev, args, 0)?;
    let mat = date(ev, args, 1)?;
    let x = num(ev, args, 2)?;
    if settle >= mat {
        return Err(ErrorKind::Num);
    }
    // Maturity at most one calendar year after settlement.
    let (y, m, d) = ymd(settle)?;
    let limit = date_to_serial(y + 1, i64::from(m), i64::from(d)).ok_or(ErrorKind::Num)?;
    if mat > limit {
        return Err(ErrorKind::Num);
    }
    if x <= 0.0 {
        return Err(ErrorKind::Num);
    }
    Ok((mat - settle, x))
}

fn tbillprice(ev: &mut Ev<'_>, args: &[Expr]) -> R<f64> {
    let (dsm, dsc) = tbill_args(ev, args)?;
    let p = 100.0 * (1.0 - dsc * dsm / 360.0);
    if p <= 0.0 {
        return Err(ErrorKind::Num);
    }
    fin(p)
}

fn tbillyield(ev: &mut Ev<'_>, args: &[Expr]) -> R<f64> {
    let (dsm, pr) = tbill_args(ev, args)?;
    fin((100.0 - pr) / pr * 360.0 / dsm)
}

fn tbilleq(ev: &mut Ev<'_>, args: &[Expr]) -> R<f64> {
    let (dsm, dsc) = tbill_args(ev, args)?;
    let price = 100.0 * (1.0 - dsc * dsm / 360.0);
    if price <= 0.0 {
        return Err(ErrorKind::Num);
    }
    if dsm <= 182.0 {
        return fin(365.0 * dsc / (360.0 - dsc * dsm));
    }
    // A full 366-day (leap) term counts as exactly one year, as in the target.
    let year = if dsm > 365.0 { 366.0 } else { 365.0 };
    let t = dsm / year;
    let disc = t * t - (2.0 * t - 1.0) * (1.0 - 100.0 / price);
    if disc < 0.0 {
        return Err(ErrorKind::Num);
    }
    fin((-t + disc.sqrt()) / (t - 0.5))
}

// ── odd first / last period bonds ────────────────────────────────────────────────────────

struct OddFirst {
    settle: f64,
    mat: f64,
    issue: f64,
    first: f64,
    rate: f64,
    freq: f64,
    basis: i32,
}

impl OddFirst {
    fn price(&self, yld: f64, red: f64) -> R<f64> {
        let f = self.freq;
        let cpn = 100.0 * self.rate / f;
        let base = 1.0 + yld / f;
        // Regular coupons after the first one.
        let regular = coupon(self.first, self.mat, f).map_or(0.0, |c| c.num);
        let first_cpn = cpn * accrual_capped(self.issue, self.first, self.first, f, self.basis)?;
        // Periods from settlement to the first coupon (quasi-coupon periods).
        let to_first = accrual_periods(self.settle, self.first, self.first, f, self.basis)?;
        let mut p = first_cpn / base.powf(to_first);
        for k in 1..=(regular as i64) {
            p += cpn / base.powf(to_first + k as f64);
        }
        p += red / base.powf(to_first + regular);
        let accrued = cpn * accrual_periods(self.issue, self.settle, self.first, f, self.basis)?;
        fin(p - accrued)
    }
}

fn oddf_args(ev: &mut Ev<'_>, args: &[Expr]) -> R<OddFirst> {
    let settle = date(ev, args, 0)?;
    let mat = date(ev, args, 1)?;
    let issue = date(ev, args, 2)?;
    let first = date(ev, args, 3)?;
    let rate = num(ev, args, 4)?;
    let freq = frequency(ev, args, 7)?;
    let basis = basis(ev, args, 8)?;
    if !(issue < settle && settle < first && first <= mat) || rate < 0.0 {
        return Err(ErrorKind::Num);
    }
    Ok(OddFirst {
        settle,
        mat,
        issue,
        first,
        rate,
        freq,
        basis,
    })
}

fn oddfprice(ev: &mut Ev<'_>, args: &[Expr]) -> R<f64> {
    let b = oddf_args(ev, args)?;
    let yld = num(ev, args, 5)?;
    let red = num(ev, args, 6)?;
    if yld < 0.0 || red <= 0.0 {
        return Err(ErrorKind::Num);
    }
    b.price(yld, red)
}

fn oddfyield(ev: &mut Ev<'_>, args: &[Expr]) -> R<f64> {
    let b = oddf_args(ev, args)?;
    let pr = num(ev, args, 5)?;
    let red = num(ev, args, 6)?;
    if pr <= 0.0 || red <= 0.0 {
        return Err(ErrorKind::Num);
    }
    solve(&|y| Ok(b.price(y, red)? - pr), 0.1, -b.freq + 1e-9, 1e4)
}

/// ODDL*: (settle, maturity, last_interest, rate, x, redemption, frequency, basis) →
/// (Σ DC/NL, Σ A/NL, Σ DSC/NL, rate, x, redemption, frequency).
fn oddl_args(ev: &mut Ev<'_>, args: &[Expr]) -> R<(f64, f64, f64, f64, f64, f64, f64)> {
    let settle = date(ev, args, 0)?;
    let mat = date(ev, args, 1)?;
    let last = date(ev, args, 2)?;
    let rate = num(ev, args, 3)?;
    let x = num(ev, args, 4)?;
    let red = num(ev, args, 5)?;
    let freq = frequency(ev, args, 6)?;
    let basis = basis(ev, args, 7)?;
    if !(last < settle && settle < mat) || rate < 0.0 || x < 0.0 || red <= 0.0 {
        return Err(ErrorKind::Num);
    }
    let dc = accrual_periods(last, mat, last, freq, basis)?;
    let a = accrual_periods(last, settle, last, freq, basis)?;
    let dsc = accrual_periods(settle, mat, last, freq, basis)?;
    Ok((dc, a, dsc, rate, x, red, freq))
}

fn oddlprice(ev: &mut Ev<'_>, args: &[Expr]) -> R<f64> {
    let (dc, a, dsc, rate, yld, red, f) = oddl_args(ev, args)?;
    let cpn = 100.0 * rate / f;
    fin((red + dc * cpn) / (1.0 + dsc * yld / f) - a * cpn)
}

fn oddlyield(ev: &mut Ev<'_>, args: &[Expr]) -> R<f64> {
    let (dc, a, dsc, rate, pr, red, f) = oddl_args(ev, args)?;
    if pr <= 0.0 {
        return Err(ErrorKind::Num);
    }
    let cpn = 100.0 * rate / f;
    let paid = pr + a * cpn;
    fin(((red + dc * cpn) - paid) / paid * f / dsc)
}

// ── French depreciation ──────────────────────────────────────────────────────────────────

fn round0(x: f64) -> f64 {
    super::math::round_to(x, 0.0)
}

/// (cost, purchased, first period end, salvage, period, rate, basis).
fn amor_args(ev: &mut Ev<'_>, args: &[Expr]) -> R<(f64, f64, f64, f64, f64)> {
    let cost = num(ev, args, 0)?;
    let bought = date(ev, args, 1)?;
    let first = date(ev, args, 2)?;
    let salvage = num(ev, args, 3)?;
    let period = num(ev, args, 4)?.trunc();
    let rate = num(ev, args, 5)?;
    let basis = basis(ev, args, 6)?;
    if basis == 2 || cost < 0.0 || salvage < 0.0 || salvage > cost || period < 0.0 || rate <= 0.0 {
        return Err(ErrorKind::Num);
    }
    if bought > first {
        return Err(ErrorKind::Num);
    }
    Ok((
        cost,
        year_frac(bought, first, basis)?,
        salvage,
        period,
        rate,
    ))
}

fn amorlinc(ev: &mut Ev<'_>, args: &[Expr]) -> R<f64> {
    let (cost, frac0, salvage, period, rate) = amor_args(ev, args)?;
    let one = cost * rate;
    let first = frac0 * rate * cost;
    let delta = cost - salvage;
    if period == 0.0 {
        return Ok(first.min(delta));
    }
    if one == 0.0 {
        return Ok(0.0);
    }
    let full = ((delta - first) / one).trunc().max(0.0);
    if period <= full {
        Ok(one)
    } else if period == full + 1.0 {
        Ok((delta - one * full - first).max(0.0))
    } else {
        Ok(0.0)
    }
}

fn amordegrc(ev: &mut Ev<'_>, args: &[Expr]) -> R<f64> {
    let (cost, frac0, salvage, period, rate) = amor_args(ev, args)?;
    let life = 1.0 / rate;
    let coef = if life < 3.0 {
        1.0
    } else if life < 5.0 {
        1.5
    } else if life <= 6.0 {
        2.0
    } else {
        2.5
    };
    let r = rate * coef;
    let mut amort = round0(frac0 * r * cost);
    let mut book = cost - amort;
    let mut rest = book - salvage;
    if period > 1e6 {
        return Err(ErrorKind::Num);
    }
    for n in 0..(period as u64) {
        amort = round0(r * book);
        rest -= amort;
        if rest < 0.0 {
            let left = period as u64 - n;
            return Ok(match left {
                0 | 1 => round0(book * 0.5),
                _ => 0.0,
            });
        }
        book -= amort;
    }
    Ok(amort)
}

fn wrap(r: R<f64>) -> EvalValue {
    super::ret(r)
}

pub fn register(r: &mut Registry) {
    r.add("ACCRINT", 6, 8, |ev, a| wrap(accrint(ev, a)));
    r.add("ACCRINTM", 3, 5, |ev, a| wrap(accrintm(ev, a)));
    r.add("AMORDEGRC", 6, 7, |ev, a| wrap(amordegrc(ev, a)));
    r.add("AMORLINC", 6, 7, |ev, a| wrap(amorlinc(ev, a)));
    r.add("COUPDAYS", 3, 4, |ev, a| wrap(coup_fn(ev, a, 0)));
    r.add("COUPDAYBS", 3, 4, |ev, a| wrap(coup_fn(ev, a, 1)));
    r.add("COUPDAYSNC", 3, 4, |ev, a| wrap(coup_fn(ev, a, 2)));
    r.add("COUPNCD", 3, 4, |ev, a| wrap(coup_fn(ev, a, 3)));
    r.add("COUPNUM", 3, 4, |ev, a| wrap(coup_fn(ev, a, 4)));
    r.add("COUPPCD", 3, 4, |ev, a| wrap(coup_fn(ev, a, 5)));
    r.add("DISC", 4, 5, |ev, a| wrap(disc(ev, a)));
    r.add("DURATION", 5, 6, |ev, a| wrap(duration_fn(ev, a, false)));
    r.add("INTRATE", 4, 5, |ev, a| wrap(intrate(ev, a)));
    r.add("MDURATION", 5, 6, |ev, a| wrap(duration_fn(ev, a, true)));
    r.add("ODDFPRICE", 8, 9, |ev, a| wrap(oddfprice(ev, a)));
    r.add("ODDFYIELD", 8, 9, |ev, a| wrap(oddfyield(ev, a)));
    r.add("ODDLPRICE", 7, 8, |ev, a| wrap(oddlprice(ev, a)));
    r.add("ODDLYIELD", 7, 8, |ev, a| wrap(oddlyield(ev, a)));
    r.add("PRICE", 6, 7, |ev, a| wrap(price_fn(ev, a)));
    r.add("PRICEDISC", 4, 5, |ev, a| wrap(pricedisc(ev, a)));
    r.add("PRICEMAT", 5, 6, |ev, a| wrap(pricemat(ev, a)));
    r.add("RECEIVED", 4, 5, |ev, a| wrap(received(ev, a)));
    r.add("TBILLEQ", 3, 3, |ev, a| wrap(tbilleq(ev, a)));
    r.add("TBILLPRICE", 3, 3, |ev, a| wrap(tbillprice(ev, a)));
    r.add("TBILLYIELD", 3, 3, |ev, a| wrap(tbillyield(ev, a)));
    r.add("YIELD", 6, 7, |ev, a| wrap(yield_fn(ev, a)));
    r.add("YIELDDISC", 4, 5, |ev, a| wrap(yielddisc(ev, a)));
    r.add("YIELDMAT", 5, 6, |ev, a| wrap(yieldmat(ev, a)));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i64, m: i64, dd: i64) -> f64 {
        date_to_serial(y, m, dd).unwrap()
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() <= 1e-8 * b.abs().max(1.0)
    }

    #[test]
    fn coupon_schedule() {
        let c = coupon(d(2020, 1, 1), d(2021, 1, 1), 2.0).unwrap();
        assert_eq!(c.pcd, d(2020, 1, 1));
        assert_eq!(c.ncd, d(2020, 7, 1));
        assert_eq!(c.num, 2.0);
        // Month-end maturity keeps month-end coupons.
        let c = coupon(d(2020, 5, 1), d(2020, 8, 31), 2.0).unwrap();
        assert_eq!(c.pcd, d(2020, 2, 29));
        assert!(coupon(d(2021, 1, 1), d(2021, 1, 1), 2.0).is_err());
    }

    #[test]
    fn price_and_yield_round_trip() {
        let b = Bond {
            settle: d(2008, 2, 15),
            mat: d(2017, 11, 15),
            rate: 0.0575,
            freq: 2.0,
            basis: 0,
        };
        let p = b.price(0.065, 100.0).unwrap();
        assert!(close(p, 94.634_361_31), "{p}");
        let y = b.yld(p, 100.0).unwrap();
        assert!(close(y, 0.065), "{y}");
        let b = Bond {
            settle: d(2018, 7, 1),
            mat: d(2048, 1, 1),
            rate: 0.08,
            freq: 2.0,
            basis: 1,
        };
        let dur = b.duration(0.09).unwrap();
        assert!((dur - 10.919_145).abs() < 1e-5, "{dur}");
    }

    #[test]
    fn accrual_30_360() {
        // 1 Mar 2008 → 1 May 2008, semiannual, 30/360: 60/180 of a period.
        let p = accrual_periods(d(2008, 3, 1), d(2008, 5, 1), d(2008, 8, 31), 2.0, 0).unwrap();
        assert!(close(p, 60.0 / 180.0), "{p}");
    }

    #[test]
    fn solver_is_bounded() {
        assert!(solve(&|_| Ok(1.0), 0.1, -1.0, 10.0).is_err());
        let r = solve(&|x| Ok(x * x - 2.0), 1.0, 0.0, 10.0).unwrap();
        assert!(close(r, 2f64.sqrt()));
    }
}
