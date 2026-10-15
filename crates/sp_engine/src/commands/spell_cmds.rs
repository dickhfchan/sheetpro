//! Spelling (`review.spelling`): check text constants in the selection (or the whole sheet
//! when one cell is selected) against a bundled English word list, with suggestions by
//! edit distance; ignore once / ignore all, add to the user dictionary (in settings),
//! change / change all.
//!
//! Word list: SCOWL size 60 (American + common English words, proper names, contractions,
//! abbreviations), `data/spelling/en_US.txt`, license beside it.

use crate::params::{cell, range, str_opt};
use crate::{CmdError, CmdResult, CommandSpec, Json, Session};
use serde_json::json;
use sp_core::{CellAddr, CellRange, Value};
use sp_model::Content;
use std::collections::HashSet;
use std::sync::OnceLock;

const WORDS: &str = include_str!("../../data/spelling/en_US.txt");

/// Caps: findings per check, cells scanned per check, suggestions per word, word length.
pub const MAX_FINDINGS: usize = 1_000;
const MAX_SUGGESTED: usize = 200;
pub const MAX_SUGGESTIONS: usize = 5;
pub const MAX_WORD_CHARS: usize = 64;
pub const MAX_USER_WORDS: usize = 10_000;
const MAX_SCAN_CELLS: u64 = 1_000_000;

/// Session-only spelling state (ignore lists; the user dictionary falls back here when
/// there is no app data directory to persist it in).
#[derive(Debug, Clone, Default)]
pub struct SpellState {
    /// (sheet, cell, word) occurrences skipped by "Ignore Once".
    pub ignore_once: HashSet<(usize, CellAddr, String)>,
    /// Lowercased words skipped by "Ignore All" for the rest of the session.
    pub ignore_all: HashSet<String>,
    /// User dictionary words added while no settings file was available.
    pub user_words: Vec<String>,
}

struct Dict {
    exact: HashSet<&'static str>,
    lower: HashSet<String>,
    /// (lowercased word, commonness level 0 = most common) by character count
    /// (index = length), for suggestions.
    by_len: Vec<Vec<(String, u8)>>,
}

fn dict() -> &'static Dict {
    static D: OnceLock<Dict> = OnceLock::new();
    D.get_or_init(|| {
        let mut exact = HashSet::new();
        let mut lower = HashSet::new();
        let mut by_len: Vec<Vec<(String, u8)>> = vec![Vec::new(); MAX_WORD_CHARS + 1];
        // The file lists words by SCOWL size level; `#NN` lines start each level.
        let mut level = 0u8;
        for w in WORDS.lines().map(str::trim).filter(|w| !w.is_empty()) {
            if w.starts_with('#') {
                level = level.saturating_add(u8::from(!exact.is_empty()));
                continue;
            }
            exact.insert(w);
            let l = w.to_lowercase();
            if lower.insert(l.clone())
                && let Some(b) = by_len.get_mut(l.chars().count())
            {
                b.push((l, level));
            }
        }
        Dict {
            exact,
            lower,
            by_len,
        }
    })
}

/// Number of entries in the bundled word list.
pub fn word_count() -> usize {
    dict().exact.len()
}

