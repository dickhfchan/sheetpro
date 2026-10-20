//! Shared formulas: one parse per *relative form*.
//!
//! A filled-down column holds `C2*D2`, `C3*D3`, ...: the same formula up to the cell's
//! position. [`rel_key`] maps formula text plus the cell's position to a *relative key*: the
//! text with every A1 cell reference rewritten relative to the cell (R1C1-like), so all of
//! those cells get one key. Recalc parses one tree per key ([`crate::AstCache`]) and
//! evaluates it at each cell with the position offset ([`crate::eval::Ev::with_origin`]);
//! bulk text rewrites (load, save, sort, insert/delete) run once per key and instantiate the
//! result for each cell ([`FormulaMemo`]).
//!
//! Exactness: the key scanner mirrors the parser's lexer for the formulas it accepts and
//! gives up — a *literal* key, the text itself — on anything unusual (non-ASCII, structured
//! references, control characters, lower-case references, very long text). Two texts with the
//! same relative key differ only inside cell-reference tokens, by the same offset in every
//! relative part, so the parser builds the same tree for both up to that offset. The first
//! time a key is used its tree is still checked against the key ([`verify`]: the same
//! references in the same order); on a mismatch the key is not shared.

use crate::ast::{CellPart, Expr, RefKind};
use crate::deps::FxMap;
use crate::parser::parse;
use sp_core::addr::parse_a1_parts;
use sp_core::{CellAddr, ErrorKind, MAX_COLS, MAX_ROWS, letters_to_col};
use std::fmt::Write as _;

/// Starts a literal key (the formula text follows verbatim).
const LITERAL: char = '\u{0}';
/// Brackets an encoded cell reference in a relative key.
const REF_START: char = '\u{1}';
const REF_END: char = '\u{2}';
/// Longer formulas get literal keys (shifting a reference changes the text length, and the
/// parser caps formula length).
const MAX_KEY_SRC: usize = 4096;

/// What [`rel_key`] produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyKind {
    /// Cell references are written relative to the position. `rel_span`: the text also has a
    /// whole-row/column reference with a relative part (`A:A`, `1:3`), kept as written.
    Relative { rel_span: bool },
    /// The key is the text itself (not shareable across positions).
    Literal,
}

/// Write the relative key of formula `src` (no leading `=`) at `at` into `out`.
pub fn rel_key(src: &str, at: CellAddr, out: &mut String) -> KeyKind {
    out.clear();
    match scan(src, at, out) {
        Some(rel_span) => KeyKind::Relative { rel_span },
        None => {
            out.clear();
            out.push(LITERAL);
            out.push_str(src);
            KeyKind::Literal
        }
    }
}

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c == b'\\' || c == b'$'
}

fn is_ident_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'$' | b'\\' | b'?')
}

fn is_space(c: u8) -> bool {
    char::from(c).is_whitespace()
}

/// Mirror of the lexer's whole-row/column span (`A:C`, `$1:$3`) at `i`: its end and whether
/// a part is relative.
fn try_span(b: &[u8], i: usize) -> Option<(usize, bool)> {
    let take = |mut j: usize, letters: bool| -> Option<(usize, usize)> {
        let s = j;
        if b.get(j) == Some(&b'$') {
            j += 1;
        }
        let body = j;
        while b.get(j).is_some_and(|c| {
            if letters {
                c.is_ascii_alphabetic()
            } else {
                c.is_ascii_digit()
            }
        }) {
            j += 1;
        }
        (j > body).then_some((s, j))
    };
    for letters in [true, false] {
        let Some((a0, a1)) = take(i, letters) else {
            continue;
        };
        if b.get(a1) != Some(&b':') {
            continue;
        }
        let Some((b0, b1)) = take(a1 + 1, letters) else {
            continue;
        };
        if b.get(b1)
            .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'(' | b'.'))
        {
            continue;
        }
        let part = |s: usize, e: usize| -> Option<&str> {
            std::str::from_utf8(b.get(s..e)?)
                .ok()
                .map(|t| t.trim_start_matches('$'))
        };
        let (Some(pa), Some(pb)) = (part(a0, a1), part(b0, b1)) else {
            continue;
        };
        let ok = if letters {
            letters_to_col(pa).is_some() && letters_to_col(pb).is_some()
        } else {
            let row = |s: &str| s.parse::<u32>().ok().filter(|n| *n >= 1 && *n <= MAX_ROWS);
            row(pa).is_some() && row(pb).is_some()
        };
        if ok {
            let rel = b.get(a0) != Some(&b'$') || b.get(b0) != Some(&b'$');
            return Some((b1, rel));
        }
    }
    None
}

