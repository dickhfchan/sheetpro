//! Find & Replace text matching: optional case sensitivity, whole-cell matching and the
//! spreadsheet wildcards `*` (any run of characters), `?` (any one character) and `~`
//! (escapes the next character, so `~*` finds a literal `*`).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tok {
    Lit(char),
    Any,
    Star,
}

/// A compiled search pattern.
#[derive(Debug, Clone)]
pub struct Pattern {
    toks: Vec<Tok>,
    match_case: bool,
    whole: bool,
    /// The folded text when the pattern has no wildcards: matched without the NFA.
    literal: Option<String>,
}

fn fold(c: char, match_case: bool) -> char {
    if match_case {
        c
    } else {
        // One char in, one char out (multi-char lowercase forms keep their first char).
        c.to_lowercase().next().unwrap_or(c)
    }
}

impl Pattern {
    pub fn new(what: &str, match_case: bool, whole: bool) -> Self {
        let mut toks = Vec::new();
        let mut it = what.chars();
        while let Some(c) = it.next() {
            toks.push(match c {
                '~' => match it.next() {
                    Some(n) => Tok::Lit(fold(n, match_case)),
                    None => Tok::Lit('~'),
                },
                '*' => Tok::Star,
                '?' => Tok::Any,
                c => Tok::Lit(fold(c, match_case)),
            });
        }
        let literal = toks
            .iter()
            .map(|t| match t {
                Tok::Lit(c) => Some(*c),
                _ => None,
            })
            .collect::<Option<String>>();
        Pattern {
            toks,
            match_case,
            whole,
            literal,
        }
    }

    /// Add `i` and every state reachable from it through `*` (which may match nothing).
    fn close(&self, i: usize, set: &mut [bool]) {
        let mut i = i;
        while let Some(s) = set.get_mut(i) {
            if *s {
                return;
            }
            *s = true;
            match self.toks.get(i) {
                Some(Tok::Star) => i += 1,
                _ => return,
            }
        }
    }

    /// End (exclusive) of the longest match of the pattern starting at `start`.
    fn longest_from(&self, t: &[char], start: usize) -> Option<usize> {
        let n = self.toks.len();
        let mut cur = vec![false; n + 1];
        self.close(0, &mut cur);
        let mut best = cur.get(n).copied().unwrap_or(false).then_some(start);
        for (off, c) in t.get(start..)?.iter().enumerate() {
            let mut next = vec![false; n + 1];
            let mut any = false;
            for (i, on) in cur.iter().enumerate() {
                if !on {
                    continue;
                }
                match self.toks.get(i) {
                    Some(Tok::Lit(x)) if *x == *c => self.close(i + 1, &mut next),
                    Some(Tok::Any) => self.close(i + 1, &mut next),
                    Some(Tok::Star) => self.close(i, &mut next),
                    _ => continue,
                }
                any = true;
            }
            if !any {
                break;
            }
            if next.get(n).copied().unwrap_or(false) {
                best = Some(start + off + 1);
            }
            cur = next;
        }
        best
    }

    /// `is_match` for a wildcard-free pattern (most searches), without allocating for
    /// ASCII text: Find All runs this over every cell.
    fn literal_match(&self, lit: &str, text: &str) -> bool {
        if self.match_case {
            return if self.whole {
                text == lit
            } else {
                text.contains(lit)
            };
        }
        if text.is_ascii() && lit.is_ascii() {
            let (t, l) = (text.as_bytes(), lit.as_bytes());
            return if self.whole {
                t.eq_ignore_ascii_case(l)
            } else {
                l.is_empty() || t.windows(l.len()).any(|w| w.eq_ignore_ascii_case(l))
            };
        }
        let folded: String = text.chars().map(|c| fold(c, false)).collect();
        if self.whole {
            folded == *lit
        } else {
            folded.contains(lit)
        }
    }

    fn chars(&self, text: &str) -> Vec<char> {
        text.chars().map(|c| fold(c, self.match_case)).collect()
    }

