//! Flash Fill: infer a string transformation from typed examples and apply it to the other
//! rows (pure; the command is `sp_engine::commands::flash_cmds`).
//!
//! Program synthesis over a small DSL. A [`Program`] is a concatenation of [`Atom`]s, each a
//! constant or a piece extracted from one source column, optionally re-cased:
//!
//! - pieces: the whole value, the k-th token of a class (word, letters, digits, capitalised
//!   word) counted from the start or the end, the first n characters of such a token, the
//!   k-th field when split by a delimiter (trimmed), or a fixed character span counted from
//!   the start or the end;
//! - cases: as is, UPPER, lower, Proper.
//!
//! Search: every example's output is a path through a DAG of output positions whose edges
//! are the atoms producing that span. A uniform-cost search walks all examples at once
//! (state = one position per example), so only atoms consistent with *every* example are
//! followed, and returns the cheapest program (extractions are cheaper than constants,
//! simple pieces cheaper than positional spans, fewer atoms cheaper than more). If another
//! program of the same cost produces a different result on a row still to fill, the
//! examples are ambiguous and nothing is filled. Inputs, outputs, states and atoms are all
//! capped, so the search is bounded in time and memory.

use std::collections::{BinaryHeap, HashMap, HashSet};
use std::cmp::Reverse;

/// Longest source value (chars) considered.
pub const MAX_SOURCE_CHARS: usize = 256;
/// Longest example output (chars).
pub const MAX_OUTPUT_CHARS: usize = 128;
/// Most source columns.
pub const MAX_COLUMNS: usize = 16;
/// Most examples used.
pub const MAX_EXAMPLES: usize = 32;
/// Most search states expanded.
const MAX_STATES: usize = 200_000;
/// Most candidate atoms per example.
const MAX_ATOMS: usize = 20_000;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FlashError {
    #[error("Flash Fill needs at least one example next to the data.")]
    NoExamples,
    #[error("Flash Fill couldn't find a pattern that matches the examples. Type another example and try again.")]
    NoPattern,
    #[error("The examples fit more than one pattern ({0}). Type another example and try again.")]
    Ambiguous(String),
    #[error("The data is too large for Flash Fill ({0}).")]
    TooLarge(String),
    #[error("There is nothing left to fill.")]
    NothingToFill,
}

/// Token classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokClass {
    /// Run of letters and digits.
    Word,
    /// Run of letters.
    Alpha,
    /// Run of digits.
    Digits,
    /// Capital letter followed by lowercase letters (splits `JohnSmith`).
    Capital,
}

impl TokClass {
    const ALL: [TokClass; 4] = [TokClass::Word, TokClass::Alpha, TokClass::Digits, TokClass::Capital];

    fn name(self) -> &'static str {
        match self {
            TokClass::Word => "word",
            TokClass::Alpha => "letters",
            TokClass::Digits => "number",
            TokClass::Capital => "capitalised word",
        }
    }

    fn cost(self) -> u32 {
        match self {
            TokClass::Word => 48,
            TokClass::Alpha => 65,
            TokClass::Digits => 66,
            TokClass::Capital => 83,
        }
    }

    /// Token spans `[start, end)` in `s`.
    fn spans(self, s: &[char]) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < s.len() {
            let Some(&c) = s.get(i) else { break };
            let starts = match self {
                TokClass::Word => c.is_alphanumeric(),
                TokClass::Alpha => c.is_alphabetic(),
                TokClass::Digits => c.is_ascii_digit(),
                TokClass::Capital => c.is_uppercase(),
            };
            if !starts {
                i += 1;
                continue;
            }
            let mut j = i + 1;
            while let Some(&d) = s.get(j) {
                let more = match self {
                    TokClass::Word => d.is_alphanumeric(),
                    TokClass::Alpha => d.is_alphabetic(),
                    TokClass::Digits => d.is_ascii_digit(),
                    TokClass::Capital => d.is_lowercase(),
                };
                if !more {
                    break;
                }
                j += 1;
            }
            out.push((i, j));
            i = j;
        }
        out
    }
}

/// Case change applied to an extracted piece.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Case {
    Same,
    Upper,
    Lower,
    Proper,
}