fn push_ref(out: &mut String, at: CellAddr, p: (u32, bool, u32, bool)) {
    let (col, col_abs, row, row_abs) = p;
    out.push(REF_START);
    if col_abs {
        let _ = write!(out, "C{col}");
    } else {
        let _ = write!(out, "c{}", i64::from(col) - i64::from(at.col));
    }
    if row_abs {
        let _ = write!(out, "R{row}");
    } else {
        let _ = write!(out, "r{}", i64::from(row) - i64::from(at.row));
    }
    out.push(REF_END);
}

/// An identifier the parser reads as a cell reference: `None` if it is not one, an error
/// (give up on the key) for lower-case references.
fn cell_ref(text: &str) -> Result<Option<(u32, bool, u32, bool)>, ()> {
    match parse_a1_parts(text) {
        Some(_) if text.bytes().any(|c| c.is_ascii_lowercase()) => Err(()),
        p => Ok(p),
    }
}

/// The relative key body, or `None` when the text needs a literal key.
fn scan(src: &str, at: CellAddr, out: &mut String) -> Option<bool> {
    let b = src.as_bytes();
    if b.len() > MAX_KEY_SRC || !src.is_ascii() || b.iter().any(|&c| c <= 2) {
        return None;
    }
    let mut rel_span = false;
    let mut i = 0;
    while let Some(&c) = b.get(i) {
        let start = i;
        match c {
            c if is_space(c) => i += 1,
            b'"' => {
                i += 1;
                loop {
                    match b.get(i) {
                        None => return None,
                        Some(b'"') if b.get(i + 1) == Some(&b'"') => i += 2,
                        Some(b'"') => {
                            i += 1;
                            break;
                        }
                        Some(_) => i += 1,
                    }
                }
            }
            b'#' => {
                let rest = src.get(i..)?;
                let n = ErrorKind::ALL
                    .iter()
                    .map(|e| e.as_str())
                    .find(|e| {
                        rest.get(..e.len())
                            .is_some_and(|p| p.eq_ignore_ascii_case(e))
                    })
                    .map_or(1, str::len);
                i += n;
            }
            b'0'..=b'9' | b'.' => {
                if let Some((n, rel)) = try_span(b, i) {
                    rel_span |= rel;
                    i = n;
                } else {
                    while b.get(i).is_some_and(|c| c.is_ascii_digit() || *c == b'.') {
                        i += 1;
                    }
                    if matches!(b.get(i), Some(b'e' | b'E'))
                        && (b.get(i + 1).is_some_and(u8::is_ascii_digit)
                            || (matches!(b.get(i + 1), Some(b'+' | b'-'))
                                && b.get(i + 2).is_some_and(u8::is_ascii_digit)))
                    {
                        i += 2;
                        while b.get(i).is_some_and(u8::is_ascii_digit) {
                            i += 1;
                        }
                    }
                }
            }
            b'\'' => {
                i += 1;
                loop {
                    match b.get(i) {
                        None => return None,
                        Some(b'\'') if b.get(i + 1) == Some(&b'\'') => i += 2,
                        Some(b'\'') => {
                            i += 1;
                            break;
                        }
                        Some(_) => i += 1,
                    }
                }
                if b.get(i) != Some(&b'!') {
                    return None;
                }
                i += 1;
                out.push_str(src.get(start..i)?);
                i = after_sheet(src, i, at, out, &mut rel_span)?;
                continue;
            }
            c if is_ident_start(c) => {
                if let Some((n, rel)) = try_span(b, i) {
                    rel_span |= rel;
                    i = n;
                } else {
                    let mut j = i;
                    while b.get(j).is_some_and(|c| is_ident_char(*c)) {
                        j += 1;
                    }
                    let text = src.get(i..j)?;
                    match b.get(j) {
                        Some(b'!') => {
                            out.push_str(text);
                            out.push('!');
                            i = after_sheet(src, j + 1, at, out, &mut rel_span)?;
                            continue;
                        }
                        Some(b'[') => return None,
                        _ => {}
                    }
                    i = j;
                    // An identifier followed by `(` is a function call.
                    let mut k = j;
                    while b.get(k).is_some_and(|c| is_space(*c)) {
                        k += 1;
                    }
                    if b.get(k) != Some(&b'(')
                        && let Some(p) = cell_ref(text).ok()?
                    {
                        push_ref(out, at, p);
                        continue;
                    }
                }
            }
            b'[' => return None,
            _ => i += 1,
        }
        out.push_str(src.get(start..i)?);
    }
    Some(rel_span)
}