/// Whether the bundled list (plus `user`) accepts `word`.
pub fn is_known(word: &str, user: &[String]) -> bool {
    let d = dict();
    if d.exact.contains(word) {
        return true;
    }
    let lower = word.to_lowercase();
    if user.iter().any(|u| u.to_lowercase() == lower) {
        return true;
    }
    // Capitalised or upper-case forms of listed words ("The", "THE", "PARIS") are fine; a
    // lower-case form of a proper name ("paris") is not.
    if word.chars().next().is_some_and(char::is_uppercase) {
        if d.exact.contains(lower.as_str()) {
            return true;
        }
        let all_upper = word.chars().all(|c| !c.is_lowercase());
        if all_upper && d.lower.contains(&lower) {
            return true;
        }
    }
    // Possessives: "John's", "teacher's".
    for suffix in ["'s", "\u{2019}s"] {
        if let Some(base) = word.strip_suffix(suffix)
            && !base.is_empty()
            && is_known(base, user)
        {
            return true;
        }
    }
    false
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

/// Words of `text` worth checking: (byte offset, word). Letters with internal apostrophes;
/// tokens touching digits, URLs, e-mail addresses and paths are skipped.
pub fn words(text: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    // Split into whitespace-separated chunks first.
    let mut chunks = Vec::new();
    let mut start: Option<usize> = None;
    for (i, c) in text.char_indices() {
        match (c.is_whitespace(), start) {
            (true, Some(st)) => {
                chunks.push((st, i));
                start = None;
            }
            (false, None) => start = Some(i),
            _ => {}
        }
    }
    if let Some(st) = start {
        chunks.push((st, text.len()));
    }
    for (a, b) in chunks {
        let Some(chunk) = text.get(a..b) else {
            continue;
        };
        if chunk.contains("://")
            || chunk.contains('@')
            || chunk.starts_with("www.")
            || chunk.contains('\\')
            || chunk.contains('/') && chunk.contains('.')
        {
            continue;
        }
        // Alphanumeric runs (with internal apostrophes) inside the chunk.
        let mut run: Option<usize> = None;
        let chars: Vec<(usize, char)> = chunk.char_indices().collect();
        let mut k = 0usize;
        while k <= chars.len() {
            let cur = chars.get(k).copied();
            let is_word_char = |c: char| c.is_alphanumeric();
            let apostrophe = |c: char| c == '\'' || c == '\u{2019}';
            match cur {
                Some((i, c)) if is_word_char(c) => {
                    if run.is_none() {
                        run = Some(i);
                    }
                }
                Some((_, c))
                    if apostrophe(c)
                        && run.is_some()
                        && chars.get(k + 1).is_some_and(|(_, n)| n.is_alphabetic()) => {}
                _ => {
                    if let Some(s) = run.take() {
                        let e = cur.map_or(chunk.len(), |(i, _)| i);
                        if let Some(w) = chunk.get(s..e)
                            && !w.chars().any(|c| c.is_numeric())
                            && w.chars().count() <= MAX_WORD_CHARS
                        {
                            out.push((a + s, w));
                        }
                    }
                }
            }
            k += 1;
        }
    }
    out
}

/// Damerau-Levenshtein (optimal string alignment) distance, or `None` above `max`.
pub fn distance(a: &[char], b: &[char], max: usize) -> Option<usize> {
    if a.len().abs_diff(b.len()) > max {
        return None;
    }
    let w = b.len() + 1;
    let mut prev2 = vec![0usize; w];
    let mut prev: Vec<usize> = (0..w).collect();
    let mut cur = vec![0usize; w];
    for (i, ca) in a.iter().enumerate() {
        if let Some(c0) = cur.first_mut() {
            *c0 = i + 1;
        }
        let mut row_min = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            let del = prev.get(j + 1).copied().unwrap_or(usize::MAX - 1) + 1;
            let ins = cur.get(j).copied().unwrap_or(usize::MAX - 1) + 1;
            let sub = prev.get(j).copied().unwrap_or(usize::MAX - 1) + cost;
            let mut v = del.min(ins).min(sub);
            if i > 0
                && j > 0
                && a.get(i - 1) == Some(cb)
                && b.get(j - 1) == Some(ca)
                && let Some(t) = prev2.get(j - 1)
            {
                v = v.min(t + 1);
            }
            if let Some(slot) = cur.get_mut(j + 1) {
                *slot = v;
            }
            row_min = row_min.min(v);
        }
        if row_min > max {
            return None;
        }
        std::mem::swap(&mut prev2, &mut prev);
        std::mem::swap(&mut prev, &mut cur);
    }
    prev.last().copied().filter(|d| *d <= max)
}

