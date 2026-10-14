//! Exponential-smoothing forecasts: FORECAST.ETS, FORECAST.ETS.CONFINT,
//! FORECAST.ETS.SEASONALITY, FORECAST.ETS.STAT (additive error, trend and seasonality:
//! Holt-Winters "AAA"), shared with the `data.forecast_sheet` command.
//!
//! Method (from the public description of the functions and the standard Holt-Winters
//! literature): the timeline is sorted, duplicate times aggregated, and the series laid on
//! a constant step (missing points interpolated or zero). Seasonality is detected from the
//! autocorrelation of the detrended series. Smoothing parameters minimize the in-sample
//! one-step squared error (grid search, then coordinate refinement). Results follow the
//! target's behaviour but not its exact optimizer, so the last digits can differ.

use super::special::norm_inv;
use super::{Registry, ret};
use crate::ast::Expr;
use crate::eval::{Ev, is_omitted};
use sp_core::{ErrorKind, Value};

type R<T> = Result<T, ErrorKind>;

/// Most points (after laying the series on its step) one fit handles.
pub const MAX_POINTS: usize = 50_000;
/// Longest season the target accepts.
pub const MAX_SEASON: usize = 8_760;

/// A series on a constant step.
#[derive(Debug, Clone, PartialEq)]
pub struct Series {
    pub start: f64,
    pub step: f64,
    pub y: Vec<f64>,
}

impl Series {
    pub fn last_time(&self) -> f64 {
        self.start + self.step * (self.y.len().saturating_sub(1)) as f64
    }
}

fn aggregate(vals: &mut [f64], how: u8) -> f64 {
    let n = vals.len() as f64;
    match how {
        2 | 3 => n,
        4 => vals.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        5 => {
            vals.sort_by(f64::total_cmp);
            let m = vals.len() / 2;
            match (vals.get(m), m.checked_sub(1).and_then(|i| vals.get(i))) {
                (Some(a), Some(b)) if vals.len().is_multiple_of(2) => (a + b) / 2.0,
                (Some(a), _) => *a,
                _ => 0.0,
            }
        }
        6 => vals.iter().copied().fold(f64::INFINITY, f64::min),
        7 => vals.iter().sum(),
        _ => vals.iter().sum::<f64>() / n,
    }
}

/// Lay `(time, value)` points on a constant step. `None` values are missing points.
/// `complete`: interpolate missing points (else zero). `agg`: 1-7 as in the functions.
pub fn prepare(points: &[(f64, Option<f64>)], complete: bool, agg: u8) -> R<Series> {
    if !(1..=7).contains(&agg) {
        return Err(ErrorKind::Num);
    }
    let mut pts: Vec<(f64, Option<f64>)> = points.to_vec();
    if pts.iter().any(|(t, v)| !t.is_finite() || v.is_some_and(|x| !x.is_finite())) {
        return Err(ErrorKind::Num);
    }
    pts.sort_by(|a, b| a.0.total_cmp(&b.0));
    // Aggregate duplicate times.
    let mut merged: Vec<(f64, Option<f64>)> = Vec::new();
    let mut i = 0;
    while let Some(&(t, _)) = pts.get(i) {
        let mut vals: Vec<f64> = Vec::new();
        while let Some(&(u, v)) = pts.get(i) {
            if u != t {
                break;
            }
            if let Some(x) = v {
                vals.push(x);
            }
            i += 1;
        }
        let v = (!vals.is_empty()).then(|| aggregate(&mut vals, agg));
        merged.push((t, v));
    }
    if merged.len() < 2 {
        return Err(ErrorKind::Num);
    }
    let step = merged
        .windows(2)
        .filter_map(|w| match w {
            [a, b] => Some(b.0 - a.0),
            _ => None,
        })
        .fold(f64::INFINITY, f64::min);
    if !(step.is_finite() && step > 0.0) {
        return Err(ErrorKind::Num);
    }
    let start = merged.first().map_or(0.0, |p| p.0);
    let last = merged.last().map_or(0.0, |p| p.0);
    let slots = ((last - start) / step).round();
    if !(slots.is_finite() && slots >= 1.0 && slots < MAX_POINTS as f64) {
        return Err(ErrorKind::Num);
    }
    let mut y: Vec<Option<f64>> = vec![None; slots as usize + 1];
    for (t, v) in &merged {
        let k = (t - start) / step;
        let kr = k.round();
        if (k - kr).abs() > 1e-6 * k.abs().max(1.0) {
            // Times not on a common step.
            return Err(ErrorKind::Num);
        }
        if let Some(slot) = y.get_mut(kr as usize) {
            *slot = *v;
        }
    }
    // Fill missing points.
    let known: Vec<usize> = (0..y.len()).filter(|i| y.get(*i).is_some_and(Option::is_some)).collect();
    if known.len() < 2 {
        return Err(ErrorKind::Num);
    }
    let mut out = Vec::with_capacity(y.len());
    for (i, v) in y.iter().enumerate() {
        out.push(match v {
            Some(x) => *x,
            None if !complete => 0.0,
            None => {
                let prev = known.iter().rev().find(|k| **k < i);
                let next = known.iter().find(|k| **k > i);
                let val = |k: &usize| y.get(*k).copied().flatten().unwrap_or(0.0);
                match (prev, next) {
                    (Some(a), Some(b)) => {
                        let f = (i - a) as f64 / (b - a) as f64;
                        val(a) + (val(b) - val(a)) * f
                    }
                    (Some(a), None) => val(a),
                    (None, Some(b)) => val(b),
                    (None, None) => 0.0,
                }
            }
        });
    }
    Ok(Series { start, step, y: out })
}

