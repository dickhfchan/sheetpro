//! AutoFill (the fill handle): extend a source block down, up, right or left, continuing
//! the pattern of each line the way spreadsheet users expect:
//!
//! - numbers: one number copies, two or more continue their linear (least-squares) trend;
//! - dates: one date counts days; dates on the same day of the month (or all month ends)
//!   step by months/years, other dates by their day difference;
//! - weekday and month names (short or long; case kept) step around the week/year;
//! - text ending in a number ("Item 1", "Q1") counts up (leading zeros kept);
//! - formulas shift their relative references; anything else repeats.
//!
//! `ctrl` (Ctrl+drag) swaps copy and series: a single number or text counts, a series copies.

use crate::edit::{OpError, moved_cell};
use crate::history::Tx;
use sp_core::{CellAddr, CellRange, MAX_CELLS_PER_OP, Value};
use sp_model::{Cell, Content, StyleId, Workbook};

const DAYS_SHORT: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const DAYS_LONG: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
const MONTHS_SHORT: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const MONTHS_LONG: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const LISTS: [&[&str]; 4] = [&DAYS_SHORT, &DAYS_LONG, &MONTHS_SHORT, &MONTHS_LONG];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Case {
    Title,
    Upper,
    Lower,
}

fn case_of(s: &str) -> Case {
    if s.chars().all(|c| !c.is_lowercase()) {
        Case::Upper
    } else if s.chars().all(|c| !c.is_uppercase()) {
        Case::Lower
    } else {
        Case::Title
    }
}

fn apply_case(s: &str, c: Case) -> String {
    match c {
        Case::Title => s.to_string(),
        Case::Upper => s.to_uppercase(),
        Case::Lower => s.to_lowercase(),
    }
}

/// (list index, position in the list, case) of a weekday/month name.
fn list_item(s: &str) -> Option<(usize, usize, Case)> {
    let t = s.trim();
    for (li, list) in LISTS.iter().enumerate() {
        if let Some(p) = list.iter().position(|x| x.eq_ignore_ascii_case(t)) {
            return Some((li, p, case_of(t)));
        }
    }
    None
}

/// Text ending in a number: (prefix, number, digit width when zero-padded).
fn trailing_number(s: &str) -> Option<(&str, f64, usize)> {
    let digits = s.bytes().rev().take_while(u8::is_ascii_digit).count();
    if digits == 0 || digits == s.len() || digits > 15 {
        return None;
    }
    let (prefix, num) = s.split_at(s.len() - digits);
    let n: f64 = num.parse().ok()?;
    let width = if num.starts_with('0') { digits } else { 0 };
    Some((prefix, n, width))
}

/// Slope and intercept of the least-squares line through (k, ys[k]).
fn trend(ys: &[f64]) -> (f64, f64) {
    let n = ys.len() as f64;
    let mx = (n - 1.0) / 2.0;
    let my = ys.iter().sum::<f64>() / n;
    let sxx: f64 = (0..ys.len()).map(|k| (k as f64 - mx).powi(2)).sum();
    let sxy: f64 = ys
        .iter()
        .enumerate()
        .map(|(k, y)| (k as f64 - mx) * (y - my))
        .sum();
    let b = if sxx == 0.0 { 0.0 } else { sxy / sxx };
    (b, my - b * mx)
}

fn add_months(serial: f64, months: i64, month_end: bool) -> Option<f64> {
    let (y, m, d) = sp_core::date::serial_to_date(serial)?;
    let total = y
        .checked_mul(12)?
        .checked_add(i64::from(m) - 1)?
        .checked_add(months)?;
    let (ny, nm) = (total.div_euclid(12), (total.rem_euclid(12) + 1) as u32);
    let last = sp_core::date::days_in_month(ny, nm);
    let day = if month_end { last } else { d.min(last) };
    Some(sp_core::date::date_to_serial(ny, i64::from(nm), i64::from(day))? + serial.fract())
}