impl Case {
    const ALL: [Case; 4] = [Case::Same, Case::Upper, Case::Lower, Case::Proper];

    fn apply(self, s: &str) -> String {
        match self {
            Case::Same => s.to_string(),
            Case::Upper => s.to_uppercase(),
            Case::Lower => s.to_lowercase(),
            Case::Proper => {
                let mut out = String::with_capacity(s.len());
                let mut start = true;
                for c in s.chars() {
                    if c.is_alphabetic() {
                        if start {
                            out.extend(c.to_uppercase());
                        } else {
                            out.extend(c.to_lowercase());
                        }
                        start = false;
                    } else {
                        out.push(c);
                        start = !c.is_alphanumeric() && c != '\'';
                    }
                }
                out
            }
        }
    }

    fn cost(self) -> u32 {
        match self {
            Case::Same => 0,
            Case::Lower => 17,
            Case::Upper => 18,
            Case::Proper => 19,
        }
    }
}

/// A piece of one source value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Piece {
    Whole,
    /// k-th token (k < 0 counts from the end, -1 = last).
    Token { class: TokClass, k: i16 },
    /// First `n` characters of the k-th token.
    Prefix { class: TokClass, k: i16, n: u8 },
    /// k-th field (trimmed) when split by `delim`.
    Field { delim: char, k: i16 },
    /// `len` characters starting `start` chars from the start (or, `from_end`, from the end).
    Span { from_end: bool, start: u16, len: u16 },
}

fn nth<T: Copy>(v: &[T], k: i16) -> Option<T> {
    let i = if k >= 0 {
        usize::try_from(k).ok()?
    } else {
        v.len().checked_sub(usize::from(k.unsigned_abs()))?
    };
    v.get(i).copied()
}

fn text(s: &[char], a: usize, b: usize) -> Option<String> {
    s.get(a..b).map(|c| c.iter().collect())
}

impl Piece {
    fn cost(&self) -> u32 {
        match self {
            Piece::Whole => 32,
            Piece::Token { class, .. } => class.cost(),
            Piece::Prefix { class, n, .. } => 34 + class.cost() + 16 * u32::from(*n),
            Piece::Field { .. } => 81,
            Piece::Span { .. } => 213,
        }
    }

    fn eval(&self, s: &[char]) -> Option<String> {
        match *self {
            Piece::Whole => Some(s.iter().collect()),
            Piece::Token { class, k } => {
                let (a, b) = nth(&class.spans(s), k)?;
                text(s, a, b)
            }
            Piece::Prefix { class, k, n } => {
                let (a, b) = nth(&class.spans(s), k)?;
                let e = a.checked_add(usize::from(n))?;
                if e > b {
                    return None;
                }
                text(s, a, e)
            }
            Piece::Field { delim, k } => {
                let joined: String = s.iter().collect();
                let fields: Vec<&str> = joined.split(delim).collect();
                if fields.len() < 2 {
                    return None;
                }
                let f = nth(&fields, k)?.trim();
                (!f.is_empty()).then(|| f.to_string())
            }
            Piece::Span {
                from_end,
                start,
                len,
            } => {
                let a = if from_end {
                    s.len().checked_sub(usize::from(start))?
                } else {
                    usize::from(start)
                };
                text(s, a, a.checked_add(usize::from(len))?)
            }
        }
    }

    fn describe(&self) -> String {
        let ord = |k: i16| -> String {
            match k {
                -1 => "last".into(),
                k if k < 0 => format!("{}-from-last", k.unsigned_abs()),
                0 => "1st".into(),
                1 => "2nd".into(),
                2 => "3rd".into(),
                k => format!("{}th", k + 1),
            }
        };
        match *self {
            Piece::Whole => "whole value".into(),
            Piece::Token { class, k } => format!("{} {}", ord(k), class.name()),
            Piece::Prefix { class, k, n } => {
                format!("first {n} char(s) of {} {}", ord(k), class.name())
            }
            Piece::Field { delim, k } => format!("{} part split by '{delim}'", ord(k)),
            Piece::Span {
                from_end,
                start,
                len,
            } => {
                if from_end {
                    format!("{len} char(s) starting {start} from the end")
                } else {
                    format!("{len} char(s) from position {}", start + 1)
                }
            }
        }
    }
}