/// Detected season length (1 = none).
pub fn detect_season(y: &[f64]) -> usize {
    let n = y.len();
    if n < 6 {
        return 1;
    }
    // Remove a least-squares line, then look for the strongest autocorrelation peak.
    let nf = n as f64;
    let mx = (nf - 1.0) / 2.0;
    let my = y.iter().sum::<f64>() / nf;
    let (mut sxy, mut sxx) = (0.0, 0.0);
    for (i, v) in y.iter().enumerate() {
        let dx = i as f64 - mx;
        sxy += dx * (v - my);
        sxx += dx * dx;
    }
    let b = if sxx > 0.0 { sxy / sxx } else { 0.0 };
    let r: Vec<f64> = y
        .iter()
        .enumerate()
        .map(|(i, v)| v - (my + b * (i as f64 - mx)))
        .collect();
    let var: f64 = r.iter().map(|v| v * v).sum();
    if var <= 1e-12 * (my.abs() + 1.0).powi(2) * nf {
        return 1;
    }
    let max_lag = (n / 2).min(MAX_SEASON);
    let acf = |lag: usize| -> f64 {
        let s: f64 = r
            .iter()
            .zip(r.iter().skip(lag))
            .map(|(a, b)| a * b)
            .sum();
        s / var
    };
    let vals: Vec<f64> = (0..=max_lag + 1).map(acf).collect();
    let mut best = (1usize, 0.0f64);
    for lag in 2..=max_lag {
        let (Some(&c), Some(&p), Some(&nx)) = (vals.get(lag), vals.get(lag - 1), vals.get(lag + 1)) else {
            continue;
        };
        // A local peak, adjusted for the shrinking overlap.
        let adj = c * nf / (nf - lag as f64);
        if c >= p && c >= nx && adj > best.1 {
            best = (lag, adj);
        }
    }
    if best.1 > 0.3 { best.0 } else { 1 }
}

/// A fitted model.
#[derive(Debug, Clone, PartialEq)]
pub struct Fit {
    pub alpha: f64,
    pub beta: f64,
    pub gamma: f64,
    /// Season length (0/1 = none).
    pub season: usize,
    pub level: f64,
    pub trend: f64,
    /// Seasonal components by absolute index modulo `season`.
    pub seasonals: Vec<f64>,
    pub n: usize,
    pub sse: f64,
    pub errors: usize,
    pub mae: f64,
    pub smape: f64,
    pub mase: f64,
    pub series: Series,
}

struct Run {
    sse: f64,
    abs: f64,
    smape: f64,
    count: usize,
    level: f64,
    trend: f64,
    seasonals: Vec<f64>,
}