/// Up to [`MAX_SUGGESTIONS`] dictionary words within edit distance 2 of `word`, closest
/// first, in the word's capitalisation.
pub fn suggest(word: &str) -> Vec<String> {
    let lower = word.to_lowercase();
    let target: Vec<char> = lower.chars().collect();
    if target.is_empty() || target.len() > MAX_WORD_CHARS {
        return Vec::new();
    }
    let first = target.first().copied();
    let d = dict();
    let max = if target.len() <= 3 { 1 } else { 2 };
    let mut sorted_target = target.clone();
    sorted_target.sort_unstable();
    // (distance, not a swap of two letters, commonness, other first letter, length
    // difference, word)
    let mut found: Vec<(usize, bool, u8, bool, usize, &str)> = Vec::new();
    let lo = target.len().saturating_sub(max).max(1);
    let hi = (target.len() + max).min(MAX_WORD_CHARS);
    for len in lo..=hi {
        let Some(bucket) = d.by_len.get(len) else {
            continue;
        };
        for (cand, level) in bucket {
            let cc: Vec<char> = cand.chars().collect();
            if let Some(dist) = distance(&target, &cc, max) {
                let swap = dist == 1 && {
                    let mut sc = cc.clone();
                    sc.sort_unstable();
                    sc == sorted_target
                };
                found.push((
                    dist,
                    !swap,
                    *level,
                    cc.first().copied() != first,
                    len.abs_diff(target.len()),
                    cand.as_str(),
                ));
            }
        }
    }
    found.sort();
    let upper = word.chars().count() > 1 && word.chars().all(|c| !c.is_lowercase());
    let title = word.chars().next().is_some_and(char::is_uppercase);
    let mut out: Vec<String> = Vec::new();
    for (.., w) in found {
        // Prefer the list's own spelling for proper names ("paris" → "Paris").
        let listed = if d.exact.contains(w) {
            w.to_string()
        } else {
            capitalize(w)
        };
        let s = if upper {
            w.to_uppercase()
        } else if title {
            capitalize(&listed)
        } else {
            listed
        };
        if !out.contains(&s) {
            out.push(s);
        }
        if out.len() >= MAX_SUGGESTIONS {
            break;
        }
    }
    out
}

const PARAMS: &str = r#"{"action"?: "check|suggest|ignore|ignore_all|add|change|change_all", "range"?: "A1:D20 (default: selection, or the whole sheet when one cell is selected)", "cell"?: "B3 (ignore/change)", "word"?: "teh", "to"?: "the (change/change_all)", "ignore_uppercase"?: true}"#;

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec::new("review.spelling", PARAMS, spelling)]
}

fn word_param(p: &Json, k: &str) -> Result<String, CmdError> {
    let w = str_opt(p, k)
        .map(str::trim)
        .ok_or_else(|| CmdError::Param(format!("missing string '{k}'")))?;
    if w.is_empty() || w.chars().count() > MAX_WORD_CHARS || w.chars().any(char::is_whitespace) {
        return Err(CmdError::Param(format!(
            "'{k}' must be one word of 1-{MAX_WORD_CHARS} characters"
        )));
    }
    Ok(w.to_string())
}

/// The user dictionary (settings, or the session fallback).
pub fn user_dictionary(s: &Session) -> Vec<String> {
    let mut v = crate::settings::current().user_dictionary;
    v.extend(s.spelling.user_words.iter().cloned());
    v
}

fn spelling(s: &mut Session, p: &Json) -> CmdResult {
    match str_opt(p, "action").unwrap_or("check") {
        "check" => check(s, p),
        "suggest" => {
            let w = word_param(p, "word")?;
            let known = is_known(&w, &user_dictionary(s));
            Ok(json!({ "word": w, "known": known, "suggestions": suggest(&w) }))
        }
        "ignore" => {
            let (sheet, at) = cell(s, p, "cell")?;
            let w = word_param(p, "word")?;
            if s.spelling.ignore_once.len() < 100_000 {
                s.spelling.ignore_once.insert((sheet, at, w.clone()));
            }
            Ok(json!({ "ignored": w, "cell": at.to_a1() }))
        }
        "ignore_all" => {
            let w = word_param(p, "word")?;
            if s.spelling.ignore_all.len() < 100_000 {
                s.spelling.ignore_all.insert(w.to_lowercase());
            }
            Ok(json!({ "ignored_all": w }))
        }
        "add" => {
            let w = word_param(p, "word")?;
            add_word(s, &w)?;
            Ok(json!({ "added": w }))
        }
        "change" => change(s, p),
        "change_all" => change_all(s, p),
        other => Err(CmdError::Param(format!(
            "unknown spelling action '{other}'"
        ))),
    }
}

