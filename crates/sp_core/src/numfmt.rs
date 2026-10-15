//! Number-format codes (`#,##0.00`, `0%`, `m/d/yyyy`, `[Red]0;(0)`…) → display text.
//!
//! Implemented from the public format-code grammar (ECMA-376 Part 1, §18.8.31). Used by the
//! grid renderer and by the `TEXT` worksheet function.

use crate::date::{serial_to_date, serial_to_time};
use crate::value::{Value, format_general};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FmtColor {
    Black,
    Blue,
    Cyan,
    Green,
    Magenta,
    Red,
    White,
    Yellow,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Formatted {
    pub text: String,
    pub color: Option<FmtColor>,
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Lit(String),
    Digit(char), // '0', '#', '?'
    Point,
    Comma,
    Percent,
    Exp(bool), // E+ (true) or E- (false)
    Text,      // '@'
    Year(usize),
    Month(usize),
    Minute(usize),
    Day(usize),
    Hour(usize),
    Second(usize),
    AmPm(bool), // true = "AM/PM", false = "A/P"
    Elapsed(char, usize),
    SubSecond(usize),
    Space,
}

#[derive(Debug, Clone, PartialEq, Default)]
struct Section {
    toks: Vec<Tok>,
    color: Option<FmtColor>,
    cond: Option<(String, f64)>,
}

impl Section {
    fn is_date(&self) -> bool {
        self.toks.iter().any(|t| {
            matches!(
                t,
                Tok::Year(_)
                    | Tok::Month(_)
                    | Tok::Day(_)
                    | Tok::Hour(_)
                    | Tok::Minute(_)
                    | Tok::Second(_)
                    | Tok::Elapsed(..)
            )
        })
    }

    fn has_digits(&self) -> bool {
        self.toks.iter().any(|t| matches!(t, Tok::Digit(_)))
    }
}

fn split_sections(code: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut in_q = false;
    let mut esc = false;
    let mut in_b = false;
    for c in code.chars() {
        let cur = out.last_mut();
        let Some(cur) = cur else { break };
        if esc {
            cur.push(c);
            esc = false;
            continue;
        }
        match c {
            '\\' if !in_q => {
                cur.push(c);
                esc = true;
            }
            '"' => {
                in_q = !in_q;
                cur.push(c);
            }
            '[' if !in_q => {
                in_b = true;
                cur.push(c);
            }
            ']' if !in_q => {
                in_b = false;
                cur.push(c);
            }
            ';' if !in_q && !in_b => out.push(String::new()),
            _ => cur.push(c),
        }
    }
    out
}

fn parse_section(s: &str) -> Section {
    let chars: Vec<char> = s.chars().collect();
    let at = |i: usize| chars.get(i).copied();
    let mut sec = Section::default();
    let mut i = 0;
    let lower = |c: Option<char>| c.map(|c| c.to_ascii_lowercase());
    while let Some(c) = at(i) {
        match c {
            '"' => {
                let mut lit = String::new();
                i += 1;
                while let Some(ch) = at(i) {
                    i += 1;
                    if ch == '"' {
                        break;
                    }
                    lit.push(ch);
                }
                sec.toks.push(Tok::Lit(lit));
            }
            '\\' => {
                if let Some(ch) = at(i + 1) {
                    sec.toks.push(Tok::Lit(ch.to_string()));
                }
                i += 2;
            }
            '_' => {
                sec.toks.push(Tok::Space);
                i += 2;
            }
            '*' => i += 2,
            '[' => {
                let mut inner = String::new();
                i += 1;
                while let Some(ch) = at(i) {
                    i += 1;
                    if ch == ']' {
                        break;
                    }
                    inner.push(ch);
                }
                let l = inner.to_ascii_lowercase();
                let color = match l.as_str() {
                    "black" => Some(FmtColor::Black),
                    "blue" => Some(FmtColor::Blue),
                    "cyan" => Some(FmtColor::Cyan),
                    "green" => Some(FmtColor::Green),
                    "magenta" => Some(FmtColor::Magenta),
                    "red" => Some(FmtColor::Red),
                    "white" => Some(FmtColor::White),
                    "yellow" => Some(FmtColor::Yellow),
                    _ => None,
                };
                if color.is_some() {
                    sec.color = color;
                } else if l.starts_with('h') || l.starts_with('m') || l.starts_with('s') {
                    let kind = l.chars().next().unwrap_or('h');
                    sec.toks.push(Tok::Elapsed(kind, l.len()));
                } else if let Some(rest) = ["<=", ">=", "<>", "<", ">", "="]
                    .iter()
                    .find(|op| l.starts_with(**op))
                    && let Ok(v) = l.get(rest.len()..).unwrap_or("").trim().parse::<f64>()
                {
                    sec.cond = Some((rest.to_string(), v));
                }
                // Locale/currency tags like [$€-407] are dropped except for the symbol.
                if let Some(sym) = inner.strip_prefix('$') {
                    let sym = sym.split('-').next().unwrap_or("");
                    if !sym.is_empty() {
                        sec.toks.push(Tok::Lit(sym.to_string()));
                    }
                }
            }
            '0' | '#' | '?' => {
                sec.toks.push(Tok::Digit(c));
                i += 1;
            }
            '.' => {
                // ".0" after seconds = sub-second precision.
                if matches!(sec.toks.last(), Some(Tok::Second(_))) && at(i + 1) == Some('0') {
                    let mut n = 0;
                    i += 1;
                    while at(i) == Some('0') {
                        n += 1;
                        i += 1;
                    }
                    sec.toks.push(Tok::SubSecond(n));
                } else {
                    sec.toks.push(Tok::Point);
                    i += 1;
                }
            }
            ',' => {
                sec.toks.push(Tok::Comma);
                i += 1;
            }
            '%' => {
                sec.toks.push(Tok::Percent);
                i += 1;
            }
            '@' => {
                sec.toks.push(Tok::Text);
                i += 1;
            }
            'E' | 'e' if matches!(at(i + 1), Some('+' | '-')) => {
                sec.toks.push(Tok::Exp(at(i + 1) == Some('+')));
                i += 2;
            }
            _ => {
                let lc = c.to_ascii_lowercase();
                let run = |i: usize| {
                    let mut n = 0;
                    while lower(at(i + n)) == Some(lc) {
                        n += 1;
                    }
                    n
                };
                match lc {
                    'y' => {
                        let n = run(i);
                        sec.toks.push(Tok::Year(n));
                        i += n;
                    }
                    'd' => {
                        let n = run(i);
                        sec.toks.push(Tok::Day(n));
                        i += n;
                    }
                    'h' => {
                        let n = run(i);
                        sec.toks.push(Tok::Hour(n));
                        i += n;
                    }
                    's' => {
                        let n = run(i);
                        sec.toks.push(Tok::Second(n));
                        i += n;
                    }
                    'm' => {
                        let n = run(i);
                        sec.toks.push(Tok::Month(n));
                        i += n;
                    }
                    'a' => {
                        let rest: String = chars
                            .get(i..)
                            .unwrap_or(&[])
                            .iter()
                            .take(5)
                            .collect::<String>()
                            .to_ascii_lowercase();
                        if rest.starts_with("am/pm") {
                            sec.toks.push(Tok::AmPm(true));
                            i += 5;
                        } else if rest.starts_with("a/p") {
                            sec.toks.push(Tok::AmPm(false));
                            i += 3;
                        } else {
                            sec.toks.push(Tok::Lit(c.to_string()));
                            i += 1;
                        }
                    }
                    _ => {
                        sec.toks.push(Tok::Lit(c.to_string()));
                        i += 1;
                    }
                }
            }
        }
    }
    // 'm' means minutes when right after an hour or right before a second.
    let n = sec.toks.len();
    for k in 0..n {
        if let Some(Tok::Month(len)) = sec.toks.get(k).cloned() {
            let prev = sec
                .toks
                .get(..k)
                .unwrap_or(&[])
                .iter()
                .rev()
                .find(|t| !matches!(t, Tok::Lit(_) | Tok::Space));
            let next = sec
                .toks
                .get(k + 1..)
                .unwrap_or(&[])
                .iter()
                .find(|t| !matches!(t, Tok::Lit(_) | Tok::Space));
            let after_hour = matches!(prev, Some(Tok::Hour(_) | Tok::Elapsed('h', _)));
            let before_sec = matches!(next, Some(Tok::Second(_) | Tok::Elapsed('s', _)));
            if (after_hour || before_sec)
                && len <= 2
                && let Some(t) = sec.toks.get_mut(k)
            {
                *t = Tok::Minute(len);
            }
        }
    }
    sec
}

const MONTH_NAMES: [&str; 12] = [
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
const DAY_NAMES: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

fn format_date(sec: &Section, v: f64) -> Option<String> {
    let has_sub = sec.toks.iter().find_map(|t| {
        if let Tok::SubSecond(n) = t {
            Some(*n)
        } else {
            None
        }
    });
    let rounding = match has_sub {
        Some(n) => 86_400.0 * 10f64.powi(n as i32),
        None => 86_400.0,
    };
    let v = (v * rounding).round() / rounding;
    let (y, mo, d) = serial_to_date(v)?;
    let (h, mi, s) = serial_to_time(v);
    let frac_sec = (v * 86_400.0).fract();
    let ampm = sec.toks.iter().any(|t| matches!(t, Tok::AmPm(_)));
    let mut out = String::new();
    let wd = crate::date::weekday(v) as usize;
    for t in &sec.toks {
        match t {
            Tok::Lit(l) => out.push_str(l),
            Tok::Space => out.push(' '),
            Tok::Year(n) => {
                if *n <= 2 {
                    out.push_str(&format!("{:02}", y.rem_euclid(100)));
                } else {
                    out.push_str(&format!("{y:04}"));
                }
            }
            Tok::Month(n) => {
                let name = MONTH_NAMES
                    .get(mo.saturating_sub(1) as usize)
                    .copied()
                    .unwrap_or("");
                match n {
                    1 => out.push_str(&mo.to_string()),
                    2 => out.push_str(&format!("{mo:02}")),
                    3 => out.push_str(name.get(..3).unwrap_or(name)),
                    5 => out.push_str(name.get(..1).unwrap_or(name)),
                    _ => out.push_str(name),
                }
            }
            Tok::Day(n) => {
                let name = DAY_NAMES.get(wd).copied().unwrap_or("");
                match n {
                    1 => out.push_str(&d.to_string()),
                    2 => out.push_str(&format!("{d:02}")),
                    3 => out.push_str(name.get(..3).unwrap_or(name)),
                    _ => out.push_str(name),
                }
            }
            Tok::Hour(n) => {
                let hh = if ampm {
                    if h % 12 == 0 { 12 } else { h % 12 }
                } else {
                    h
                };
                if *n >= 2 {
                    out.push_str(&format!("{hh:02}"));
                } else {
                    out.push_str(&hh.to_string());
                }
            }
            Tok::Minute(n) => {
                if *n >= 2 {
                    out.push_str(&format!("{mi:02}"));
                } else {
                    out.push_str(&mi.to_string());
                }
            }
            Tok::Second(n) => {
                if *n >= 2 {
                    out.push_str(&format!("{s:02}"));
                } else {
                    out.push_str(&s.to_string());
                }
            }
            Tok::SubSecond(n) => {
                let digits = format!("{:.*}", *n, frac_sec.abs());
                out.push_str(digits.get(1..).unwrap_or(""));
            }
            Tok::AmPm(full) => {
                let pm = h >= 12;
                out.push_str(match (full, pm) {
                    (true, false) => "AM",
                    (true, true) => "PM",
                    (false, false) => "A",
                    (false, true) => "P",
                });
            }
            Tok::Elapsed(kind, len) => {
                let total = v * 86_400.0;
                let n = match kind {
                    'h' => (total / 3600.0).floor(),
                    'm' => (total / 60.0).floor(),
                    _ => total.floor(),
                } as i64;
                out.push_str(&format!(
                    "{:0width$}",
                    n,
                    width = len.saturating_sub(2).max(1)
                ));
            }
            // Separators are literal inside date formats.
            Tok::Comma => out.push(','),
            Tok::Point => out.push('.'),
            Tok::Percent => out.push('%'),
            Tok::Digit(_) | Tok::Exp(_) | Tok::Text => {}
        }
    }
    Some(out)
}

fn group_thousands(int: &str) -> String {
    let bytes: Vec<char> = int.chars().collect();
    let mut out = String::new();
    for (i, c) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*c);
    }
    out
}