/// One step of a program.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Atom {
    Const(String),
    Extract { col: usize, piece: Piece, case: Case },
}

impl Atom {
    fn cost(&self) -> u32 {
        match self {
            // Separators are cheap; letters and digits are probably data, not constants.
            Atom::Const(s) => {
                64 + s
                    .chars()
                    .map(|c| if c.is_alphanumeric() { 160 } else { 16 })
                    .sum::<u32>()
            }
            Atom::Extract { piece, case, .. } => piece.cost() + case.cost(),
        }
    }

    fn eval(&self, row: &[Vec<char>]) -> Option<String> {
        match self {
            Atom::Const(s) => Some(s.clone()),
            Atom::Extract { col, piece, case } => {
                let v = piece.eval(row.get(*col)?)?;
                (!v.is_empty()).then(|| case.apply(&v))
            }
        }
    }

    fn describe(&self, headers: &[String]) -> String {
        match self {
            Atom::Const(s) => format!("\"{s}\""),
            Atom::Extract { col, piece, case } => {
                let src = headers
                    .get(*col)
                    .filter(|h| !h.is_empty())
                    .cloned()
                    .unwrap_or_else(|| format!("column {}", col + 1));
                let c = match case {
                    Case::Same => "",
                    Case::Upper => " in UPPER case",
                    Case::Lower => " in lower case",
                    Case::Proper => " in Proper Case",
                };
                format!("{} of {src}{c}", piece.describe())
            }
        }
    }
}

/// A synthesized transformation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    pub atoms: Vec<Atom>,
    pub cost: u32,
}

impl Program {
    /// Output for one row of source values (`None` when a piece does not exist there).
    pub fn apply(&self, row: &[String]) -> Option<String> {
        let chars = to_chars(row);
        self.apply_chars(&chars)
    }

    fn apply_chars(&self, row: &[Vec<char>]) -> Option<String> {
        let mut out = String::new();
        for a in &self.atoms {
            out.push_str(&a.eval(row)?);
        }
        Some(out)
    }

    /// Human-readable summary (`1st word of Name in lower case + "." + …`).
    pub fn describe(&self, headers: &[String]) -> String {
        self.atoms
            .iter()
            .map(|a| a.describe(headers))
            .collect::<Vec<_>>()
            .join(" + ")
    }
}

fn to_chars(row: &[String]) -> Vec<Vec<char>> {
    row.iter()
        .take(MAX_COLUMNS)
        .map(|s| s.chars().take(MAX_SOURCE_CHARS).collect())
        .collect()
}

