//! Chart geometry (UI-free): nice axis ticks, value scaling, histogram bins, and a flat list
//! of drawing primitives for a chart or sparkline laid out in a `w × h` box. The shell only
//! maps primitives to its painter and `Paint` tokens to theme colours.

use sp_model::Color;
use sp_model::chart::{ChartKind, Grouping, LegendPos, SeriesKind, SparklineGroup, SparklineKind};

/// Resolved data for one series.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SeriesData {
    pub name: String,
    /// `None` = blank / non-numeric point (a gap).
    pub values: Vec<Option<f64>>,
    pub kind: SeriesKind,
    pub secondary: bool,
    pub color: Option<Color>,
}

/// Everything needed to draw a chart, with references already resolved to values.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ChartData {
    pub kind: ChartKind,
    pub grouping: Grouping,
    pub categories: Vec<String>,
    /// Scatter: X values (from the first series' categories); empty = 1, 2, 3…
    pub xs: Vec<Option<f64>>,
    pub series: Vec<SeriesData>,
    pub title: Option<String>,
    pub x_title: Option<String>,
    pub y_title: Option<String>,
    pub legend: LegendPos,
    pub data_labels: bool,
    pub markers: bool,
    pub bins: Option<u32>,
}

/// Colour roles; the shell maps them to theme tokens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Paint {
    /// The n-th colour of the series palette.
    Series(usize),
    /// An explicit colour from the file / params.
    Rgb(Color),
    Background,
    Border,
    Axis,
    Gridline,
    Text,
    /// Muted text (tick labels).
    SubText,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HAnchor {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VAnchor {
    Top,
    Center,
    Bottom,
}

/// A drawing primitive in chart-local px (origin = chart top-left).
#[derive(Debug, Clone, PartialEq)]
pub enum Prim {
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        fill: Option<Paint>,
        stroke: Option<Paint>,
    },
    /// Open polyline.
    Line {
        pts: Vec<(f32, f32)>,
        width: f32,
        paint: Paint,
    },
    /// Filled convex polygon.
    Poly {
        pts: Vec<(f32, f32)>,
        fill: Paint,
    },
    Circle {
        x: f32,
        y: f32,
        r: f32,
        fill: Paint,
    },
    Text {
        x: f32,
        y: f32,
        text: String,
        size: f32,
        h: HAnchor,
        v: VAnchor,
        paint: Paint,
        /// Rotated 90° counter-clockwise (vertical axis titles).
        vertical: bool,
        bold: bool,
    },
}

/// Approximate rendered width of `text` at font `size` px (proportional face).
pub fn text_width(text: &str, size: f32) -> f32 {
    text.chars().count() as f32 * size * 0.55
}

// ---------------------------------------------------------------------------------------
// Scales

/// A "nice" axis: `lo..=hi` in `step` increments.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NiceScale {
    pub lo: f64,
    pub hi: f64,
    pub step: f64,
}

impl NiceScale {
    pub fn ticks(&self) -> Vec<f64> {
        let mut v = Vec::new();
        if self.step.is_nan() || self.step <= 0.0 || !self.lo.is_finite() || !self.hi.is_finite() {
            return v;
        }
        let n = ((self.hi - self.lo) / self.step).round();
        if !(0.0..=1000.0).contains(&n) {
            return v;
        }
        for i in 0..=(n as i64) {
            let t = self.lo + i as f64 * self.step;
            // Kill float noise like 0.30000000000000004.
            v.push(round_to_step(t, self.step));
        }
        v
    }

    /// Position of `v` in 0..=1 along the scale.
    pub fn frac(&self, v: f64) -> f64 {
        let span = self.hi - self.lo;
        if span.abs() < f64::EPSILON || !span.is_finite() {
            0.0
        } else {
            (v - self.lo) / span
        }
    }
}

fn round_to_step(t: f64, step: f64) -> f64 {
    let dec = decimals_for(step) as i32;
    let p = 10f64.powi(dec.min(15));
    (t * p).round() / p
}

/// A nice step (1, 2, 5 × 10ⁿ) for a raw step.
pub fn nice_step(raw: f64) -> f64 {
    if raw.is_nan() || raw <= 0.0 || !raw.is_finite() {
        return 1.0;
    }
    let exp = raw.log10().floor();
    let base = 10f64.powf(exp);
    let f = raw / base;
    let nf = if f <= 1.0 {
        1.0
    } else if f <= 2.0 {
        2.0
    } else if f <= 5.0 {
        5.0
    } else {
        10.0
    };
    nf * base
}

/// Axis scale covering `min..=max` with about `max_ticks` intervals. Value axes of bar,
/// column, line and area charts include zero when the data is all one sign.
pub fn nice_scale(min: f64, max: f64, max_ticks: usize, include_zero: bool) -> NiceScale {
    let (mut lo, mut hi) = if min.is_finite() && max.is_finite() && min <= max {
        (min, max)
    } else {
        (0.0, 1.0)
    };
    if include_zero {
        lo = lo.min(0.0);
        hi = hi.max(0.0);
    }
    if (hi - lo).abs() < f64::EPSILON {
        if hi == 0.0 {
            hi = 1.0;
        } else if hi > 0.0 {
            lo = if include_zero {
                0.0
            } else {
                hi - hi.abs() * 0.5
            };
            hi += hi.abs() * 0.1;
        } else {
            hi = 0.0;
        }
    }
    let ticks = max_ticks.clamp(2, 20) as f64;
    let step = nice_step((hi - lo) / ticks);
    let nlo = (lo / step).floor() * step;
    let nhi = (hi / step).ceil() * step;
    NiceScale {
        lo: round_to_step(nlo, step),
        hi: round_to_step(nhi, step),
        step,
    }
}

/// Decimal places needed to print multiples of `step` exactly.
pub fn decimals_for(step: f64) -> usize {
    if step.is_nan() || step <= 0.0 || !step.is_finite() {
        return 0;
    }
    let d = -step.log10().floor();
    if d <= 0.0 { 0 } else { (d as usize).min(10) }
}

