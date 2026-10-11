//! Text long tail: width conversion (ASC/DBCS), PHONETIC, BAHTTEXT, byte-count variants
//! (single-byte locale: same as the character versions), VALUETOTEXT and ARRAYTOTEXT.

use super::{Registry, lift1};
use crate::ast::Expr;
use crate::eval::{Ev, EvalValue, to_text};
use sp_core::{ErrorKind, Value};

/// Full-width ASCII variants (U+FF01..U+FF5E) and the ideographic space to half width.
pub fn to_half_width(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{3000}' => ' ',
            '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
            _ => c,
        })
        .collect()
}

/// Half-width ASCII (0x21..0x7E) and space to full width.
pub fn to_full_width(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            ' ' => '\u{3000}',
            '!'..='~' => char::from_u32(c as u32 + 0xFEE0).unwrap_or(c),
            _ => c,
        })
        .collect()
}

fn width(ev: &mut Ev<'_>, args: &[Expr], f: fn(&str) -> String) -> EvalValue {
    let Some(a) = args.first() else {
        return EvalValue::err(ErrorKind::Value);
    };
    let v = ev.eval(a);
    lift1(ev, v, |x| match to_text(x) {
        Ok(s) => Value::text(f(&s)),
        Err(e) => Value::Error(e),
    })
}

const THAI_DIGITS: [&str; 10] = [
    "ศูนย์", "หนึ่ง", "สอง", "สาม", "สี่", "ห้า", "หก", "เจ็ด", "แปด", "เก้า",
];
const THAI_PLACES: [&str; 6] = ["", "สิบ", "ร้อย", "พัน", "หมื่น", "แสน"];

/// Thai words for 0 < n < 1,000,000 (no "zero").
fn thai_group(n: u64) -> String {
    let digits: Vec<u64> = n.to_string().bytes().map(|b| u64::from(b - b'0')).collect();
    let len = digits.len();
    let mut out = String::new();
    for (i, d) in digits.iter().enumerate() {
        let place = len - 1 - i;
        if *d == 0 {
            continue;
        }
        let word = match (place, d) {
            (1, 1) => "",
            (1, 2) => "ยี่",
            (0, 1) if len > 1 => "เอ็ด",
            _ => THAI_DIGITS.get(*d as usize).copied().unwrap_or(""),
        };
        out.push_str(word);
        out.push_str(THAI_PLACES.get(place).copied().unwrap_or(""));
    }
    out
}

/// Thai words for any non-negative integer (millions recurse with "ล้าน").
fn thai_number(n: u64) -> String {
    if n == 0 {
        return String::new();
    }
    let (hi, lo) = (n / 1_000_000, n % 1_000_000);
    let mut out = String::new();
    if hi > 0 {
        out.push_str(&thai_number(hi));
        out.push_str("ล้าน");
    }
    if lo > 0 {
        out.push_str(&thai_group(lo));
    }
    out
}

pub fn bahttext(x: f64) -> Result<String, ErrorKind> {
    if !x.is_finite() || x.abs() >= 1e15 {
        return Err(ErrorKind::Num);
    }
    let neg = x < 0.0;
    let satang_total = (x.abs() * 100.0).round() as u64;
    let (baht, satang) = (satang_total / 100, satang_total % 100);
    let mut out = String::new();
    if neg {
        out.push_str("ลบ");
    }
    if baht == 0 && satang == 0 {
        out.push_str("ศูนย์บาทถ้วน");
        return Ok(out);
    }
    if baht > 0 {
        out.push_str(&thai_number(baht));
        out.push_str("บาท");
    }
    if satang == 0 {
        out.push_str("ถ้วน");
    } else {
        out.push_str(&thai_number(satang));
        out.push_str("สตางค์");
    }
    Ok(out)
}

/// Text of a value for VALUETOTEXT/ARRAYTOTEXT (strict format quotes strings).
fn value_text(v: &Value, strict: bool) -> String {
    match v {
        Value::Text(s) if strict => format!("\"{}\"", s.replace('"', "\"\"")),
        Value::Text(s) => s.to_string(),
        Value::Empty => String::new(),
        other => other.to_display(),
    }
}

