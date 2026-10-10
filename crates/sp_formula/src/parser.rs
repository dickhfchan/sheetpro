//! Lexer + precedence-climbing parser: formula text (without `=`) → `Expr`.

use crate::ast::{BinOp, CellPart, Expr, RefExpr, RefKind, UnOp};
use sp_core::addr::parse_a1_parts;
use sp_core::{ErrorKind, MAX_COLS, MAX_ROWS, letters_to_col};

/// Maximum nesting depth of parentheses / function calls / unary operators.
pub const MAX_DEPTH: usize = 256;
/// Maximum formula length in characters (target app limit).
pub const MAX_FORMULA_LEN: usize = 8192;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(pub String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "formula parse error: {}", self.0)
    }
}

impl std::error::Error for ParseError {}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Str(String),
    Err(ErrorKind),
    /// Identifier-like token (function name, name, bool, or A1 ref), with optional sheet.
    Ident {
        sheet: Option<String>,
        text: String,
    },
    /// Whole-row/col range `A:C` or `1:3`, with optional sheet.
    Span {
        sheet: Option<String>,
        a: String,
        b: String,
    },
    Op(&'static str),
    LParen,
    RParen,
    LBrace,
    RBrace,
    Comma,
    Semi,
    Hash,
}

#[derive(Debug, Clone)]
struct Token {
    tok: Tok,
    space_before: bool,
}

fn strip_prefixes(s: &str) -> String {
    let mut out = s;
    loop {
        let lower = out.to_ascii_lowercase();
        if let Some(p) = ["_xlfn.", "_xlws.", "_xlpm."]
            .iter()
            .find(|p| lower.starts_with(**p))
        {
            out = out.get(p.len()..).unwrap_or("");
        } else {
            break;
        }
    }
    out.to_string()
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_' || c == '\\' || c == '$'
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '.' || c == '$' || c == '\\' || c == '?'
}

