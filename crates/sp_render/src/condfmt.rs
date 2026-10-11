//! Conditional-format evaluation for the visible cells of a sheet. Range statistics (sorted
//! values, averages, duplicate counts) are computed at most once per rule per call, i.e. once
//! per frame; formula rules are evaluated relative to the top-left of the rule's first range.

use sp_core::{CellAddr, CellRange, Value};
use sp_formula::Expr;
use sp_model::{
    CfKind, CfRule, Cfvo, CfvoKind, Color, CondFormat, DiffStyle, Sheet, TextOp, TimePeriod,
    Workbook,
};
use std::collections::HashMap;

/// Cells scanned per rule when computing range statistics.
const MAX_STAT_CELLS: usize = 1_000_000;

/// In-cell data bar: bar length as a fraction of the cell width.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DataBarDraw {
    pub frac: f32,
    pub color: Color,
    pub gradient: bool,
}

/// Icon of an icon set: which set, which icon (0 = lowest bucket), and how many icons.
#[derive(Debug, Clone, PartialEq)]
pub struct IconDraw {
    pub set: String,
    pub index: u8,
    pub count: u8,
}

/// What conditional formats change on one cell.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CfOverride {
    pub style: DiffStyle,
    pub data_bar: Option<DataBarDraw>,
    pub icon: Option<IconDraw>,
    /// Data bar / icon set with "show bar/icon only".
    pub hide_value: bool,
}

impl CfOverride {
    pub fn is_empty(&self) -> bool {
        self.style.is_empty() && self.data_bar.is_none() && self.icon.is_none()
    }

    /// Fill attributes not already set (higher-priority rules come first).
    fn merge(&mut self, d: &DiffStyle) {
        let s = &mut self.style;
        s.font_color = s.font_color.or(d.font_color);
        s.bold = s.bold.or(d.bold);
        s.italic = s.italic.or(d.italic);
        s.underline = s.underline.or(d.underline);
        s.strike = s.strike.or(d.strike);
        s.fill = s.fill.or(d.fill);
        if s.num_fmt.is_none() {
            s.num_fmt.clone_from(&d.num_fmt);
        }
        s.border = s.border.or(d.border);
    }
}

/// Per-rule statistics over all cells of the rule's ranges.
#[derive(Default)]
struct Stats {
    /// Numeric values, ascending.
    nums: Vec<f64>,
    /// Non-blank value key → occurrences.
    counts: HashMap<String, u32>,
}

impl Stats {
    fn min(&self) -> Option<f64> {
        self.nums.first().copied()
    }
    fn max(&self) -> Option<f64> {
        self.nums.last().copied()
    }
    fn mean(&self) -> Option<f64> {
        (!self.nums.is_empty()).then(|| self.nums.iter().sum::<f64>() / self.nums.len() as f64)
    }
    fn std_dev(&self) -> f64 {
        let n = self.nums.len();
        let Some(m) = self.mean() else { return 0.0 };
        if n < 2 {
            return 0.0;
        }
        (self.nums.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (n - 1) as f64).sqrt()
    }
    fn percentile(&self, p: f64) -> Option<f64> {
        let n = self.nums.len();
        if n == 0 {
            return None;
        }
        let rank = (p / 100.0).clamp(0.0, 1.0) * (n - 1) as f64;
        let lo = rank.floor() as usize;
        let hi = rank.ceil() as usize;
        let a = *self.nums.get(lo)?;
        let b = *self.nums.get(hi)?;
        Some(a + (b - a) * (rank - lo as f64))
    }
}

fn value_key(v: &Value) -> Option<String> {
    match v {
        Value::Empty => None,
        Value::Text(t) => Some(format!("t{}", t.to_lowercase())),
        Value::Number(n) => Some(format!("n{n}")),
        Value::Bool(b) => Some(format!("b{b}")),
        Value::Error(e) => Some(format!("e{}", e.as_str())),
    }
}

fn compute_stats(sheet: &Sheet, ranges: &[CellRange]) -> Stats {
    let mut st = Stats::default();
    let mut seen = 0usize;
    'outer: for r in ranges {
        for (_, c) in sheet.cells_in(*r) {
            seen += 1;
            if seen > MAX_STAT_CELLS {
                break 'outer;
            }
            if let Value::Number(n) = c.value {
                st.nums.push(n);
            }
            if let Some(k) = value_key(&c.value) {
                *st.counts.entry(k).or_insert(0) += 1;
            }
        }
    }
    st.nums.sort_by(f64::total_cmp);
    st
}