fn run(y: &[f64], season: usize, a: f64, b: f64, g: f64) -> Option<Run> {
    let l_season = if season >= 2 { season } else { 0 };
    let n = y.len();
    let mean = |s: &[f64]| s.iter().sum::<f64>() / s.len().max(1) as f64;
    let (mut level, mut trend, mut seas, start) = if l_season > 0 {
        let first = y.get(..l_season)?;
        let second = y.get(l_season..2 * l_season)?;
        let l0 = mean(first);
        let t0 = (mean(second) - l0) / l_season as f64;
        let s: Vec<f64> = first.iter().map(|v| v - l0).collect();
        // States at the end of the first season.
        (l0 + t0 * (l_season as f64 - 1.0) / 2.0, t0, s, l_season)
    } else {
        let y0 = *y.first()?;
        let y1 = *y.get(1)?;
        (y0, y1 - y0, Vec::new(), 1)
    };
    let mut r = Run {
        sse: 0.0,
        abs: 0.0,
        smape: 0.0,
        count: 0,
        level: 0.0,
        trend: 0.0,
        seasonals: Vec::new(),
    };
    for t in start..n {
        let yt = *y.get(t)?;
        let si = if l_season > 0 { t % l_season } else { 0 };
        let s = seas.get(si).copied().unwrap_or(0.0);
        let pred = level + trend + s;
        let e = yt - pred;
        r.sse += e * e;
        r.abs += e.abs();
        let d = yt.abs() + pred.abs();
        if d > 0.0 {
            r.smape += 2.0 * e.abs() / d;
        }
        r.count += 1;
        let new_level = a * (yt - s) + (1.0 - a) * (level + trend);
        trend = b * (new_level - level) + (1.0 - b) * trend;
        level = new_level;
        if let Some(slot) = seas.get_mut(si) {
            *slot = g * (yt - level) + (1.0 - g) * s;
        }
        if !r.sse.is_finite() {
            return None;
        }
    }
    r.level = level;
    r.trend = trend;
    r.seasonals = seas;
    Some(r)
}

/// Fit the AAA model with season length `season` (0/1 = none).
pub fn fit(series: &Series, season: usize) -> R<Fit> {
    let y = &series.y;
    let n = y.len();
    let season = if season >= 2 { season } else { 0 };
    if season > MAX_SEASON || (season > 0 && n < 2 * season) || n < 3 {
        return Err(ErrorKind::Num);
    }
    let clamp = |x: f64| x.clamp(0.001, 0.999);
    let gammas: &[f64] = if season > 0 { &[0.05, 0.25, 0.5, 0.75, 0.95] } else { &[0.0] };
    let grid = [0.05, 0.25, 0.5, 0.75, 0.95];
    let mut best = (f64::INFINITY, 0.5, 0.1, 0.1);
    for &a in &grid {
        for &b in &grid {
            for &g in gammas {
                if let Some(r) = run(y, season, a, b, g)
                    && r.sse < best.0
                {
                    best = (r.sse, a, b, g);
                }
            }
        }
    }
    if !best.0.is_finite() {
        return Err(ErrorKind::Num);
    }
    // Coordinate refinement with shrinking steps.
    let (mut sse, mut a, mut b, mut g) = best;
    let mut step = 0.1;
    for _ in 0..40 {
        let mut improved = false;
        for k in 0..3 {
            if k == 2 && season == 0 {
                continue;
            }
            for dir in [-1.0, 1.0] {
                let (na, nb, ng) = match k {
                    0 => (clamp(a + dir * step), b, g),
                    1 => (a, clamp(b + dir * step), g),
                    _ => (a, b, clamp(g + dir * step)),
                };
                if let Some(r) = run(y, season, na, nb, ng)
                    && r.sse < sse
                {
                    (sse, a, b, g) = (r.sse, na, nb, ng);
                    improved = true;
                }
            }
        }
        if !improved {
            step /= 2.0;
            if step < 1e-4 {
                break;
            }
        }
    }
    let r = run(y, season, a, b, g).ok_or(ErrorKind::Num)?;
    let naive: f64 = y
        .windows(2)
        .filter_map(|w| match w {
            [p, q] => Some((q - p).abs()),
            _ => None,
        })
        .sum::<f64>()
        / (n - 1) as f64;
    let cnt = r.count.max(1) as f64;
    let mae = r.abs / cnt;
    Ok(Fit {
        alpha: a,
        beta: b,
        gamma: if season > 0 { g } else { 0.0 },
        season,
        level: r.level,
        trend: r.trend,
        seasonals: r.seasonals,
        n,
        sse: r.sse,
        errors: r.count,
        mae,
        smape: r.smape / cnt,
        mase: if naive > 0.0 { mae / naive } else { 0.0 },
        series: series.clone(),
    })
}