fn lex(src: &str) -> Result<Vec<Token>, ParseError> {
    let chars: Vec<char> = src.chars().collect();
    let at = |i: usize| chars.get(i).copied();
    let mut i = 0;
    let mut out: Vec<Token> = Vec::new();
    let mut space = false;
    while let Some(c) = at(i) {
        if c.is_whitespace() {
            space = true;
            i += 1;
            continue;
        }
        let start = i;
        let tok = match c {
            '"' => {
                let mut s = String::new();
                i += 1;
                loop {
                    match at(i) {
                        None => return Err(ParseError("unterminated string".into())),
                        Some('"') if at(i + 1) == Some('"') => {
                            s.push('"');
                            i += 2;
                        }
                        Some('"') => {
                            i += 1;
                            break;
                        }
                        Some(ch) => {
                            s.push(ch);
                            i += 1;
                        }
                    }
                }
                Tok::Str(s)
            }
            '#' => {
                // Error literal, or spill operator after a reference.
                let rest: String = chars
                    .get(i..)
                    .unwrap_or(&[])
                    .iter()
                    .take(16)
                    .collect::<String>()
                    .to_ascii_uppercase();
                if let Some(e) = ErrorKind::ALL.iter().find(|e| rest.starts_with(e.as_str())) {
                    i += e.as_str().chars().count();
                    Tok::Err(*e)
                } else {
                    i += 1;
                    Tok::Hash
                }
            }
            '0'..='9' | '.' => {
                // Whole-row span like 1:3 (optionally $1:$3) takes priority.
                if let Some((t, n)) = try_span(&chars, i, None) {
                    i = n;
                    t
                } else {
                    let mut j = i;
                    while at(j).is_some_and(|c| c.is_ascii_digit() || c == '.') {
                        j += 1;
                    }
                    if matches!(at(j), Some('e' | 'E'))
                        && (at(j + 1).is_some_and(|c| c.is_ascii_digit())
                            || (matches!(at(j + 1), Some('+' | '-'))
                                && at(j + 2).is_some_and(|c| c.is_ascii_digit())))
                    {
                        j += 2;
                        while at(j).is_some_and(|c| c.is_ascii_digit()) {
                            j += 1;
                        }
                    }
                    let s: String = chars.get(i..j).unwrap_or(&[]).iter().collect();
                    let n: f64 = s
                        .parse()
                        .map_err(|_| ParseError(format!("bad number '{s}'")))?;
                    i = j;
                    Tok::Num(n)
                }
            }
            '\'' => {
                // Quoted sheet name: 'My Sheet'!A1
                let mut s = String::new();
                i += 1;
                loop {
                    match at(i) {
                        None => return Err(ParseError("unterminated sheet name".into())),
                        Some('\'') if at(i + 1) == Some('\'') => {
                            s.push('\'');
                            i += 2;
                        }
                        Some('\'') => {
                            i += 1;
                            break;
                        }
                        Some(ch) => {
                            s.push(ch);
                            i += 1;
                        }
                    }
                }
                if at(i) != Some('!') {
                    return Err(ParseError("expected ! after sheet name".into()));
                }
                i += 1;
                lex_after_sheet(&chars, &mut i, s)?
            }
            c if is_ident_start(c) => {
                if let Some((t, n)) = try_span(&chars, i, None) {
                    i = n;
                    t
                } else {
                    let mut j = i;
                    while at(j).is_some_and(is_ident_char) {
                        j += 1;
                    }
                    let text: String = chars.get(i..j).unwrap_or(&[]).iter().collect();
                    i = j;
                    if at(i) == Some('!') {
                        i += 1;
                        lex_after_sheet(&chars, &mut i, text)?
                    } else {
                        Tok::Ident {
                            sheet: None,
                            text: strip_prefixes(&text),
                        }
                    }
                }
            }
            '[' => {
                return Err(ParseError(
                    "structured / external references are not supported yet".into(),
                ));
            }
            '(' => {
                i += 1;
                Tok::LParen
            }
            ')' => {
                i += 1;
                Tok::RParen
            }
            '{' => {
                i += 1;
                Tok::LBrace
            }
            '}' => {
                i += 1;
                Tok::RBrace
            }
            ',' => {
                i += 1;
                Tok::Comma
            }
            ';' => {
                i += 1;
                Tok::Semi
            }
            _ => {
                let two: String = chars.get(i..i + 2).unwrap_or(&[]).iter().collect();
                let op = match two.as_str() {
                    "<=" => Some("<="),
                    ">=" => Some(">="),
                    "<>" => Some("<>"),
                    _ => None,
                };
                if let Some(op) = op {
                    i += 2;
                    Tok::Op(op)
                } else {
                    i += 1;
                    Tok::Op(match c {
                        '+' => "+",
                        '-' => "-",
                        '*' => "*",
                        '/' => "/",
                        '^' => "^",
                        '&' => "&",
                        '=' => "=",
                        '<' => "<",
                        '>' => ">",
                        '%' => "%",
                        ':' => ":",
                        '@' => "@",
                        _ => return Err(ParseError(format!("unexpected character '{c}'"))),
                    })
                }
            }
        };
        debug_assert!(i > start);
        out.push(Token {
            tok,
            space_before: space,
        });
        space = false;
    }
    Ok(out)
}

fn lex_after_sheet(chars: &[char], i: &mut usize, sheet: String) -> Result<Tok, ParseError> {
    if let Some((t, n)) = try_span(chars, *i, Some(sheet.clone())) {
        *i = n;
        return Ok(t);
    }
    if chars.get(*i) == Some(&'#') {
        // Sheet1!#REF!
        let rest: String = chars.get(*i..).unwrap_or(&[]).iter().take(6).collect();
        if rest.eq_ignore_ascii_case("#REF!") {
            *i += 5;
            return Ok(Tok::Err(ErrorKind::Ref));
        }
    }
    let mut j = *i;
    while chars.get(j).is_some_and(|c| is_ident_char(*c)) {
        j += 1;
    }
    let text: String = chars.get(*i..j).unwrap_or(&[]).iter().collect();
    if text.is_empty() {
        return Err(ParseError("expected reference after sheet name".into()));
    }
    *i = j;
    Ok(Tok::Ident {
        sheet: Some(sheet),
        text,
    })
}