fn add_word(s: &mut Session, w: &str) -> Result<(), CmdError> {
    if crate::settings::app_dir().is_some() {
        crate::settings::update(|st| {
            if !st.user_dictionary.iter().any(|x| x == w) {
                if st.user_dictionary.len() >= MAX_USER_WORDS {
                    return Err("the user dictionary is full".into());
                }
                st.user_dictionary.push(w.to_string());
            }
            Ok(())
        })?;
    } else if !s.spelling.user_words.iter().any(|x| x == w) {
        if s.spelling.user_words.len() >= MAX_USER_WORDS {
            return Err(CmdError::Failed("the user dictionary is full".into()));
        }
        s.spelling.user_words.push(w.to_string());
    }
    Ok(())
}

/// The range a check covers: `range`, else the selection, else (one cell selected) the
/// sheet's used range.
fn scope(s: &Session, p: &Json) -> Result<(usize, Option<CellRange>), CmdError> {
    if str_opt(p, "range").is_some() {
        let (i, r) = range(s, p, "range")?;
        return Ok((i, Some(r)));
    }
    let si = s.sheet_index();
    let sel = s.sel.range();
    if sel.cell_count() > 1 {
        return Ok((si, Some(sel)));
    }
    Ok((si, s.wb.sheet(si).and_then(|sh| sh.used_range())))
}

/// Text constants in scope: (cell, text). Formulas are not checked.
fn text_cells(s: &Session, sheet: usize, r: CellRange) -> Vec<(CellAddr, String)> {
    let Some(sh) = s.wb.sheet(sheet) else {
        return Vec::new();
    };
    sh.cells_in(r)
        .take(usize::try_from(MAX_SCAN_CELLS).unwrap_or(usize::MAX))
        .filter_map(|(a, c)| match &c.content {
            Content::Value(Value::Text(t)) => Some((a, t.to_string())),
            _ => None,
        })
        .collect()
}

fn check(s: &mut Session, p: &Json) -> CmdResult {
    let (sheet, r) = scope(s, p)?;
    let ignore_upper = p
        .get("ignore_uppercase")
        .and_then(Json::as_bool)
        .unwrap_or(true);
    let user = user_dictionary(s);
    let mut findings = Vec::new();
    let mut truncated = false;
    let mut suggested = 0usize;
    let mut checked = 0usize;
    if let Some(r) = r {
        'cells: for (a, text) in text_cells(s, sheet, r) {
            checked += 1;
            for (off, w) in words(&text) {
                if ignore_upper && w.chars().count() > 1 && w.chars().all(|c| !c.is_lowercase()) {
                    continue;
                }
                if is_known(w, &user)
                    || s.spelling.ignore_all.contains(&w.to_lowercase())
                    || s.spelling.ignore_once.contains(&(sheet, a, w.to_string()))
                {
                    continue;
                }
                if findings.len() >= MAX_FINDINGS {
                    truncated = true;
                    break 'cells;
                }
                let sugg = if suggested < MAX_SUGGESTED {
                    suggested += 1;
                    suggest(w)
                } else {
                    Vec::new()
                };
                findings.push(json!({
                    "cell": a.to_a1(),
                    "word": w,
                    "offset": text.get(..off).map_or(0, |t| t.chars().count()),
                    "suggestions": sugg,
                }));
            }
        }
    }
    let sheet_name = s.wb.sheet(sheet).map(|x| x.name.clone());
    s.status = if findings.is_empty() {
        "Spelling check complete.".into()
    } else {
        format!("{} possible misspellings", findings.len())
    };
    Ok(json!({
        "sheet": sheet_name,
        "cells_checked": checked,
        "misspellings": findings,
        "truncated": truncated,
    }))
}

