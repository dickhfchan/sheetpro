//! A small, bounded backtracking regular-expression engine for REGEXTEST / REGEXEXTRACT /
//! REGEXREPLACE (PCRE-style subset).
//!
//! Supported: literals, `.`, classes `[a-z]` / `[^...]`, escapes `\d \w \s \D \W \S \b \B`
//! and escaped punctuation, anchors `^ $`, groups `( )`, non-capturing `(?: )`,
//! alternation `|`, quantifiers `* + ? {m} {m,} {m,n}` with lazy `?` variants.
//! Work is capped by a step budget and a nesting limit, so hostile patterns fail with an
//! error instead of hanging or overflowing the stack.

use super::Registry;
use crate::ast::Expr;
use crate::eval::{Array2, Ev, EvalValue};
use sp_core::{ErrorKind, Value};

const MAX_STEPS: u64 = 2_000_000;
const MAX_DEPTH: usize = 400;
const MAX_PATTERN: usize = 4096;
const MAX_REPEAT: u32 = 10_000;

#[derive(Debug, Clone)]
enum ClassItem {
    Range(char, char),
    Digit(bool),
    Word(bool),
    Space(bool),
}

#[derive(Debug, Clone)]
enum Node {
    Char(char),
    Any,
    Class(Vec<ClassItem>, bool),
    Start,
    End,
    WordBoundary(bool),
    Group(Box<Node>, Option<usize>),
    Concat(Vec<Node>),
    Alt(Vec<Node>),
    Repeat {
        node: Box<Node>,
        min: u32,
        max: u32,
        greedy: bool,
    },
}

pub struct Regex {
    root: Node,
    groups: usize,
    icase: bool,
}

struct Parser<'a> {
    p: &'a [char],
    i: usize,
    groups: usize,
    depth: usize,
}

type PResult<T> = Result<T, ErrorKind>;

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.p.get(self.i).copied()
    }

    fn alt(&mut self) -> PResult<Node> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(ErrorKind::Value);
        }
        let mut branches = vec![self.concat()?];
        while self.peek() == Some('|') {
            self.i += 1;
            branches.push(self.concat()?);
        }
        self.depth -= 1;
        Ok(if branches.len() == 1 {
            branches.pop().unwrap_or(Node::Concat(Vec::new()))
        } else {
            Node::Alt(branches)
        })
    }

    fn concat(&mut self) -> PResult<Node> {
        let mut items = Vec::new();
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            let atom = self.atom()?;
            items.push(self.quantified(atom)?);
        }
        Ok(Node::Concat(items))
    }

    fn number(&mut self) -> Option<u32> {
        let start = self.i;
        while matches!(self.peek(), Some('0'..='9')) {
            self.i += 1;
        }
        self.p
            .get(start..self.i)
            .and_then(|s| s.iter().collect::<String>().parse().ok())
    }

    fn quantified(&mut self, atom: Node) -> PResult<Node> {
        let (min, max) = match self.peek() {
            Some('*') => (0, u32::MAX),
            Some('+') => (1, u32::MAX),
            Some('?') => (0, 1),
            Some('{') => {
                let save = self.i;
                self.i += 1;
                let Some(min) = self.number() else {
                    self.i = save;
                    return Ok(atom);
                };
                let max = if self.peek() == Some(',') {
                    self.i += 1;
                    self.number().unwrap_or(u32::MAX)
                } else {
                    min
                };
                if self.peek() != Some('}') || max < min || min > MAX_REPEAT {
                    return Err(ErrorKind::Value);
                }
                (min, max)
            }
            _ => return Ok(atom),
        };
        self.i += 1;
        if matches!(atom, Node::Start | Node::End | Node::WordBoundary(_)) {
            return Err(ErrorKind::Value);
        }
        let greedy = if self.peek() == Some('?') {
            self.i += 1;
            false
        } else {
            if self.peek() == Some('+') {
                self.i += 1;
            }
            true
        };
        Ok(Node::Repeat {
            node: Box::new(atom),
            min,
            max,
            greedy,
        })
    }

    fn escape_item(c: char) -> Option<ClassItem> {
        Some(match c {
            'd' => ClassItem::Digit(true),
            'D' => ClassItem::Digit(false),
            'w' => ClassItem::Word(true),
            'W' => ClassItem::Word(false),
            's' => ClassItem::Space(true),
            'S' => ClassItem::Space(false),
            _ => return None,
        })
    }

    fn escaped_char(c: char) -> char {
        match c {
            'n' => '\n',
            't' => '\t',
            'r' => '\r',
            other => other,
        }
    }

    fn atom(&mut self) -> PResult<Node> {
        let c = self.peek().ok_or(ErrorKind::Value)?;
        self.i += 1;
        Ok(match c {
            '.' => Node::Any,
            '^' => Node::Start,
            '$' => Node::End,
            '(' => {
                let cap = if self.p.get(self.i..self.i + 2) == Some(&['?', ':']) {
                    self.i += 2;
                    None
                } else if self.peek() == Some('?') {
                    return Err(ErrorKind::Value);
                } else {
                    self.groups += 1;
                    Some(self.groups)
                };
                let inner = self.alt()?;
                if self.peek() != Some(')') {
                    return Err(ErrorKind::Value);
                }
                self.i += 1;
                Node::Group(Box::new(inner), cap)
            }
            '[' => self.class()?,
            '\\' => {
                let e = self.peek().ok_or(ErrorKind::Value)?;
                self.i += 1;
                match e {
                    'b' => Node::WordBoundary(true),
                    'B' => Node::WordBoundary(false),
                    _ => match Self::escape_item(e) {
                        Some(item) => Node::Class(vec![item], false),
                        None => Node::Char(Self::escaped_char(e)),
                    },
                }
            }
            '*' | '+' | '?' | ')' => return Err(ErrorKind::Value),
            other => Node::Char(other),
        })
    }

    fn class(&mut self) -> PResult<Node> {
        let mut neg = false;
        if self.peek() == Some('^') {
            neg = true;
            self.i += 1;
        }
        let mut items = Vec::new();
        let mut first = true;
        loop {
            let c = self.peek().ok_or(ErrorKind::Value)?;
            self.i += 1;
            if c == ']' && !first {
                break;
            }
            first = false;
            let lo = if c == '\\' {
                let e = self.peek().ok_or(ErrorKind::Value)?;
                self.i += 1;
                if let Some(item) = Self::escape_item(e) {
                    items.push(item);
                    continue;
                }
                Self::escaped_char(e)
            } else {
                c
            };
            if self.peek() == Some('-') && self.p.get(self.i + 1).is_some_and(|n| *n != ']') {
                self.i += 1;
                let mut hi = self.peek().ok_or(ErrorKind::Value)?;
                self.i += 1;
                if hi == '\\' {
                    hi = Self::escaped_char(self.peek().ok_or(ErrorKind::Value)?);
                    self.i += 1;
                }
                if hi < lo {
                    return Err(ErrorKind::Value);
                }
                items.push(ClassItem::Range(lo, hi));
            } else {
                items.push(ClassItem::Range(lo, lo));
            }
        }
        Ok(Node::Class(items, neg))
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

type Caps = Vec<Option<(usize, usize)>>;

struct Matcher<'a> {
    t: &'a [char],
    icase: bool,
    steps: u64,
    depth: usize,
    exhausted: bool,
}