/// Recognise `A:C`, `$A:$C`, `1:3`, `$1:$3` at position `i`.
fn try_span(chars: &[char], i: usize, sheet: Option<String>) -> Option<(Tok, usize)> {
    let take_part = |mut j: usize, letters: bool| -> Option<(String, usize)> {
        let s = j;
        if chars.get(j) == Some(&'$') {
            j += 1;
        }
        let body = j;
        while chars.get(j).is_some_and(|c| {
            if letters {
                c.is_ascii_alphabetic()
            } else {
                c.is_ascii_digit()
            }
        }) {
            j += 1;
        }
        if j == body {
            return None;
        }
        Some((chars.get(s..j)?.iter().collect(), j))
    };
    for letters in [true, false] {
        if let Some((a, j)) = take_part(i, letters)
            && chars.get(j) == Some(&':')
            && let Some((b, k)) = take_part(j + 1, letters)
        {
            // The span must end the token (not be the prefix of A1:B2).
            if chars
                .get(k)
                .is_some_and(|c| c.is_alphanumeric() || *c == '_' || *c == '(' || *c == '.')
            {
                continue;
            }
            let ok = if letters {
                letters_to_col(a.trim_start_matches('$')).is_some()
                    && letters_to_col(b.trim_start_matches('$')).is_some()
            } else {
                let p = |s: &str| {
                    s.trim_start_matches('$')
                        .parse::<u32>()
                        .ok()
                        .filter(|n| *n >= 1 && *n <= MAX_ROWS)
                };
                p(&a).is_some() && p(&b).is_some()
            };
            if ok {
                return Some((Tok::Span { sheet, a, b }, k));
            }
        }
    }
    None
}

struct Parser {
    toks: Vec<Token>,
    pos: usize,
    depth: usize,
}

/// Parse formula text (with or without a leading `=`).
pub fn parse(src: &str) -> Result<Expr, ParseError> {
    if src.chars().count() > MAX_FORMULA_LEN {
        return Err(ParseError("formula too long".into()));
    }
    let src = src.strip_prefix('=').unwrap_or(src);
    let toks = lex(src)?;
    let mut p = Parser {
        toks,
        pos: 0,
        depth: 0,
    };
    if p.toks.is_empty() {
        return Err(ParseError("empty formula".into()));
    }
    let e = p.expr(0, false)?;
    if p.pos < p.toks.len() {
        return Err(ParseError(format!(
            "unexpected token {:?}",
            p.toks.get(p.pos).map(|t| &t.tok)
        )));
    }
    Ok(e)
}