/// `text` with whole-word occurrences of `word` replaced by `to` (`first_only`: just the
/// first). Returns the new text and the number of replacements.
pub fn replace_word(text: &str, word: &str, to: &str, first_only: bool) -> (String, usize) {
    let mut out = String::with_capacity(text.len());
    let mut last = 0usize;
    let mut n = 0usize;
    for (off, w) in words(text) {
        if w == word {
            out.push_str(text.get(last..off).unwrap_or(""));
            out.push_str(to);
            last = off + w.len();
            n += 1;
            if first_only {
                break;
            }
        }
    }
    out.push_str(text.get(last..).unwrap_or(""));
    (out, n)
}

fn replacement(p: &Json) -> Result<String, CmdError> {
    let to = str_opt(p, "to").ok_or_else(|| CmdError::Param("missing string 'to'".into()))?;
    if to.chars().count() > MAX_WORD_CHARS * 4 || to.chars().any(char::is_control) {
        return Err(CmdError::Param("'to' is not a valid replacement".into()));
    }
    Ok(to.to_string())
}

fn set_texts(
    s: &mut Session,
    sheet: usize,
    edits: Vec<(CellAddr, String)>,
    label: &str,
) -> Result<(), CmdError> {
    for (_, t) in &edits {
        if t.chars().count() > sp_core::MAX_TEXT_LEN {
            return Err(CmdError::Failed(
                "the changed text would exceed 32,767 characters".into(),
            ));
        }
    }
    s.transact(label, |tx, _| {
        for (a, t) in edits {
            let Some(mut c) = tx.wb.sheet(sheet).and_then(|sh| sh.get(a)).cloned() else {
                continue;
            };
            let v = Value::text(t);
            c.content = Content::Value(v.clone());
            c.value = v;
            tx.set_cell(sheet, a, Some(c));
        }
        Ok(())
    })
}

fn change(s: &mut Session, p: &Json) -> CmdResult {
    let (sheet, at) = cell(s, p, "cell")?;
    let w = word_param(p, "word")?;
    let to = replacement(p)?;
    let text = match s
        .wb
        .sheet(sheet)
        .and_then(|sh| sh.get(at))
        .map(|c| &c.content)
    {
        Some(Content::Value(Value::Text(t))) => t.to_string(),
        _ => {
            return Err(CmdError::Failed(format!(
                "{} does not hold text",
                at.to_a1()
            )));
        }
    };
    let (new, n) = replace_word(&text, &w, &to, true);
    if n == 0 {
        return Err(CmdError::Failed(format!(
            "'{w}' was not found in {}",
            at.to_a1()
        )));
    }
    set_texts(s, sheet, vec![(at, new.clone())], "Spelling")?;
    Ok(json!({ "cell": at.to_a1(), "text": new }))
}

