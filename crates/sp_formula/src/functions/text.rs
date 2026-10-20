//! Text functions.

use super::criteria::wildcard_match;
use super::{Registry, for_each_value, lift1};
use crate::ast::Expr;
use crate::eval::{Ev, EvalValue, text_to_number, to_text};
use sp_core::numfmt::{format_value, is_date_format};
use sp_core::{ErrorKind, MAX_TEXT_LEN, Value};

fn s1(ev: &mut Ev<'_>, args: &[Expr], f: fn(&str) -> String) -> EvalValue {
    let Some(a) = args.first() else {
        return EvalValue::err(ErrorKind::Value);
    };
    let v = ev.eval(a);
    lift1(ev, v, |x| match to_text(x) {
        Ok(s) => Value::text(f(&s)),
        Err(e) => Value::Error(e),
    })
}

// Text positions and lengths are counted in UTF-16 code units, as the target does: a
// character outside the Basic Multilingual Plane (an emoji, say) counts as 2, so
// `LEN("😀")` is 2. Our strings are Rust `String`s and cannot hold a lone surrogate, so
// where the target would cut a surrogate pair in half (`LEFT("😀",1)`), the half that is
// kept becomes U+FFFD (the replacement character). It is one code unit long, so lengths
// and positions computed from the result still agree with the target's.

/// Length of `s` in UTF-16 code units.
pub(crate) fn u16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// The UTF-16 code units `start..end` of `s` (clamped). A surrogate pair cut by either
/// bound contributes U+FFFD for its kept half.
pub(crate) fn u16_slice(s: &str, start: usize, end: usize) -> String {
    let mut out = String::new();
    if start >= end {
        return out;
    }
    let mut pos = 0usize;
    for c in s.chars() {
        let w = c.len_utf16();
        let (a, b) = (pos, pos + w);
        pos = b;
        if b <= start {
            continue;
        }
        if a >= end {
            break;
        }
        if a >= start && b <= end {
            out.push(c);
        } else {
            out.push('\u{FFFD}');
        }
    }
    out
}

/// UTF-16 offset of every character boundary of `s`, paired with its byte offset
/// (including the end of the text).
fn u16_boundaries(s: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::with_capacity(s.len() + 1);
    let mut pos = 0usize;
    for (b, c) in s.char_indices() {
        out.push((pos, b));
        pos += c.len_utf16();
    }
    out.push((pos, s.len()));
    out
}

fn char_count_arg(
    ev: &mut Ev<'_>,
    args: &[Expr],
    i: usize,
    default: f64,
) -> Result<usize, ErrorKind> {
    let n = ev.num_or(args, i, default)?;
    if n < 0.0 {
        return Err(ErrorKind::Value);
    }
    Ok(n.trunc().min(MAX_TEXT_LEN as f64) as usize)
}

fn left_right(ev: &mut Ev<'_>, args: &[Expr], left: bool) -> EvalValue {
    let Some(a) = args.first() else {
        return EvalValue::err(ErrorKind::Value);
    };
    // The text argument's error wins over a bad count (`LEFT(#DIV/0!,-1)` is #DIV/0!).
    let n = char_count_arg(ev, args, 1, 1.0);
    let v = ev.eval(a);
    lift1(ev, v, |x| match (to_text(x), n) {
        (Err(e), _) | (Ok(_), Err(e)) => Value::Error(e),
        (Ok(s), Ok(n)) => {
            let len = u16_len(&s);
            Value::text(if left {
                u16_slice(&s, 0, n)
            } else {
                u16_slice(&s, len.saturating_sub(n), len)
            })
        }
    })
}

fn mid(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let (Some(a), Some(b), Some(c)) = (args.first(), args.get(1), args.get(2)) else {
        return EvalValue::err(ErrorKind::Value);
    };
    let (s, start, n) = match (ev.text(a), ev.num(b), ev.num(c)) {
        (Ok(s), Ok(st), Ok(n)) => (s, st.trunc(), n.trunc()),
        (Err(e), ..) | (_, Err(e), _) | (.., Err(e)) => return EvalValue::err(e),
    };
    if start < 1.0 || n < 0.0 {
        return EvalValue::err(ErrorKind::Value);
    }
    let start = (start.min(MAX_TEXT_LEN as f64 * 2.0) as usize).saturating_sub(1);
    let n = n.min(MAX_TEXT_LEN as f64) as usize;
    EvalValue::text(u16_slice(&s, start, start.saturating_add(n)))
}