/// Every extraction atom for one row, with its value (deduplicated per value+atom).
fn extraction_atoms(row: &[Vec<char>], wanted: &str) -> Vec<(Atom, String)> {
    let wanted_lower = wanted.to_lowercase();
    let mut pieces: Vec<(usize, Piece)> = Vec::new();
    for (col, s) in row.iter().enumerate() {
        if s.is_empty() {
            continue;
        }
        pieces.push((col, Piece::Whole));
        for class in TokClass::ALL {
            let n = class.spans(s).len();
            for i in 0..n.min(24) {
                let k = i as i16;
                let back = -((n - i) as i16);
                for kk in [k, back] {
                    pieces.push((col, Piece::Token { class, k: kk }));
                    for len in 1..=3u8 {
                        pieces.push((col, Piece::Prefix { class, k: kk, n: len }));
                    }
                }
            }
        }
        let mut delims: Vec<char> = s
            .iter()
            .copied()
            .filter(|c| !c.is_alphanumeric())
            .collect();
        delims.sort_unstable();
        delims.dedup();
        for d in delims.into_iter().take(12) {
            let n = s.iter().filter(|c| **c == d).count() + 1;
            for i in 0..n.min(24) {
                pieces.push((col, Piece::Field { delim: d, k: i as i16 }));
                pieces.push((col, Piece::Field { delim: d, k: -((n - i) as i16) }));
            }
        }
        // Fixed spans: only where the (case-folded) span occurs in the wanted output.
        let lower: Vec<char> = s.iter().flat_map(|c| c.to_lowercase()).collect();
        if lower.len() == s.len() {
            for a in 0..s.len() {
                for b in (a + 2)..=s.len().min(a + MAX_OUTPUT_CHARS) {
                    let Some(sub) = text(&lower, a, b) else { continue };
                    if !wanted_lower.contains(&sub) {
                        break;
                    }
                    let len = (b - a) as u16;
                    pieces.push((col, Piece::Span { from_end: false, start: a as u16, len }));
                    pieces.push((
                        col,
                        Piece::Span {
                            from_end: true,
                            start: (s.len() - a) as u16,
                            len,
                        },
                    ));
                }
            }
        }
    }
    let mut out = Vec::new();
    let mut seen: HashSet<(Atom, String)> = HashSet::new();
    for (col, piece) in pieces {
        let Some(v) = row.get(col).and_then(|s| piece.eval(s)) else {
            continue;
        };
        // Extractions carry content: never a bare separator.
        if v.is_empty() || !v.chars().any(char::is_alphanumeric) {
            continue;
        }
        for case in Case::ALL {
            let cv = case.apply(&v);
            if !wanted.contains(&cv) {
                continue;
            }
            let atom = Atom::Extract { col, piece, case };
            if seen.insert((atom.clone(), cv.clone())) {
                out.push((atom, cv));
            }
            if out.len() >= MAX_ATOMS {
                return out;
            }
        }
    }
    out
}

struct Example {
    row: Vec<Vec<char>>,
    out: Vec<char>,
}

impl Example {
    /// Whether `v` matches the output at char position `p`; returns the new position.
    fn step(&self, p: usize, v: &str) -> Option<usize> {
        let mut q = p;
        for c in v.chars() {
            if self.out.get(q) != Some(&c) {
                return None;
            }
            q += 1;
        }
        (q > p).then_some(q)
    }
}