impl Fit {
    /// Steps ahead of the last point (`#NUM!` before it).
    fn horizon(&self, target: f64) -> R<f64> {
        let h = (target - self.series.last_time()) / self.series.step;
        if !h.is_finite() || h < -1e-9 {
            return Err(ErrorKind::Num);
        }
        Ok(h.max(0.0))
    }

    pub fn forecast(&self, target: f64) -> R<f64> {
        let h = self.horizon(target)?;
        let k = h.ceil() as usize;
        let s = if self.season >= 2 {
            let idx = (self.n - 1 + k.max(1)) % self.season;
            if k == 0 {
                self.seasonals.get((self.n - 1) % self.season).copied().unwrap_or(0.0)
            } else {
                self.seasonals.get(idx).copied().unwrap_or(0.0)
            }
        } else {
            0.0
        };
        let v = self.level + h * self.trend + s;
        if v.is_finite() { Ok(v) } else { Err(ErrorKind::Num) }
    }

    /// Half-width of the prediction interval at `target`.
    pub fn confint(&self, target: f64, conf: f64) -> R<f64> {
        if !(conf > 0.0 && conf < 1.0) {
            return Err(ErrorKind::Num);
        }
        let h = self.horizon(target)?;
        let k = (h.ceil() as usize).max(1);
        let sigma2 = self.sse / self.errors.max(1) as f64;
        let mut mult = 1.0;
        for j in 1..k.min(1_000_000) {
            let seasonal = if self.season >= 2 && j.is_multiple_of(self.season) { self.gamma } else { 0.0 };
            let c = self.alpha * (1.0 + j as f64 * self.beta) + seasonal;
            mult += c * c;
        }
        let z = norm_inv((1.0 + conf) / 2.0);
        let v = z * (sigma2 * mult).sqrt();
        if v.is_finite() { Ok(v) } else { Err(ErrorKind::Num) }
    }
}

/// Season length from the `seasonality` argument (1 = detect, 0 = none, else that length).
pub fn season_for(series: &Series, seasonality: f64) -> R<usize> {
    let s = seasonality.trunc();
    if !(0.0..=MAX_SEASON as f64).contains(&s) {
        return Err(ErrorKind::Num);
    }
    Ok(match s as usize {
        1 => detect_season(&series.y),
        k => k,
    })
}

// ---------------------------------------------------------------------------------------
// Worksheet functions

fn opt_num(ev: &mut Ev<'_>, args: &[Expr], i: usize, default: f64) -> R<f64> {
    match args.get(i) {
        None | Some(Expr::Missing) => Ok(default),
        Some(e) => {
            let v = ev.eval(e);
            if is_omitted(&v) {
                return Ok(default);
            }
            match ev.to_scalar(v) {
                Value::Error(e) => Err(e),
                Value::Empty => Ok(default),
                other => crate::eval::to_number(&other),
            }
        }
    }
}

/// (values, timeline) arguments → points.
fn points(ev: &mut Ev<'_>, args: &[Expr], vi: usize, ti: usize) -> R<Vec<(f64, Option<f64>)>> {
    let vals = super::arg_array(ev, args, vi)?;
    let times = super::arg_array(ev, args, ti)?;
    if vals.data.len() != times.data.len() {
        return Err(ErrorKind::NA);
    }
    if vals.data.len() > MAX_POINTS {
        return Err(ErrorKind::Num);
    }
    let mut out = Vec::with_capacity(vals.data.len());
    for (v, t) in vals.data.iter().zip(&times.data) {
        let t = match t {
            Value::Number(n) => *n,
            Value::Error(e) => return Err(*e),
            Value::Empty => continue,
            _ => return Err(ErrorKind::Value),
        };
        let v = match v {
            Value::Number(n) => Some(*n),
            Value::Empty => None,
            Value::Error(e) => return Err(*e),
            _ => return Err(ErrorKind::Value),
        };
        out.push((t, v));
    }
    Ok(out)
}