fn is_err(v: &Value) -> bool {
    matches!(v, Value::Error(_))
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Number(n) => *n != 0.0,
        _ => false,
    }
}

/// Evaluation context for one sheet during one frame.
pub struct CfEval<'a> {
    wb: &'a Workbook,
    sheet_idx: usize,
    sheet: &'a Sheet,
    /// Rules flattened and sorted by priority: (format index, rule).
    rules: Vec<(usize, &'a CfRule)>,
    stats: HashMap<usize, Stats>,
    asts: HashMap<String, Option<Expr>>,
    today: f64,
}

impl<'a> CfEval<'a> {
    pub fn new(wb: &'a Workbook, sheet_idx: usize, sheet: &'a Sheet) -> Self {
        let mut rules: Vec<(usize, &CfRule)> = sheet
            .cond_formats
            .iter()
            .enumerate()
            .flat_map(|(i, cf)| cf.rules.iter().map(move |r| (i, r)))
            .collect();
        rules.sort_by_key(|(_, r)| r.priority);
        CfEval {
            wb,
            sheet_idx,
            sheet,
            rules,
            stats: HashMap::new(),
            asts: HashMap::new(),
            today: sp_core::date::now_serial().floor(),
        }
    }

    /// Fix "today" (deterministic tests).
    pub fn with_today(mut self, serial: f64) -> Self {
        self.today = serial.floor();
        self
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    fn format(&self, i: usize) -> Option<&'a CondFormat> {
        self.sheet.cond_formats.get(i)
    }

    fn stats(&mut self, i: usize) -> &Stats {
        let sheet = self.sheet;
        let ranges = self.format(i).map(|f| f.ranges.as_slice()).unwrap_or(&[]);
        self.stats
            .entry(i)
            .or_insert_with(|| compute_stats(sheet, ranges))
    }

    /// Evaluate formula `src` (no `=`) for `at`, relative to `anchor`.
    fn eval(&mut self, src: &str, anchor: CellAddr, at: CellAddr) -> Value {
        let src = src.trim().trim_start_matches('=');
        if src.is_empty() {
            return Value::Empty;
        }
        let ast = self
            .asts
            .entry(src.to_string())
            .or_insert_with(|| sp_formula::parse(src).ok());
        let Some(ast) = ast else {
            return Value::Error(sp_core::ErrorKind::Name);
        };
        let mut e = ast.clone();
        sp_formula::shift::shift_for_copy(
            &mut e,
            i64::from(at.row) - i64::from(anchor.row),
            i64::from(at.col) - i64::from(anchor.col),
        );
        sp_formula::recalc::eval_expr(self.wb, self.sheet_idx, at, &e)
    }

    fn cfvo(&mut self, i: usize, v: &Cfvo, anchor: CellAddr) -> Option<f64> {
        let num = |s: &str| s.trim().parse::<f64>().ok();
        let st = self.stats(i);
        let (min, max) = (st.min(), st.max());
        Some(match v.kind {
            CfvoKind::Min => min?,
            CfvoKind::AutoMin => min?.min(0.0),
            CfvoKind::Max => max?,
            CfvoKind::AutoMax => max?.max(0.0),
            CfvoKind::Percent => {
                let p = num(&v.value).unwrap_or(0.0);
                min? + (max? - min?) * p / 100.0
            }
            CfvoKind::Percentile => st.percentile(num(&v.value).unwrap_or(0.0))?,
            CfvoKind::Num | CfvoKind::Formula => match num(&v.value) {
                Some(n) => n,
                None => match self.eval(&v.value, anchor, anchor) {
                    Value::Number(n) => n,
                    _ => return None,
                },
            },
        })
    }

