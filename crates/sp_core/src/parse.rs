//! Parsing typed text into numbers, the way cell entry and text→number coercion do.

use crate::date::{date_to_serial, days_in_month};

/// Number format implied by how a number was typed (`50%` → percent, `1/2/2024` → date…).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatHint {
    None,
    Percent,
    Currency,
    Thousands,
    Date,
    Time,
    DateTime,
    Scientific,
}

impl FormatHint {
    pub fn format_code(self) -> Option<&'static str> {
        match self {
            FormatHint::None => None,
            FormatHint::Percent => Some("0%"),
            FormatHint::Currency => Some("\"$\"#,##0.00"),
            FormatHint::Thousands => Some("#,##0"),
            FormatHint::Date => Some("m/d/yyyy"),
            FormatHint::Time => Some("h:mm"),
            FormatHint::DateTime => Some("m/d/yyyy h:mm"),
            FormatHint::Scientific => Some("0.00E+00"),
        }
    }
}

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

fn month_from_name(s: &str) -> Option<i64> {
    let l = s.to_ascii_lowercase();
    if l.len() < 3 {
        return None;
    }
    MONTHS
        .iter()
        .position(|m| l.starts_with(m))
        .map(|i| i as i64 + 1)
}

fn plain_number(s: &str) -> Option<f64> {
    if s.is_empty()
        || !s
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E' | b'+' | b'-'))
    {
        return None;
    }
    if !s.bytes().any(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<f64>().ok().filter(|n| n.is_finite())
}

/// Parse a number with optional sign, thousands separators, currency, percent and
/// accounting parentheses. Returns the value and the implied format.
pub fn parse_number(input: &str) -> Option<(f64, FormatHint)> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(v) = plain_number(s) {
        let hint = if s.contains(['e', 'E']) {
            FormatHint::Scientific
        } else {
            FormatHint::None
        };
        return Some((v, hint));
    }
    let mut t = s.to_string();
    let mut neg = false;
    let mut hint = FormatHint::None;
    if t.starts_with('(') && t.ends_with(')') && t.len() > 2 {
        neg = true;
        t = t.get(1..t.len() - 1)?.to_string();
    }
    if let Some(r) = t.strip_prefix('-') {
        neg = !neg;
        t = r.trim_start().to_string();
    } else if let Some(r) = t.strip_prefix('+') {
        t = r.trim_start().to_string();
    }
    if t.starts_with(['-', '+']) {
        return None; // "--8", "+-8": a second sign is not a number
    }
    if let Some(r) = t.strip_prefix('$') {
        hint = FormatHint::Currency;
        t = r.trim_start().to_string();
        if let Some(r) = t.strip_prefix('-') {
            neg = !neg;
            t = r.to_string();
        }
    }
    let mut pct = false;
    if let Some(r) = t.strip_suffix('%') {
        pct = true;
        hint = FormatHint::Percent;
        t = r.trim_end().to_string();
    }
    if t.contains(',') {
        // Thousands separators must group by three to the left of the decimal point.
        let int_part = t.split('.').next().unwrap_or("");
        let groups: Vec<&str> = int_part.split(',').collect();
        let ok = groups
            .first()
            .is_some_and(|g| !g.is_empty() && g.len() <= 3)
            && groups
                .iter()
                .skip(1)
                .all(|g| g.len() == 3 && g.bytes().all(|b| b.is_ascii_digit()));
        if !ok {
            return None;
        }
        if hint == FormatHint::None {
            hint = FormatHint::Thousands;
        }
        t = t.replace(',', "");
    }
    let mut v = plain_number(&t)?;
    if pct {
        v /= 100.0;
    }
    if neg {
        v = -v;
    }
    Some((v, hint))
}

/// Parse a time-of-day like `13:45`, `1:45 PM`, `13:45:30.5` into a day fraction. Hours may
/// exceed 23 without a meridiem (`25:00` → 25/24); a seconds field of 60 is accepted.
pub fn parse_time(input: &str) -> Option<f64> {
    let s = input.trim().to_ascii_lowercase();
    let (body, ampm) = if let Some(b) = s.strip_suffix("am").or_else(|| s.strip_suffix('a')) {
        (b.trim_end().to_string(), Some(false))
    } else if let Some(b) = s.strip_suffix("pm").or_else(|| s.strip_suffix('p')) {
        (b.trim_end().to_string(), Some(true))
    } else {
        (s.clone(), None)
    };
    let parts: Vec<&str> = body.split(':').collect();
    if parts.is_empty() || parts.len() > 3 || (parts.len() == 1 && ampm.is_none()) {
        return None;
    }
    let num = |p: Option<&&str>| -> Option<f64> {
        match p {
            None => Some(0.0),
            Some(x) if !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit() || b == b'.') => {
                x.parse().ok()
            }
            _ => None,
        }
    };
    // Only the last field may carry a decimal fraction.
    let whole = |p: Option<&&str>| p.is_none_or(|x| !x.contains('.'));
    if !whole(parts.first()) || (parts.len() == 3 && !whole(parts.get(1))) {
        return None;
    }
    let mut h = num(parts.first())?;
    let m = num(parts.get(1))?;
    let sec = num(parts.get(2))?;
    if m >= 60.0 || sec > 60.0 || h > 32_767.0 {
        return None;
    }
    if let Some(pm) = ampm {
        if h > 12.0 {
            return None;
        }
        if h == 12.0 {
            h = 0.0;
        }
        if pm {
            h += 12.0;
        }
    }
    Some((h * 3600.0 + m * 60.0 + sec) / 86_400.0)
}