/// Month step between consecutive dates when they all fall on the same day of the month
/// (or all on month ends).
fn month_step(serials: &[f64]) -> Option<(i64, bool)> {
    let dates: Vec<(i64, u32, u32)> = serials
        .iter()
        .map(|s| sp_core::date::serial_to_date(*s))
        .collect::<Option<_>>()?;
    let is_end = |(y, m, d): &(i64, u32, u32)| *d == sp_core::date::days_in_month(*y, *m);
    let month_end = dates.iter().all(is_end) && dates.iter().any(|(_, _, d)| *d < 31);
    let pairs = || dates.iter().zip(dates.iter().skip(1));
    let same_day = pairs().all(|(a, b)| a.2 == b.2);
    if !(same_day || month_end) {
        return None;
    }
    let idx = |(y, m, _): &(i64, u32, u32)| y * 12 + i64::from(*m);
    let (first, second) = (dates.first()?, dates.get(1)?);
    let step = idx(second) - idx(first);
    let steady = pairs().all(|(a, b)| idx(b) - idx(a) == step);
    (step != 0 && steady).then_some((step, month_end && !same_day))
}

/// How one source line continues.
enum Pattern {
    /// Repeat the source cells (formulas shift).
    Copy,
    /// value(k) = intercept + slope * k.
    Linear { slope: f64, intercept: f64 },
    /// Dates stepping by whole months from the first one.
    Months {
        first: f64,
        step: i64,
        month_end: bool,
    },
    /// A weekday/month list: list `list`, position of k=0, step per cell.
    List {
        list: usize,
        start: i64,
        step: i64,
        case: Case,
    },
    /// "prefix" + number.
    Numbered {
        prefix: String,
        slope: f64,
        intercept: f64,
        width: usize,
    },
}

fn classify(cells: &[Option<Cell>], wb: &Workbook, ctrl: bool) -> Pattern {
    let contents: Vec<Option<&Content>> = cells
        .iter()
        .map(|c| c.as_ref().map(|c| &c.content))
        .collect();
    let nums: Option<Vec<f64>> = contents
        .iter()
        .map(|c| match c {
            Some(Content::Value(Value::Number(n))) => Some(*n),
            _ => None,
        })
        .collect();
    if let Some(nums) = nums {
        let dated = cells.iter().all(|c| {
            c.as_ref()
                .is_some_and(|c| sp_core::numfmt::is_date_format(&wb.styles.get(c.style).num_fmt))
        });
        let series = if nums.len() == 1 {
            ctrl != dated
        } else {
            !ctrl
        };
        if !series {
            return Pattern::Copy;
        }
        if let [only] = nums.as_slice() {
            return Pattern::Linear {
                slope: 1.0,
                intercept: *only,
            };
        }
        if dated && let Some((step, month_end)) = month_step(&nums) {
            return Pattern::Months {
                first: nums.first().copied().unwrap_or_default(),
                step,
                month_end,
            };
        }
        let (slope, intercept) = trend(&nums);
        return Pattern::Linear { slope, intercept };
    }
    let texts: Option<Vec<&str>> = contents
        .iter()
        .map(|c| match c {
            Some(Content::Value(Value::Text(t))) => Some(t.as_ref()),
            _ => None,
        })
        .collect();
    let Some(texts) = texts else {
        return Pattern::Copy;
    };
    if ctrl {
        return Pattern::Copy;
    }
    // Weekday / month names, all from the same list.
    let items: Option<Vec<(usize, usize, Case)>> = texts.iter().map(|t| list_item(t)).collect();
    if let Some(items) = items
        && let Some(first) = items.first()
        && items.iter().all(|i| i.0 == first.0)
    {
        let n = LISTS.get(first.0).map_or(1, |l| l.len()) as i64;
        let step = match items.get(1) {
            Some(second) => (second.1 as i64 - first.1 as i64).rem_euclid(n),
            None => 1,
        };
        let steady = items
            .iter()
            .zip(items.iter().skip(1))
            .all(|(a, b)| (b.1 as i64 - a.1 as i64).rem_euclid(n) == step);
        if !steady {
            return Pattern::Copy;
        }
        return Pattern::List {
            list: first.0,
            start: first.1 as i64,
            step,
            case: first.2,
        };
    }
    let numbered: Option<Vec<(&str, f64, usize)>> =
        texts.iter().map(|t| trailing_number(t)).collect();
    if let Some(numbered) = numbered
        && let Some(first) = numbered.first()
        && numbered.iter().all(|n| n.0 == first.0)
    {
        let ys: Vec<f64> = numbered.iter().map(|n| n.1).collect();
        let (slope, intercept) = match ys.as_slice() {
            [only] => (1.0, *only),
            _ => trend(&ys),
        };
        return Pattern::Numbered {
            prefix: first.0.to_string(),
            slope,
            intercept,
            width: numbered.iter().map(|n| n.2).max().unwrap_or(0),
        };
    }
    Pattern::Copy
}