fn format_number(sec: &Section, v: f64) -> String {
    let toks = &sec.toks;
    let point = toks.iter().position(|t| *t == Tok::Point);
    let exp = toks.iter().position(|t| matches!(t, Tok::Exp(_)));
    let int_end = point.or(exp).unwrap_or(toks.len());
    let int_digits: Vec<char> = toks
        .get(..int_end)
        .unwrap_or(&[])
        .iter()
        .filter_map(|t| {
            if let Tok::Digit(c) = t {
                Some(*c)
            } else {
                None
            }
        })
        .collect();
    let frac_end = exp.unwrap_or(toks.len());
    let frac_digits: Vec<char> = match point {
        Some(p) => toks
            .get(p + 1..frac_end)
            .unwrap_or(&[])
            .iter()
            .filter_map(|t| {
                if let Tok::Digit(c) = t {
                    Some(*c)
                } else {
                    None
                }
            })
            .collect(),
        None => Vec::new(),
    };
    let percent = toks.iter().filter(|t| **t == Tok::Percent).count();
    // Commas directly after the last digit placeholder scale by 1000 each.
    let last_digit = toks
        .get(..int_end)
        .unwrap_or(&[])
        .iter()
        .rposition(|t| matches!(t, Tok::Digit(_)));
    let mut scale_commas = 0;
    if let Some(ld) = last_digit {
        let mut k = ld + 1;
        while toks.get(k) == Some(&Tok::Comma) {
            scale_commas += 1;
            k += 1;
        }
    }
    let thousands = toks
        .get(..int_end)
        .unwrap_or(&[])
        .iter()
        .enumerate()
        .any(|(i, t)| {
            *t == Tok::Comma
                && toks
                    .get(..i)
                    .unwrap_or(&[])
                    .iter()
                    .any(|x| matches!(x, Tok::Digit(_)))
                && Some(i) < last_digit
        });
    let mut x = v.abs() * 100f64.powi(percent as i32) / 1000f64.powi(scale_commas);
    let ndec = frac_digits.len();

    let (int_str, frac_str, exp_str) = if let Some(e) = exp {
        let exp_plus = matches!(toks.get(e), Some(Tok::Exp(true)));
        let exp_digits = toks
            .get(e + 1..)
            .unwrap_or(&[])
            .iter()
            .filter(|t| matches!(t, Tok::Digit(_)))
            .count()
            .max(1);
        let int_n = int_digits.len().max(1) as i32;
        let mut e10 = if x == 0.0 {
            0
        } else {
            x.log10().floor() as i32 - (int_n - 1)
        };
        let mut mant = x / 10f64.powi(e10);
        let r = format!("{:.*}", ndec, mant);
        if r.parse::<f64>().unwrap_or(0.0) >= 10f64.powi(int_n) {
            e10 += 1;
            mant = x / 10f64.powi(e10);
        }
        let s = format!("{:.*}", ndec, mant);
        let (i, f) = s.split_once('.').unwrap_or((s.as_str(), ""));
        let sign = if e10 < 0 {
            "-"
        } else if exp_plus {
            "+"
        } else {
            ""
        };
        (
            i.to_string(),
            f.to_string(),
            Some(format!("E{sign}{:0w$}", e10.abs(), w = exp_digits)),
        )
    } else {
        x = (x * 10f64.powi(ndec as i32)).round() / 10f64.powi(ndec as i32);
        let s = format!("{:.*}", ndec, x);
        let (i, f) = s.split_once('.').unwrap_or((s.as_str(), ""));
        (i.to_string(), f.to_string(), None)
    };

    // Integer part: pad to the number of '0' placeholders; drop a lone 0 if only '#'.
    let min_int = int_digits.iter().filter(|c| **c == '0').count();
    let mut int_s = int_str.trim_start_matches('0').to_string();
    while int_s.len() < min_int {
        int_s.insert(0, '0');
    }
    if thousands {
        int_s = group_thousands(&int_s);
    }
    // Fraction: trim trailing zeros where placeholders are '#'.
    let mut frac_chars: Vec<char> = frac_str.chars().collect();
    for (k, ph) in frac_digits.iter().enumerate().rev() {
        if *ph == '0' {
            break;
        }
        if frac_chars.get(k) == Some(&'0') {
            if *ph == '?' {
                if let Some(c) = frac_chars.get_mut(k) {
                    *c = ' ';
                }
            } else {
                frac_chars.truncate(k);
            }
        } else {
            break;
        }
    }
    let frac_s: String = frac_chars.into_iter().collect();

    // Integer placeholders separated by literals ("00-00-00-00") take the digits right to
    // left, one per placeholder; the leftmost placeholder takes any extra leading digits.
    let int_slots: Vec<usize> = toks
        .get(..int_end)
        .unwrap_or(&[])
        .iter()
        .enumerate()
        .filter(|(_, t)| matches!(t, Tok::Digit(_)))
        .map(|(i, _)| i)
        .collect();
    let distributed = match (int_slots.first(), int_slots.last()) {
        (Some(&a), Some(&b)) => toks
            .get(a..b)
            .unwrap_or(&[])
            .iter()
            .any(|t| matches!(t, Tok::Lit(_) | Tok::Space)),
        _ => false,
    };
    let mut slot_text: Vec<String> = vec![String::new(); int_slots.len()];
    if distributed {
        let mut digits: Vec<char> = int_str.trim_start_matches('0').chars().collect();
        for (k, &ti) in int_slots.iter().enumerate().rev() {
            let ph = match toks.get(ti) {
                Some(Tok::Digit(c)) => *c,
                _ => '#',
            };
            let piece = if k == 0 {
                std::mem::take(&mut digits).into_iter().collect::<String>()
            } else {
                digits.pop().map(String::from).unwrap_or_default()
            };
            if let Some(slot) = slot_text.get_mut(k) {
                *slot = if !piece.is_empty() {
                    piece
                } else {
                    match ph {
                        '0' => "0".into(),
                        '?' => " ".into(),
                        _ => String::new(),
                    }
                };
            }
        }
    }

    // Re-emit with literals in place: literals before the first digit, between, and after.
    let mut out = String::new();
    let mut int_emitted = false;
    let mut frac_emitted = false;
    for (i, t) in toks.iter().enumerate() {
        match t {
            Tok::Digit(_) => {
                if i < int_end && distributed {
                    if let Some(k) = int_slots.iter().position(|&x| x == i)
                        && let Some(t) = slot_text.get(k)
                    {
                        out.push_str(t);
                    }
                    int_emitted = true;
                } else if i < int_end {
                    if !int_emitted {
                        out.push_str(&int_s);
                        int_emitted = true;
                    }
                } else if Some(i) < exp.or(Some(usize::MAX))
                    && point.is_some_and(|p| i > p)
                    && !frac_emitted
                {
                    out.push_str(&frac_s);
                    frac_emitted = true;
                }
            }
            Tok::Point => {
                if !int_emitted {
                    out.push_str(&int_s);
                    int_emitted = true;
                }
                out.push('.');
            }
            Tok::Exp(_) => {
                if let Some(e) = &exp_str {
                    out.push_str(e);
                }
                break;
            }
            Tok::Percent => out.push('%'),
            Tok::Lit(l) => out.push_str(l),
            Tok::Space => out.push(' '),
            Tok::Comma | Tok::Text => {}
            _ => {}
        }
    }
    if !int_emitted && !sec.has_digits() {
        out.push_str(&int_s);
    }
    // A '#'-only fraction that trims to nothing keeps its point ("#.##" of 5 → "5.").
    out
}