fn cell_part(text: &str) -> Option<CellPart> {
    parse_a1_parts(text).map(|(col, col_abs, row, row_abs)| CellPart {
        row,
        col,
        row_abs,
        col_abs,
    })
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.toks.get(self.pos)
    }

    fn next(&mut self) -> Option<Token> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }

    fn enter(&mut self) -> Result<(), ParseError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(ParseError("formula nested too deeply".into()));
        }
        Ok(())
    }

    fn binop_at(&self, in_args: bool) -> Option<BinOp> {
        let t = self.peek()?;
        match &t.tok {
            Tok::Op(o) => Some(match *o {
                "+" => BinOp::Add,
                "-" => BinOp::Sub,
                "*" => BinOp::Mul,
                "/" => BinOp::Div,
                "^" => BinOp::Pow,
                "&" => BinOp::Concat,
                "=" => BinOp::Eq,
                "<>" => BinOp::Ne,
                "<" => BinOp::Lt,
                "<=" => BinOp::Le,
                ">" => BinOp::Gt,
                ">=" => BinOp::Ge,
                ":" => BinOp::Range,
                _ => return None,
            }),
            Tok::Comma if !in_args => Some(BinOp::Union),
            // A space followed by something that starts an operand = intersection.
            Tok::Ident { .. } | Tok::Span { .. } | Tok::LParen if t.space_before => {
                Some(BinOp::Isect)
            }
            _ => None,
        }
    }

    /// `in_args`: commas separate arguments rather than forming unions.
    fn expr(&mut self, min_prec: u8, in_args: bool) -> Result<Expr, ParseError> {
        self.enter()?;
        let mut lhs = self.unary()?;
        loop {
            // Postfix percent binds tighter than any binary operator.
            if matches!(self.peek().map(|t| &t.tok), Some(Tok::Op("%"))) {
                self.pos += 1;
                lhs = Expr::Unary(UnOp::Percent, Box::new(lhs));
                continue;
            }
            if matches!(self.peek().map(|t| &t.tok), Some(Tok::Hash)) {
                self.pos += 1;
                lhs = Expr::Spill(Box::new(lhs));
                continue;
            }
            let Some(op) = self.binop_at(in_args) else {
                break;
            };
            let prec = op.precedence();
            if prec < min_prec {
                break;
            }
            if op != BinOp::Isect {
                self.pos += 1;
            }
            // `^` is left-associative in the target app, like the others.
            let rhs = self.expr(prec + 1, in_args)?;
            lhs = if op == BinOp::Range {
                fold_range(lhs, rhs)
            } else {
                Expr::Binary(op, Box::new(lhs), Box::new(rhs))
            };
        }
        self.depth -= 1;
        Ok(lhs)
    }

    fn unary(&mut self) -> Result<Expr, ParseError> {
        match self.peek().map(|t| t.tok.clone()) {
            Some(Tok::Op("-")) => {
                self.pos += 1;
                self.enter()?;
                // Negation binds tighter than ^ in the target app: -2^2 = 4.
                let e = self.unary()?;
                self.depth -= 1;
                let e = self.postfix(e)?;
                Ok(Expr::Unary(UnOp::Neg, Box::new(e)))
            }
            Some(Tok::Op("+")) => {
                self.pos += 1;
                self.enter()?;
                let e = self.unary()?;
                self.depth -= 1;
                Ok(Expr::Unary(UnOp::Plus, Box::new(e)))
            }
            Some(Tok::Op("@")) => {
                // Implicit-intersection operator: treated as a no-op wrapper.
                self.pos += 1;
                self.unary()
            }
            _ => {
                let p = self.primary()?;
                self.postfix(p)
            }
        }
    }

    /// Range operator binds tighter than unary minus: `-A1:A3` = `-(A1:A3)`.
    fn postfix(&mut self, mut e: Expr) -> Result<Expr, ParseError> {
        while matches!(self.peek().map(|t| &t.tok), Some(Tok::Op(":"))) {
            self.pos += 1;
            let rhs = self.primary()?;
            e = fold_range(e, rhs);
        }
        Ok(e)
    }

    fn primary(&mut self) -> Result<Expr, ParseError> {
        let Some(t) = self.next() else {
            return Err(ParseError("unexpected end of formula".into()));
        };
        match t.tok {
            Tok::Num(n) => Ok(Expr::Number(n)),
            Tok::Str(s) => Ok(Expr::Text(s)),
            Tok::Err(e) => Ok(Expr::Error(e)),
            Tok::LParen => {
                self.enter()?;
                let e = self.expr(0, false)?;
                self.depth -= 1;
                match self.next().map(|t| t.tok) {
                    Some(Tok::RParen) => Ok(Expr::Paren(Box::new(e))),
                    _ => Err(ParseError("expected )".into())),
                }
            }
            Tok::LBrace => self.array(),
            Tok::Span { sheet, a, b } => Ok(span_ref(sheet, &a, &b)),
            Tok::Ident { sheet, text } => {
                if matches!(self.peek().map(|t| &t.tok), Some(Tok::LParen)) && sheet.is_none() {
                    self.pos += 1;
                    return self.call(text.to_ascii_uppercase());
                }
                if sheet.is_none() {
                    if text.eq_ignore_ascii_case("TRUE") {
                        return Ok(Expr::Bool(true));
                    }
                    if text.eq_ignore_ascii_case("FALSE") {
                        return Ok(Expr::Bool(false));
                    }
                }
                match cell_part(&text) {
                    Some(c) => Ok(Expr::Ref(RefExpr {
                        sheet,
                        kind: RefKind::Cell(c),
                    })),
                    None if sheet.is_none() => Ok(Expr::Name(text)),
                    None => Err(ParseError(format!("bad reference '{text}'"))),
                }
            }
            Tok::Op(o) => Err(ParseError(format!("unexpected operator '{o}'"))),
            other => Err(ParseError(format!("unexpected token {other:?}"))),
        }
    }

    fn call(&mut self, name: String) -> Result<Expr, ParseError> {
        self.enter()?;
        let mut args = Vec::new();
        if matches!(self.peek().map(|t| &t.tok), Some(Tok::RParen)) {
            self.pos += 1;
            self.depth -= 1;
            return Ok(Expr::Call(name, args));
        }
        loop {
            let arg = match self.peek().map(|t| &t.tok) {
                Some(Tok::Comma) | Some(Tok::RParen) => Expr::Missing,
                _ => self.expr(0, true)?,
            };
            args.push(arg);
            if args.len() > 255 {
                return Err(ParseError("too many arguments".into()));
            }
            match self.next().map(|t| t.tok) {
                Some(Tok::Comma) => continue,
                Some(Tok::RParen) => break,
                _ => return Err(ParseError(format!("expected , or ) in {name}()"))),
            }
        }
        self.depth -= 1;
        Ok(Expr::Call(name, args))
    }

    fn array(&mut self) -> Result<Expr, ParseError> {
        let mut rows = vec![Vec::new()];
        loop {
            let neg = matches!(self.peek().map(|t| &t.tok), Some(Tok::Op("-")));
            if neg {
                self.pos += 1;
            }
            let item = match self.next().map(|t| t.tok) {
                Some(Tok::Num(n)) => Expr::Number(if neg { -n } else { n }),
                Some(Tok::Str(s)) if !neg => Expr::Text(s),
                Some(Tok::Err(e)) if !neg => Expr::Error(e),
                Some(Tok::Ident { sheet: None, text })
                    if !neg && text.eq_ignore_ascii_case("TRUE") =>
                {
                    Expr::Bool(true)
                }
                Some(Tok::Ident { sheet: None, text })
                    if !neg && text.eq_ignore_ascii_case("FALSE") =>
                {
                    Expr::Bool(false)
                }
                _ => return Err(ParseError("invalid array constant".into())),
            };
            if let Some(r) = rows.last_mut() {
                r.push(item);
            }
            match self.next().map(|t| t.tok) {
                Some(Tok::Comma) => {}
                Some(Tok::Semi) => rows.push(Vec::new()),
                Some(Tok::RBrace) => break,
                _ => return Err(ParseError("expected , ; or } in array".into())),
            }
        }
        let w = rows.first().map_or(0, Vec::len);
        if rows.iter().any(|r| r.len() != w) {
            return Err(ParseError("array rows must have equal length".into()));
        }
        Ok(Expr::Array(rows))
    }
}