/// Parse a date like `2024-01-15`, `1/15/2024`, `15-Jan-2024`, `Jan 15, 2024`, `1/15`
/// (current year is not known here, so `m/d` uses `default_year`).
pub fn parse_date(input: &str, default_year: i64) -> Option<f64> {
    parse_date_impl(input, default_year, true)
}

fn parse_date_impl(input: &str, default_year: i64, month_first_names: bool) -> Option<f64> {
    let s = input.trim();
    let valid = |y: i64, m: i64, d: i64| -> Option<f64> {
        // The target's calendar keeps the phantom 1900-02-29 (serial 60).
        let dim = if (y, m) == (1900, 2) {
            29
        } else if (1..=12).contains(&m) {
            i64::from(days_in_month(y, m as u32))
        } else {
            return None;
        };
        if !(1900..=9999).contains(&y) || d < 1 || d > dim {
            return None;
        }
        date_to_serial(y, m, d)
    };
    let year = |t: &str| -> Option<i64> {
        if !t.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let y: i64 = t.parse().ok()?;
        Some(match t.len() {
            1 | 2 => {
                if y < 30 {
                    2000 + y
                } else {
                    1900 + y
                }
            }
            4 => y,
            _ => return None,
        })
    };
    let int = |t: &str| -> Option<i64> {
        if t.is_empty() || t.len() > 2 || !t.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        t.parse().ok()
    };
    // ISO yyyy-mm-dd or yyyy/mm/dd
    let iso: Vec<&str> = s.split(['-', '/']).collect();
    if iso.len() == 3 && iso.first().is_some_and(|y| y.len() == 4) {
        return valid(year(iso.first()?)?, int(iso.get(1)?)?, int(iso.get(2)?)?);
    }
    // m/d/yyyy (falling back to d/m/yyyy when the month is impossible), m/d
    let num3: Vec<&str> = if s.contains('/') {
        s.split('/').collect()
    } else {
        s.split('-').collect()
    };
    if let [a, b, c] = num3.as_slice()
        && let (Some(a), Some(b)) = (int(a), int(b))
    {
        let y = year(c)?;
        return valid(y, a, b).or_else(|| valid(y, b, a));
    }
    if let [a, b] = num3.as_slice()
        && s.contains('/')
    {
        return valid(default_year, int(a)?, int(b)?);
    }
    // d-mmm-yyyy, d mmm yyyy, d-mmm, mmm-yyyy
    let dm: Vec<&str> = s.split(['-', ' ']).filter(|p| !p.is_empty()).collect();
    match dm.as_slice() {
        [d, m, y] if int(d).is_some() => return valid(year(y)?, month_from_name(m)?, int(d)?),
        [d, m] if int(d).is_some() => return valid(default_year, month_from_name(m)?, int(d)?),
        [m, y] if y.len() == 4 && month_from_name(m).is_some() => {
            return valid(year(y)?, month_from_name(m)?, 1);
        }
        _ => {}
    }
    // mmm d, yyyy
    if month_first_names {
        let cleaned = s.replace(',', " ");
        let md: Vec<&str> = cleaned.split_whitespace().collect();
        if let [m, d, y] = md.as_slice() {
            return valid(year(y)?, month_from_name(m)?, int(d)?);
        }
    }
    None
}

/// Drop whitespace next to the `-`, `/` and `:` separators (`2026 - 01 - 01`, `4 : 35`).
fn squeeze_separators(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut pending_space = false;
    for c in s.trim().chars() {
        if c.is_whitespace() {
            pending_space = true;
            continue;
        }
        let sep = matches!(c, '-' | '/' | ':');
        if pending_space && !sep && !out.ends_with(['-', '/', ':']) {
            out.push(' ');
        }
        pending_space = false;
        out.push(c);
    }
    out
}

/// Parse date/time text the way date functions coerce a text argument (`DATEVALUE`,
/// `TIMEVALUE`, `DAYS`, ...): a date, a time, or a date followed by a time. Whitespace around
/// separators is ignored; hours past 23 roll into the next day. Month-name-first forms such as
/// `July 1, 2026` are not accepted here (observed: such arguments are `#VALUE!`).
pub fn parse_date_time_text(input: &str, default_year: i64) -> Option<f64> {
    let s = squeeze_separators(input);
    if s.is_empty() {
        return None;
    }
    if let Some(d) = parse_date_impl(&s, default_year, false) {
        return Some(d);
    }
    if let Some(t) = parse_time(&s) {
        return Some(t);
    }
    let (d, t) = s.split_once(' ')?;
    let d = parse_date_impl(d, default_year, false)?;
    let t = parse_time(t)?;
    let v = d + t;
    (v < (crate::date::MAX_SERIAL + 1) as f64).then_some(v)
}