fn proper(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_letter = false;
    for c in s.chars() {
        if c.is_alphabetic() {
            if prev_letter {
                out.extend(c.to_lowercase());
            } else {
                out.extend(c.to_uppercase());
            }
            prev_letter = true;
        } else {
            out.push(c);
            prev_letter = false;
        }
    }
    out
}

fn trim(s: &str) -> String {
    s.split(' ')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn concat(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let mut out = String::new();
    let r = for_each_value(ev, args, &mut |v, _| {
        out.push_str(&to_text(v)?);
        if out.len() > MAX_TEXT_LEN * 4 {
            return Err(ErrorKind::Value);
        }
        Ok(())
    });
    match r {
        Ok(()) if out.chars().count() <= MAX_TEXT_LEN => EvalValue::text(out),
        Ok(()) => EvalValue::err(ErrorKind::Value),
        Err(e) => EvalValue::err(e),
    }
}

fn concatenate(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let mut out = String::new();
    for a in args {
        match ev.text(a) {
            Ok(s) => out.push_str(&s),
            Err(e) => return EvalValue::err(e),
        }
    }
    if out.chars().count() > MAX_TEXT_LEN {
        EvalValue::err(ErrorKind::Value)
    } else {
        EvalValue::text(out)
    }
}

fn textjoin(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let (Some(d), Some(ig)) = (args.first(), args.get(1)) else {
        return EvalValue::err(ErrorKind::Value);
    };
    let delim = match ev.text(d) {
        Ok(s) => s,
        Err(e) => return EvalValue::err(e),
    };
    let ignore = match ev.boolean(ig) {
        Ok(b) => b,
        Err(e) => return EvalValue::err(e),
    };
    let mut parts: Vec<String> = Vec::new();
    let rest = args.get(2..).unwrap_or(&[]);
    let r = if ignore {
        for_each_value(ev, rest, &mut |v, _| {
            let s = to_text(v)?;
            if !s.is_empty() {
                parts.push(s);
            }
            Ok(())
        })
    } else {
        // Not ignoring empties: blank cells inside references count as "" too.
        textjoin_all(ev, rest, &mut parts)
    };
    if let Err(e) = r {
        return EvalValue::err(e);
    }
    let out = parts.join(&delim);
    if out.chars().count() > MAX_TEXT_LEN {
        EvalValue::err(ErrorKind::Value)
    } else {
        EvalValue::text(out)
    }
}

/// Every value of `args` as text, blank cells of references included.
fn textjoin_all(ev: &mut Ev<'_>, args: &[Expr], parts: &mut Vec<String>) -> Result<(), ErrorKind> {
    for a in args {
        let refs = match ev.eval(a) {
            EvalValue::Ref(r) => vec![r],
            EvalValue::Refs(rs) => rs,
            other => {
                let arr = ev.to_array(other)?;
                for v in &arr.data {
                    parts.push(to_text(v)?);
                }
                continue;
            }
        };
        for r in refs {
            let arr = ev.ref_to_array(r)?;
            for v in &arr.data {
                parts.push(to_text(v)?);
            }
        }
    }
    Ok(())
}

fn rept(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let (Some(a), Some(b)) = (args.first(), args.get(1)) else {
        return EvalValue::err(ErrorKind::Value);
    };
    let (s, n) = match (ev.text(a), ev.num(b)) {
        (Ok(s), Ok(n)) => (s, n.trunc()),
        (Err(e), _) | (_, Err(e)) => return EvalValue::err(e),
    };
    if n < 0.0 {
        return EvalValue::err(ErrorKind::Value);
    }
    if (s.chars().count() as f64) * n > MAX_TEXT_LEN as f64 {
        return EvalValue::err(ErrorKind::Value);
    }
    EvalValue::text(s.repeat(n as usize))
}

fn substitute(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let (Some(a), Some(b), Some(c)) = (args.first(), args.get(1), args.get(2)) else {
        return EvalValue::err(ErrorKind::Value);
    };
    let (s, old, new) = match (ev.text(a), ev.text(b), ev.text(c)) {
        (Ok(x), Ok(y), Ok(z)) => (x, y, z),
        (Err(e), ..) | (_, Err(e), _) | (.., Err(e)) => return EvalValue::err(e),
    };
    if old.is_empty() {
        return EvalValue::text(s);
    }
    let out = match args.get(3) {
        None => s.replace(&old, &new),
        Some(e) => {
            let n = match ev.num(e) {
                Ok(n) if n >= 1.0 => n.trunc() as usize,
                Ok(_) => return EvalValue::err(ErrorKind::Value),
                Err(e) => return EvalValue::err(e),
            };
            match s.match_indices(&old).nth(n - 1) {
                Some((idx, _)) => format!(
                    "{}{}{}",
                    s.get(..idx).unwrap_or(""),
                    new,
                    s.get(idx + old.len()..).unwrap_or("")
                ),
                None => s,
            }
        }
    };
    if out.chars().count() > MAX_TEXT_LEN {
        EvalValue::err(ErrorKind::Value)
    } else {
        EvalValue::text(out)
    }
}

fn replace(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let (Some(a), Some(b), Some(c), Some(d)) =
        (args.first(), args.get(1), args.get(2), args.get(3))
    else {
        return EvalValue::err(ErrorKind::Value);
    };
    let (s, start, n, new) = match (ev.text(a), ev.num(b), ev.num(c), ev.text(d)) {
        (Ok(s), Ok(st), Ok(n), Ok(new)) => (s, st.trunc(), n.trunc(), new),
        (Err(e), ..) | (_, Err(e), ..) | (_, _, Err(e), _) | (.., Err(e)) => {
            return EvalValue::err(e);
        }
    };
    if start < 1.0 || n < 0.0 {
        return EvalValue::err(ErrorKind::Value);
    }
    let len = u16_len(&s);
    let st = (start.min(MAX_TEXT_LEN as f64 * 2.0) as usize)
        .saturating_sub(1)
        .min(len);
    let en = st
        .saturating_add(n.min(MAX_TEXT_LEN as f64) as usize)
        .min(len);
    let mut out = u16_slice(&s, 0, st);
    out.push_str(&new);
    out.push_str(&u16_slice(&s, en, len));
    EvalValue::text(out)
}

fn find(ev: &mut Ev<'_>, args: &[Expr], search: bool) -> EvalValue {
    let (Some(a), Some(b)) = (args.first(), args.get(1)) else {
        return EvalValue::err(ErrorKind::Value);
    };
    let (needle, hay) = match (ev.text(a), ev.text(b)) {
        (Ok(x), Ok(y)) => (x, y),
        (Err(e), _) | (_, Err(e)) => return EvalValue::err(e),
    };
    let start = match ev.num_or(args, 2, 1.0) {
        Ok(s) => s.trunc(),
        Err(e) => return EvalValue::err(e),
    };
    let bounds = u16_boundaries(&hay);
    let len = bounds.last().map_or(0, |b| b.0);
    if start < 1.0 || start > len as f64 + 1.0 {
        return EvalValue::err(ErrorKind::Value);
    }
    let st = start as usize - 1;
    let wild = search && needle.contains(['*', '?', '~']);
    let needle = if wild {
        search_pattern(&needle)
    } else {
        needle
    };
    if needle.is_empty() {
        return EvalValue::num(start);
    }
    let pat = format!("{needle}*");
    let lower_needle = needle.to_lowercase();
    // Candidate positions are character boundaries at or after the start position (a start
    // inside a surrogate pair begins at the next character).
    for &(pos, byte) in bounds.iter().filter(|b| b.0 >= st && b.0 < len) {
        let rest = hay.get(byte..).unwrap_or("");
        let hit = if search {
            // Wildcards: match the needle as a prefix pattern at this position.
            if wild {
                wildcard_match(&pat, rest)
            } else {
                rest.to_lowercase().starts_with(&lower_needle)
            }
        } else {
            rest.starts_with(&needle)
        };
        if hit {
            return EvalValue::num(pos as f64 + 1.0);
        }
    }
    EvalValue::err(ErrorKind::Value)
}

/// SEARCH's needle in `wildcard_match` syntax: in SEARCH a tilde escapes whatever follows
/// it (`~d` is `d`) and a trailing lone tilde matches nothing.
fn search_pattern(needle: &str) -> String {
    let mut out = String::with_capacity(needle.len());
    let mut it = needle.chars();
    while let Some(c) = it.next() {
        if c == '~' {
            if let Some(n) = it.next() {
                if matches!(n, '*' | '?' | '~') {
                    out.push('~');
                }
                out.push(n);
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn text_fn(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let (Some(a), Some(b)) = (args.first(), args.get(1)) else {
        return EvalValue::err(ErrorKind::Value);
    };
    let v = ev.eval_scalar(a);
    let code = match ev.text(b) {
        Ok(c) => c,
        Err(e) => return EvalValue::err(e),
    };
    let v = match v {
        Value::Error(e) => return EvalValue::err(e),
        Value::Text(s) => match text_to_number(&s) {
            Some(n) => Value::Number(n),
            None => Value::Text(s),
        },
        Value::Empty => Value::Number(0.0),
        other => other,
    };
    if let Value::Number(n) = v {
        if code.trim().eq_ignore_ascii_case("general") {
            return EvalValue::text(general_11(n));
        }
        if is_date_format(&code) {
            // Dates outside 1900-01-00 .. 9999-12-31, or date codes mixed with digit
            // placeholders ("mm###"), are #VALUE! here (the grid shows ####).
            if !(0.0..2_958_466.0).contains(&n) || unquoted_has_any(&code, &['#', '?']) {
                return EvalValue::err(ErrorKind::Value);
            }
        }
    }
    EvalValue::text(format_value(&v, &code).text)
}

/// Whether `code` contains any of `chars` outside quotes, brackets and `\` escapes.
fn unquoted_has_any(code: &str, chars: &[char]) -> bool {
    let (mut in_q, mut in_b, mut esc) = (false, false, false);
    for c in code.chars() {
        if esc {
            esc = false;
            continue;
        }
        match c {
            '\\' if !in_q => esc = true,
            '"' => in_q = !in_q,
            '[' if !in_q => in_b = true,
            ']' if !in_q => in_b = false,
            c if !in_q && !in_b && chars.contains(&c) => return true,
            _ => {}
        }
    }
    false
}

/// "General" as `TEXT` renders it: at most 11 characters (sign excluded), switching to
/// scientific notation when that represents the number more accurately.
pub fn general_11(n: f64) -> String {
    const WIDTH: usize = 11;
    if n == 0.0 || !n.is_finite() {
        return "0".into();
    }
    let sign = if n < 0.0 { "-" } else { "" };
    let a = n.abs();
    let trim = |s: String| -> String {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s
        }
    };
    // Scientific: mantissa with as many decimals as fit next to the exponent.
    let sci = {
        let e10 = a.log10().floor() as i32;
        let mut e = e10;
        let mut mant = a / 10f64.powi(e);
        let exp_len = |e: i32| 2 + e.unsigned_abs().to_string().len().max(2);
        let decs = WIDTH.saturating_sub(2 + exp_len(e));
        let mut m = format!("{mant:.decs$}");
        if m.parse::<f64>().unwrap_or(0.0) >= 10.0 {
            e += 1;
            mant = a / 10f64.powi(e);
            m = format!("{mant:.decs$}");
        }
        let es = if e < 0 { "-" } else { "+" };
        format!("{}E{es}{:02}", trim(m), e.abs())
    };
    let int_digits = if a < 1.0 {
        1
    } else {
        a.log10().floor() as usize + 1
    };
    if int_digits > WIDTH {
        return format!("{sign}{sci}");
    }
    let decs = WIDTH.saturating_sub(int_digits + 1);
    let dec = trim(format!("{a:.decs$}"));
    if dec.split('.').next().is_some_and(|i| i.len() > WIDTH) {
        return format!("{sign}{sci}");
    }
    let err = |s: &str| s.parse::<f64>().map_or(f64::INFINITY, |x| (x - a).abs());
    if dec != "0" && err(&dec) <= err(&sci) {
        format!("{sign}{dec}")
    } else {
        format!("{sign}{sci}")
    }
}

fn fixed(ev: &mut Ev<'_>, args: &[Expr], dollar: bool) -> EvalValue {
    let Some(a) = args.first() else {
        return EvalValue::err(ErrorKind::Value);
    };
    let x = match ev.num(a) {
        Ok(x) => x,
        Err(e) => return EvalValue::err(e),
    };
    let d = match ev.num_or(args, 1, 2.0) {
        Ok(d) => d.trunc().clamp(-127.0, 127.0),
        Err(e) => return EvalValue::err(e),
    };
    let no_commas = if dollar {
        false
    } else {
        match ev.bool_or(args, 2, false) {
            Ok(b) => b,
            Err(e) => return EvalValue::err(e),
        }
    };
    let rounded = super::math::round_to(x, d);
    let decimals = if d > 0.0 {
        "0".repeat(d as usize)
    } else {
        String::new()
    };
    let int = if no_commas { "0" } else { "#,##0" };
    let code = if decimals.is_empty() {
        int.to_string()
    } else {
        format!("{int}.{decimals}")
    };
    let code = if dollar {
        format!("\"$\"{code};(\"$\"{code})")
    } else {
        code
    };
    EvalValue::text(format_value(&Value::Number(rounded), &code).text)
}

fn numbervalue(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let Some(a) = args.first() else {
        return EvalValue::err(ErrorKind::Value);
    };
    let s = match ev.text(a) {
        Ok(s) => s,
        Err(e) => return EvalValue::err(e),
    };
    let dec = match args.get(1) {
        Some(e) => match ev.text(e) {
            Ok(d) => d.chars().next().unwrap_or('.'),
            Err(e) => return EvalValue::err(e),
        },
        None => '.',
    };
    let grp = match args.get(2) {
        Some(e) => match ev.text(e) {
            Ok(g) => g.chars().next().unwrap_or(','),
            Err(e) => return EvalValue::err(e),
        },
        None => ',',
    };
    let cleaned: String = s
        .chars()
        .filter(|c| !c.is_whitespace() && *c != grp)
        .map(|c| if c == dec { '.' } else { c })
        .collect();
    if cleaned.is_empty() {
        return EvalValue::num(0.0);
    }
    let pct = cleaned.chars().rev().take_while(|c| *c == '%').count();
    let body = cleaned.trim_end_matches('%');
    match body.parse::<f64>() {
        Ok(n) if n.is_finite() => EvalValue::num(n / 100f64.powi(pct as i32)),
        _ => EvalValue::err(ErrorKind::Value),
    }
}

fn before_after(ev: &mut Ev<'_>, args: &[Expr], before: bool) -> EvalValue {
    let (Some(a), Some(b)) = (args.first(), args.get(1)) else {
        return EvalValue::err(ErrorKind::Value);
    };
    let (s, d) = match (ev.text(a), ev.text(b)) {
        (Ok(x), Ok(y)) => (x, y),
        (Err(e), _) | (_, Err(e)) => return EvalValue::err(e),
    };
    // Fractional instances round down (-1.2 is the second match from the end).
    let n = match ev.num_or(args, 2, 1.0) {
        Ok(n) if n.is_finite() && n.abs() < 1e15 => n.floor() as i64,
        Ok(_) => return EvalValue::err(ErrorKind::Value),
        Err(e) => return EvalValue::err(e),
    };
    let ci = match ev.num_or(args, 3, 0.0) {
        Ok(m) => m != 0.0,
        Err(e) => return EvalValue::err(e),
    };
    let match_end = match ev.num_or(args, 4, 0.0) {
        Ok(m) if m == 0.0 || m == 1.0 => m == 1.0,
        Ok(_) => return EvalValue::err(ErrorKind::Value),
        Err(e) => return EvalValue::err(e),
    };
    let len = u16_len(&s);
    let instance_given = !matches!(args.get(2), None | Some(Expr::Missing));
    if n == 0
        || n.unsigned_abs() as usize > len.max(1)
            && !(d.is_empty() && n.unsigned_abs() as usize == len + 1)
        || instance_given && u16_len(&d) > len
    {
        return EvalValue::err(ErrorKind::Value);
    }
    let (hay, needle) = if ci {
        (s.to_lowercase(), d.to_lowercase())
    } else {
        (s.clone(), d.clone())
    };
    // Matches as (byte offset, byte length). With match_end the end of the text (counting
    // forwards) or its start (counting backwards) acts as one more delimiter.
    let mut idxs: Vec<(usize, usize)> = if needle.is_empty() {
        (0..=hay.len())
            .filter(|i| hay.is_char_boundary(*i))
            .map(|i| (i, 0))
            .collect()
    } else {
        hay.match_indices(&needle)
            .map(|(i, m)| (i, m.len()))
            .collect()
    };
    if match_end && !needle.is_empty() {
        if n > 0 {
            idxs.push((hay.len(), 0));
        } else {
            idxs.insert(0, (0, 0));
        }
    }
    let pick = if n > 0 {
        idxs.get((n - 1) as usize)
    } else {
        idxs.len()
            .checked_sub(n.unsigned_abs() as usize)
            .and_then(|i| idxs.get(i))
    };
    match pick {
        // Offsets index the (possibly lower-cased) haystack; fall back to it when case
        // folding changed byte lengths.
        Some(&(i, m)) => {
            let src = if hay.len() == s.len() { &s } else { &hay };
            if before {
                EvalValue::text(src.get(..i).unwrap_or(""))
            } else {
                EvalValue::text(src.get(i + m..).unwrap_or(""))
            }
        }
        None => match args.get(5) {
            Some(Expr::Missing) | None => EvalValue::err(ErrorKind::NA),
            Some(e) => match ev.eval_scalar(e) {
                Value::Empty => EvalValue::text(""),
                v => EvalValue::Scalar(v),
            },
        },
    }
}

pub fn register(r: &mut Registry) {
    r.add("LEN", 1, 1, |ev, a| {
        let Some(x) = a.first() else {
            return EvalValue::err(ErrorKind::Value);
        };
        let v = ev.eval(x);
        lift1(ev, v, |x| match to_text(x) {
            Ok(s) => Value::Number(u16_len(&s) as f64),
            Err(e) => Value::Error(e),
        })
    });
    r.add("LEFT", 1, 2, |ev, a| left_right(ev, a, true));
    r.add("RIGHT", 1, 2, |ev, a| left_right(ev, a, false));
    r.add("MID", 3, 3, mid);
    r.add("UPPER", 1, 1, |ev, a| s1(ev, a, str::to_uppercase));
    r.add("LOWER", 1, 1, |ev, a| s1(ev, a, str::to_lowercase));
    r.add("PROPER", 1, 1, |ev, a| s1(ev, a, proper));
    r.add("TRIM", 1, 1, |ev, a| s1(ev, a, trim));
    r.add("CLEAN", 1, 1, |ev, a| {
        s1(ev, a, |s| s.chars().filter(|c| (*c as u32) >= 32).collect())
    });
    r.add("CONCAT", 1, 255, concat);
    r.add("CONCATENATE", 1, 255, concatenate);
    r.add("TEXTJOIN", 3, 255, textjoin);
    r.add("REPT", 2, 2, rept);
    r.add("SUBSTITUTE", 3, 4, substitute);
    r.add("REPLACE", 4, 4, replace);
    r.add("FIND", 2, 3, |ev, a| find(ev, a, false));
    r.add("SEARCH", 2, 3, |ev, a| find(ev, a, true));
    r.add("EXACT", 2, 2, |ev, a| {
        match (a.first().map(|x| ev.text(x)), a.get(1).map(|x| ev.text(x))) {
            (Some(Ok(x)), Some(Ok(y))) => EvalValue::bool(x == y),
            (Some(Err(e)), _) | (_, Some(Err(e))) => EvalValue::err(e),
            _ => EvalValue::err(ErrorKind::Value),
        }
    });
    r.add("CHAR", 1, 1, |ev, a| match a.first().map(|x| ev.num(x)) {
        Some(Ok(n)) if (1.0..256.0).contains(&n.trunc()) => {
            // Code page 1252 for 128..159; Latin-1 elsewhere.
            const CP1252: [u32; 32] = [
                0x20AC, 0x81, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030,
                0x0160, 0x2039, 0x0152, 0x8D, 0x017D, 0x8F, 0x90, 0x2018, 0x2019, 0x201C, 0x201D,
                0x2022, 0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x9D, 0x017E,
                0x0178,
            ];
            let n = n.trunc() as u32;
            let cp = if (128..160).contains(&n) {
                CP1252.get((n - 128) as usize).copied().unwrap_or(n)
            } else {
                n
            };
            EvalValue::text(char::from_u32(cp).map(String::from).unwrap_or_default())
        }
        Some(Ok(_)) => EvalValue::err(ErrorKind::Value),
        Some(Err(e)) => EvalValue::err(e),
        None => EvalValue::err(ErrorKind::Value),
    });
    r.add("CODE", 1, 1, |ev, a| match a.first().map(|x| ev.text(x)) {
        Some(Ok(s)) => match s.chars().next() {
            Some(c) => EvalValue::num(if (c as u32) < 256 {
                c as u32 as f64
            } else {
                63.0
            }),
            None => EvalValue::err(ErrorKind::Value),
        },
        Some(Err(e)) => EvalValue::err(e),
        None => EvalValue::err(ErrorKind::Value),
    });
    r.add("UNICHAR", 1, 1, |ev, a| {
        match a.first().map(|x| ev.num(x)) {
            Some(Ok(n)) => match char::from_u32(n.trunc() as u32).filter(|_| n >= 1.0) {
                Some(c) => EvalValue::text(String::from(c)),
                None => EvalValue::err(ErrorKind::Value),
            },
            Some(Err(e)) => EvalValue::err(e),
            None => EvalValue::err(ErrorKind::Value),
        }
    });
    r.add("UNICODE", 1, 1, |ev, a| {
        match a.first().map(|x| ev.text(x)) {
            Some(Ok(s)) => match s.chars().next() {
                Some(c) => EvalValue::num(c as u32 as f64),
                None => EvalValue::err(ErrorKind::Value),
            },
            Some(Err(e)) => EvalValue::err(e),
            None => EvalValue::err(ErrorKind::Value),
        }
    });
    r.add("VALUE", 1, 1, |ev, a| {
        match a.first().map(|x| ev.eval_scalar(x)) {
            Some(Value::Number(n)) => EvalValue::num(n),
            Some(Value::Empty) => EvalValue::num(0.0),
            Some(Value::Text(s)) => match text_to_number(&s) {
                Some(n) => EvalValue::num(n),
                None => EvalValue::err(ErrorKind::Value),
            },
            Some(Value::Error(e)) => EvalValue::err(e),
            _ => EvalValue::err(ErrorKind::Value),
        }
    });
    r.add("T", 1, 1, |ev, a| {
        match a.first().map(|x| ev.eval_scalar(x)) {
            Some(Value::Text(s)) => EvalValue::Scalar(Value::Text(s)),
            Some(Value::Error(e)) => EvalValue::err(e),
            _ => EvalValue::text(""),
        }
    });
    r.add("TEXT", 2, 2, text_fn);
    r.add("FIXED", 1, 3, |ev, a| fixed(ev, a, false));
    r.add("DOLLAR", 1, 2, |ev, a| fixed(ev, a, true));
    r.add("NUMBERVALUE", 1, 3, numbervalue);
    r.add("TEXTBEFORE", 2, 6, |ev, a| before_after(ev, a, true));
    r.add("TEXTAFTER", 2, 6, |ev, a| before_after(ev, a, false));
}

#[cfg(test)]
mod tests {
    use super::{u16_boundaries, u16_len, u16_slice};

    #[test]
    fn utf16_helpers() {
        assert_eq!(u16_len("héllo 😀"), 8);
        assert_eq!(u16_slice("a😀b", 0, 4), "a😀b");
        assert_eq!(u16_slice("a😀b", 1, 2), "\u{FFFD}");
        assert_eq!(u16_slice("a😀b", 2, 4), "\u{FFFD}b");
        assert_eq!(u16_slice("a😀b", 3, 1), "");
        assert_eq!(u16_slice("ab", 5, usize::MAX), "");
        assert_eq!(u16_boundaries("a😀"), vec![(0, 0), (1, 1), (3, 5)]);
    }
}