impl Matcher<'_> {
    fn tick(&mut self) -> bool {
        self.steps += 1;
        if self.steps > MAX_STEPS {
            self.exhausted = true;
        }
        !self.exhausted
    }

    fn eq(&self, a: char, b: char) -> bool {
        a == b || (self.icase && a.to_lowercase().eq(b.to_lowercase()))
    }

    fn class_has(&self, items: &[ClassItem], c: char) -> bool {
        items.iter().any(|it| match it {
            ClassItem::Range(lo, hi) => {
                (*lo <= c && c <= *hi)
                    || (self.icase
                        && c.to_lowercase()
                            .chain(c.to_uppercase())
                            .any(|x| *lo <= x && x <= *hi))
            }
            ClassItem::Digit(p) => c.is_ascii_digit() == *p,
            ClassItem::Word(p) => is_word(c) == *p,
            ClassItem::Space(p) => c.is_whitespace() == *p,
        })
    }

    /// Single-character node test at `pos` (None if the node is not single-character).
    fn single(&self, n: &Node, pos: usize) -> Option<bool> {
        let c = self.t.get(pos).copied();
        match n {
            Node::Char(x) => Some(c.is_some_and(|c| self.eq(c, *x))),
            Node::Any => Some(c.is_some_and(|c| c != '\n')),
            Node::Class(items, neg) => Some(c.is_some_and(|c| self.class_has(items, c) != *neg)),
            _ => None,
        }
    }

    fn m(&mut self, n: &Node, pos: usize, caps: &mut Caps, k: &mut dyn FnMut(&mut Self, usize, &mut Caps) -> bool) -> bool {
        if !self.tick() || self.depth > 600 {
            self.exhausted = true;
            return false;
        }
        self.depth += 1;
        let r = self.m_inner(n, pos, caps, k);
        self.depth -= 1;
        r
    }

    fn m_inner(&mut self, n: &Node, pos: usize, caps: &mut Caps, k: &mut dyn FnMut(&mut Self, usize, &mut Caps) -> bool) -> bool {
        if let Some(ok) = self.single(n, pos) {
            return ok && k(self, pos + 1, caps);
        }
        match n {
            Node::Start => pos == 0 && k(self, pos, caps),
            Node::End => pos == self.t.len() && k(self, pos, caps),
            Node::WordBoundary(want) => {
                let before = pos > 0 && self.t.get(pos - 1).is_some_and(|c| is_word(*c));
                let after = self.t.get(pos).is_some_and(|c| is_word(*c));
                ((before != after) == *want) && k(self, pos, caps)
            }
            Node::Group(inner, cap) => {
                let idx = *cap;
                self.m(inner, pos, caps, &mut |me: &mut Self, end, caps: &mut Caps| {
                    let saved = idx.and_then(|i| caps.get(i).copied().flatten());
                    if let Some(slot) = idx.and_then(|i| caps.get_mut(i)) {
                        *slot = Some((pos, end));
                    }
                    if k(me, end, caps) {
                        return true;
                    }
                    if let Some(slot) = idx.and_then(|i| caps.get_mut(i)) {
                        *slot = saved;
                    }
                    false
                })
            }
            Node::Concat(items) => self.concat(items, 0, pos, caps, k),
            Node::Alt(branches) => {
                for b in branches {
                    if self.m(b, pos, caps, k) {
                        return true;
                    }
                    if self.exhausted {
                        return false;
                    }
                }
                false
            }
            Node::Repeat {
                node,
                min,
                max,
                greedy,
            } => {
                if self.single(node, pos).is_some() {
                    // Fast path: count matching characters, then try continuations.
                    let mut cnt = 0u32;
                    while cnt < *max && self.single(node, pos + cnt as usize) == Some(true) {
                        cnt += 1;
                        if !self.tick() {
                            return false;
                        }
                    }
                    if cnt < *min {
                        return false;
                    }
                    let order: Box<dyn Iterator<Item = u32>> = if *greedy {
                        Box::new((*min..=cnt).rev())
                    } else {
                        Box::new(*min..=cnt)
                    };
                    for c in order {
                        if k(self, pos + c as usize, caps) {
                            return true;
                        }
                        if !self.tick() {
                            return false;
                        }
                    }
                    false
                } else {
                    self.rep(node, *min, *max, *greedy, 0, pos, caps, k)
                }
            }
            _ => false,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn rep(
        &mut self,
        node: &Node,
        min: u32,
        max: u32,
        greedy: bool,
        count: u32,
        pos: usize,
        caps: &mut Caps,
        k: &mut dyn FnMut(&mut Self, usize, &mut Caps) -> bool,
    ) -> bool {
        let more = |me: &mut Self, caps: &mut Caps, k: &mut dyn FnMut(&mut Self, usize, &mut Caps) -> bool| -> bool {
            if count >= max {
                return false;
            }
            me.m(node, pos, caps, &mut |me2: &mut Self, end, caps: &mut Caps| {
                // Empty iterations past the minimum cannot make progress.
                if end == pos && count >= min {
                    return false;
                }
                me2.rep(node, min, max, greedy, count + 1, end, caps, k)
            })
        };
        if count < min {
            return more(self, caps, k);
        }
        if greedy {
            more(self, caps, k) || (!self.exhausted && k(self, pos, caps))
        } else {
            k(self, pos, caps) || (!self.exhausted && more(self, caps, k))
        }
    }

    fn concat(&mut self, items: &[Node], i: usize, pos: usize, caps: &mut Caps, k: &mut dyn FnMut(&mut Self, usize, &mut Caps) -> bool) -> bool {
        match items.get(i) {
            None => k(self, pos, caps),
            Some(n) => self.m(n, pos, caps, &mut |me: &mut Self, p, caps: &mut Caps| {
                me.concat(items, i + 1, p, caps, k)
            }),
        }
    }
}

/// A match: whole span plus capture-group spans (char indices).
pub struct Match {
    pub span: (usize, usize),
    pub groups: Caps,
}

impl Regex {
    pub fn new(pattern: &str, icase: bool) -> Result<Regex, ErrorKind> {
        let p: Vec<char> = pattern.chars().collect();
        if p.len() > MAX_PATTERN {
            return Err(ErrorKind::Value);
        }
        let mut parser = Parser {
            p: &p,
            i: 0,
            groups: 0,
            depth: 0,
        };
        let root = parser.alt()?;
        if parser.i != p.len() {
            return Err(ErrorKind::Value);
        }
        Ok(Regex {
            root,
            groups: parser.groups,
            icase,
        })
    }

    /// Leftmost match starting at or after `from`.
    pub fn find_at(&self, t: &[char], from: usize) -> Result<Option<Match>, ErrorKind> {
        let mut m = Matcher {
            t,
            icase: self.icase,
            steps: 0,
            depth: 0,
            exhausted: false,
        };
        for start in from..=t.len() {
            let mut caps: Caps = vec![None; self.groups + 1];
            let mut end = None;
            let ok = m.m(&self.root, start, &mut caps, &mut |_, e, caps: &mut Caps| {
                end = Some(e);
                if let Some(c0) = caps.get_mut(0) {
                    *c0 = Some((start, e));
                }
                true
            });
            if m.exhausted {
                return Err(ErrorKind::Value);
            }
            if ok && let Some(e) = end {
                return Ok(Some(Match {
                    span: (start, e),
                    groups: caps,
                }));
            }
        }
        Ok(None)
    }

    pub fn find_all(&self, t: &[char]) -> Result<Vec<Match>, ErrorKind> {
        let mut out = Vec::new();
        let mut from = 0;
        while from <= t.len() {
            let Some(m) = self.find_at(t, from)? else { break };
            from = if m.span.1 > m.span.0 { m.span.1 } else { m.span.1 + 1 };
            out.push(m);
            if out.len() > 100_000 {
                return Err(ErrorKind::Value);
            }
        }
        Ok(out)
    }
}

fn slice(t: &[char], s: (usize, usize)) -> String {
    t.get(s.0..s.1).map(|x| x.iter().collect()).unwrap_or_default()
}

/// Expand `$0`..`$99` / `${n}` in a replacement string.
fn expand(rep: &str, t: &[char], m: &Match) -> String {
    let r: Vec<char> = rep.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while let Some(&c) = r.get(i) {
        if c == '$' {
            let mut j = i + 1;
            let braced = r.get(j) == Some(&'{');
            if braced {
                j += 1;
            }
            let start = j;
            while r.get(j).is_some_and(|d| d.is_ascii_digit()) && j - start < 2 {
                j += 1;
            }
            if j > start && (!braced || r.get(j) == Some(&'}')) {
                let n: usize = r
                    .get(start..j)
                    .map(|d| d.iter().collect::<String>())
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                if let Some(Some(span)) = m.groups.get(n) {
                    out.push_str(&slice(t, *span));
                }
                i = if braced { j + 1 } else { j };
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

// ── worksheet functions ──────────────────────────────────────────────────────────────────

fn text_arg(ev: &mut Ev<'_>, args: &[Expr], i: usize) -> Result<String, ErrorKind> {
    ev.text(args.get(i).ok_or(ErrorKind::Value)?)
}

fn int_or(ev: &mut Ev<'_>, args: &[Expr], i: usize, d: f64) -> Result<i64, ErrorKind> {
    let n = ev.num_or(args, i, d)?;
    if !n.is_finite() || n.abs() > 1e9 {
        return Err(ErrorKind::Value);
    }
    Ok(n.trunc() as i64)
}

fn compile(ev: &mut Ev<'_>, args: &[Expr], pi: usize, ci: usize) -> Result<Regex, ErrorKind> {
    let pat = text_arg(ev, args, pi)?;
    let icase = match int_or(ev, args, ci, 0.0)? {
        0 => false,
        1 => true,
        _ => return Err(ErrorKind::Value),
    };
    Regex::new(&pat, icase)
}

fn regextest(ev: &mut Ev<'_>, args: &[Expr]) -> Result<EvalValue, ErrorKind> {
    let t: Vec<char> = text_arg(ev, args, 0)?.chars().collect();
    let re = compile(ev, args, 1, 2)?;
    Ok(EvalValue::bool(re.find_at(&t, 0)?.is_some()))
}

fn regexextract(ev: &mut Ev<'_>, args: &[Expr]) -> Result<EvalValue, ErrorKind> {
    let t: Vec<char> = text_arg(ev, args, 0)?.chars().collect();
    let mode = int_or(ev, args, 2, 0.0)?;
    let re = compile(ev, args, 1, 3)?;
    match mode {
        0 => match re.find_at(&t, 0)? {
            Some(m) => Ok(EvalValue::text(slice(&t, m.span))),
            None => Err(ErrorKind::NA),
        },
        1 => {
            let all = re.find_all(&t)?;
            if all.is_empty() {
                return Err(ErrorKind::NA);
            }
            let data: Vec<Value> = all.iter().map(|m| Value::text(slice(&t, m.span))).collect();
            Ok(EvalValue::Array(Array2::new(data.len(), 1, data)))
        }
        2 => {
            let m = re.find_at(&t, 0)?.ok_or(ErrorKind::NA)?;
            let data: Vec<Value> = m
                .groups
                .iter()
                .skip(1)
                .map(|g| match g {
                    Some(s) => Value::text(slice(&t, *s)),
                    None => Value::text(""),
                })
                .collect();
            if data.is_empty() {
                return Err(ErrorKind::NA);
            }
            Ok(EvalValue::Array(Array2::new(1, data.len(), data)))
        }
        _ => Err(ErrorKind::Value),
    }
}

fn regexreplace(ev: &mut Ev<'_>, args: &[Expr]) -> Result<EvalValue, ErrorKind> {
    let t: Vec<char> = text_arg(ev, args, 0)?.chars().collect();
    let rep = text_arg(ev, args, 2)?;
    let occ = int_or(ev, args, 3, 0.0)?;
    let re = compile(ev, args, 1, 4)?;
    let all = re.find_all(&t)?;
    let n = all.len() as i64;
    let pick = |i: i64| -> bool {
        match occ {
            0 => true,
            o if o > 0 => i + 1 == o,
            o => i == n + o,
        }
    };
    let mut out = String::new();
    let mut last = 0;
    for (i, m) in all.iter().enumerate() {
        if !pick(i as i64) {
            continue;
        }
        out.push_str(&slice(&t, (last, m.span.0)));
        out.push_str(&expand(&rep, &t, m));
        last = m.span.1;
    }
    out.push_str(&slice(&t, (last, t.len())));
    Ok(EvalValue::text(out))
}

fn wrap(r: Result<EvalValue, ErrorKind>) -> EvalValue {
    r.unwrap_or_else(EvalValue::err)
}

pub fn register(r: &mut Registry) {
    r.add("REGEXTEST", 2, 3, |ev, a| wrap(regextest(ev, a)));
    r.add("REGEXEXTRACT", 2, 4, |ev, a| wrap(regexextract(ev, a)));
    r.add("REGEXREPLACE", 3, 5, |ev, a| wrap(regexreplace(ev, a)));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(p: &str, t: &str) -> Option<String> {
        let tc: Vec<char> = t.chars().collect();
        Regex::new(p, false)
            .unwrap()
            .find_at(&tc, 0)
            .unwrap()
            .map(|m| slice(&tc, m.span))
    }

    #[test]
    fn basics() {
        assert_eq!(find("[A-Z]her", "Mr. Sherlock").as_deref(), Some("Sher"));
        assert_eq!(find("[0-9]+", "abc"), None);
        assert_eq!(find(r"\d{2,3}", "a12345").as_deref(), Some("123"));
        assert_eq!(find("a.*?c", "abcbc").as_deref(), Some("abc"));
        assert_eq!(find("(cat|dog)s?$", "hotdogs").as_deref(), Some("dogs"));
        assert_eq!(find(r"\bPlanet", "LonelyPlanet"), None);
        assert_eq!(find("[A-Z][a-z]+$", "LonelyPlanet").as_deref(), Some("Planet"));
    }

    #[test]
    fn replace_groups() {
        let t: Vec<char> = "JamesBond".chars().collect();
        let re = Regex::new("([A-Z][a-z]+)([A-Z][a-z]+)", false).unwrap();
        let m = re.find_at(&t, 0).unwrap().unwrap();
        assert_eq!(expand("$2, $1 $2", &t, &m), "Bond, James Bond");
    }

    #[test]
    fn hostile_patterns_are_bounded() {
        let t: Vec<char> = "a".repeat(40).chars().chain("!".chars()).collect();
        let re = Regex::new("(a+)+b", false).unwrap();
        assert!(re.find_at(&t, 0).is_err() || re.find_at(&t, 0).unwrap().is_none());
        assert!(Regex::new("(((", false).is_err());
        assert!(Regex::new("a{5,2}", false).is_err());
        assert!(Regex::new(&"(".repeat(5000), false).is_err());
        let long: Vec<char> = "x".repeat(20_000).chars().collect();
        let re = Regex::new("(x|y)*z", false).unwrap();
        let _ = re.find_at(&long, 0);
    }

    #[test]
    fn case_insensitive() {
        let t: Vec<char> = "HELLO".chars().collect();
        assert!(Regex::new("hel+o", true).unwrap().find_at(&t, 0).unwrap().is_some());
        assert!(Regex::new("[a-z]+", true).unwrap().find_at(&t, 0).unwrap().is_some());
    }
}