fn span_ref(sheet: Option<String>, a: &str, b: &str) -> Expr {
    let abs = |s: &str| s.starts_with('$');
    let (ta, tb) = (a.trim_start_matches('$'), b.trim_start_matches('$'));
    if let (Some(c0), Some(c1)) = (letters_to_col(ta), letters_to_col(tb)) {
        let (lo, hi, lo_abs, hi_abs) = if c0 <= c1 {
            (c0, c1, abs(a), abs(b))
        } else {
            (c1, c0, abs(b), abs(a))
        };
        return Expr::Ref(RefExpr {
            sheet,
            kind: RefKind::Cols {
                c0: lo,
                c0_abs: lo_abs,
                c1: hi.min(MAX_COLS - 1),
                c1_abs: hi_abs,
            },
        });
    }
    let r = |s: &str| s.parse::<u32>().unwrap_or(1).saturating_sub(1);
    let (r0, r1) = (r(ta), r(tb));
    let (lo, hi, lo_abs, hi_abs) = if r0 <= r1 {
        (r0, r1, abs(a), abs(b))
    } else {
        (r1, r0, abs(b), abs(a))
    };
    Expr::Ref(RefExpr {
        sheet,
        kind: RefKind::Rows {
            r0: lo,
            r0_abs: lo_abs,
            r1: hi,
            r1_abs: hi_abs,
        },
    })
}