    /// Does a highlight-type rule match `v` at `at`? `None` for visual rules.
    fn matches(&mut self, i: usize, rule: &CfRule, at: CellAddr, v: &Value) -> bool {
        let anchor = self.format(i).map(|f| f.anchor()).unwrap_or_default();
        match &rule.kind {
            CfKind::CellIs { op, f1, f2 } => {
                if is_err(v) {
                    return false;
                }
                let x = if v.is_empty() {
                    Value::Number(0.0)
                } else {
                    v.clone()
                };
                let a = self.eval(f1, anchor, at);
                let b = if f2.trim().is_empty() {
                    a.clone()
                } else {
                    self.eval(f2, anchor, at)
                };
                if is_err(&a) || is_err(&b) {
                    return false;
                }
                let cmp = sp_formula::eval::compare;
                use sp_model::CfOp;
                use std::cmp::Ordering as O;
                let (lo, hi) = if cmp(&a, &b) == O::Greater {
                    (&b, &a)
                } else {
                    (&a, &b)
                };
                match op {
                    CfOp::Between => cmp(&x, lo) != O::Less && cmp(&x, hi) != O::Greater,
                    CfOp::NotBetween => cmp(&x, lo) == O::Less || cmp(&x, hi) == O::Greater,
                    CfOp::Equal => cmp(&x, &a) == O::Equal,
                    CfOp::NotEqual => cmp(&x, &a) != O::Equal,
                    CfOp::Greater => cmp(&x, &a) == O::Greater,
                    CfOp::Less => cmp(&x, &a) == O::Less,
                    CfOp::GreaterEq => cmp(&x, &a) != O::Less,
                    CfOp::LessEq => cmp(&x, &a) != O::Greater,
                }
            }
            CfKind::Text { op, text } => {
                if is_err(v) {
                    return false;
                }
                let hay = v.to_display().to_lowercase();
                let needle = text.to_lowercase();
                match op {
                    TextOp::Contains => hay.contains(&needle),
                    TextOp::NotContains => !hay.contains(&needle),
                    TextOp::BeginsWith => hay.starts_with(&needle),
                    TextOp::EndsWith => hay.ends_with(&needle),
                }
            }
            CfKind::TimePeriod(p) => match v {
                Value::Number(n) => in_period(*p, n.floor(), self.today),
                _ => false,
            },
            CfKind::Duplicates | CfKind::Unique => {
                let Some(k) = value_key(v) else { return false };
                let n = self.stats(i).counts.get(&k).copied().unwrap_or(0);
                if matches!(rule.kind, CfKind::Duplicates) {
                    n > 1
                } else {
                    n == 1
                }
            }
            CfKind::Top {
                bottom,
                percent,
                rank,
            } => {
                let Value::Number(x) = v else { return false };
                let st = self.stats(i);
                let len = st.nums.len();
                if len == 0 {
                    return false;
                }
                let n = if *percent {
                    ((len as f64 * f64::from(*rank) / 100.0).floor() as usize).max(1)
                } else {
                    (*rank as usize).max(1)
                }
                .min(len);
                if *bottom {
                    st.nums.get(n - 1).is_some_and(|t| *x <= *t)
                } else {
                    st.nums.get(len - n).is_some_and(|t| *x >= *t)
                }
            }
            CfKind::Average {
                below,
                equal,
                std_dev,
            } => {
                let Value::Number(x) = v else { return false };
                let st = self.stats(i);
                let Some(m) = st.mean() else { return false };
                let k = f64::from(*std_dev) * st.std_dev();
                let t = if *below { m - k } else { m + k };
                match (below, equal) {
                    (false, false) => *x > t,
                    (false, true) => *x >= t,
                    (true, false) => *x < t,
                    (true, true) => *x <= t,
                }
            }
            CfKind::Blanks => match v {
                Value::Empty => true,
                Value::Text(t) => t.trim().is_empty(),
                _ => false,
            },
            CfKind::NoBlanks => {
                !matches!(v, Value::Empty) && !matches!(v, Value::Text(t) if t.trim().is_empty())
            }
            CfKind::Errors => is_err(v),
            CfKind::NoErrors => !is_err(v),
            CfKind::Expression(f) => {
                let f = f.clone();
                truthy(&self.eval(&f, anchor, at))
            }
            CfKind::DataBar { .. } | CfKind::ColorScale { .. } | CfKind::IconSet { .. } => false,
        }
    }

