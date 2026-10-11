//! Criteria strings for COUNTIF / SUMIF / AVERAGEIF / *IFS / database functions.

use super::Registry;
use crate::ast::Expr;
use crate::eval::{Array2, Ev, EvalValue, SheetRange, text_to_number};
use sp_core::{CellAddr, CellRange, ErrorKind, MAX_CELLS_PER_OP, MAX_COLS, MAX_ROWS, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, PartialEq)]
enum Target {
    /// Criterion text with nothing after the operator (`""`, `"="`, `"<>"`).
    Blank,
    Number(f64),
    Text(String),
    Bool(bool),
    Error(ErrorKind),
}

#[derive(Debug, Clone)]
pub struct Criterion {
    op: Op,
    /// The operator was written out (`"=x"` rather than `"x"`).
    explicit: bool,
    target: Target,
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
        let eq = |target| Criterion {
            op: Op::Eq,
            explicit: false,
            target,
        };
        let s = match v {
            Value::Text(s) => s.to_string(),
            // A blank criterion cell behaves like 0.
            Value::Empty => return eq(Target::Number(0.0)),
            Value::Number(n) => return eq(Target::Number(*n)),
            Value::Bool(b) => return eq(Target::Bool(*b)),
            Value::Error(e) => return eq(Target::Error(*e)),
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
            return Criterion {
                op: Op::Eq,
                explicit: false,
                target: Self::target_of(&s),
            };
        };
        Criterion {
            op,
            explicit: true,
            target: Self::target_of(rest),
        }
    }

    /// Criterion from a database criteria cell: like [`Criterion::parse`], except that text
    /// without an operator matches values beginning with it.
    pub fn parse_database(v: &Value) -> Criterion {
        let mut c = Criterion::parse(v);
        if let (Op::Eq, false, Target::Text(t)) = (c.op, c.explicit, &mut c.target) {
            t.push('*');
        }
        c
    }

    fn target_of(rest: &str) -> Target {
        if rest.is_empty() {
            Target::Blank
        } else if let Some(n) = text_to_number(rest) {
            Target::Number(n)
        } else if rest.eq_ignore_ascii_case("TRUE") {
            Target::Bool(true)
        } else if rest.eq_ignore_ascii_case("FALSE") {
            Target::Bool(false)
        } else if let Some(e) = ErrorKind::parse(rest) {
            Target::Error(e)
        } else {
            Target::Text(rest.to_string())
        }
    }

    /// Whether this criterion matches an empty cell (used to count unstored cells).
    pub fn matches_empty(&self) -> bool {
        self.matches(&Value::Empty)
    }

    pub fn matches(&self, v: &Value) -> bool {
        use std::cmp::Ordering::Equal;
        match &self.target {
            Target::Blank => match self.op {
                Op::Eq if self.explicit => v.is_empty(),
                Op::Eq => v.is_empty() || matches!(v, Value::Text(s) if s.is_empty()),
                Op::Ne => !v.is_empty(),
                _ => false,
            },
            Target::Number(n) => match v {
                Value::Number(x) => self.cmp_ok(x.partial_cmp(n).unwrap_or(Equal)),
                // Equality also accepts numeric text ("=23" counts "23"); "<>" does not.
                Value::Text(s) if self.op == Op::Eq => text_to_number(s) == Some(*n),
                _ => self.op == Op::Ne,
            },
            Target::Text(t) => match v {
                Value::Text(s) => match self.op {
                    Op::Eq => wildcard_match(t, s),
                    Op::Ne => !wildcard_match(t, s),
                    _ => self.cmp_ok(s.to_lowercase().cmp(&t.to_lowercase())),
                },
                _ => self.op == Op::Ne,
            },
            Target::Bool(b) => match v {
                Value::Bool(x) => self.cmp_ok(x.cmp(b)),
                _ => self.op == Op::Ne,
            },
            Target::Error(e) => match v {
                Value::Error(x) => match self.op {
                    Op::Eq => x == e,
                    Op::Ne => x != e,
                    _ => false,
                },
                _ => self.op == Op::Ne,
            },
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

// ── criteria ranges ─────────────────────────────────────────────────────────────────────

/// A criteria/target range argument: a reference (possibly a whole row/column, mostly
/// empty) or an in-memory array.
#[derive(Clone)]
enum Src {
    Ref(SheetRange),
    Arr(Array2),
}

impl Src {
    fn eval(ev: &mut Ev<'_>, e: &Expr) -> Result<Src, ErrorKind> {
        match ev.eval(e) {
            EvalValue::Ref(r) => Ok(Src::Ref(r)),
            EvalValue::Refs(rs) => match rs.as_slice() {
                [r] => Ok(Src::Ref(*r)),
                _ => Err(ErrorKind::Value),
            },
            EvalValue::Scalar(Value::Error(e)) => Err(e),
            EvalValue::Scalar(v) => Ok(Src::Arr(Array2::scalar(v))),
            EvalValue::Array(a) => Ok(Src::Arr(a)),
        }
    }

    /// Logical shape (rows, cols).
    fn dims(&self) -> (u64, u64) {
        match self {
            Src::Ref(r) => (u64::from(r.range.rows()), u64::from(r.range.cols())),
            Src::Arr(a) => (a.rows as u64, a.cols as u64),
        }
    }

    /// Rows/cols from the top-left that can hold stored cells.
    fn extent(&self, ev: &Ev<'_>) -> (u64, u64) {
        match self {
            Src::Ref(r) => match ev.host.used_range(r.sheet) {
                Some(u) => {
                    let rows = (u64::from(u.end.row) + 1).saturating_sub(u64::from(r.range.start.row));
                    let cols = (u64::from(u.end.col) + 1).saturating_sub(u64::from(r.range.start.col));
                    let (dr, dc) = self.dims();
                    (rows.min(dr), cols.min(dc))
                }
                None => (0, 0),
            },
            Src::Arr(a) => (a.rows as u64, a.cols as u64),
        }
    }

    /// Re-anchor a reference to `dims` from its top-left (SUMIF's sum range rule).
    fn resized(self, dims: (u64, u64)) -> Src {
        match self {
            Src::Ref(mut r) => {
                let rows = u32::try_from(dims.0.saturating_sub(1)).unwrap_or(u32::MAX);
                let cols = u32::try_from(dims.1.saturating_sub(1)).unwrap_or(u32::MAX);
                r.range.end.row = r.range.start.row.saturating_add(rows).min(MAX_ROWS - 1);
                r.range.end.col = r.range.start.col.saturating_add(cols).min(MAX_COLS - 1);
                Src::Ref(r)
            }
            a => a,
        }
    }

    /// Materialise the top-left `rows × cols` block (cells outside the source read empty).
    fn block(&self, ev: &Ev<'_>, rows: u64, cols: u64) -> Result<Array2, ErrorKind> {
        if rows.saturating_mul(cols) > MAX_CELLS_PER_OP {
            return Err(ErrorKind::Num);
        }
        let (r, c) = (rows as usize, cols as usize);
        match self {
            Src::Arr(a) => {
                let mut data = Vec::with_capacity(r * c);
                for i in 0..r {
                    for j in 0..c {
                        data.push(a.get(i, j).clone());
                    }
                }
                Ok(Array2::new(r, c, data))
            }
            Src::Ref(sr) => {
                if r == 0 || c == 0 {
                    return Ok(Array2::new(r, c, Vec::new()));
                }
                let mut a = Array2::filled(r, c, Value::Empty);
                let start = sr.range.start;
                let end = CellAddr::new(
                    start.row.saturating_add(r as u32 - 1).min(MAX_ROWS - 1),
                    start.col.saturating_add(c as u32 - 1).min(MAX_COLS - 1),
                );
                let rg = CellRange::new(start, end);
                ev.host.for_each_stored(sr.sheet, rg, &mut |addr, v| {
                    let rr = (addr.row - start.row) as usize;
                    let cc = (addr.col - start.col) as usize;
                    if let Some(slot) = a.data.get_mut(rr * c + cc) {
                        *slot = v.clone();
                    }
                });
                Ok(a)
            }
        }
    }
}

/// Criteria ranges (and an optional target range) materialised over the block that can
/// hold stored cells; everything outside the block is empty in every range.
struct Prepared {
    blocks: Vec<Array2>,
    target: Option<Array2>,
    outside: u64,
}

impl Prepared {
    /// Evaluate the range arguments of `(range, criterion)` pairs, optionally with a target
    /// range (`resize_target`: SUMIF-style re-anchoring instead of a shape check).
    fn new(
        ev: &mut Ev<'_>,
        target: Option<&Expr>,
        pairs: &[Expr],
        resize_target: bool,
    ) -> Result<Prepared, ErrorKind> {
        if pairs.is_empty() || !pairs.len().is_multiple_of(2) {
            return Err(ErrorKind::Value);
        }
        let mut srcs: Vec<Src> = Vec::with_capacity(pairs.len() / 2);
        for re in pairs.iter().step_by(2) {
            srcs.push(Src::eval(ev, re)?);
        }
        let dims = srcs.first().map(Src::dims).ok_or(ErrorKind::Value)?;
        if srcs.iter().any(|s| s.dims() != dims) {
            return Err(ErrorKind::Value);
        }
        let tsrc = match target {
            Some(t) => {
                let s = Src::eval(ev, t)?;
                let s = if resize_target { s.resized(dims) } else { s };
                if !resize_target && s.dims() != dims {
                    return Err(ErrorKind::Value);
                }
                Some(s)
            }
            None => None,
        };
        let (mut rows, mut cols) = (0u64, 0u64);
        for s in srcs.iter().chain(tsrc.iter()) {
            let (r, c) = s.extent(ev);
            rows = rows.max(r);
            cols = cols.max(c);
        }
        rows = rows.min(dims.0);
        cols = cols.min(dims.1);
        let mut blocks = Vec::with_capacity(srcs.len());
        for s in &srcs {
            blocks.push(s.block(ev, rows, cols)?);
        }
        let target = match &tsrc {
            Some(s) => Some(s.block(ev, rows, cols)?),
            None => None,
        };
        Ok(Prepared {
            blocks,
            target,
            outside: dims.0.saturating_mul(dims.1).saturating_sub(rows * cols),
        })
    }

    fn mask(&self, crits: &[Criterion]) -> Vec<bool> {
        let n = self.blocks.first().map_or(0, |b| b.data.len());
        let mut mask = vec![true; n];
        for (block, crit) in self.blocks.iter().zip(crits) {
            for (m, v) in mask.iter_mut().zip(&block.data) {
                if *m && !crit.matches(v) {
                    *m = false;
                }
            }
        }
        mask
    }

    fn count(&self, crits: &[Criterion]) -> f64 {
        let inside = self.mask(crits).iter().filter(|b| **b).count() as u64;
        let outside = if crits.iter().all(Criterion::matches_empty) {
            self.outside
        } else {
            0
        };
        (inside + outside) as f64
    }

    /// Numbers of the target where every criterion holds (errors there propagate).
    fn numbers(&self, crits: &[Criterion]) -> Result<Vec<f64>, ErrorKind> {
        let mut out = Vec::new();
        if let Some(t) = &self.target {
            for (v, ok) in t.data.iter().zip(self.mask(crits)) {
                if ok {
                    match v {
                        Value::Number(n) => out.push(*n),
                        Value::Error(e) => return Err(*e),
                        _ => {}
                    }
                }
            }
        }
        Ok(out)
    }
}

/// Evaluate the criterion arguments of `pairs` and run `f` once per criteria tuple: once
/// for scalar criteria, elementwise (broadcast) when a criterion is an array or range.
fn per_criteria(
    ev: &mut Ev<'_>,
    pairs: &[Expr],
    f: &dyn Fn(&[Criterion]) -> Value,
) -> EvalValue {
    let mut cvals: Vec<Array2> = Vec::with_capacity(pairs.len() / 2);
    let (mut rows, mut cols) = (1usize, 1usize);
    for ce in pairs.iter().skip(1).step_by(2) {
        let v = ev.eval(ce);
        let multi = match &v {
            EvalValue::Array(a) => a.rows * a.cols != 1,
            EvalValue::Ref(r) => !r.range.is_single(),
            EvalValue::Refs(_) => true,
            EvalValue::Scalar(_) => false,
        };
        let a = if multi {
            match ev.to_array(v) {
                Ok(a) => a,
                Err(e) => return EvalValue::err(e),
            }
        } else {
            Array2::scalar(ev.to_scalar(v))
        };
        rows = rows.max(a.rows);
        cols = cols.max(a.cols);
        cvals.push(a);
    }
    if rows.saturating_mul(cols) > 65_536 {
        return EvalValue::err(ErrorKind::Num);
    }
    let crits_at = |r: usize, c: usize| -> Vec<Criterion> {
        cvals
            .iter()
            .map(|a| Criterion::parse(&a.get_broadcast(r, c)))
            .collect()
    };
    if rows == 1 && cols == 1 {
        return EvalValue::Scalar(f(&crits_at(0, 0)));
    }
    let mut data = Vec::with_capacity(rows * cols);
    for r in 0..rows {
        for c in 0..cols {
            data.push(f(&crits_at(r, c)));
        }
    }
    EvalValue::Array(Array2::new(rows, cols, data))
}

fn to_value(r: Result<f64, ErrorKind>) -> Value {
    match r {
        Ok(n) => crate::eval::num_value(n),
        Err(e) => Value::Error(e),
    }
}

fn countifs(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let p = match Prepared::new(ev, None, args, false) {
        Ok(p) => p,
        Err(e) => return EvalValue::err(e),
    };
    per_criteria(ev, args, &|c| Value::Number(p.count(c)))
}

/// SUMIF / AVERAGEIF: (range, criterion, [target range]).
fn single_if(ev: &mut Ev<'_>, args: &[Expr], f: fn(&[f64]) -> Result<f64, ErrorKind>) -> EvalValue {
    let pairs = args.get(..2).unwrap_or(&[]);
    let target = match args.get(2) {
        Some(Expr::Missing) | None => args.first(),
        Some(t) => Some(t),
    };
    let p = match Prepared::new(ev, target, pairs, true) {
        Ok(p) => p,
        Err(e) => return EvalValue::err(e),
    };
    per_criteria(ev, pairs, &|c| to_value(p.numbers(c).and_then(|v| f(&v))))
}

/// SUMIFS / AVERAGEIFS / MAXIFS / MINIFS: (target, range1, crit1, ...).
fn multi_if(ev: &mut Ev<'_>, args: &[Expr], f: fn(&[f64]) -> Result<f64, ErrorKind>) -> EvalValue {
    let pairs = args.get(1..).unwrap_or(&[]);
    let p = match Prepared::new(ev, args.first(), pairs, false) {
        Ok(p) => p,
        Err(e) => return EvalValue::err(e),
    };
    per_criteria(ev, pairs, &|c| to_value(p.numbers(c).and_then(|v| f(&v))))
}

fn sum(v: &[f64]) -> Result<f64, ErrorKind> {
    Ok(v.iter().sum())
}

fn mean(v: &[f64]) -> Result<f64, ErrorKind> {
    if v.is_empty() {
        return Err(ErrorKind::Div0);
    }
    Ok(v.iter().sum::<f64>() / v.len() as f64)
}

fn max(v: &[f64]) -> Result<f64, ErrorKind> {
    Ok(v.iter().copied().reduce(f64::max).unwrap_or(0.0))
}

fn min(v: &[f64]) -> Result<f64, ErrorKind> {
    Ok(v.iter().copied().reduce(f64::min).unwrap_or(0.0))
}

/// Mask over the (materialised) criteria ranges; used by older callers.
pub fn criteria_mask(
    ev: &mut Ev<'_>,
    pairs: &[Expr],
    _shape: Option<(usize, usize)>,
) -> Result<Vec<bool>, ErrorKind> {
    let p = Prepared::new(ev, None, pairs, false)?;
    let mut crits = Vec::with_capacity(pairs.len() / 2);
    for ce in pairs.iter().skip(1).step_by(2) {
        crits.push(Criterion::parse(&ev.eval_scalar(ce)));
    }
    Ok(p.mask(&crits))
}

/// Registers the criteria-driven aggregates. Must run after `math`/`stats` so these
/// implementations take precedence.
pub fn register(r: &mut Registry) {
    r.add("COUNTIF", 2, 2, countifs);
    r.add("COUNTIFS", 2, 254, countifs);
    r.add("SUMIF", 2, 3, |ev, a| single_if(ev, a, sum));
    r.add("AVERAGEIF", 2, 3, |ev, a| single_if(ev, a, mean));
    r.add("SUMIFS", 3, 255, |ev, a| multi_if(ev, a, sum));
    r.add("AVERAGEIFS", 3, 255, |ev, a| multi_if(ev, a, mean));
    r.add("MAXIFS", 3, 255, |ev, a| multi_if(ev, a, max));
    r.add("MINIFS", 3, 255, |ev, a| multi_if(ev, a, min));
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