    /// Does `text` match (the whole text, or somewhere inside it)?
    pub fn is_match(&self, text: &str) -> bool {
        if let Some(lit) = &self.literal {
            return self.literal_match(lit, text);
        }
        let t = self.chars(text);
        if self.whole {
            return self.longest_from(&t, 0) == Some(t.len());
        }
        (0..=t.len()).any(|s| {
            self.longest_from(&t, s)
                .is_some_and(|e| e > s || self.toks.is_empty())
        })
    }

    /// Replace every (leftmost, longest, non-overlapping) match in `text` with `with`;
    /// `None` when nothing matched.
    pub fn replace(&self, text: &str, with: &str) -> Option<String> {
        let folded = self.chars(text);
        let orig: Vec<char> = text.chars().collect();
        if self.whole {
            return (self.longest_from(&folded, 0) == Some(folded.len())).then(|| with.to_string());
        }
        let mut out = String::new();
        let mut i = 0;
        let mut found = false;
        while i < folded.len() {
            match self.longest_from(&folded, i).filter(|e| *e > i) {
                Some(e) => {
                    out.push_str(with);
                    found = true;
                    i = e;
                }
                None => {
                    if let Some(c) = orig.get(i) {
                        out.push(*c);
                    }
                    i += 1;
                }
            }
        }
        found.then_some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards_case_and_whole() {
        let p = Pattern::new("a*e", false, false);
        assert!(p.is_match("Apple"));
        assert!(!p.is_match("xyz"));
        assert!(Pattern::new("?pple", false, true).is_match("apple"));
        assert!(!Pattern::new("?pple", false, true).is_match("pineapple"));
        assert!(Pattern::new("a~*b", false, false).is_match("xa*b"));
        assert!(!Pattern::new("a~*b", false, false).is_match("axb"));
        assert!(!Pattern::new("Apple", true, false).is_match("apple"));
        assert!(Pattern::new("*", false, true).is_match(""));
    }

    #[test]
    fn literal_fast_path_agrees_with_the_matcher() {
        // The same pattern through the fast path and through the wildcard matcher
        // (a trailing `*` forces the NFA without changing what matches, when not whole).
        let texts = [
            "Apples",
            "apple pie",
            "APPLE",
            "",
            "pineapple",
            "ÄPFEL äpfel",
            "Kelvin K",
            "a~b",
        ];
        // (Not the empty pattern: `*` deliberately skips empty cells, `""` matches them.)
        for what in ["apple", "APPLE", "äpfel", "k", "~~"] {
            for case in [false, true] {
                let fast = Pattern::new(what, case, false);
                let slow = Pattern::new(&format!("{what}*"), case, false);
                assert!(fast.literal.is_some() && slow.literal.is_none());
                for t in texts {
                    assert_eq!(
                        fast.is_match(t),
                        slow.is_match(t),
                        "{what:?} in {t:?} case={case}"
                    );
                }
            }
        }
        assert!(Pattern::new("apple", false, true).is_match("APPLE"));
        assert!(!Pattern::new("apple", false, true).is_match("apples"));
        assert!(Pattern::new("äpfel", false, true).is_match("ÄPFEL"));
        assert!(!Pattern::new("Apple", true, true).is_match("apple"));
        assert!(Pattern::new("", false, true).is_match(""));
    }

    #[test]
    fn replace_matches() {
        let p = Pattern::new("c?t", false, false);
        assert_eq!(p.replace("cat hat CUT", "X").as_deref(), Some("X hat X"));
        assert_eq!(p.replace("dog", "X"), None);
        let p = Pattern::new("strasse", false, false);
        assert_eq!(
            p.replace("STRASSE straße", "road").as_deref(),
            Some("road straße")
        );
        let p = Pattern::new("cat", true, true);
        assert_eq!(p.replace("cat hat", "dog"), None);
        assert_eq!(p.replace("cat", "dog").as_deref(), Some("dog"));
        // Non-ASCII case folding keeps the rest of the text intact.
        let p = Pattern::new("é", false, false);
        assert_eq!(p.replace("CAFÉ é", "e").as_deref(), Some("CAFe e"));
    }
}