/// Synthesize the cheapest program consistent with every example and the value it gives
/// each of `targets` (the rows still to fill). Equally cheap programs giving different
/// values on a target row make the examples ambiguous; a row where only some of them apply
/// stays blank.
pub fn synthesize(
    examples: &[(Vec<String>, String)],
    targets: &[Vec<String>],
) -> Result<(Program, Vec<Option<String>>), FlashError> {
    if examples.is_empty() {
        return Err(FlashError::NoExamples);
    }
    let exs: Vec<Example> = examples
        .iter()
        .take(MAX_EXAMPLES)
        .map(|(row, out)| Example {
            row: to_chars(row),
            out: out.chars().collect(),
        })
        .collect();
    if exs.iter().any(|e| e.out.len() > MAX_OUTPUT_CHARS) {
        return Err(FlashError::TooLarge(format!(
            "examples longer than {MAX_OUTPUT_CHARS} characters"
        )));
    }
    if exs.iter().any(|e| e.out.is_empty()) {
        return Err(FlashError::NoPattern);
    }
    let Some(first) = exs.first() else {
        return Err(FlashError::NoExamples);
    };
    // Edges of the first example's DAG: position → (atom, end).
    let first_out: String = first.out.iter().collect();
    let mut edges: Vec<Vec<(Atom, usize)>> = vec![Vec::new(); first.out.len()];
    for (atom, v) in extraction_atoms(&first.row, &first_out) {
        let vc: Vec<char> = v.chars().collect();
        for p in 0..first.out.len() {
            if first.out.get(p..p + vc.len()) == Some(vc.as_slice())
                && let Some(slot) = edges.get_mut(p)
            {
                slot.push((atom.clone(), p + vc.len()));
            }
        }
    }
    for (p, slot) in edges.iter_mut().enumerate() {
        for q in (p + 1)..=first.out.len() {
            if let Some(s) = text(&first.out, p, q) {
                slot.push((Atom::Const(s), q));
            }
        }
    }
    let target_rows: Vec<Vec<Vec<char>>> = targets.iter().map(|r| to_chars(r)).collect();
    // Memoized atom values per example (index ≥ 1).
    let mut memo: HashMap<(Atom, usize), Option<String>> = HashMap::new();
    let mut value = |atom: &Atom, e: usize, exs: &[Example]| -> Option<String> {
        if let Atom::Const(s) = atom {
            return Some(s.clone());
        }
        memo.entry((atom.clone(), e))
            .or_insert_with(|| exs.get(e).and_then(|x| atom.eval(&x.row)))
            .clone()
    };
    // Uniform-cost search over position tuples.
    let start: Vec<usize> = vec![0; exs.len()];
    let goal: Vec<usize> = exs.iter().map(|e| e.out.len()).collect();
    let mut dist: HashMap<Vec<usize>, u32> = HashMap::new();
    // Optimal predecessor edges: state → [(prev state, atom)].
    let mut preds: HashMap<Vec<usize>, Vec<(Vec<usize>, Atom)>> = HashMap::new();
    let mut heap: BinaryHeap<Reverse<(u32, Vec<usize>)>> = BinaryHeap::new();
    dist.insert(start.clone(), 0);
    heap.push(Reverse((0, start.clone())));
    let mut best: Option<u32> = None;
    let mut expanded = 0usize;
    while let Some(Reverse((d, st))) = heap.pop() {
        if best.is_some_and(|b| d > b) {
            break;
        }
        if dist.get(&st).is_some_and(|x| *x < d) {
            continue;
        }
        if st == goal {
            best = Some(d);
            continue;
        }
        expanded += 1;
        if expanded > MAX_STATES {
            return Err(FlashError::TooLarge("search limit reached".into()));
        }
        let Some(p0) = st.first().copied() else { break };
        let Some(out_edges) = edges.get(p0) else { continue };
        for (atom, end0) in out_edges {
            let mut next = Vec::with_capacity(st.len());
            next.push(*end0);
            let mut ok = true;
            for (e, &pe) in st.iter().enumerate().skip(1) {
                let Some(ex) = exs.get(e) else {
                    ok = false;
                    break;
                };
                match value(atom, e, &exs).and_then(|v| ex.step(pe, &v)) {
                    Some(q) => next.push(q),
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if !ok {
                continue;
            }
            let nd = d + atom.cost();
            let cur = dist.get(&next).copied();
            if cur.is_none_or(|c| nd < c) {
                dist.insert(next.clone(), nd);
                preds.insert(next.clone(), vec![(st.clone(), atom.clone())]);
                heap.push(Reverse((nd, next)));
            } else if cur == Some(nd)
                && let Some(v) = preds.get_mut(&next)
                && v.len() < 64
            {
                v.push((st.clone(), atom.clone()));
            }
        }
    }
    let Some(best) = best else {
        return Err(FlashError::NoPattern);
    };
    // One optimal program: follow the first predecessor back from the goal.
    let mut atoms = Vec::new();
    let mut cur = goal.clone();
    let mut guard = 0;
    while cur != start {
        guard += 1;
        if guard > MAX_OUTPUT_CHARS + 1 {
            return Err(FlashError::NoPattern);
        }
        let Some((prev, atom)) = preds.get(&cur).and_then(|v| v.first()).cloned() else {
            return Err(FlashError::NoPattern);
        };
        atoms.push(atom);
        cur = prev;
    }
    atoms.reverse();
    let atoms = merge_consts(atoms);
    if atoms.iter().all(|a| matches!(a, Atom::Const(_))) {
        return Err(FlashError::NoPattern);
    }
    let program = Program { atoms, cost: best };
    // Ambiguity: every optimal path must agree on every row still to fill.
    let mut fills = Vec::with_capacity(target_rows.len());
    for row in &target_rows {
        let all = optimal_outputs(&goal, &start, &preds, row);
        let outs: Vec<&String> = all.iter().flatten().collect();
        if outs.len() > 1 {
            let shown: Vec<String> = outs.iter().map(|o| format!("\"{o}\"")).collect();
            return Err(FlashError::Ambiguous(format!(
                "a row could become {}",
                shown.join(" or ")
            )));
        }
        // Filled only when every equally simple program gives that value.
        fills.push(match all.as_slice() {
            [Some(v)] => Some(v.clone()),
            _ => None,
        });
    }
    Ok((program, fills))
}

/// Distinct outputs (at most 3) of the optimal programs on `row`; `None` = the program
/// does not apply to that row.
fn optimal_outputs(
    goal: &[usize],
    start: &[usize],
    preds: &HashMap<Vec<usize>, Vec<(Vec<usize>, Atom)>>,
    row: &[Vec<char>],
) -> Vec<Option<String>> {
    // Outputs of the prefix programs ending at each state, computed from the start.
    let mut memo: HashMap<Vec<usize>, Vec<Option<String>>> = HashMap::new();
    fn go(
        st: &[usize],
        start: &[usize],
        preds: &HashMap<Vec<usize>, Vec<(Vec<usize>, Atom)>>,
        row: &[Vec<char>],
        memo: &mut HashMap<Vec<usize>, Vec<Option<String>>>,
        depth: usize,
    ) -> Vec<Option<String>> {
        if st == start {
            return vec![Some(String::new())];
        }
        if let Some(v) = memo.get(st) {
            return v.clone();
        }
        if depth > MAX_OUTPUT_CHARS + 1 {
            return Vec::new();
        }
        let mut out: Vec<Option<String>> = Vec::new();
        for (prev, atom) in preds.get(st).map_or(&[][..], Vec::as_slice) {
            let tail = atom.eval(row);
            for head in go(prev, start, preds, row, memo, depth + 1) {
                let v = match (head, &tail) {
                    (Some(h), Some(t)) => Some(h + t),
                    _ => None,
                };
                if !out.contains(&v) {
                    out.push(v);
                }
                if out.len() > 2 {
                    break;
                }
            }
            if out.len() > 2 {
                break;
            }
        }
        memo.insert(st.to_vec(), out.clone());
        out
    }
    go(goal, start, preds, row, &mut memo, 0)
}

fn merge_consts(atoms: Vec<Atom>) -> Vec<Atom> {
    let mut out: Vec<Atom> = Vec::with_capacity(atoms.len());
    for a in atoms {
        if let (Some(Atom::Const(prev)), Atom::Const(s)) = (out.last_mut(), &a) {
            prev.push_str(s);
            continue;
        }
        out.push(a);
    }
    out
}

/// Fill a column: `rows[i]` are the source values of row i, `outputs[i]` its typed value
/// (`Some` = an example). Returns the program and the value for every row without one
/// (`None` where the program does not apply).
pub fn flash_fill(
    rows: &[Vec<String>],
    outputs: &[Option<String>],
) -> Result<(Program, Vec<Option<String>>), FlashError> {
    let mut examples = Vec::new();
    let mut targets = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        match outputs.get(i).and_then(|o| o.as_ref()) {
            Some(o) => examples.push((row.clone(), o.clone())),
            None if row.iter().any(|v| !v.trim().is_empty()) => targets.push(row.clone()),
            None => {}
        }
    }
    if examples.is_empty() {
        return Err(FlashError::NoExamples);
    }
    if targets.is_empty() {
        return Err(FlashError::NothingToFill);
    }
    let (program, fills) = synthesize(&examples, &targets)?;
    let mut fills = fills.into_iter();
    let filled = rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            if outputs.get(i).and_then(|o| o.as_ref()).is_some()
                || row.iter().all(|v| v.trim().is_empty())
            {
                None
            } else {
                fills.next().flatten()
            }
        })
        .collect();
    Ok((program, filled))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn rows(src: &[&[&str]]) -> Vec<Vec<String>> {
        src.iter()
            .map(|r| r.iter().map(|s| s.to_string()).collect())
            .collect()
    }

    /// Run with the first `n` outputs as examples; check the rest.
    fn case(src: &[&[&str]], want: &[&str], n: usize) -> Result<(), FlashError> {
        let r = rows(src);
        let outs: Vec<Option<String>> = (0..r.len())
            .map(|i| (i < n).then(|| want[i].to_string()))
            .collect();
        let (prog, filled) = flash_fill(&r, &outs)?;
        for i in n..r.len() {
            assert_eq!(
                filled[i].as_deref(),
                Some(want[i]),
                "row {i} with {}",
                prog.describe(&[])
            );
        }
        Ok(())
    }

    #[test]
    fn first_names() {
        case(
            &[&["John Smith"], &["Mary Johnson"], &["Ahmed Khan"], &["Li Wei"]],
            &["John", "Mary", "Ahmed", "Li"],
            1,
        )
        .unwrap();
    }

    #[test]
    fn last_names() {
        case(
            &[&["John Smith"], &["Mary Johnson"], &["Ahmed Khan"]],
            &["Smith", "Johnson", "Khan"],
            1,
        )
        .unwrap();
    }

    #[test]
    fn last_name_comma_first() {
        case(
            &[&["Smith, John"], &["Johnson, Mary"], &["Khan, Ahmed"]],
            &["John Smith", "Mary Johnson", "Ahmed Khan"],
            1,
        )
        .unwrap();
    }

    #[test]
    fn initials() {
        case(
            &[&["John Smith"], &["mary johnson"], &["ahmed khan"]],
            &["JS", "MJ", "AK"],
            2,
        )
        .unwrap();
    }

    #[test]
    fn initial_dot_surname() {
        case(
            &[&["John Smith"], &["Mary Johnson"], &["Ahmed Khan"]],
            &["J. Smith", "M. Johnson", "A. Khan"],
            1,
        )
        .unwrap();
    }

    #[test]
    fn email_from_first_last_columns() {
        case(
            &[&["John", "Smith"], &["Mary", "Johnson"], &["Ahmed", "Khan"]],
            &[
                "john.smith@example.com",
                "mary.johnson@example.com",
                "ahmed.khan@example.com",
            ],
            1,
        )
        .unwrap();
    }

    #[test]
    fn user_from_email() {
        case(
            &[&["jsmith@acme.org"], &["mary.j@contoso.net"], &["akhan@mail.co"]],
            &["jsmith", "mary.j", "akhan"],
            2,
        )
        .unwrap();
    }

    #[test]
    fn domain_from_email() {
        case(
            &[&["jsmith@acme.org"], &["mary.j@contoso.net"], &["akhan@mail.co"]],
            &["acme.org", "contoso.net", "mail.co"],
            1,
        )
        .unwrap();
    }

    #[test]
    fn phone_digits_to_format() {
        case(
            &[&["5551234567"], &["2125550199"], &["4155550123"]],
            &["(555) 123-4567", "(212) 555-0199", "(415) 555-0123"],
            1,
        )
        .unwrap();
    }

    #[test]
    fn phone_dashes_to_dots() {
        case(
            &[&["555-123-4567"], &["212-555-0199"], &["415-555-0123"]],
            &["555.123.4567", "212.555.0199", "415.555.0123"],
            1,
        )
        .unwrap();
    }

    #[test]
    fn iso_date_text_to_dmy() {
        case(
            &[&["2024-03-15"], &["2023-11-02"], &["1999-12-31"]],
            &["15/03/2024", "02/11/2023", "31/12/1999"],
            1,
        )
        .unwrap();
    }

    #[test]
    fn year_from_us_date() {
        case(
            &[&["3/15/2024"], &["11/2/2023"], &["12/31/1999"]],
            &["2024", "2023", "1999"],
            1,
        )
        .unwrap();
    }

    #[test]
    fn product_code_parts() {
        case(
            &[&["SKU-00123-BLUE"], &["SKU-04410-RED"], &["SKU-77001-GREEN"]],
            &["BLUE-00123", "RED-04410", "GREEN-77001"],
            1,
        )
        .unwrap();
    }

    #[test]
    fn product_code_lowercase_concat() {
        case(
            &[&["AB-123"], &["CD-9"], &["EF-4410"]],
            &["ab123", "cd9", "ef4410"],
            1,
        )
        .unwrap();
    }

    #[test]
    fn upper_case_city() {
        case(
            &[&["london"], &["paris"], &["new york"]],
            &["LONDON", "PARIS", "NEW YORK"],
            1,
        )
        .unwrap();
    }

    #[test]
    fn proper_case_names() {
        case(
            &[&["jOHN smith"], &["MARY JOHNSON"], &["ahmed khan"]],
            &["John Smith", "Mary Johnson", "Ahmed Khan"],
            1,
        )
        .unwrap();
    }

    #[test]
    fn camel_case_split() {
        case(
            &[&["JohnSmith"], &["MaryJohnson"], &["AhmedKhan"]],
            &["John Smith", "Mary Johnson", "Ahmed Khan"],
            1,
        )
        .unwrap();
    }

    #[test]
    fn number_from_label() {
        case(
            &[&["Order #4410 (paid)"], &["Order #17 (open)"], &["Order #900 (paid)"]],
            &["4410", "17", "900"],
            1,
        )
        .unwrap();
    }

    #[test]
    fn file_extension() {
        case(
            &[&["report.final.pdf"], &["photo.jpeg"], &["data.2024.csv"], &["notes.txt"]],
            &["pdf", "jpeg", "csv", "txt"],
            2,
        )
        .unwrap();
    }

    #[test]
    fn two_examples_disambiguate_middle_names() {
        // One example cannot tell 2nd word from last word; the 3-word row makes it ambiguous.
        let src: &[&[&str]] = &[&["John Smith"], &["Mary Ann Lee"], &["Ahmed Khan"]];
        let e = case(src, &["Smith", "Lee", "Khan"], 1).unwrap_err();
        assert!(matches!(e, FlashError::Ambiguous(_)), "{e:?}");
        // A second example settles it.
        case(src, &["Smith", "Lee", "Khan"], 2).unwrap();
    }

    #[test]
    fn refuses_unrelated_output() {
        let e = case(&[&["apple"], &["pear"]], &["xyz", "x"], 1).unwrap_err();
        assert_eq!(e, FlashError::NoPattern);
    }

    #[test]
    fn refuses_inconsistent_examples() {
        let e = case(&[&["John Smith"], &["Mary Jones"], &["Ahmed Khan"]], &["John", "Jones", "x"], 2)
            .unwrap_err();
        assert_eq!(e, FlashError::NoPattern);
    }

    #[test]
    fn no_examples_and_nothing_to_fill() {
        let r = rows(&[&["a b"], &["c d"]]);
        assert_eq!(flash_fill(&r, &[None, None]).unwrap_err(), FlashError::NoExamples);
        let full = [Some("a".to_string()), Some("c".to_string())];
        assert_eq!(flash_fill(&r, &full).unwrap_err(), FlashError::NothingToFill);
    }

    #[test]
    fn rows_where_the_piece_is_missing_stay_blank() {
        let r = rows(&[&["A-17"], &["B"], &["C-9"], &["D-"]]);
        let outs = [Some("17".to_string()), None, Some("9".to_string()), None];
        let (_, filled) = flash_fill(&r, &outs).unwrap();
        assert_eq!(filled[1], None);
        assert_eq!(filled[3], None);
        // 2nd word or last word? Only one applies to "Cher": left blank, not guessed.
        let r = rows(&[&["John Smith"], &["Cher"], &["Ahmed Khan"]]);
        let outs = [Some("Smith".to_string()), None, Some("Khan".to_string())];
        let (_, filled) = flash_fill(&r, &outs).unwrap();
        assert_eq!(filled[1], None);
    }

    #[test]
    fn bounded_on_hostile_input() {
        let long = "ab ".repeat(2_000);
        let r = vec![vec![long.clone(); 40], vec![long; 40]];
        let outs = [Some("x".repeat(500)), None];
        assert!(matches!(flash_fill(&r, &outs), Err(FlashError::TooLarge(_))));
        // Long but legal inputs finish quickly.
        let src = "a-b c/d e.f ".repeat(20);
        let r = vec![vec![src.clone(); 16]; 4];
        let t = std::time::Instant::now();
        let outs = [Some("a b".to_string()), Some("a b".to_string()), None, None];
        let _ = flash_fill(&r, &outs);
        assert!(t.elapsed().as_secs() < 10);
    }

    #[test]
    fn describe_reads_well() {
        let r = rows(&[&["John Smith"], &["Mary Johnson"]]);
        let (p, _) = flash_fill(&r, &[Some("john".into()), None]).unwrap();
        assert_eq!(p.describe(&["Name".to_string()]), "1st word of Name in lower case");
    }
}