fn change_all(s: &mut Session, p: &Json) -> CmdResult {
    let (sheet, r) = scope(s, p)?;
    let w = word_param(p, "word")?;
    let to = replacement(p)?;
    let mut edits = Vec::new();
    let mut count = 0usize;
    if let Some(r) = r {
        for (a, text) in text_cells(s, sheet, r) {
            let (new, n) = replace_word(&text, &w, &to, false);
            if n > 0 {
                count += n;
                edits.push((a, new));
            }
        }
    }
    let cells = edits.len();
    if cells > 0 {
        set_texts(s, sheet, edits, "Spelling")?;
    }
    Ok(json!({ "replaced": count, "cells": cells }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(s: &mut Session, a: &str, t: &str) {
        s.execute("cell.set", &json!({"cell": a, "input": t}))
            .unwrap();
    }

    #[test]
    fn dictionary_and_case_rules() {
        assert!(word_count() > 100_000);
        let u: Vec<String> = Vec::new();
        for ok in [
            "the",
            "The",
            "THE",
            "Paris",
            "PARIS",
            "don't",
            "teacher's",
            "John's",
            "color",
            "running",
            "spreadsheet",
        ] {
            assert!(is_known(ok, &u), "{ok}");
        }
        for bad in ["teh", "recieve", "paris", "Spredsheet"] {
            assert!(!is_known(bad, &u), "{bad}");
        }
        assert!(is_known("SheetPro", &["sheetpro".to_string()]));
    }

    #[test]
    fn tokenizer_skips_numbers_urls_and_paths() {
        let w: Vec<&str> =
            words("Teh  quick-brown fox's 3rd A1 https://x.y/z a@b.c C:\\dir it's ‘q’.")
                .into_iter()
                .map(|(_, w)| w)
                .collect();
        assert_eq!(w, vec!["Teh", "quick", "brown", "fox's", "it's", "q"]);
        assert_eq!(words("café naïve")[1], (6, "naïve"));
    }

    #[test]
    fn suggestions_rank_by_distance() {
        let s = suggest("teh");
        assert!(s.iter().any(|x| x == "the"), "{s:?}");
        let s = suggest("recieve");
        assert_eq!(s.first().map(String::as_str), Some("receive"), "{s:?}");
        let s = suggest("Acommodate");
        assert_eq!(s.first().map(String::as_str), Some("Accommodate"), "{s:?}");
        assert_eq!(suggest("TEH").first().map(String::as_str), Some("THE"));
        assert!(suggest("").is_empty());
        assert!(suggest(&"x".repeat(500)).is_empty());
        assert_eq!(distance(&['a', 'b'], &['b', 'a'], 2), Some(1));
        assert_eq!(distance(&['a'; 9], &['b'; 9], 2), None);
    }

    #[test]
    fn check_ignore_add_change() {
        let mut s = Session::new();
        set(&mut s, "A1", "Teh quick fox");
        set(&mut s, "A2", "recieve the teh");
        set(&mut s, "A3", "=\"teh\"");
        set(&mut s, "B1", "NASA ASAP Zorblax");
        let r = s.execute("review.spelling", &json!({})).unwrap();
        let words: Vec<(&str, &str)> = r["misspellings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| (f["cell"].as_str().unwrap(), f["word"].as_str().unwrap()))
            .collect();
        assert_eq!(
            words,
            vec![
                ("A1", "Teh"),
                ("B1", "Zorblax"),
                ("A2", "recieve"),
                ("A2", "teh")
            ]
        );
        // Ignore once / all, add.
        s.execute(
            "review.spelling",
            &json!({"action": "ignore", "cell": "A1", "word": "Teh"}),
        )
        .unwrap();
        s.execute(
            "review.spelling",
            &json!({"action": "add", "word": "Zorblax"}),
        )
        .unwrap();
        let r = s.execute("review.spelling", &json!({})).unwrap();
        assert_eq!(r["misspellings"].as_array().unwrap().len(), 2);
        s.execute(
            "review.spelling",
            &json!({"action": "ignore_all", "word": "recieve"}),
        )
        .unwrap();
        let r = s
            .execute("review.spelling", &json!({"range": "A2"}))
            .unwrap();
        assert_eq!(r["misspellings"][0]["word"], "teh");
        assert_eq!(r["misspellings"][0]["offset"], 12);
        // Change one, then change all (one undo step each).
        s.execute(
            "review.spelling",
            &json!({"action": "change", "cell": "A2", "word": "recieve", "to": "receive"}),
        )
        .unwrap();
        let t = |s: &Session, a: &str| s.wb.sheets[0].value(CellAddr::parse(a).unwrap()).clone();
        assert_eq!(t(&s, "A2"), Value::text("receive the teh"));
        let r = s
            .execute(
                "review.spelling",
                &json!({"action": "change_all", "range": "A1:A3", "word": "teh", "to": "the"}),
            )
            .unwrap();
        assert_eq!(r["replaced"], 1); // case-sensitive: "Teh" stays; A3 is a formula
        assert_eq!(t(&s, "A2"), Value::text("receive the the"));
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(t(&s, "A2"), Value::text("receive the teh"));
        assert!(
            s.execute(
                "review.spelling",
                &json!({"action": "change", "cell": "A3", "word": "teh", "to": "x"})
            )
            .is_err()
        );
        assert!(
            s.execute(
                "review.spelling",
                &json!({"action": "add", "word": "two words"})
            )
            .is_err()
        );
    }
}