/// Format a value with a number-format code.
pub fn format_value(v: &Value, code: &str) -> Formatted {
    let code = code.trim();
    let plain = |t: String| Formatted {
        text: t,
        color: None,
    };
    if code.is_empty() || code.eq_ignore_ascii_case("general") {
        return plain(v.to_display());
    }
    let secs: Vec<Section> = split_sections(code)
        .iter()
        .map(|s| parse_section(s))
        .collect();
    match v {
        Value::Number(n) => {
            let n = *n;
            let has_cond = secs.iter().any(|s| s.cond.is_some());
            let (sec, val) = if has_cond {
                let hit = secs.iter().find(|s| match &s.cond {
                    Some((op, c)) => match op.as_str() {
                        "<" => n < *c,
                        "<=" => n <= *c,
                        ">" => n > *c,
                        ">=" => n >= *c,
                        "=" => n == *c,
                        "<>" => n != *c,
                        _ => false,
                    },
                    None => true,
                });
                (hit, n)
            } else {
                match secs.len() {
                    0 | 1 => (secs.first(), n),
                    2 => {
                        if n < 0.0 {
                            (secs.get(1), -n)
                        } else {
                            (secs.first(), n)
                        }
                    }
                    _ => {
                        if n > 0.0 {
                            (secs.first(), n)
                        } else if n < 0.0 {
                            (secs.get(1), -n)
                        } else {
                            (secs.get(2), n)
                        }
                    }
                }
            };
            let Some(sec) = sec else {
                return plain(format_general(n));
            };
            let neg_in_first = secs.len() == 1 || (has_cond && val < 0.0);
            let text = if sec.is_date() {
                if val < 0.0 {
                    return plain("#".repeat(10));
                }
                format_date(sec, val).unwrap_or_else(|| "#".repeat(10))
            } else if sec.has_digits() {
                let s = format_number(sec, val);
                if neg_in_first && val < 0.0 && s.chars().any(|c| c.is_ascii_digit() && c != '0') {
                    format!("-{s}")
                } else {
                    s
                }
            } else {
                let mut s = String::new();
                for t in &sec.toks {
                    match t {
                        Tok::Lit(l) => s.push_str(l),
                        Tok::Space => s.push(' '),
                        Tok::Text => s.push_str(&format_general(val)),
                        _ => {}
                    }
                }
                if sec.toks.iter().any(|t| matches!(t, Tok::Text)) || !s.is_empty() {
                    s
                } else {
                    format_general(val)
                }
            };
            Formatted {
                text,
                color: sec.color,
            }
        }
        Value::Text(t) => match secs
            .get(3)
            .or_else(|| secs.iter().find(|s| s.toks.contains(&Tok::Text)))
        {
            Some(sec) => {
                let mut s = String::new();
                for tk in &sec.toks {
                    match tk {
                        Tok::Lit(l) => s.push_str(l),
                        Tok::Space => s.push(' '),
                        Tok::Text => s.push_str(t),
                        _ => {}
                    }
                }
                Formatted {
                    text: s,
                    color: sec.color,
                }
            }
            None => plain(t.to_string()),
        },
        other => plain(other.to_display()),
    }
}