/// Tick label: thousands separators, decimals matching the step.
pub fn format_tick(v: f64, step: f64, percent: bool) -> String {
    if percent {
        return format!("{:.0}%", v * 100.0);
    }
    let dec = decimals_for(step);
    let s = format!("{:.*}", dec, v.abs());
    let (int, frac) = s.split_once('.').map_or((s.as_str(), ""), |(a, b)| (a, b));
    let mut grouped = String::new();
    let n = int.len();
    for (i, ch) in int.chars().enumerate() {
        if i > 0 && (n - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    let neg = v < 0.0 && s.chars().any(|c| c.is_ascii_digit() && c != '0');
    format!(
        "{}{}{}{}",
        if neg { "-" } else { "" },
        grouped,
        if frac.is_empty() { "" } else { "." },
        frac
    )
}

/// Short label for a data point value.
pub fn format_value(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format_tick(v, 1.0, false)
    } else {
        let mag = v.abs();
        let dec = if mag >= 100.0 {
            1
        } else if mag >= 1.0 {
            2
        } else {
            3
        };
        let s = format!("{v:.dec$}");
        let s = s.trim_end_matches('0').trim_end_matches('.');
        s.to_string()
    }
}

// ---------------------------------------------------------------------------------------
// Histogram

/// One histogram bin: `(lo, hi]` (the first bin also includes `lo`).
#[derive(Debug, Clone, PartialEq)]
pub struct Bin {
    pub lo: f64,
    pub hi: f64,
    pub count: usize,
}

/// Maximum bins drawn.
pub const MAX_BINS: usize = 500;

/// Bin `values` like the target's automatic histogram: Scott's normal reference rule for the
/// width (`3.5 σ / n^(1/3)`), or `bins` equal-width bins when given.
pub fn histogram_bins(values: &[f64], bins: Option<u32>) -> Vec<Bin> {
    let vals: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
    let (Some(min), Some(max)) = (
        vals.iter().copied().reduce(f64::min),
        vals.iter().copied().reduce(f64::max),
    ) else {
        return Vec::new();
    };
    let n = vals.len() as f64;
    let count = match bins {
        Some(b) => (b as usize).clamp(1, MAX_BINS),
        None => {
            let mean = vals.iter().sum::<f64>() / n;
            let var = vals.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n.max(1.0);
            let width = 3.5 * var.sqrt() / n.cbrt();
            if width > 0.0 && (max - min) > 0.0 {
                (((max - min) / width).ceil() as usize).clamp(1, MAX_BINS)
            } else {
                1
            }
        }
    };
    let span = max - min;
    let width = if span > 0.0 { span / count as f64 } else { 1.0 };
    let mut out: Vec<Bin> = (0..count)
        .map(|i| Bin {
            lo: min + i as f64 * width,
            hi: if i + 1 == count {
                max.max(min + width)
            } else {
                min + (i + 1) as f64 * width
            },
            count: 0,
        })
        .collect();
    for v in vals {
        let i = if width > 0.0 {
            // (lo, hi] bins: a value on a boundary belongs to the lower bin.
            let k = ((v - min) / width).ceil() as isize - 1;
            k.clamp(0, count as isize - 1) as usize
        } else {
            0
        };
        if let Some(b) = out.get_mut(i) {
            b.count += 1;
        }
    }
    out
}

/// Category label for a bin, `[lo, hi]` for the first and `(lo, hi]` after.
pub fn bin_label(b: &Bin, first: bool) -> String {
    let span = (b.hi - b.lo).abs();
    let step = if span > 0.0 {
        nice_step(span) / 100.0
    } else {
        1.0
    };
    let f = |v: f64| {
        let t = format_tick(v, step.max(1e-9), false);
        if t.contains('.') {
            t.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            t
        }
    };
    format!("{}{}, {}]", if first { "[" } else { "(" }, f(b.lo), f(b.hi))
}

// ---------------------------------------------------------------------------------------
// Chart layout

struct Ctx {
    out: Vec<Prim>,
    s: f32,
}

impl Ctx {
    #[allow(clippy::too_many_arguments)]
    fn text(&mut self, x: f32, y: f32, t: &str, size: f32, h: HAnchor, v: VAnchor, paint: Paint) {
        self.out.push(Prim::Text {
            x,
            y,
            text: t.to_string(),
            size: size * self.s,
            h,
            v,
            paint,
            vertical: false,
            bold: false,
        });
    }
}

fn series_paint(sd: &SeriesData, i: usize) -> Paint {
    sd.color.map_or(Paint::Series(i), Paint::Rgb)
}

/// Lay out a chart in a `w × h` px box; `scale` multiplies font sizes and marks (zoom).
pub fn chart_scene(d: &ChartData, w: f32, h: f32, scale: f32) -> Vec<Prim> {
    let s = scale.clamp(0.1, 8.0);
    let mut c = Ctx { out: Vec::new(), s };
    if !(w > 4.0 && h > 4.0) {
        return c.out;
    }
    c.out.push(Prim::Rect {
        x: 0.0,
        y: 0.0,
        w,
        h,
        fill: Some(Paint::Background),
        stroke: Some(Paint::Border),
    });
    let pad = 8.0 * s;
    let mut area = (pad, pad, w - pad, h - pad); // x0, y0, x1, y1
    if let Some(t) = d.title.as_deref().filter(|t| !t.is_empty()) {
        c.out.push(Prim::Text {
            x: w / 2.0,
            y: pad,
            text: t.to_string(),
            size: 14.0 * s,
            h: HAnchor::Center,
            v: VAnchor::Top,
            paint: Paint::Text,
            vertical: false,
            bold: true,
        });
        area.1 += 24.0 * s;
    }
    // Histogram: replace the data with bins.
    let hist;
    let d = if d.kind == ChartKind::Histogram {
        let vals: Vec<f64> = d
            .series
            .first()
            .map(|sd| sd.values.iter().flatten().copied().collect())
            .unwrap_or_default();
        let bins = histogram_bins(&vals, d.bins);
        hist = ChartData {
            kind: ChartKind::Histogram,
            categories: bins
                .iter()
                .enumerate()
                .map(|(i, b)| bin_label(b, i == 0))
                .collect(),
            series: vec![SeriesData {
                name: d.series.first().map(|x| x.name.clone()).unwrap_or_default(),
                values: bins.iter().map(|b| Some(b.count as f64)).collect(),
                color: d.series.first().and_then(|x| x.color),
                ..SeriesData::default()
            }],
            legend: LegendPos::None,
            ..d.clone()
        };
        &hist
    } else {
        d
    };
    // Legend entries.
    let entries: Vec<(String, Paint)> = if d.kind.is_round() {
        let sd = d.series.first();
        (0..sd.map_or(0, |x| x.values.len()))
            .map(|i| {
                (
                    d.categories
                        .get(i)
                        .cloned()
                        .unwrap_or_else(|| (i + 1).to_string()),
                    Paint::Series(i),
                )
            })
            .collect()
    } else {
        d.series
            .iter()
            .enumerate()
            .map(|(i, sd)| {
                let name = if sd.name.is_empty() {
                    format!("Series{}", i + 1)
                } else {
                    sd.name.clone()
                };
                (name, series_paint(sd, i))
            })
            .collect()
    };
    if d.legend != LegendPos::None && !entries.is_empty() {
        area = legend(&mut c, &entries, d.legend, area);
    }
    if d.kind.is_round() {
        pie(&mut c, d, area);
    } else {
        axes_chart(&mut c, d, area);
    }
    c.out
}

fn legend(
    c: &mut Ctx,
    entries: &[(String, Paint)],
    pos: LegendPos,
    area: (f32, f32, f32, f32),
) -> (f32, f32, f32, f32) {
    let s = c.s;
    let fs = 9.0;
    let sw = 8.0 * s;
    let gap = 4.0 * s;
    let row_h = 14.0 * s;
    let (x0, y0, x1, y1) = area;
    let item_w = |t: &str| sw + gap + text_width(t, fs * s) + 10.0 * s;
    let put = |c: &mut Ctx, x: f32, y: f32, t: &str, p: Paint| {
        c.out.push(Prim::Rect {
            x,
            y: y - sw / 2.0,
            w: sw,
            h: sw,
            fill: Some(p),
            stroke: None,
        });
        c.text(
            x + sw + gap,
            y,
            t,
            fs,
            HAnchor::Left,
            VAnchor::Center,
            Paint::SubText,
        );
    };
    match pos {
        LegendPos::Right | LegendPos::Left => {
            let lw = entries
                .iter()
                .map(|(t, _)| item_w(t))
                .fold(0.0f32, f32::max)
                .min((x1 - x0) * 0.4);
            let total = row_h * entries.len() as f32;
            let mut y = (y0 + y1) / 2.0 - total / 2.0 + row_h / 2.0;
            let x = if pos == LegendPos::Right { x1 - lw } else { x0 };
            for (t, p) in entries {
                if y > y1 {
                    break;
                }
                put(c, x, y, t, *p);
                y += row_h;
            }
            if pos == LegendPos::Right {
                (x0, y0, x1 - lw - gap, y1)
            } else {
                (x0 + lw + gap, y0, x1, y1)
            }
        }
        LegendPos::Bottom | LegendPos::Top => {
            let total: f32 = entries.iter().map(|(t, _)| item_w(t)).sum();
            let mut x = ((x0 + x1) / 2.0 - total / 2.0).max(x0);
            let y = if pos == LegendPos::Bottom {
                y1 - row_h / 2.0
            } else {
                y0 + row_h / 2.0
            };
            for (t, p) in entries {
                if x > x1 {
                    break;
                }
                put(c, x, y, t, *p);
                x += item_w(t);
            }
            if pos == LegendPos::Bottom {
                (x0, y0, x1, y1 - row_h - gap)
            } else {
                (x0, y0 + row_h + gap, x1, y1)
            }
        }
        LegendPos::None => area,
    }
}

/// Arc segments per full turn for pie slices.
const ARC_STEPS: f32 = 120.0;

fn pie(c: &mut Ctx, d: &ChartData, area: (f32, f32, f32, f32)) {
    let (x0, y0, x1, y1) = area;
    let cx = (x0 + x1) / 2.0;
    let cy = (y0 + y1) / 2.0;
    let r = ((x1 - x0).min(y1 - y0) / 2.0 - 4.0 * c.s).max(1.0);
    let inner = if d.kind == ChartKind::Doughnut {
        r * 0.5
    } else {
        0.0
    };
    let vals: Vec<f64> = d
        .series
        .first()
        .map(|sd| sd.values.iter().map(|v| v.unwrap_or(0.0).abs()).collect())
        .unwrap_or_default();
    let total: f64 = vals.iter().sum();
    if total.is_nan() || total <= 0.0 {
        return;
    }
    let tau = std::f32::consts::TAU;
    // First slice starts at 12 o'clock and runs clockwise (screen y grows downward).
    let mut a = -tau / 4.0;
    let pt = |ang: f32, rad: f32| (cx + rad * ang.cos(), cy + rad * ang.sin());
    let mut labels = Vec::new();
    for (i, v) in vals.iter().enumerate() {
        let sweep = (*v / total) as f32 * tau;
        if sweep <= 0.0 {
            continue;
        }
        // Convex fans of at most half a turn each, finely subdivided along the arc.
        let fill = Paint::Series(i);
        let pieces = ((sweep / std::f32::consts::PI).ceil() as usize).max(1);
        for k in 0..pieces {
            let a0 = a + sweep * k as f32 / pieces as f32;
            let a1 = a + sweep * (k + 1) as f32 / pieces as f32;
            let steps = (((a1 - a0) / tau * ARC_STEPS).ceil() as usize).max(1);
            let mut pts = vec![(cx, cy)];
            for j in 0..=steps {
                pts.push(pt(a0 + (a1 - a0) * j as f32 / steps as f32, r));
            }
            c.out.push(Prim::Poly { pts, fill });
        }
        // Slice separators.
        c.out.push(Prim::Line {
            pts: vec![pt(a, inner), pt(a, r)],
            width: 1.5 * c.s,
            paint: Paint::Background,
        });
        if d.data_labels {
            let mid = a + sweep / 2.0;
            let (lx, ly) = pt(
                mid,
                if inner > 0.0 {
                    (r + inner) / 2.0
                } else {
                    r * 0.65
                },
            );
            labels.push((lx, ly, format!("{:.0}%", v / total * 100.0)));
        }
        a += sweep;
    }
    if inner > 0.0 {
        c.out.push(Prim::Circle {
            x: cx,
            y: cy,
            r: inner,
            fill: Paint::Background,
        });
    }
    for (x, y, t) in labels {
        c.text(
            x,
            y,
            &t,
            9.0,
            HAnchor::Center,
            VAnchor::Center,
            Paint::Background,
        );
    }
}

/// Value range of the series plotted on one axis, honouring stacking.
fn value_range(d: &ChartData, secondary: bool) -> Option<(f64, f64)> {
    let sers: Vec<&SeriesData> = d
        .series
        .iter()
        .filter(|s| s.secondary == secondary)
        .collect();
    if sers.is_empty() {
        return None;
    }
    if d.grouping == Grouping::PercentStacked && stacks(d) {
        let any_neg = sers
            .iter()
            .flat_map(|s| s.values.iter().flatten())
            .any(|v| *v < 0.0);
        return Some((if any_neg { -1.0 } else { 0.0 }, 1.0));
    }
    let n = sers.iter().map(|s| s.values.len()).max().unwrap_or(0);
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    if d.grouping == Grouping::Stacked && stacks(d) {
        for i in 0..n {
            let (mut pos, mut neg) = (0.0, 0.0);
            for s in &sers {
                let v = s.values.get(i).copied().flatten().unwrap_or(0.0);
                if v >= 0.0 {
                    pos += v;
                } else {
                    neg += v;
                }
            }
            lo = lo.min(neg);
            hi = hi.max(pos);
        }
    } else {
        for v in sers.iter().flat_map(|s| s.values.iter().flatten()) {
            lo = lo.min(*v);
            hi = hi.max(*v);
        }
    }
    (lo.is_finite() && hi.is_finite()).then_some((lo, hi))
}

fn stacks(d: &ChartData) -> bool {
    matches!(
        d.kind,
        ChartKind::Column | ChartKind::Bar | ChartKind::Line | ChartKind::Area
    )
}

/// Where a series' point `i` sits after stacking: (base, top) in data units.
fn stacked_point(d: &ChartData, si: usize, i: usize) -> Option<(f64, f64)> {
    let sd = d.series.get(si)?;
    let v = sd.values.get(i).copied().flatten();
    if d.grouping == Grouping::Clustered || !stacks(d) {
        return v.map(|v| (0.0, v));
    }
    let v = v.unwrap_or(0.0);
    let mut base = 0.0;
    let mut total = 0.0;
    for (k, o) in d.series.iter().enumerate() {
        if o.secondary != sd.secondary {
            continue;
        }
        let ov = o.values.get(i).copied().flatten().unwrap_or(0.0);
        total += ov.abs();
        if k < si && (ov >= 0.0) == (v >= 0.0) {
            base += ov;
        }
    }
    if d.grouping == Grouping::PercentStacked {
        if total <= 0.0 {
            return Some((0.0, 0.0));
        }
        Some((base / total, (base + v) / total))
    } else {
        Some((base, base + v))
    }
}

fn axes_chart(c: &mut Ctx, d: &ChartData, area: (f32, f32, f32, f32)) {
    let s = c.s;
    let fs = 9.0;
    let (mut x0, y0, x1, mut y1) = area;
    let horizontal = d.kind == ChartKind::Bar;
    let scatter = d.kind == ChartKind::Scatter;
    let percent = d.grouping == Grouping::PercentStacked && stacks(d);
    let n_cat = d
        .series
        .iter()
        .map(|sd| sd.values.len())
        .max()
        .unwrap_or(0)
        .max(d.categories.len().min(10_000))
        .max(1);
    // Axis titles.
    if let Some(t) = d.y_title.as_deref().filter(|t| !t.is_empty()) {
        c.out.push(Prim::Text {
            x: x0 + 6.0 * s,
            y: (y0 + y1) / 2.0,
            text: t.to_string(),
            size: 10.0 * s,
            h: HAnchor::Center,
            v: VAnchor::Center,
            paint: Paint::SubText,
            vertical: true,
            bold: true,
        });
        x0 += 16.0 * s;
    }
    if let Some(t) = d.x_title.as_deref().filter(|t| !t.is_empty()) {
        c.text(
            (x0 + x1) / 2.0,
            y1,
            t,
            10.0,
            HAnchor::Center,
            VAnchor::Bottom,
            Paint::SubText,
        );
        if let Some(Prim::Text { bold, .. }) = c.out.last_mut() {
            *bold = true;
        }
        y1 -= 16.0 * s;
    }
    let primary_range = value_range(d, false).or_else(|| value_range(d, true));
    let secondary_range =
        if d.series.iter().any(|x| x.secondary) && d.series.iter().any(|x| !x.secondary) {
            value_range(d, true)
        } else {
            None
        };
    let (vmin, vmax) = primary_range.unwrap_or((0.0, 1.0));
    let val_len = if horizontal { x1 - x0 } else { y1 - y0 };
    let max_ticks = ((val_len / (28.0 * s)).floor() as usize).clamp(2, 10);
    let vscale = if percent {
        let lo = if vmin < 0.0 { -1.0 } else { 0.0 };
        NiceScale {
            lo,
            hi: 1.0,
            step: if lo < 0.0 { 0.5 } else { 0.2 },
        }
    } else {
        nice_scale(vmin, vmax, max_ticks, !scatter)
    };
    let sscale = secondary_range.map(|(a, b)| {
        // Same tick count as the primary axis so gridlines line up.
        let n = ((vscale.hi - vscale.lo) / vscale.step).round().max(1.0);
        let base = nice_scale(a, b, n as usize, true);
        let step = nice_step((base.hi - base.lo) / n);
        let lo = (base.lo / step).floor() * step;
        NiceScale {
            lo,
            hi: lo + step * n,
            step,
        }
    });
    // X scale for scatter.
    let xs: Vec<Option<f64>> = if scatter {
        if d.xs.iter().any(Option::is_some) {
            d.xs.clone()
        } else {
            (1..=n_cat).map(|i| Some(i as f64)).collect()
        }
    } else {
        Vec::new()
    };
    let xscale = scatter.then(|| {
        let lo = xs.iter().flatten().copied().fold(f64::INFINITY, f64::min);
        let hi = xs
            .iter()
            .flatten()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max);
        let lo = if lo.is_finite() { lo } else { 0.0 };
        let hi = if hi.is_finite() { hi } else { 1.0 };
        nice_scale(
            lo.min(0.0),
            hi,
            (((x1 - x0) / (60.0 * s)) as usize).clamp(2, 10),
            true,
        )
    });
    // Margins for tick labels.
    let vticks = vscale.ticks();
    let vlabels: Vec<String> = vticks
        .iter()
        .map(|t| format_tick(*t, vscale.step, percent))
        .collect();
    let tick_h = 14.0 * s;
    let (px0, py0, px1, py1) = if horizontal {
        let cat_w = d
            .categories
            .iter()
            .take(200)
            .map(|t| text_width(t, fs * s))
            .fold(0.0f32, f32::max)
            .min((x1 - x0) * 0.35);
        (
            x0 + cat_w + 6.0 * s,
            y0 + 4.0 * s,
            x1 - 6.0 * s,
            y1 - tick_h,
        )
    } else {
        let lw = vlabels
            .iter()
            .map(|t| text_width(t, fs * s))
            .fold(0.0f32, f32::max);
        let rw = sscale.map_or(6.0 * s, |sc| {
            sc.ticks()
                .iter()
                .map(|t| text_width(&format_tick(*t, sc.step, false), fs * s))
                .fold(0.0f32, f32::max)
                + 6.0 * s
        });
        (x0 + lw + 6.0 * s, y0 + 4.0 * s, x1 - rw, y1 - tick_h)
    };
    if !(px1 - px0 > 4.0 && py1 - py0 > 4.0) {
        return;
    }
    // Map value → screen along the value axis.
    let vpos = |sc: &NiceScale, v: f64| -> f32 {
        let f = sc.frac(v) as f32;
        if horizontal {
            px0 + f * (px1 - px0)
        } else {
            py1 - f * (py1 - py0)
        }
    };
    // Gridlines + value tick labels.
    for (t, lab) in vticks.iter().zip(&vlabels) {
        let p = vpos(&vscale, *t);
        if horizontal {
            c.out.push(Prim::Line {
                pts: vec![(p, py0), (p, py1)],
                width: 1.0,
                paint: Paint::Gridline,
            });
            c.text(
                p,
                py1 + 3.0 * s,
                lab,
                fs,
                HAnchor::Center,
                VAnchor::Top,
                Paint::SubText,
            );
        } else {
            c.out.push(Prim::Line {
                pts: vec![(px0, p), (px1, p)],
                width: 1.0,
                paint: Paint::Gridline,
            });
            c.text(
                px0 - 4.0 * s,
                p,
                lab,
                fs,
                HAnchor::Right,
                VAnchor::Center,
                Paint::SubText,
            );
        }
    }
    if let Some(sc) = &sscale {
        for t in sc.ticks() {
            let p = vpos(sc, t);
            c.text(
                px1 + 4.0 * s,
                p,
                &format_tick(t, sc.step, false),
                fs,
                HAnchor::Left,
                VAnchor::Center,
                Paint::SubText,
            );
        }
    }
    // Category / X axis.
    let cat_len = if horizontal { py1 - py0 } else { px1 - px0 };
    let band = cat_len / n_cat as f32;
    // Category axis line at value zero (or the scale's bottom).
    let zero = vscale.lo.max(0.0).min(vscale.hi);
    let zp = vpos(&vscale, zero);
    if let (Some(xsc), true) = (&xscale, scatter) {
        let xt = xsc.ticks();
        for t in &xt {
            let p = px0 + xsc.frac(*t) as f32 * (px1 - px0);
            c.out.push(Prim::Line {
                pts: vec![(p, py0), (p, py1)],
                width: 1.0,
                paint: Paint::Gridline,
            });
            c.text(
                p,
                py1 + 3.0 * s,
                &format_tick(*t, xsc.step, false),
                fs,
                HAnchor::Center,
                VAnchor::Top,
                Paint::SubText,
            );
        }
        c.out.push(Prim::Line {
            pts: vec![(px0, zp), (px1, zp)],
            width: 1.0,
            paint: Paint::Axis,
        });
    } else {
        // Thin out labels that would overlap.
        let widest = d
            .categories
            .iter()
            .take(2000)
            .map(|t| text_width(t, fs * s))
            .fold(0.0f32, f32::max)
            .max(8.0 * s);
        let need = if horizontal { tick_h } else { widest + 6.0 * s };
        let every = ((need / band.max(0.01)).ceil() as usize).max(1);
        for i in (0..n_cat).step_by(every) {
            let label = d
                .categories
                .get(i)
                .cloned()
                .unwrap_or_else(|| (i + 1).to_string());
            let mid = (i as f32 + 0.5) * band;
            if horizontal {
                // First category at the bottom, like the target app.
                let y = py1 - mid;
                c.text(
                    px0 - 4.0 * s,
                    y,
                    &label,
                    fs,
                    HAnchor::Right,
                    VAnchor::Center,
                    Paint::SubText,
                );
            } else {
                c.text(
                    px0 + mid,
                    py1 + 3.0 * s,
                    &label,
                    fs,
                    HAnchor::Center,
                    VAnchor::Top,
                    Paint::SubText,
                );
            }
        }
        if horizontal {
            c.out.push(Prim::Line {
                pts: vec![(zp, py0), (zp, py1)],
                width: 1.0,
                paint: Paint::Axis,
            });
        } else {
            c.out.push(Prim::Line {
                pts: vec![(px0, zp), (px1, zp)],
                width: 1.0,
                paint: Paint::Axis,
            });
        }
    }
    // Series marks.
    let bar_series: Vec<usize> = d
        .series
        .iter()
        .enumerate()
        .filter(|(_, sd)| is_bar(d, sd))
        .map(|(i, _)| i)
        .collect();
    let clustered = d.grouping == Grouping::Clustered || d.kind == ChartKind::Combo;
    let gap = if d.kind == ChartKind::Histogram {
        0.04
    } else {
        0.35
    };
    let slots = if clustered {
        bar_series.len().max(1)
    } else {
        1
    };
    let inner = band * (1.0 - gap);
    let slot_w = inner / slots as f32;
    let mut labels: Vec<(f32, f32, String)> = Vec::new();
    for (slot, si) in bar_series.iter().enumerate() {
        let Some(sd) = d.series.get(*si) else {
            continue;
        };
        let sc = if sd.secondary {
            sscale.as_ref().unwrap_or(&vscale)
        } else {
            &vscale
        };
        let paint = series_paint(sd, *si);
        for i in 0..sd.values.len().min(n_cat) {
            let Some((b, t)) = stacked_point(d, *si, i) else {
                continue;
            };
            let (pb, pt) = (
                vpos(sc, b.clamp(sc.lo, sc.hi)),
                vpos(sc, t.clamp(sc.lo, sc.hi)),
            );
            let off = i as f32 * band
                + band * gap / 2.0
                + if clustered { slot as f32 * slot_w } else { 0.0 };
            let (rx, ry, rw, rh) = if horizontal {
                (pb.min(pt), py1 - off - slot_w, (pt - pb).abs(), slot_w)
            } else {
                (px0 + off, pb.min(pt), slot_w, (pt - pb).abs())
            };
            c.out.push(Prim::Rect {
                x: rx,
                y: ry,
                w: rw.max(0.5),
                h: rh.max(0.5),
                fill: Some(paint),
                stroke: (d.kind == ChartKind::Histogram).then_some(Paint::Background),
            });
            if d.data_labels {
                let raw = sd.values.get(i).copied().flatten().unwrap_or(0.0);
                let text = if percent {
                    format!("{:.0}%", (t - b) * 100.0)
                } else {
                    format_value(raw)
                };
                if horizontal {
                    labels.push((pt.max(pb) + 3.0 * s, ry + rh / 2.0, text));
                } else {
                    labels.push((rx + rw / 2.0, pt.min(pb) - 2.0 * s, text));
                }
            }
        }
    }
    // Areas first (behind lines), then lines, then scatter points.
    for (si, sd) in d.series.iter().enumerate() {
        if is_bar(d, sd) {
            continue;
        }
        let sc = if sd.secondary {
            sscale.as_ref().unwrap_or(&vscale)
        } else {
            &vscale
        };
        let paint = series_paint(sd, si);
        let area_kind = d.kind == ChartKind::Area
            || (d.kind == ChartKind::Combo && sd.kind == SeriesKind::Area);
        let xpos = |i: usize| -> f32 {
            if area_kind && n_cat > 1 {
                px0 + i as f32 * (px1 - px0) / (n_cat - 1) as f32
            } else {
                px0 + (i as f32 + 0.5) * band
            }
        };
        if scatter {
            let Some(xsc) = &xscale else { continue };
            let mut pts = Vec::new();
            for (i, v) in sd.values.iter().enumerate() {
                if let (Some(v), Some(Some(x))) = (v, xs.get(i)) {
                    let px = px0 + xsc.frac(*x) as f32 * (px1 - px0);
                    let py = vpos(sc, *v);
                    pts.push((px, py));
                    if d.data_labels {
                        labels.push((px, py - 4.0 * s, format_value(*v)));
                    }
                }
            }
            if d.markers {
                c.out.push(Prim::Line {
                    pts: pts.clone(),
                    width: 1.5 * s,
                    paint,
                });
            }
            for (x, y) in pts {
                c.out.push(Prim::Circle {
                    x,
                    y,
                    r: 3.0 * s,
                    fill: paint,
                });
            }
            continue;
        }
        if area_kind {
            // One convex quad per segment (stacked: between the series base and top).
            for i in 0..sd.values.len().min(n_cat).saturating_sub(1) {
                let (Some((b0, t0)), Some((b1, t1))) =
                    (stacked_point(d, si, i), stacked_point(d, si, i + 1))
                else {
                    continue;
                };
                let (xa, xb) = (xpos(i), xpos(i + 1));
                let q = |v: f64| vpos(sc, v.clamp(sc.lo, sc.hi));
                // Split where the segment crosses its base so each piece stays convex.
                let pts = vec![(xa, q(b0)), (xa, q(t0)), (xb, q(t1)), (xb, q(b1))];
                if (t0 - b0) * (t1 - b1) < 0.0 {
                    let f = ((t0 - b0) / ((t0 - b0) - (t1 - b1))) as f32;
                    let xm = xa + f * (xb - xa);
                    let ym = q(b0 + f64::from(f) * (b1 - b0));
                    c.out.push(Prim::Poly {
                        pts: vec![(xa, q(b0)), (xa, q(t0)), (xm, ym)],
                        fill: paint,
                    });
                    c.out.push(Prim::Poly {
                        pts: vec![(xm, ym), (xb, q(t1)), (xb, q(b1))],
                        fill: paint,
                    });
                } else {
                    c.out.push(Prim::Poly { pts, fill: paint });
                }
            }
            continue;
        }
        // Line: gaps break the line.
        let mut run: Vec<(f32, f32)> = Vec::new();
        let mut marks = Vec::new();
        for i in 0..sd.values.len().min(n_cat) {
            match stacked_point(d, si, i) {
                Some((_, t)) if sd.values.get(i).copied().flatten().is_some() => {
                    let p = (xpos(i), vpos(sc, t.clamp(sc.lo, sc.hi)));
                    run.push(p);
                    marks.push(p);
                    if d.data_labels {
                        let raw = sd.values.get(i).copied().flatten().unwrap_or(0.0);
                        labels.push((p.0, p.1 - 4.0 * s, format_value(raw)));
                    }
                }
                _ => {
                    if run.len() > 1 {
                        c.out.push(Prim::Line {
                            pts: std::mem::take(&mut run),
                            width: 2.0 * s,
                            paint,
                        });
                    }
                    run.clear();
                }
            }
        }
        if run.len() > 1 {
            c.out.push(Prim::Line {
                pts: run,
                width: 2.0 * s,
                paint,
            });
        }
        if d.markers || marks.len() == 1 {
            for (x, y) in marks {
                c.out.push(Prim::Circle {
                    x,
                    y,
                    r: 2.5 * s,
                    fill: paint,
                });
            }
        }
    }
    for (x, y, t) in labels {
        let v = if horizontal {
            VAnchor::Center
        } else {
            VAnchor::Bottom
        };
        let h = if horizontal {
            HAnchor::Left
        } else {
            HAnchor::Center
        };
        c.text(x, y, &t, 8.0, h, v, Paint::Text);
    }
}