/// Full typed-number parse: number, date, time or date+time.
pub fn parse_numeric_input(input: &str, default_year: i64) -> Option<(f64, FormatHint)> {
    if let Some(r) = parse_number(input) {
        return Some(r);
    }
    if let Some(d) = parse_date(input, default_year) {
        return Some((d, FormatHint::Date));
    }
    if let Some(t) = parse_time(input) {
        return Some((t, FormatHint::Time));
    }
    let s = input.trim();
    if let Some((d, t)) = s.split_once(' ')
        && let (Some(d), Some(t)) = (parse_date(d, default_year), parse_time(t))
    {
        return Some((d + t, FormatHint::DateTime));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        assert_eq!(parse_number("12"), Some((12.0, FormatHint::None)));
        assert_eq!(
            parse_number(" -1.5e3 "),
            Some((-1500.0, FormatHint::Scientific))
        );
        assert_eq!(
            parse_number("1,234.5"),
            Some((1234.5, FormatHint::Thousands))
        );
        assert_eq!(parse_number("50%"), Some((0.5, FormatHint::Percent)));
        assert_eq!(parse_number("$1,000"), Some((1000.0, FormatHint::Currency)));
        assert_eq!(parse_number("(42)"), Some((-42.0, FormatHint::None)));
        assert_eq!(parse_number("1,23"), None);
        assert_eq!(parse_number("abc"), None);
        assert_eq!(parse_number("."), None);
        assert_eq!(parse_number("inf"), None);
        assert_eq!(parse_number("--8"), None);
        assert_eq!(parse_number("+-8"), None);
    }

    #[test]
    fn dates_times() {
        assert_eq!(parse_date("2024-01-15", 2000), Some(45306.0));
        assert_eq!(parse_date("1/15/2024", 2000), Some(45306.0));
        assert_eq!(parse_date("15-Jan-2024", 2000), Some(45306.0));
        assert_eq!(parse_date("Jan 15, 2024", 2000), Some(45306.0));
        assert_eq!(parse_date("2/30/2024", 2000), None);
        assert_eq!(parse_time("12:00"), Some(0.5));
        assert_eq!(parse_time("6 PM"), Some(0.75));
        assert_eq!(parse_time("12:00 AM"), Some(0.0));
        assert_eq!(parse_time("5"), None);
        assert_eq!(parse_time("13:00 PM"), None);
        assert_eq!(parse_time("23:59:60"), Some(1.0));
        assert_eq!(parse_time("0:0:86400"), None);
        assert_eq!(parse_time("4.35"), None);
        assert_eq!(parse_time("4:35 A.M."), None);
        assert_eq!(parse_time("-4:35 AM"), None);
        assert_eq!(
            parse_numeric_input("1/15/2024 12:00", 2000),
            Some((45306.5, FormatHint::DateTime))
        );
    }

    #[test]
    fn date_edge_cases() {
        // Phantom 1900-02-29 and the 1900..=9999 window.
        assert_eq!(parse_date("29-Feb-1900", 2000), Some(60.0));
        assert_eq!(parse_date("1900-02-29", 2000), Some(60.0));
        assert_eq!(parse_date("1900-03-01", 2000), Some(61.0));
        assert_eq!(parse_date("1789-05-05", 2000), None);
        assert_eq!(parse_date("10000-01-01", 2000), None);
        assert_eq!(parse_date("2100-02-29", 2000), None);
        assert_eq!(parse_date("2026.01.01", 2000), None);
        assert_eq!(parse_date("2026-01-01x", 2000), None);
        assert_eq!(parse_date("-2026-01-01", 2000), None);
        assert_eq!(parse_date("2026-1-1", 2000), Some(46023.0));
        // Numeric d-m-y fallback when the month is impossible.
        assert_eq!(parse_date("26-01-01", 2000), Some(36917.0));
        assert_eq!(parse_date("1/1/30", 2000), date_to_serial(1930, 1, 1));
        assert_eq!(parse_date("Jan-2024", 2000), date_to_serial(2024, 1, 1));
    }

    #[test]
    fn date_time_text() {
        let p = |s: &str| parse_date_time_text(s, 2000);
        assert_eq!(p("2026 - 01 - 01"), Some(46023.0));
        assert_eq!(p(" 2026-01-01 "), Some(46023.0));
        assert_eq!(p("2026-01-01 24:00:00").map(f64::floor), Some(46024.0));
        assert_eq!(p("2026-01-01 25:00:00").map(f64::floor), Some(46024.0));
        assert_eq!(p("2026-01-01 23:59:59").map(f64::floor), Some(46023.0));
        assert_eq!(p("2026-07-16 4:35 PM").map(f64::floor), Some(46219.0));
        assert_eq!(p("4 : 35"), parse_time("4:35"));
        assert_eq!(p("July 1, 2026"), None);
        assert_eq!(p("\"2026-01-12\""), None);
        assert_eq!(p(""), None);
        assert_eq!(p("abc"), None);
        // Cell entry still accepts the month-name-first form.
        assert!(parse_date("July 1, 2026", 2000).is_some());
    }
}
