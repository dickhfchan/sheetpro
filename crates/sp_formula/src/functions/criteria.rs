//! Criteria strings for COUNTIF / SUMIF / AVERAGEIF / *IFS / database functions.

use crate::eval::text_to_number;
use sp_core::{ErrorKind, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone)]
pub struct Criterion {
    op: Op,
    target: Value,
}

/// Wildcard match: `*` any run, `?` one char, `~` escapes. Case-insensitive.
pub fn wildcard_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    // Tokenise pattern into (char, is_wild) with escapes resolved.
    let mut toks: Vec<(char, bool)> = Vec::with_capacity(p.len());
    let mut i = 0;
    while let Some(&c) = p.get(i) {
        if c == '~' && matches!(p.get(i + 1), Some('*' | '?' | '~')) {
            if let Some(&n) = p.get(i + 1) {
                toks.push((n, false));
            }
            i += 2;
        } else {
            toks.push((c, c == '*' || c == '?'));
            i += 1;
        }
    }
    // Iterative glob matching with backtracking on the last '*'.
    let (mut ti, mut pi) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        match toks.get(pi) {
            Some(('?', true)) => {
                ti += 1;
                pi += 1;
            }
            Some(('*', true)) => {
                star = Some((pi, ti));
                pi += 1;
            }
            Some((c, false)) if t.get(ti) == Some(c) => {
                ti += 1;
                pi += 1;
            }
            _ => match star {
                Some((sp, st)) => {
                    pi = sp + 1;
                    ti = st + 1;
                    star = Some((sp, st + 1));
                }
                None => return false,
            },
        }
    }
    toks.get(pi..)
        .unwrap_or(&[])
        .iter()
        .all(|t| *t == ('*', true))
}

impl Criterion {
    pub fn parse(v: &Value) -> Criterion {
        let s = match v {
            Value::Text(s) => s.to_string(),
            other => {
                return Criterion {
                    op: Op::Eq,
                    target: other.clone(),
                };
            }
        };
        let (op, rest) = if let Some(r) = s.strip_prefix("<=") {
            (Op::Le, r)
        } else if let Some(r) = s.strip_prefix(">=") {
            (Op::Ge, r)
        } else if let Some(r) = s.strip_prefix("<>") {
            (Op::Ne, r)
        } else if let Some(r) = s.strip_prefix('<') {
            (Op::Lt, r)
        } else if let Some(r) = s.strip_prefix('>') {
            (Op::Gt, r)
        } else if let Some(r) = s.strip_prefix('=') {
            (Op::Eq, r)
        } else {
            (Op::Eq, s.as_str())
        };
        let target = if let Some(n) = text_to_number(rest) {
            Value::Number(n)
        } else if rest.eq_ignore_ascii_case("TRUE") {
            Value::Bool(true)
        } else if rest.eq_ignore_ascii_case("FALSE") {
            Value::Bool(false)
        } else if let Some(e) = ErrorKind::parse(rest) {
            Value::Error(e)
        } else {
            Value::text(rest)
        };
        Criterion { op, target }
    }

    pub fn matches(&self, v: &Value) -> bool {
        use std::cmp::Ordering::*;
        match (&self.target, v) {
            // Blank criterion: "=" matches empty cells; "" matches empty and empty text.
            (Value::Text(t), _) if t.is_empty() => {
                let blank =
                    matches!(v, Value::Empty) || matches!(v, Value::Text(s) if s.is_empty());
                match self.op {
                    Op::Eq => blank,
                    Op::Ne => !blank,
                    _ => false,
                }
            }
            (Value::Number(n), _) => {
                let x = match v {
                    Value::Number(x) => Some(*x),
                    Value::Text(s) if self.op == Op::Eq || self.op == Op::Ne => text_to_number(s),
                    _ => None,
                };
                match x {
                    Some(x) => {
                        let o = x.partial_cmp(n).unwrap_or(Equal);
                        self.cmp_ok(o)
                    }
                    None => self.op == Op::Ne,
                }
            }
            (Value::Text(t), Value::Text(s)) => match self.op {
                Op::Eq => wildcard_match(t, s),
                Op::Ne => !wildcard_match(t, s),
                _ => {
                    let o = s.to_lowercase().cmp(&t.to_lowercase());
                    self.cmp_ok(o)
                }
            },
            (Value::Text(_), Value::Empty) => self.op == Op::Ne,
            (Value::Text(_), _) => self.op == Op::Ne,
            (Value::Bool(b), Value::Bool(x)) => self.cmp_ok(x.cmp(b)),
            (Value::Error(e), Value::Error(x)) => (e == x) == (self.op == Op::Eq),
            (Value::Empty, Value::Empty) => self.op == Op::Eq,
            (Value::Empty, Value::Number(x)) => self.cmp_ok(x.partial_cmp(&0.0).unwrap_or(Equal)),
            _ => self.op == Op::Ne,
        }
    }

    fn cmp_ok(&self, o: std::cmp::Ordering) -> bool {
        use std::cmp::Ordering::*;
        match self.op {
            Op::Eq => o == Equal,
            Op::Ne => o != Equal,
            Op::Lt => o == Less,
            Op::Le => o != Greater,
            Op::Gt => o == Greater,
            Op::Ge => o != Less,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(c: &str, v: Value) -> bool {
        Criterion::parse(&Value::text(c)).matches(&v)
    }

    #[test]
    fn criteria() {
        assert!(m(">5", Value::Number(6.0)));
        assert!(!m(">5", Value::Number(5.0)));
        assert!(m("<>a", Value::text("b")));
        assert!(m("ap*", Value::text("Apple")));
        assert!(m("?pple", Value::text("apple")));
        assert!(!m("ap?", Value::text("apple")));
        assert!(m("~*", Value::text("*")));
        assert!(m("5", Value::Number(5.0)));
        assert!(m("", Value::Empty));
        assert!(m("<>", Value::Number(1.0)));
        assert!(!m("<>", Value::Empty));
        assert!(Criterion::parse(&Value::Number(3.0)).matches(&Value::Number(3.0)));
    }

    #[test]
    fn wildcard_star_heavy() {
        assert!(wildcard_match("*a*b*c*", "xxaxxbxxcxx"));
        assert!(!wildcard_match(&"*a".repeat(50), &"a".repeat(30)));
    }
}