fn constant(v: Value, style: StyleId) -> Cell {
    Cell {
        content: Content::Value(v.clone()),
        value: v,
        style,
    }
}

/// Cell at position `k` (relative to the line's first source cell; negative = before it).
fn generate(p: &Pattern, cells: &[Option<Cell>], k: i64, vertical: bool) -> Option<Cell> {
    let len = cells.len() as i64;
    let src_k = k.rem_euclid(len);
    let base = cells.get(src_k as usize).cloned().flatten();
    let style = base.as_ref().map_or(StyleId::DEFAULT, |c| c.style);
    let kf = k as f64;
    match p {
        Pattern::Copy => {
            let off = k - src_k;
            base.map(|c| {
                if vertical {
                    moved_cell(&c, off, 0)
                } else {
                    moved_cell(&c, 0, off)
                }
            })
        }
        Pattern::Linear { slope, intercept } => {
            let v = intercept + slope * kf;
            Some(constant(
                if v.is_finite() {
                    Value::Number(v)
                } else {
                    Value::Error(sp_core::ErrorKind::Num)
                },
                style,
            ))
        }
        Pattern::Months {
            first,
            step,
            month_end,
        } => {
            let v = step
                .checked_mul(k)
                .and_then(|m| add_months(*first, m, *month_end))
                .map_or(Value::Error(sp_core::ErrorKind::Num), Value::Number);
            Some(constant(v, style))
        }
        Pattern::List {
            list,
            start,
            step,
            case,
        } => {
            let l = LISTS.get(*list)?;
            let i = (start + step.checked_mul(k)?).rem_euclid(l.len() as i64);
            let word = l.get(i as usize)?;
            Some(constant(Value::Text(apply_case(word, *case).into()), style))
        }
        Pattern::Numbered {
            prefix,
            slope,
            intercept,
            width,
        } => {
            let n = (intercept + slope * kf).abs();
            let num = if n.fract() == 0.0 && n < 1e15 {
                format!("{:0width$}", n as u64, width = *width)
            } else {
                Value::Number(n).to_display()
            };
            Some(constant(
                Value::Text(format!("{prefix}{num}").into()),
                style,
            ))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dir {
    Down,
    Up,
    Right,
    Left,
}

/// AutoFill `src` into `dest`, which contains `src` and extends it in one direction.
pub fn autofill(
    tx: &mut Tx<'_>,
    sheet: usize,
    src: CellRange,
    dest: CellRange,
    ctrl: bool,
) -> Result<usize, OpError> {
    if dest.cell_count() > MAX_CELLS_PER_OP {
        return Err(OpError::TooLarge(dest.cell_count()));
    }
    let same_cols = dest.start.col == src.start.col && dest.end.col == src.end.col;
    let same_rows = dest.start.row == src.start.row && dest.end.row == src.end.row;
    let dir = if same_cols && dest.start.row == src.start.row && dest.end.row > src.end.row {
        Dir::Down
    } else if same_cols && dest.end.row == src.end.row && dest.start.row < src.start.row {
        Dir::Up
    } else if same_rows && dest.start.col == src.start.col && dest.end.col > src.end.col {
        Dir::Right
    } else if same_rows && dest.end.col == src.end.col && dest.start.col < src.start.col {
        Dir::Left
    } else {
        return Err(OpError::Invalid(
            "the fill range must extend the source down, up, right or left".into(),
        ));
    };
    let vertical = matches!(dir, Dir::Down | Dir::Up);
    let s = tx.wb.sheet(sheet).ok_or(OpError::NoSheet)?;
    let (lines, len) = if vertical {
        (src.cols(), src.rows())
    } else {
        (src.rows(), src.cols())
    };
    // Positions to write, relative to the source's first cell along the fill axis.
    let ks: Vec<i64> = match dir {
        Dir::Down => (i64::from(len)..i64::from(dest.rows())).collect(),
        Dir::Right => (i64::from(len)..i64::from(dest.cols())).collect(),
        Dir::Up => (-i64::from(src.start.row - dest.start.row)..0).collect(),
        Dir::Left => (-i64::from(src.start.col - dest.start.col)..0).collect(),
    };
    let mut writes = Vec::new();
    for line in 0..lines {
        let at = |k: i64| -> Option<CellAddr> {
            if vertical {
                src.start.offset(k, i64::from(line))
            } else {
                src.start.offset(i64::from(line), k)
            }
        };
        let cells: Vec<Option<Cell>> = (0..i64::from(len))
            .map(|k| at(k).and_then(|a| s.get(a)).cloned())
            .collect();
        let pattern = classify(&cells, tx.wb, ctrl);
        for k in &ks {
            let Some(a) = at(*k) else { continue };
            writes.push((a, generate(&pattern, &cells, *k, vertical)));
        }
    }
    let n = writes.len();
    for (a, c) in writes {
        tx.set_cell(sheet, a, c);
    }
    Ok(n)
}

/// Fill-handle double-click: how far down `src` would fill, following the contiguous data
/// in the column left of it (or, if that is empty, right of it). `None` = nothing to fill.
pub fn double_click_extent(wb: &Workbook, sheet: usize, src: CellRange) -> Option<CellRange> {
    let s = wb.sheet(sheet)?;
    let below = src.end.row.checked_add(1)?;
    let filled = |col: u32, row: u32| {
        s.get(CellAddr::new(row, col)).is_some_and(|c| {
            !c.value.is_empty() || !matches!(c.content, Content::Value(Value::Empty))
        })
    };
    let run = |col: u32| -> u32 {
        let mut r = below;
        while r < sp_core::MAX_ROWS && filled(col, r) {
            r += 1;
        }
        r - below
    };
    let mut n = 0;
    if let Some(left) = src.start.col.checked_sub(1) {
        n = run(left);
    }
    if n == 0 && src.end.col + 1 < sp_core::MAX_COLS {
        n = run(src.end.col + 1);
    }
    (n > 0).then(|| CellRange::new(src.start, CellAddr::new(src.end.row + n, src.end.col)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(trailing_number("Item 09"), Some(("Item ", 9.0, 2)));
        assert_eq!(trailing_number("123"), None);
        assert_eq!(list_item("wed"), Some((0, 3, Case::Lower)));
        assert_eq!(list_item("JANUARY").map(|x| x.0), Some(3));
        // 2024-01-31, 2024-02-29: month ends.
        assert_eq!(month_step(&[45322.0, 45351.0]), Some((1, true)));
        assert_eq!(month_step(&[45306.0, 45307.0]), None);
    }

    #[test]
    fn hostile_inputs_do_not_panic() {
        let mut wb = Workbook::new();
        wb.sheets[0].set(CellAddr::new(0, 0), Cell::constant(Value::Number(f64::MAX)));
        wb.sheets[0].set(
            CellAddr::new(1, 0),
            Cell::constant(Value::Number(-f64::MAX)),
        );
        let mut tx = Tx::new(&mut wb);
        let src = CellRange::parse("A1:A2").unwrap();
        autofill(&mut tx, 0, src, CellRange::parse("A1:A5").unwrap(), false).unwrap();
        assert!(autofill(&mut tx, 0, src, CellRange::parse("B1:B5").unwrap(), false).is_err());
        let last = CellRange::new(
            CellAddr::new(sp_core::MAX_ROWS - 2, 0),
            CellAddr::new(sp_core::MAX_ROWS - 1, 0),
        );
        assert!(double_click_extent(tx.wb, 0, last).is_none());
    }
}