/// Mirror of the lexer after `Sheet!`: a span, `#REF!`, or a (cell) reference.
fn after_sheet(
    src: &str,
    i: usize,
    at: CellAddr,
    out: &mut String,
    rel_span: &mut bool,
) -> Option<usize> {
    let b = src.as_bytes();
    if let Some((n, rel)) = try_span(b, i) {
        *rel_span |= rel;
        out.push_str(src.get(i..n)?);
        return Some(n);
    }
    let rest = src.get(i..)?;
    if b.get(i) == Some(&b'#') {
        // The lexer only takes `Sheet!#REF!` at the very end of the formula.
        return rest.eq_ignore_ascii_case("#REF!").then(|| {
            out.push_str(rest);
            src.len()
        });
    }
    let mut j = i;
    while b.get(j).is_some_and(|c| is_ident_char(*c)) {
        j += 1;
    }
    let text = src.get(i..j)?;
    if text.is_empty() {
        return None;
    }
    match cell_ref(text).ok()? {
        Some(p) => push_ref(out, at, p),
        None => out.push_str(text),
    }
    Some(j)
}

/// One encoded reference: (col, col_abs, row, row_abs) at `at`, if on the sheet.
fn decode(enc: &str, at: CellAddr) -> Option<CellPart> {
    let b = enc.as_bytes();
    let r = enc.find(['r', 'R'])?;
    let num = |s: &str| s.parse::<i64>().ok();
    let col_abs = *b.first()? == b'C';
    let row_abs = *b.get(r)? == b'R';
    let c = num(enc.get(1..r)?)?;
    let rr = num(enc.get(r + 1..)?)?;
    let col = if col_abs { c } else { i64::from(at.col) + c };
    let row = if row_abs { rr } else { i64::from(at.row) + rr };
    ((0..i64::from(MAX_COLS)).contains(&col) && (0..i64::from(MAX_ROWS)).contains(&row)).then_some(
        CellPart {
            row: row as u32,
            col: col as u32,
            row_abs,
            col_abs,
        },
    )
}

/// Formula text of `key` at `at` into `out`; false if a reference falls off the sheet.
pub fn instantiate(key: &str, at: CellAddr, out: &mut String) -> bool {
    out.clear();
    if let Some(lit) = key.strip_prefix(LITERAL) {
        out.push_str(lit);
        return true;
    }
    let mut rest = key;
    while let Some(s) = rest.find(REF_START) {
        out.push_str(rest.get(..s).unwrap_or(""));
        let tail = rest.get(s + 1..).unwrap_or("");
        let Some(e) = tail.find(REF_END) else {
            return false;
        };
        let Some(p) = tail.get(..e).and_then(|enc| decode(enc, at)) else {
            return false;
        };
        out.push_str(&crate::print::cell_part(&p));
        rest = tail.get(e + 1..).unwrap_or("");
    }
    out.push_str(rest);
    true
}

/// Cell references of a relative `key` at `at`, in text order (`None` if one is off the
/// sheet or the key is literal).
fn key_refs(key: &str, at: CellAddr) -> Option<Vec<CellPart>> {
    if key.starts_with(LITERAL) {
        return None;
    }
    let mut out = Vec::new();
    let mut rest = key;
    while let Some(s) = rest.find(REF_START) {
        let tail = rest.get(s + 1..)?;
        let e = tail.find(REF_END)?;
        out.push(decode(tail.get(..e)?, at)?);
        rest = tail.get(e + 1..)?;
    }
    Some(out)
}