fn opts(ev: &mut Ev<'_>, args: &[Expr], first: usize) -> R<(bool, u8)> {
    let c = opt_num(ev, args, first, 1.0)?;
    let complete = match c.trunc() as i64 {
        0 => false,
        1 => true,
        _ => return Err(ErrorKind::Num),
    };
    let agg = opt_num(ev, args, first + 1, 1.0)?.trunc();
    if !(1.0..=7.0).contains(&agg) {
        return Err(ErrorKind::Num);
    }
    Ok((complete, agg as u8))
}

fn fitted(ev: &mut Ev<'_>, args: &[Expr], vi: usize, ti: usize, si: usize, oi: usize) -> R<Fit> {
    let pts = points(ev, args, vi, ti)?;
    let seasonality = opt_num(ev, args, si, 1.0)?;
    let (complete, agg) = opts(ev, args, oi)?;
    let series = prepare(&pts, complete, agg)?;
    let season = season_for(&series, seasonality)?;
    fit(&series, season)
}

/// FORECAST.ETS(target_date, values, timeline, [seasonality], [data_completion], [aggregation])
fn ets(ev: &mut Ev<'_>, a: &[Expr]) -> R<f64> {
    let target = ev.num(a.first().ok_or(ErrorKind::Value)?)?;
    fitted(ev, a, 1, 2, 3, 4)?.forecast(target)
}

/// FORECAST.ETS.CONFINT(target_date, values, timeline, [confidence_level], [seasonality],
/// [data_completion], [aggregation])
fn ets_confint(ev: &mut Ev<'_>, a: &[Expr]) -> R<f64> {
    let target = ev.num(a.first().ok_or(ErrorKind::Value)?)?;
    let conf = opt_num(ev, a, 3, 0.95)?;
    fitted(ev, a, 1, 2, 4, 5)?.confint(target, conf)
}

/// FORECAST.ETS.SEASONALITY(values, timeline, [data_completion], [aggregation])
fn ets_seasonality(ev: &mut Ev<'_>, a: &[Expr]) -> R<f64> {
    let pts = points(ev, a, 0, 1)?;
    let (complete, agg) = opts(ev, a, 2)?;
    let series = prepare(&pts, complete, agg)?;
    Ok(detect_season(&series.y) as f64)
}

/// FORECAST.ETS.STAT(values, timeline, statistic_type, [seasonality], [data_completion],
/// [aggregation])
fn ets_stat(ev: &mut Ev<'_>, a: &[Expr]) -> R<f64> {
    let kind = ev.num(a.get(2).ok_or(ErrorKind::Value)?)?.trunc();
    if !(1.0..=8.0).contains(&kind) {
        return Err(ErrorKind::Num);
    }
    let f = fitted(ev, a, 0, 1, 3, 4)?;
    Ok(match kind as u8 {
        1 => f.alpha,
        2 => f.beta,
        3 => f.gamma,
        4 => f.mase,
        5 => f.smape,
        6 => f.mae,
        7 => (f.sse / f.errors.max(1) as f64).sqrt(),
        _ => f.series.step,
    })
}