    /// Everything the sheet's rules change on cell `at` holding `v`.
    pub fn cell(&mut self, at: CellAddr, v: &Value) -> Option<CfOverride> {
        let mut out = CfOverride::default();
        let rules = self.rules.clone();
        for (i, rule) in rules {
            if !self.format(i).is_some_and(|f| f.covers(at)) {
                continue;
            }
            let anchor = self.format(i).map(|f| f.anchor()).unwrap_or_default();
            let hit = match &rule.kind {
                CfKind::DataBar {
                    min,
                    max,
                    color,
                    gradient,
                    show_value,
                } => {
                    let Value::Number(x) = v else { continue };
                    if out.data_bar.is_some() {
                        continue;
                    }
                    let (Some(lo), Some(hi)) =
                        (self.cfvo(i, min, anchor), self.cfvo(i, max, anchor))
                    else {
                        continue;
                    };
                    let t = if hi > lo {
                        ((x - lo) / (hi - lo)).clamp(0.0, 1.0)
                    } else {
                        1.0
                    };
                    out.data_bar = Some(DataBarDraw {
                        frac: (0.1 + 0.9 * t) as f32,
                        color: *color,
                        gradient: *gradient,
                    });
                    out.hide_value |= !show_value;
                    true
                }
                CfKind::ColorScale { points } => {
                    let Value::Number(x) = v else { continue };
                    if out.style.fill.is_some() {
                        continue;
                    }
                    let mut stops: Vec<(f64, Color)> = Vec::new();
                    for (p, c) in points {
                        if let Some(t) = self.cfvo(i, p, anchor) {
                            stops.push((t, *c));
                        }
                    }
                    let Some(c) = scale_color(&stops, *x) else {
                        continue;
                    };
                    out.style.fill = Some(c);
                    true
                }
                CfKind::IconSet {
                    set,
                    points,
                    reverse,
                    show_value,
                } => {
                    let Value::Number(x) = v else { continue };
                    if out.icon.is_some() {
                        continue;
                    }
                    let count = CfKind::icon_count(set);
                    let mut index = 0usize;
                    for (k, p) in points.iter().enumerate().skip(1).take(count - 1) {
                        let Some(t) = self.cfvo(i, p, anchor) else {
                            continue;
                        };
                        if (p.gte && *x >= t) || (!p.gte && *x > t) {
                            index = k;
                        }
                    }
                    if *reverse {
                        index = count - 1 - index.min(count - 1);
                    }
                    out.icon = Some(IconDraw {
                        set: set.clone(),
                        index: u8::try_from(index).unwrap_or(0),
                        count: u8::try_from(count).unwrap_or(3),
                    });
                    out.hide_value |= !show_value;
                    true
                }
                _ => {
                    if self.matches(i, rule, at, v) {
                        out.merge(&rule.style);
                        true
                    } else {
                        false
                    }
                }
            };
            if hit && rule.stop_if_true {
                break;
            }
        }
        (!out.is_empty()).then_some(out)
    }
}

/// Interpolate a colour scale at `x`; stops are (threshold, colour), ascending.
pub fn scale_color(stops: &[(f64, Color)], x: f64) -> Option<Color> {
    let first = stops.first()?;
    let last = stops.last()?;
    if x <= first.0 {
        return Some(first.1);
    }
    if x >= last.0 {
        return Some(last.1);
    }
    for w in stops.windows(2) {
        if let [(a, ca), (b, cb)] = w
            && x >= *a
            && x <= *b
        {
            let t = if b > a { (x - a) / (b - a) } else { 0.0 };
            let mix =
                |p: u8, q: u8| (f64::from(p) + (f64::from(q) - f64::from(p)) * t).round() as u8;
            return Some(Color(mix(ca.0, cb.0), mix(ca.1, cb.1), mix(ca.2, cb.2)));
        }
    }
    Some(last.1)
}