/// Number of cell references in a relative key.
fn ref_count(key: &str) -> usize {
    key.matches(REF_START).count()
}

/// How a parsed formula relates to its relative key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Verified {
    /// The tree holds exactly the key's references in order: it can be shared, evaluated
    /// at other positions with the offset.
    pub shareable: bool,
    /// Also printing it at another position gives that position's text (no range with one
    /// end absolute and the other relative, which the parser may reorder).
    pub text_safe: bool,
}

/// Check the tree parsed from a formula against its relative key at `at`.
pub fn verify(expr: &Expr, key: &str, at: CellAddr) -> Verified {
    let mut parts: Vec<CellPart> = Vec::new();
    let mut text_safe = true;
    expr.walk(&mut |e| {
        if let Expr::Ref(r) = e {
            match &r.kind {
                RefKind::Cell(c) => parts.push(*c),
                RefKind::Area(a, b) => {
                    text_safe &= a.row_abs == b.row_abs && a.col_abs == b.col_abs;
                    parts.push(*a);
                    parts.push(*b);
                }
                RefKind::Cols { .. } | RefKind::Rows { .. } => {}
            }
        }
    });
    let shareable = key_refs(key, at).is_some_and(|k| k == parts);
    Verified {
        shareable,
        text_safe: shareable && text_safe,
    }
}

/// Learned outcome of a rewrite for one relative key.
enum Memo {
    /// Seen, not learned yet (`n` failed attempts): learning starts at the second sighting,
    /// so unique formulas pay no verification.
    Seen(u8),
    Unchanged,
    /// Output as a relative key at the output position.
    Text(String),
    /// Not memoizable.
    Off,
}

/// Entries kept per memo (bounds memory on workbooks of unique formulas).
const MAX_MEMO: usize = 1 << 18;
/// Attempts to learn a key whose result could not be generalized before giving up on it.
const MAX_LEARN_TRIES: u8 = 4;

/// Memoizes a bulk formula-text rewrite by relative key: computed once per filled-down group
/// and instantiated for each cell.
#[derive(Default)]
pub struct FormulaMemo {
    map: FxMap<String, Memo>,
    key: String,
    tmp: String,
}

impl FormulaMemo {
    pub fn new() -> Self {
        Self::default()
    }

    /// `f(src)` for the formula `src` at `at`: the text of the rewritten formula, now at
    /// `out_at` (`None` = unchanged). `f` must commute with moving the formula: for a formula
    /// moved by an offset (relative references shifted by it), `f` gives the same result
    /// moved by the same offset. `tag` names anything else `f`'s result depends on (results
    /// are only shared between calls with equal tags).
    pub fn rewrite(
        &mut self,
        src: &str,
        at: CellAddr,
        out_at: CellAddr,
        tag: &str,
        f: impl FnOnce(&str) -> Option<String>,
    ) -> Option<String> {
        self.run(
            src,
            at,
            out_at,
            |_, _, out| {
                out.push_str(tag);
                true
            },
            f,
        )
    }

    /// [`FormulaMemo::rewrite`] with a tag computed from the formula's cell references at
    /// `at` (in text order): `tag` writes it and returns false when the result must not be
    /// shared (computed directly).
    pub fn rewrite_with(
        &mut self,
        src: &str,
        at: CellAddr,
        out_at: CellAddr,
        tag: impl FnOnce(&[CellPart], &mut String) -> bool,
        f: impl FnOnce(&str) -> Option<String>,
    ) -> Option<String> {
        self.run(
            src,
            at,
            out_at,
            |key, at, out| key_refs(key, at).is_some_and(|parts| tag(&parts, out)),
            f,
        )
    }