fn is_bar(d: &ChartData, sd: &SeriesData) -> bool {
    match d.kind {
        ChartKind::Column | ChartKind::Bar | ChartKind::Histogram => true,
        ChartKind::Combo => sd.kind == SeriesKind::Column,
        _ => false,
    }
}

// ---------------------------------------------------------------------------------------
// Anchors

use sp_model::Sheet;
use sp_model::chart::Anchor;

fn span_px(
    a: u32,
    b: u32,
    default_px: f32,
    sizes: &std::collections::BTreeMap<u32, f64>,
    hidden: &std::collections::BTreeSet<u32>,
    to_px: fn(f64) -> f32,
) -> f32 {
    // Pixels covered by indices a..b (b exclusive), without iterating every index.
    if b <= a {
        return 0.0;
    }
    let mut px = (b - a) as f32 * default_px;
    for (i, sz) in sizes.range(a..b) {
        if !hidden.contains(i) {
            px += to_px(*sz) - default_px;
        }
    }
    px -= hidden.range(a..b).count() as f32 * default_px;
    px
}

/// Horizontal px (100% zoom) from the left edge of column `origin` to the left edge of
/// column `col` (negative when `col` is left of `origin`).
pub fn col_offset_px(sheet: &Sheet, origin: u32, col: u32) -> f32 {
    let def = crate::col_width_px(sheet.default_col_width);
    let f = |a, b| {
        span_px(
            a,
            b,
            def,
            &sheet.col_widths,
            &sheet.hidden_cols,
            crate::col_width_px,
        )
    };
    if col >= origin {
        f(origin, col)
    } else {
        -f(col, origin)
    }
}