fn format_arg(ev: &mut Ev<'_>, args: &[Expr], i: usize) -> Result<bool, ErrorKind> {
    match ev.num_or(args, i, 0.0)?.trunc() {
        0.0 => Ok(false),
        1.0 => Ok(true),
        _ => Err(ErrorKind::Value),
    }
}

fn valuetotext(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let Some(a) = args.first() else {
        return EvalValue::err(ErrorKind::Value);
    };
    let strict = match format_arg(ev, args, 1) {
        Ok(s) => s,
        Err(e) => return EvalValue::err(e),
    };
    let v = ev.eval(a);
    lift1(ev, v, |x| Value::text(value_text(x, strict)))
}

fn arraytotext(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let Some(a) = args.first() else {
        return EvalValue::err(ErrorKind::Value);
    };
    let strict = match format_arg(ev, args, 1) {
        Ok(s) => s,
        Err(e) => return EvalValue::err(e),
    };
    let v = ev.eval(a);
    let arr = match ev.to_array(v) {
        Ok(x) => x,
        Err(e) => return EvalValue::err(e),
    };
    let out = if strict {
        let rows: Vec<String> = (0..arr.rows)
            .map(|r| {
                arr.row(r)
                    .map(|v| value_text(v, true))
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .collect();
        format!("{{{}}}", rows.join(";"))
    } else {
        arr.data
            .iter()
            .map(|v| value_text(v, false))
            .collect::<Vec<_>>()
            .join(", ")
    };
    if out.chars().count() > sp_core::MAX_TEXT_LEN {
        return EvalValue::err(ErrorKind::Value);
    }
    EvalValue::text(out)
}

/// Delimiter list from a text or array argument (`None` when omitted).
fn delims(ev: &mut Ev<'_>, args: &[Expr], i: usize) -> Result<Option<Vec<String>>, ErrorKind> {
    match args.get(i) {
        None | Some(Expr::Missing) => Ok(None),
        Some(e) => {
            let v = ev.eval(e);
            let a = ev.to_array(v)?;
            let mut out = Vec::with_capacity(a.data.len());
            for x in &a.data {
                let s = to_text(x)?;
                if s.is_empty() {
                    return Err(ErrorKind::Value);
                }
                out.push(s);
            }
            Ok(Some(out))
        }
    }
}

/// Split `s` on any of `ds` (leftmost, longest delimiter first at each position).
fn split_on(s: &str, ds: &[String], icase: bool) -> Vec<String> {
    let hay: Vec<char> = s.chars().collect();
    let pats: Vec<Vec<char>> = ds
        .iter()
        .map(|d| {
            if icase {
                d.to_lowercase().chars().collect()
            } else {
                d.chars().collect()
            }
        })
        .collect();
    let cmp: Vec<char> = if icase {
        hay.iter().map(|c| c.to_lowercase().next().unwrap_or(*c)).collect()
    } else {
        hay.clone()
    };
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut i = 0;
    while i < hay.len() {
        let hit = pats
            .iter()
            .filter(|p| !p.is_empty() && cmp.get(i..i + p.len()) == Some(p.as_slice()))
            .map(Vec::len)
            .max();
        match hit {
            Some(n) => {
                out.push(std::mem::take(&mut cur));
                i += n;
            }
            None => {
                if let Some(c) = hay.get(i) {
                    cur.push(*c);
                }
                i += 1;
            }
        }
    }
    out.push(cur);
    out
}

fn textsplit(ev: &mut Ev<'_>, args: &[Expr]) -> Result<EvalValue, ErrorKind> {
    let text = ev.text(args.first().ok_or(ErrorKind::Value)?)?;
    let cols = delims(ev, args, 1)?;
    let rows = delims(ev, args, 2)?;
    if cols.is_none() && rows.is_none() {
        return Err(ErrorKind::Value);
    }
    let ignore_empty = ev.bool_or(args, 3, false)?;
    let icase = match ev.num_or(args, 4, 0.0)?.trunc() {
        0.0 => false,
        1.0 => true,
        _ => return Err(ErrorKind::Value),
    };
    let pad = match args.get(5) {
        None | Some(Expr::Missing) => Value::Error(ErrorKind::NA),
        Some(e) => ev.eval_scalar(e),
    };
    let row_parts = match &rows {
        Some(r) => split_on(&text, r, icase),
        None => vec![text],
    };
    let mut grid: Vec<Vec<String>> = Vec::new();
    for rp in row_parts {
        if ignore_empty && rp.is_empty() {
            continue;
        }
        let mut cells = match &cols {
            Some(c) => split_on(&rp, c, icase),
            None => vec![rp],
        };
        if ignore_empty {
            cells.retain(|c| !c.is_empty());
        }
        grid.push(cells);
    }
    let ncols = grid.iter().map(Vec::len).max().unwrap_or(0).max(1);
    let nrows = grid.len().max(1);
    if nrows.saturating_mul(ncols) > 1_000_000 {
        return Err(ErrorKind::Num);
    }
    let mut data = Vec::with_capacity(nrows * ncols);
    for r in 0..nrows {
        for c in 0..ncols {
            data.push(match grid.get(r).and_then(|row| row.get(c)) {
                Some(s) => Value::text(s.as_str()),
                None => pad.clone(),
            });
        }
    }
    Ok(EvalValue::Array(crate::eval::Array2::new(nrows, ncols, data)))
}

pub fn register(r: &mut Registry) {
    r.add("TEXTSPLIT", 2, 6, |ev, a| textsplit(ev, a).unwrap_or_else(EvalValue::err));
    r.add("ASC", 1, 1, |ev, a| width(ev, a, to_half_width));
    r.add("DBCS", 1, 1, |ev, a| width(ev, a, to_full_width));
    r.add("PHONETIC", 1, 1, |ev, a| width(ev, a, |s| s.to_string()));
    r.add("BAHTTEXT", 1, 1, |ev, a| {
        let Some(x) = a.first() else {
            return EvalValue::err(ErrorKind::Value);
        };
        let v = ev.eval(x);
        lift1(ev, v, |x| match crate::eval::to_number(x).and_then(bahttext) {
            Ok(s) => Value::text(s),
            Err(e) => Value::Error(e),
        })
    });
    r.add("VALUETOTEXT", 1, 2, valuetotext);
    r.add("ARRAYTOTEXT", 1, 2, arraytotext);
    // Byte-count variants: identical in single-byte locales.
    for (b, c) in [
        ("FINDB", "FIND"),
        ("LEFTB", "LEFT"),
        ("LENB", "LEN"),
        ("MIDB", "MID"),
        ("REPLACEB", "REPLACE"),
        ("RIGHTB", "RIGHT"),
        ("SEARCHB", "SEARCH"),
    ] {
        r.alias(b, c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split() {
        let ds = vec![",".to_string(), ";".to_string()];
        assert_eq!(split_on("a,b;;c", &ds, false), vec!["a", "b", "", "c"]);
        assert_eq!(split_on("aXbxc", &["x".to_string()], true), vec!["a", "b", "c"]);
    }

    #[test]
    fn widths() {
        assert_eq!(to_half_width("ＡＢＣ　１２"), "ABC 12");
        assert_eq!(to_full_width("Ab 1"), "Ａｂ　１");
    }

    #[test]
    fn baht() {
        assert_eq!(bahttext(1234.0).unwrap(), "หนึ่งพันสองร้อยสามสิบสี่บาทถ้วน");
        assert_eq!(bahttext(21.25).unwrap(), "ยี่สิบเอ็ดบาทยี่สิบห้าสตางค์");
        assert_eq!(bahttext(0.0).unwrap(), "ศูนย์บาทถ้วน");
        assert_eq!(bahttext(11.0).unwrap(), "สิบเอ็ดบาทถ้วน");
        assert!(bahttext(f64::INFINITY).is_err());
    }
}