    fn run(
        &mut self,
        src: &str,
        at: CellAddr,
        out_at: CellAddr,
        tag: impl FnOnce(&str, CellAddr, &mut String) -> bool,
        f: impl FnOnce(&str) -> Option<String>,
    ) -> Option<String> {
        if rel_key(src, at, &mut self.key) != (KeyKind::Relative { rel_span: false }) {
            return f(src);
        }
        let refs = ref_count(&self.key);
        let body_len = self.key.len();
        self.tmp.clear();
        if !tag(&self.key, at, &mut self.tmp) {
            return f(src);
        }
        self.key.push(LITERAL);
        self.key.push_str(&self.tmp);
        match self.map.get(&self.key) {
            Some(Memo::Unchanged) => return None,
            Some(Memo::Text(k)) => {
                let mut s = String::new();
                return if instantiate(k, out_at, &mut s) {
                    Some(s)
                } else {
                    f(src)
                };
            }
            Some(Memo::Off) => return f(src),
            Some(Memo::Seen(_)) => {}
            None => {
                if self.map.len() < MAX_MEMO {
                    self.map.insert(self.key.clone(), Memo::Seen(0));
                }
                return f(src);
            }
        }
        let r = f(src);
        let learned = self.learn(src, at, out_at, refs, body_len, r.as_deref());
        if let Some(slot) = self.map.get_mut(&self.key) {
            *slot = match (learned, &*slot) {
                (Some(m), _) => m,
                (None, Memo::Seen(n)) if *n + 1 < MAX_LEARN_TRIES => Memo::Seen(n + 1),
                (None, _) => Memo::Off,
            };
        }
        r
    }