fn in_period(p: TimePeriod, d: f64, today: f64) -> bool {
    let wk_start = today - f64::from(sp_core::date::weekday(today));
    let month = |s: f64| sp_core::date::serial_to_date(s).map(|(y, m, _)| y * 12 + i64::from(m));
    let (Some(dm), Some(tm)) = (month(d), month(today)) else {
        return false;
    };
    match p {
        TimePeriod::Yesterday => d == today - 1.0,
        TimePeriod::Today => d == today,
        TimePeriod::Tomorrow => d == today + 1.0,
        TimePeriod::Last7Days => d <= today && d > today - 7.0,
        TimePeriod::LastWeek => d >= wk_start - 7.0 && d < wk_start,
        TimePeriod::ThisWeek => d >= wk_start && d < wk_start + 7.0,
        TimePeriod::NextWeek => d >= wk_start + 7.0 && d < wk_start + 14.0,
        TimePeriod::LastMonth => dm == tm - 1,
        TimePeriod::ThisMonth => dm == tm,
        TimePeriod::NextMonth => dm == tm + 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sp_model::{Cell, CfOp};

    fn wb_nums(vals: &[f64]) -> Workbook {
        let mut wb = Workbook::new();
        for (i, v) in vals.iter().enumerate() {
            wb.sheets[0].set(
                CellAddr::new(i as u32, 0),
                Cell::constant(Value::Number(*v)),
            );
        }
        wb
    }

    fn add(wb: &mut Workbook, range: &str, kind: CfKind, priority: u32, stop: bool) {
        wb.sheets[0].cond_formats.push(CondFormat {
            ranges: vec![CellRange::parse(range).unwrap()],
            rules: vec![CfRule {
                kind,
                style: DiffStyle::preset("red"),
                priority,
                stop_if_true: stop,
            }],
        });
    }

    fn hits(wb: &Workbook, n: u32) -> Vec<bool> {
        let s = &wb.sheets[0];
        let mut ev = CfEval::new(wb, 0, s).with_today(45000.0);
        (0..n)
            .map(|r| {
                let a = CellAddr::new(r, 0);
                ev.cell(a, s.value(a)).is_some()
            })
            .collect()
    }

    #[test]
    fn cell_is_and_formula() {
        let mut wb = wb_nums(&[1.0, 5.0, 10.0]);
        add(
            &mut wb,
            "A1:A3",
            CfKind::CellIs {
                op: CfOp::Greater,
                f1: "4".into(),
                f2: String::new(),
            },
            1,
            false,
        );
        assert_eq!(hits(&wb, 3), vec![false, true, true]);
        wb.sheets[0].cond_formats.clear();
        add(
            &mut wb,
            "A1:A3",
            CfKind::CellIs {
                op: CfOp::Between,
                f1: "6".into(),
                f2: "2".into(),
            },
            1,
            false,
        );
        assert_eq!(hits(&wb, 3), vec![false, true, false]);
        wb.sheets[0].cond_formats.clear();
        // Relative formula: compares each cell with the one below it.
        add(
            &mut wb,
            "A1:A3",
            CfKind::Expression("A1<A2".into()),
            1,
            false,
        );
        assert_eq!(hits(&wb, 3), vec![true, true, false]);
    }

    #[test]
    fn top_average_dupes() {
        let mut wb = wb_nums(&[1.0, 2.0, 3.0, 4.0, 4.0]);
        add(
            &mut wb,
            "A1:A5",
            CfKind::Top {
                bottom: false,
                percent: false,
                rank: 2,
            },
            1,
            false,
        );
        assert_eq!(hits(&wb, 5), vec![false, false, false, true, true]);
        wb.sheets[0].cond_formats.clear();
        add(
            &mut wb,
            "A1:A5",
            CfKind::Average {
                below: true,
                equal: false,
                std_dev: 0,
            },
            1,
            false,
        );
        assert_eq!(hits(&wb, 5), vec![true, true, false, false, false]);
        wb.sheets[0].cond_formats.clear();
        add(&mut wb, "A1:A5", CfKind::Duplicates, 1, false);
        assert_eq!(hits(&wb, 5), vec![false, false, false, true, true]);
    }

    #[test]
    fn text_blanks_dates() {
        let mut wb = Workbook::new();
        let s = &mut wb.sheets[0];
        s.set(
            CellAddr::new(0, 0),
            Cell::constant(Value::text("Apple pie")),
        );
        s.set(CellAddr::new(2, 0), Cell::constant(Value::Number(44999.0)));
        add(
            &mut wb,
            "A1:A3",
            CfKind::Text {
                op: TextOp::Contains,
                text: "PIE".into(),
            },
            1,
            false,
        );
        add(&mut wb, "A1:A3", CfKind::Blanks, 2, false);
        add(
            &mut wb,
            "A1:A3",
            CfKind::TimePeriod(TimePeriod::Yesterday),
            3,
            false,
        );
        assert_eq!(hits(&wb, 3), vec![true, true, true]);
    }

    #[test]
    fn priority_and_stop() {
        let mut wb = wb_nums(&[5.0]);
        wb.sheets[0].cond_formats.push(CondFormat {
            ranges: vec![CellRange::parse("A1").unwrap()],
            rules: vec![
                CfRule {
                    kind: CfKind::NoBlanks,
                    style: DiffStyle {
                        bold: Some(true),
                        ..DiffStyle::default()
                    },
                    priority: 2,
                    stop_if_true: false,
                },
                CfRule {
                    kind: CfKind::NoErrors,
                    style: DiffStyle::preset("green"),
                    priority: 1,
                    stop_if_true: false,
                },
            ],
        });
        let s = &wb.sheets[0];
        let o = CfEval::new(&wb, 0, s)
            .cell(CellAddr::new(0, 0), &Value::Number(5.0))
            .unwrap();
        assert_eq!(o.style.fill, DiffStyle::preset("green").fill);
        assert_eq!(o.style.bold, Some(true));
        wb.sheets[0].cond_formats[0].rules[1].stop_if_true = true;
        let s = &wb.sheets[0];
        let o = CfEval::new(&wb, 0, s)
            .cell(CellAddr::new(0, 0), &Value::Number(5.0))
            .unwrap();
        assert_eq!(o.style.bold, None);
    }

    #[test]
    fn visual_rules() {
        let mut wb = wb_nums(&[0.0, 50.0, 100.0]);
        wb.sheets[0].cond_formats.push(CondFormat {
            ranges: vec![CellRange::parse("A1:A3").unwrap()],
            rules: vec![
                CfRule {
                    kind: CfKind::DataBar {
                        min: Cfvo::new(CfvoKind::Min, ""),
                        max: Cfvo::new(CfvoKind::Max, ""),
                        color: Color(99, 142, 198),
                        gradient: true,
                        show_value: true,
                    },
                    style: DiffStyle::default(),
                    priority: 1,
                    stop_if_true: false,
                },
                CfRule {
                    kind: CfKind::ColorScale {
                        points: vec![
                            (Cfvo::new(CfvoKind::Min, ""), Color(0, 0, 0)),
                            (Cfvo::new(CfvoKind::Max, ""), Color(200, 100, 0)),
                        ],
                    },
                    style: DiffStyle::default(),
                    priority: 2,
                    stop_if_true: false,
                },
                CfRule {
                    kind: CfKind::IconSet {
                        set: "3Arrows".into(),
                        points: vec![
                            Cfvo::new(CfvoKind::Percent, "0"),
                            Cfvo::new(CfvoKind::Percent, "33"),
                            Cfvo::new(CfvoKind::Percent, "67"),
                        ],
                        reverse: false,
                        show_value: true,
                    },
                    style: DiffStyle::default(),
                    priority: 3,
                    stop_if_true: false,
                },
            ],
        });
        let s = &wb.sheets[0];
        let mut ev = CfEval::new(&wb, 0, s);
        let mid = ev.cell(CellAddr::new(1, 0), &Value::Number(50.0)).unwrap();
        assert!((mid.data_bar.unwrap().frac - 0.55).abs() < 1e-6);
        assert_eq!(mid.style.fill, Some(Color(100, 50, 0)));
        assert_eq!(mid.icon.unwrap().index, 1);
        let top = ev.cell(CellAddr::new(2, 0), &Value::Number(100.0)).unwrap();
        assert_eq!(top.icon.unwrap().index, 2);
        assert_eq!(top.data_bar.unwrap().frac, 1.0);
        assert!(ev.cell(CellAddr::new(5, 0), &Value::Number(1.0)).is_none());
    }

    #[test]
    fn hostile_rules_do_not_panic() {
        let mut wb = wb_nums(&[1.0]);
        add(&mut wb, "A1", CfKind::Expression("SUM(".into()), 1, false);
        add(
            &mut wb,
            "A1",
            CfKind::Top {
                bottom: true,
                percent: true,
                rank: u32::MAX,
            },
            2,
            false,
        );
        add(
            &mut wb,
            "A1",
            CfKind::IconSet {
                set: "5Quarters".into(),
                points: vec![],
                reverse: true,
                show_value: false,
            },
            3,
            false,
        );
        let _ = hits(&wb, 2);
    }
}