/// Vertical px (100% zoom) from the top of row `origin` to the top of row `row`.
pub fn row_offset_px(sheet: &Sheet, origin: u32, row: u32) -> f32 {
    let def = crate::row_height_px(sheet.default_row_height);
    let f = |a, b| {
        span_px(
            a,
            b,
            def,
            &sheet.row_heights,
            &sheet.hidden_rows,
            crate::row_height_px,
        )
    };
    if row >= origin {
        f(origin, row)
    } else {
        -f(row, origin)
    }
}

/// The anchor's rectangle in px at 100% zoom, relative to the top-left of `origin`:
/// `(x, y, w, h)`.
pub fn anchor_rect(sheet: &Sheet, origin: sp_core::CellAddr, a: &Anchor) -> (f32, f32, f32, f32) {
    let x0 = col_offset_px(sheet, origin.col, a.from.col) + a.from_off.0 as f32;
    let y0 = row_offset_px(sheet, origin.row, a.from.row) + a.from_off.1 as f32;
    let x1 = col_offset_px(sheet, origin.col, a.to.col) + a.to_off.0 as f32;
    let y1 = row_offset_px(sheet, origin.row, a.to.row) + a.to_off.1 as f32;
    (x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0))
}

/// The cell containing the point `px` pixels right of column `origin` (and the remaining
/// offset inside it). Walks at most the sheet's width.
pub fn col_at_px(sheet: &Sheet, origin: u32, px: f64) -> (u32, f64) {
    let mut c = origin;
    let mut left = px.max(0.0);
    while c + 1 < sp_core::MAX_COLS {
        let w = f64::from(crate::col_width_px(sheet.col_width(c)));
        if left < w {
            break;
        }
        left -= w;
        c += 1;
    }
    (c, left)
}