    /// What to remember for the current key given one computed result (`None`: nothing yet,
    /// e.g. a reference pushed off the sheet at this position only).
    fn learn(
        &mut self,
        src: &str,
        at: CellAddr,
        out_at: CellAddr,
        refs: usize,
        body_len: usize,
        out: Option<&str>,
    ) -> Option<Memo> {
        let ok_in = parse(src)
            .is_ok_and(|e| verify(&e, self.key.get(..body_len).unwrap_or(""), at).text_safe);
        if !ok_in {
            return Some(Memo::Off);
        }
        let Some(out) = out else {
            return Some(Memo::Unchanged);
        };
        if rel_key(out, out_at, &mut self.tmp) != (KeyKind::Relative { rel_span: false }) {
            return Some(Memo::Off);
        }
        if ref_count(&self.tmp) != refs {
            return None;
        }
        let mut back = String::new();
        let ok_out = instantiate(&self.tmp, out_at, &mut back)
            && back == out
            && parse(out).is_ok_and(|e| verify(&e, &self.tmp, out_at).text_safe);
        Some(if ok_out {
            Memo::Text(self.tmp.clone())
        } else {
            Memo::Off
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shift::shift_for_copy;

    fn a(s: &str) -> CellAddr {
        CellAddr::parse(s).unwrap()
    }

    fn key(src: &str, at: &str) -> (String, KeyKind) {
        let mut k = String::new();
        let kind = rel_key(src, a(at), &mut k);
        (k, kind)
    }

    #[test]
    fn filled_down_formulas_share_a_key() {
        let (k1, kind) = key("C2*D2+$A$1+SUM(B$1:B2)", "E2");
        assert_eq!(kind, KeyKind::Relative { rel_span: false });
        let (k2, _) = key("C3*D3+$A$1+SUM(B$1:B3)", "E3");
        assert_eq!(k1, k2);
        let (k3, _) = key("C3*D3+$A$1+SUM(B$1:B3)", "E2");
        assert_ne!(k1, k3);
        let mut s = String::new();
        assert!(instantiate(&k1, a("E100"), &mut s));
        assert_eq!(s, "C100*D100+$A$1+SUM(B$1:B100)");
        // Off the sheet.
        assert!(!instantiate(&k1, a("A1"), &mut s));
    }

    #[test]
    fn non_references_stay_literal() {
        for (src, at) in [
            ("LOG10(A1)", "B1"),
            ("\"A1\"&A1", "B1"),
            ("Sheet1!A1+'My A1'!B2", "C3"),
            ("A1:B2", "C3"),
            ("A:A+1:1+$B:$B", "C3"),
            ("#REF!+A1", "B2"),
            ("IF(TRUE,A1,B1)", "C1"),
        ] {
            let (k, kind) = key(src, at);
            assert!(matches!(kind, KeyKind::Relative { .. }), "{src}");
            let mut s = String::new();
            assert!(instantiate(&k, a(at), &mut s));
            assert_eq!(s, src, "round trip of {src}");
            let e = parse(src).unwrap();
            assert!(verify(&e, &k, a(at)).shareable, "{src}");
        }
        assert_eq!(key("A:A", "B1").1, KeyKind::Relative { rel_span: true });
        assert_eq!(key("$A:$A", "B1").1, KeyKind::Relative { rel_span: false });
        for src in ["a1+1", "T[Col]", "\u{e9}+A1", "A1\u{1}"] {
            assert_eq!(key(src, "B1").1, KeyKind::Literal, "{src}");
        }
    }

    /// Two positions of one relative key parse to the same tree up to the offset.
    #[test]
    fn shared_trees_match_shifted_parses() {
        for src in [
            "A1*2+1",
            "IF(E1>500,\"hi\",\"lo\")",
            "SUM($A$1:$A$9)/COUNT(A1:B9)",
            "Sheet2!B3+'x y'!C$4",
            "-A1^2%+B1#",
            "A1 B1:C2",
            "1E5+A1",
        ] {
            let at = a("D5");
            let (k, _) = key(src, at.to_a1().as_str());
            let e = parse(src).unwrap();
            let v = verify(&e, &k, at);
            assert!(v.shareable, "{src}");
            for to in ["D6", "E5", "Z99", "D4"] {
                let to = a(to);
                let mut text = String::new();
                if !instantiate(&k, to, &mut text) {
                    continue;
                }
                let mut shifted = e.clone();
                shift_for_copy(
                    &mut shifted,
                    i64::from(to.row) - i64::from(at.row),
                    i64::from(to.col) - i64::from(at.col),
                );
                assert_eq!(parse(&text).unwrap(), shifted, "{src} at {}", to.to_a1());
                let (k2, _) = key(&text, &to.to_a1());
                assert_eq!(k2, k);
            }
        }
    }

    #[test]
    fn mixed_ranges_are_not_text_safe() {
        let (k, _) = key("SUM($A$1:A5)", "B5");
        let v = verify(&parse("SUM($A$1:A5)").unwrap(), &k, a("B5"));
        assert!(v.shareable && !v.text_safe);
        // Reversed ranges are reordered by the parser: not shareable.
        let (k, _) = key("SUM(A5:A1)", "B5");
        assert!(!verify(&parse("SUM(A5:A1)").unwrap(), &k, a("B5")).shareable);
    }

    #[test]
    fn memo_rewrites_once_per_group() {
        let mut m = FormulaMemo::new();
        let calls = std::cell::Cell::new(0);
        let up = |src: &str| {
            calls.set(calls.get() + 1);
            let mut e = parse(src).ok()?;
            shift_for_copy(&mut e, -1, 0);
            Some(crate::to_formula(&e))
        };
        for r in 2..200u32 {
            let src = format!("A{r}*2+$B$1+IF(C{r}>1,1,0)");
            let at = CellAddr::new(r - 1, 3);
            let out = m.rewrite(&src, at, CellAddr::new(r - 2, 3), "up", up);
            let want = format!("A{}*2+$B$1+IF(C{}>1,1,0)", r - 1, r - 1);
            assert_eq!(out.as_deref(), Some(want.as_str()));
        }
        assert_eq!(calls.get(), 2);
        // Moved so that a reference falls off the sheet: computed, not instantiated.
        let mut mv = |src: &str, at: &str, to: &str| {
            let (at, to) = (a(at), a(to));
            let dr = i64::from(to.row) - i64::from(at.row);
            m.rewrite(src, at, to, "mv", |s| {
                let mut e = parse(s).ok()?;
                shift_for_copy(&mut e, dr, 0);
                Some(crate::to_formula(&e))
            })
        };
        assert_eq!(mv("B4+1", "C5", "C9").as_deref(), Some("B8+1"));
        assert_eq!(mv("B5+1", "C6", "C9").as_deref(), Some("B8+1"));
        assert_eq!(mv("B6+1", "C7", "C1").as_deref(), Some("#REF!+1"));
        assert_eq!(mv("B6+1", "C7", "C2").as_deref(), Some("B1+1"));
    }
}