/// `A1:B2` with two plain cell refs on the same sheet becomes a single `Area`.
fn fold_range(lhs: Expr, rhs: Expr) -> Expr {
    if let (
        Expr::Ref(RefExpr {
            sheet: s1,
            kind: RefKind::Cell(a),
        }),
        Expr::Ref(RefExpr {
            sheet: s2,
            kind: RefKind::Cell(b),
        }),
    ) = (&lhs, &rhs)
        && (s2.is_none() || s2 == s1)
    {
        let (mut a, mut b) = (*a, *b);
        if a.row > b.row {
            std::mem::swap(&mut a.row, &mut b.row);
            std::mem::swap(&mut a.row_abs, &mut b.row_abs);
        }
        if a.col > b.col {
            std::mem::swap(&mut a.col, &mut b.col);
            std::mem::swap(&mut a.col_abs, &mut b.col_abs);
        }
        return Expr::Ref(RefExpr {
            sheet: s1.clone(),
            kind: RefKind::Area(a, b),
        });
    }
    Expr::Binary(BinOp::Range, Box::new(lhs), Box::new(rhs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::print::to_formula;

    fn rt(s: &str) -> String {
        to_formula(&parse(s).unwrap_or_else(|e| panic!("{s}: {e}")))
    }

    #[test]
    fn roundtrips() {
        for s in [
            "1+2*3",
            "SUM(A1:B2,$C$3)",
            "IF(A1>0,\"yes\",\"no\")",
            "Sheet2!A1+'My Sheet'!$B$2",
            "-A1^2",
            "A:A",
            "$1:$3",
            "{1,2;3,4}",
            "IF(A1,,2)",
            "50%",
            "\"a\"\"b\"&C1",
            "#N/A",
            "SUM(Sheet1!A1:B3)",
            "STDEV.S(A1:A5)",
            "A1:A3 A2:C2",
            "SUM((A1,B1))",
            "A1#",
            "TRUE",
        ] {
            assert_eq!(rt(s), s, "roundtrip {s}");
        }
    }

    #[test]
    fn precedence() {
        assert_eq!(rt("1+2*3^2"), "1+2*3^2");
        assert!(matches!(
            parse("1+2*3").unwrap(),
            Expr::Binary(BinOp::Add, _, _)
        ));
        // -2^2 is (-2)^2 = 4 in the target app: negation binds tighter than ^.
        assert!(
            matches!(parse("-2^2").unwrap(), Expr::Binary(BinOp::Pow, ref a, _) if matches!(**a, Expr::Unary(UnOp::Neg, _)))
        );
        assert!(matches!(
            parse("1=1&\"\"").unwrap(),
            Expr::Binary(BinOp::Eq, _, _)
        ));
    }

    #[test]
    fn prefixes_and_errors() {
        assert_eq!(rt("_xlfn.CONCAT(A1)"), "CONCAT(A1)");
        assert!(parse("SUM(").is_err());
        assert!(parse("1+").is_err());
        assert!(parse("\"abc").is_err());
        assert!(parse("").is_err());
        assert!(parse(&"(".repeat(1000)).is_err());
        assert!(parse("Table1[Col]").is_err());
    }
}