/// Whether a format code displays dates/times (used to pick alignment and editing text).
pub fn is_date_format(code: &str) -> bool {
    split_sections(code)
        .first()
        .is_some_and(|s| parse_section(s).is_date())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(v: f64, code: &str) -> String {
        format_value(&Value::Number(v), code).text
    }

    #[test]
    fn numbers() {
        assert_eq!(f(1234.567, "#,##0.00"), "1,234.57");
        assert_eq!(f(0.5, "0%"), "50%");
        assert_eq!(f(0.1234, "0.0%"), "12.3%");
        assert_eq!(f(-5.0, "0.00"), "-5.00");
        assert_eq!(f(-5.0, "0;(0)"), "(5)");
        assert_eq!(f(0.0, "0;-0;\"zero\""), "zero");
        assert_eq!(f(5.0, "\"$\"#,##0"), "$5");
        assert_eq!(f(1234567.0, "#,##0,\"K\""), "1,235K");
        assert_eq!(f(12345.0, "0.00E+00"), "1.23E+04");
        assert_eq!(f(0.5, "#.##"), ".5");
        assert_eq!(f(12.0, "#.#"), "12.");
        assert_eq!(f(0.0, "#.#"), ".");
        assert_eq!(f(12.0, "00-00-00-00"), "00-00-00-12");
        assert_eq!(f(12345.6789, "00-00-00-00"), "00-01-23-46");
        assert_eq!(f(123456789012.0, "00-00-00-00"), "123456-78-90-12");
        assert_eq!(f(-12.0, "00-00-00-00"), "-00-00-00-12");
        assert_eq!(f(7.0, "000"), "007");
        assert_eq!(f(1.23456, "0.00"), "1.23");
        assert_eq!(f(1234.5, "General"), "1234.5");
        assert_eq!(
            format_value(&Value::Number(-1.0), "[Red]0;[Blue]0").color,
            Some(FmtColor::Blue)
        );
    }

    #[test]
    fn dates() {
        assert_eq!(f(45306.0, "m/d/yyyy"), "1/15/2024");
        assert_eq!(f(45306.0, "yyyy-mm-dd"), "2024-01-15");
        assert_eq!(f(45306.0, "d-mmm-yy"), "15-Jan-24");
        assert_eq!(f(45306.0, "dddd, mmmm d"), "Monday, January 15");
        assert_eq!(f(45306.75, "h:mm AM/PM"), "6:00 PM");
        assert_eq!(f(45306.75, "hh:mm:ss"), "18:00:00");
        assert_eq!(f(1.5, "[h]:mm"), "36:00");
    }

    #[test]
    fn text_section() {
        assert_eq!(
            format_value(&Value::text("x"), "0;0;0;\"<\"@\">\"").text,
            "<x>"
        );
        assert_eq!(format_value(&Value::text("x"), "0.00").text, "x");
    }
}