/// Row counterpart of `col_at_px` (walks at most 100k rows).
pub fn row_at_px(sheet: &Sheet, origin: u32, px: f64) -> (u32, f64) {
    let mut r = origin;
    let mut top = px.max(0.0);
    let limit = origin.saturating_add(100_000).min(sp_core::MAX_ROWS - 1);
    while r < limit {
        let h = f64::from(crate::row_height_px(sheet.row_height(r)));
        if top < h {
            break;
        }
        top -= h;
        r += 1;
    }
    (r, top)
}

// ---------------------------------------------------------------------------------------
// Sparklines

/// Lay out a sparkline in a `w × h` cell box (already inset by the caller).
pub fn sparkline_scene(g: &SparklineGroup, values: &[Option<f64>], w: f32, h: f32) -> Vec<Prim> {
    let mut out = Vec::new();
    let pts: Vec<(usize, f64)> = values
        .iter()
        .enumerate()
        .filter_map(|(i, v)| v.filter(|x| x.is_finite()).map(|x| (i, x)))
        .collect();
    let n = values.len();
    if pts.is_empty() || n == 0 || !(w > 1.0 && h > 1.0) {
        return out;
    }
    let main = g.color.map_or(Paint::Series(0), Paint::Rgb);
    let neg = g.negative_color.map_or(Paint::Series(1), Paint::Rgb);
    let high_p = g.high_color.map_or(Paint::Series(1), Paint::Rgb);
    let low_p = g.low_color.map_or(Paint::Series(1), Paint::Rgb);
    let mark_p = g.marker_color.map_or(main, Paint::Rgb);
    let lo = pts.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
    let hi = pts.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max);
    let first_i = pts.first().map(|p| p.0);
    let last_i = pts.last().map(|p| p.0);
    let special = |i: usize, v: f64| -> Option<Paint> {
        if g.high && v == hi {
            Some(high_p)
        } else if g.low && v == lo {
            Some(low_p)
        } else if g.first && Some(i) == first_i {
            Some(high_p)
        } else if g.last && Some(i) == last_i {
            Some(mark_p)
        } else if g.negative && v < 0.0 {
            Some(neg)
        } else {
            None
        }
    };
    match g.kind {
        SparklineKind::Line => {
            let span = hi - lo;
            let y = |v: f64| -> f32 {
                if span > 0.0 {
                    h - ((v - lo) / span) as f32 * h
                } else {
                    h / 2.0
                }
            };
            let x = |i: usize| -> f32 {
                if n > 1 {
                    i as f32 * w / (n - 1) as f32
                } else {
                    w / 2.0
                }
            };
            let line: Vec<(f32, f32)> = pts.iter().map(|(i, v)| (x(*i), y(*v))).collect();
            if line.len() > 1 {
                out.push(Prim::Line {
                    pts: line.clone(),
                    width: 1.25,
                    paint: main,
                });
            }
            for ((i, v), (px, py)) in pts.iter().zip(line) {
                let p = special(*i, *v).or(g.markers.then_some(mark_p));
                if let Some(p) = p {
                    out.push(Prim::Circle {
                        x: px,
                        y: py,
                        r: 2.0,
                        fill: p,
                    });
                }
            }
        }
        SparklineKind::Column | SparklineKind::WinLoss => {
            let win = g.kind == SparklineKind::WinLoss;
            let (vlo, vhi) = if win {
                (-1.0, 1.0)
            } else {
                (lo.min(0.0), hi.max(0.0))
            };
            let span = (vhi - vlo).max(f64::EPSILON);
            let y = |v: f64| h - ((v - vlo) / span) as f32 * h;
            let band = w / n as f32;
            let bw = (band * 0.75).max(1.0);
            for (i, v) in &pts {
                let vv = if win { v.signum() } else { *v };
                if win && vv == 0.0 {
                    continue;
                }
                let (y0, y1) = (y(0.0), y(vv));
                let mut paint = special(*i, *v).unwrap_or(main);
                if *v < 0.0 && special(*i, *v).is_none() {
                    paint = if win || g.negative { neg } else { main };
                }
                out.push(Prim::Rect {
                    x: *i as f32 * band + (band - bw) / 2.0,
                    y: y0.min(y1),
                    w: bw,
                    h: (y1 - y0).abs().max(1.0),
                    fill: Some(paint),
                    stroke: None,
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nice_steps() {
        assert_eq!(nice_step(0.13), 0.2);
        assert_eq!(nice_step(3.0), 5.0);
        assert_eq!(nice_step(7.0), 10.0);
        assert_eq!(nice_step(1.0), 1.0);
        assert_eq!(nice_step(-1.0), 1.0);
        assert_eq!(nice_step(f64::NAN), 1.0);
    }

    #[test]
    fn nice_scales() {
        let s = nice_scale(3.0, 47.0, 5, true);
        assert_eq!((s.lo, s.hi, s.step), (0.0, 50.0, 10.0));
        assert_eq!(s.ticks(), vec![0.0, 10.0, 20.0, 30.0, 40.0, 50.0]);
        let s = nice_scale(-12.0, 8.0, 4, true);
        assert!(s.lo <= -12.0 && s.hi >= 8.0);
        let s = nice_scale(0.1, 0.7, 8, true);
        assert_eq!(s.step, 0.1);
        assert_eq!(s.ticks().get(3), Some(&0.3));
        // Degenerate inputs.
        let s = nice_scale(5.0, 5.0, 5, false);
        assert!(s.lo < 5.0 && s.hi > 5.0);
        let s = nice_scale(f64::NAN, 1.0, 5, true);
        assert!(s.hi > s.lo);
        let s = nice_scale(0.0, 0.0, 5, true);
        assert_eq!((s.lo, s.hi), (0.0, 1.0));
    }

    #[test]
    fn tick_format() {
        assert_eq!(format_tick(1_234_567.0, 1000.0, false), "1,234,567");
        assert_eq!(format_tick(0.30000000000000004, 0.1, false), "0.3");
        assert_eq!(format_tick(-2500.0, 500.0, false), "-2,500");
        assert_eq!(format_tick(0.4, 0.2, true), "40%");
        assert_eq!(format_value(12.5), "12.5");
        assert_eq!(format_value(1000.0), "1,000");
    }

    #[test]
    fn bins() {
        let v: Vec<f64> = (1..=100).map(f64::from).collect();
        let b = histogram_bins(&v, Some(4));
        assert_eq!(b.len(), 4);
        assert_eq!(b.iter().map(|x| x.count).sum::<usize>(), 100);
        assert_eq!(b.first().map(|x| x.count), Some(25));
        let auto = histogram_bins(&v, None);
        // Scott: 3.5·28.87/4.64 ≈ 21.8 → ceil(99/21.8) = 5 bins.
        assert_eq!(auto.len(), 5);
        assert!(histogram_bins(&[], None).is_empty());
        let same = histogram_bins(&[2.0, 2.0], None);
        assert_eq!(same.len(), 1);
        assert_eq!(same.first().map(|x| x.count), Some(2));
        assert_eq!(bin_label(&b[0], true), "[1, 25.8]");
        assert!(histogram_bins(&[1.0, 2.0], Some(u32::MAX)).len() <= MAX_BINS);
    }

    fn data(kind: ChartKind) -> ChartData {
        ChartData {
            kind,
            categories: vec!["A".into(), "B".into(), "C".into()],
            series: vec![
                SeriesData {
                    name: "S1".into(),
                    values: vec![Some(1.0), Some(5.0), None],
                    ..Default::default()
                },
                SeriesData {
                    name: "S2".into(),
                    values: vec![Some(-2.0), Some(3.0), Some(4.0)],
                    secondary: kind == ChartKind::Combo,
                    kind: if kind == ChartKind::Combo {
                        SeriesKind::Line
                    } else {
                        SeriesKind::Column
                    },
                    ..Default::default()
                },
            ],
            title: Some("T".into()),
            x_title: Some("X".into()),
            y_title: Some("Y".into()),
            data_labels: true,
            markers: true,
            ..Default::default()
        }
    }

    #[test]
    fn every_kind_lays_out_inside_the_box() {
        for kind in [
            ChartKind::Column,
            ChartKind::Bar,
            ChartKind::Line,
            ChartKind::Pie,
            ChartKind::Doughnut,
            ChartKind::Area,
            ChartKind::Scatter,
            ChartKind::Histogram,
            ChartKind::Combo,
        ] {
            for grouping in [
                Grouping::Clustered,
                Grouping::Stacked,
                Grouping::PercentStacked,
            ] {
                for legend in [LegendPos::Right, LegendPos::Bottom, LegendPos::None] {
                    let mut d = data(kind);
                    d.grouping = grouping;
                    d.legend = legend;
                    let prims = chart_scene(&d, 400.0, 250.0, 1.0);
                    assert!(prims.len() > 5, "{kind:?}");
                    for p in &prims {
                        let pts: Vec<(f32, f32)> = match p {
                            Prim::Rect { x, y, w, h, .. } => vec![(*x, *y), (x + w, y + h)],
                            Prim::Line { pts, .. } | Prim::Poly { pts, .. } => pts.clone(),
                            Prim::Circle { x, y, .. } | Prim::Text { x, y, .. } => vec![(*x, *y)],
                        };
                        for (x, y) in pts {
                            assert!(x.is_finite() && y.is_finite());
                            assert!(
                                (-1.0..=401.0).contains(&x) && (-1.0..=251.0).contains(&y),
                                "{kind:?} {p:?}"
                            );
                        }
                    }
                }
            }
        }
        // Tiny or hostile boxes never panic.
        assert!(chart_scene(&data(ChartKind::Column), 0.0, 0.0, 1.0).is_empty());
        let _ = chart_scene(&data(ChartKind::Line), 20.0, 10.0, 1.0);
        let _ = chart_scene(&ChartData::default(), 300.0, 200.0, f32::NAN);
    }

    #[test]
    fn stacking() {
        let mut d = data(ChartKind::Column);
        d.grouping = Grouping::Stacked;
        assert_eq!(stacked_point(&d, 1, 1), Some((5.0, 8.0)));
        assert_eq!(stacked_point(&d, 1, 0), Some((0.0, -2.0)));
        d.grouping = Grouping::PercentStacked;
        assert_eq!(stacked_point(&d, 0, 1), Some((0.0, 5.0 / 8.0)));
        assert_eq!(value_range(&d, false), Some((-1.0, 1.0)));
    }

    #[test]
    fn anchors() {
        let mut sh = Sheet::new("S");
        sh.col_widths.insert(2, 20.0);
        sh.hidden_cols.insert(3);
        // Default col = 64 px, col C = 145 px, D hidden.
        assert_eq!(col_offset_px(&sh, 0, 2), 128.0);
        assert_eq!(col_offset_px(&sh, 0, 5), 128.0 + 145.0 + 64.0);
        assert_eq!(col_offset_px(&sh, 5, 0), -(128.0 + 145.0 + 64.0));
        assert_eq!(row_offset_px(&sh, 0, 10), 200.0);
        let a = Anchor {
            from: sp_core::CellAddr::new(1, 1),
            from_off: (4.0, 0.0),
            to: sp_core::CellAddr::new(11, 2),
            to_off: (0.0, 0.0),
        };
        assert_eq!(
            anchor_rect(&sh, sp_core::CellAddr::new(0, 0), &a),
            (68.0, 20.0, 60.0, 200.0)
        );
        assert_eq!(col_at_px(&sh, 0, 130.0), (2, 2.0));
        assert_eq!(row_at_px(&sh, 0, 45.0), (2, 5.0));
        assert_eq!(col_at_px(&sh, 0, 1e18).0, sp_core::MAX_COLS - 1);
    }

    #[test]
    fn sparklines() {
        let g = SparklineGroup {
            high: true,
            low: true,
            markers: true,
            ..Default::default()
        };
        let v = vec![Some(1.0), Some(3.0), None, Some(-2.0)];
        let p = sparkline_scene(&g, &v, 60.0, 16.0);
        assert!(p.iter().any(|x| matches!(x, Prim::Line { .. })));
        assert_eq!(
            p.iter()
                .filter(|x| matches!(x, Prim::Circle { .. }))
                .count(),
            3
        );
        for kind in [SparklineKind::Column, SparklineKind::WinLoss] {
            let g = SparklineGroup {
                kind,
                ..Default::default()
            };
            let p = sparkline_scene(&g, &v, 60.0, 16.0);
            assert_eq!(p.len(), 3);
        }
        assert!(sparkline_scene(&g, &[], 60.0, 16.0).is_empty());
    }
}