pub fn register(r: &mut Registry) {
    r.add("FORECAST.ETS", 3, 6, |ev, a| ret(ets(ev, a)));
    r.add("FORECAST.ETS.CONFINT", 3, 7, |ev, a| ret(ets_confint(ev, a)));
    r.add("FORECAST.ETS.SEASONALITY", 2, 4, |ev, a| ret(ets_seasonality(ev, a)));
    r.add("FORECAST.ETS.STAT", 3, 6, |ev, a| ret(ets_stat(ev, a)));
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn seasonal(n: usize) -> Vec<(f64, Option<f64>)> {
        let pattern = [10.0, 14.0, 8.0, 12.0];
        (0..n)
            .map(|i| (i as f64 + 1.0, Some(100.0 + 2.0 * i as f64 + pattern[i % 4])))
            .collect()
    }

    #[test]
    fn detects_season_and_forecasts_trend_plus_pattern() {
        let s = prepare(&seasonal(24), true, 1).unwrap();
        assert_eq!(s.step, 1.0);
        assert_eq!(detect_season(&s.y), 4);
        let f = fit(&s, 4).unwrap();
        // Next point: 100 + 2*24 + pattern[0] = 158.
        let v = f.forecast(25.0).unwrap();
        assert!((v - 158.0).abs() < 1.0, "{v}");
        let v = f.forecast(27.0).unwrap();
        assert!((v - (100.0 + 52.0 + 8.0)).abs() < 1.5, "{v}");
        assert!(f.confint(25.0, 0.95).unwrap() < 2.0);
        assert!(f.confint(30.0, 0.95).unwrap() >= f.confint(25.0, 0.95).unwrap());
        assert_eq!(f.forecast(10.0), Err(ErrorKind::Num));
    }

    #[test]
    fn linear_series_without_season() {
        let pts: Vec<(f64, Option<f64>)> = (0..12).map(|i| (i as f64 * 7.0, Some(5.0 + 3.0 * i as f64))).collect();
        let s = prepare(&pts, true, 1).unwrap();
        assert_eq!(s.step, 7.0);
        assert_eq!(detect_season(&s.y), 1);
        let f = fit(&s, 0).unwrap();
        let v = f.forecast(12.0 * 7.0).unwrap();
        assert!((v - 41.0).abs() < 1e-6, "{v}");
    }

    #[test]
    fn missing_duplicates_and_bad_steps() {
        let pts = vec![(1.0, Some(1.0)), (2.0, Some(2.0)), (2.0, Some(4.0)), (4.0, Some(4.0)), (5.0, None), (6.0, Some(6.0))];
        let s = prepare(&pts, true, 1).unwrap();
        assert_eq!(s.y, vec![1.0, 3.0, 3.5, 4.0, 5.0, 6.0]);
        let s = prepare(&pts, false, 7).unwrap();
        assert_eq!(s.y, vec![1.0, 6.0, 0.0, 4.0, 0.0, 6.0]);
        assert!(prepare(&[(1.0, Some(1.0)), (3.0, Some(1.0)), (4.5, Some(1.0))], true, 1).is_err());
        assert!(prepare(&[(1.0, Some(1.0))], true, 1).is_err());
        assert!(prepare(&[(0.0, Some(1.0)), (1e-9, Some(2.0)), (1e6, Some(3.0))], true, 1).is_err());
    }

    #[test]
    fn worksheet_functions() {
        use crate::recalc::{AstCache, RecalcOptions, recalc};
        use sp_core::CellAddr;
        use sp_model::{Cell, Workbook};
        let mut wb = Workbook::new();
        for (i, (t, v)) in seasonal(24).iter().enumerate() {
            wb.sheets[0].set(CellAddr::new(i as u32, 0), Cell::constant(Value::Number(*t)));
            wb.sheets[0].set(CellAddr::new(i as u32, 1), Cell::constant(Value::Number(v.unwrap())));
        }
        let fs = [
            "FORECAST.ETS(25,B1:B24,A1:A24)",
            "FORECAST.ETS.SEASONALITY(B1:B24,A1:A24)",
            "FORECAST.ETS.STAT(B1:B24,A1:A24,8)",
            "FORECAST.ETS.CONFINT(25,B1:B24,A1:A24,0.9)",
            "FORECAST.ETS(25,B1:B24,A1:A23)",
            "FORECAST.ETS(1,B1:B24,A1:A24)",
            "FORECAST.ETS.STAT(B1:B24,A1:A24,9)",
            "FORECAST.ETS(25,B1:B24,A1:A24,0)",
        ];
        for (i, f) in fs.iter().enumerate() {
            wb.sheets[0].set(CellAddr::new(i as u32, 3), Cell::formula(*f));
        }
        recalc(&mut wb, &mut AstCache::default(), RecalcOptions::default());
        let v = |r: u32| wb.sheets[0].value(CellAddr::new(r, 3)).clone();
        assert!(matches!(v(0), Value::Number(n) if (n - 158.0).abs() < 1.0), "{:?}", v(0));
        assert_eq!(v(1), Value::Number(4.0));
        assert_eq!(v(2), Value::Number(1.0));
        assert!(matches!(v(3), Value::Number(n) if n > 0.0));
        assert_eq!(v(4), Value::Error(ErrorKind::NA));
        assert_eq!(v(5), Value::Error(ErrorKind::Num));
        assert_eq!(v(6), Value::Error(ErrorKind::Num));
        assert!(matches!(v(7), Value::Number(_)));
    }
}
