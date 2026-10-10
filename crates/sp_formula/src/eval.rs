//! Expression evaluator.

use crate::ast::{BinOp, Expr, RefExpr, RefKind, UnOp};
use crate::functions::{self, Registry};
use sp_core::parse::parse_numeric_input;
use sp_core::{CellAddr, CellRange, ErrorKind, MAX_CELLS_PER_OP, MAX_COLS, MAX_ROWS, Value};
use std::sync::Arc;

/// Maximum evaluation depth (names, LET, nested calls) before giving up with `#CALC!`.
pub const MAX_EVAL_DEPTH: usize = 512;

/// A rectangular block of values, row-major.
#[derive(Debug, Clone, PartialEq)]
pub struct Array2 {
    pub rows: usize,
    pub cols: usize,
    pub data: Vec<Value>,
}

impl Array2 {
    pub fn new(rows: usize, cols: usize, data: Vec<Value>) -> Self {
        let mut data = data;
        data.resize(rows.saturating_mul(cols), Value::Empty);
        Array2 { rows, cols, data }
    }

    pub fn scalar(v: Value) -> Self {
        Array2 {
            rows: 1,
            cols: 1,
            data: vec![v],
        }
    }

    pub fn filled(rows: usize, cols: usize, v: Value) -> Self {
        Array2 {
            rows,
            cols,
            data: vec![v; rows.saturating_mul(cols)],
        }
    }

    pub fn get(&self, r: usize, c: usize) -> &Value {
        static EMPTY: Value = Value::Empty;
        if r >= self.rows || c >= self.cols {
            return &EMPTY;
        }
        self.data.get(r * self.cols + c).unwrap_or(&EMPTY)
    }

    /// Element with broadcasting of 1-wide/1-tall arrays; `#N/A` outside the shape.
    pub fn get_broadcast(&self, r: usize, c: usize) -> Value {
        let rr = if self.rows == 1 { 0 } else { r };
        let cc = if self.cols == 1 { 0 } else { c };
        if rr >= self.rows || cc >= self.cols {
            return Value::Error(ErrorKind::NA);
        }
        self.get(rr, cc).clone()
    }

    pub fn transpose(&self) -> Array2 {
        let mut data = Vec::with_capacity(self.data.len());
        for c in 0..self.cols {
            for r in 0..self.rows {
                data.push(self.get(r, c).clone());
            }
        }
        Array2 {
            rows: self.cols,
            cols: self.rows,
            data,
        }
    }

    pub fn row(&self, r: usize) -> impl Iterator<Item = &Value> {
        (0..self.cols).map(move |c| self.get(r, c))
    }

    pub fn col(&self, c: usize) -> impl Iterator<Item = &Value> {
        (0..self.rows).map(move |r| self.get(r, c))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SheetRange {
    pub sheet: usize,
    pub range: CellRange,
}

/// Intermediate evaluation result.
#[derive(Debug, Clone, PartialEq)]
pub enum EvalValue {
    Scalar(Value),
    Array(Array2),
    Ref(SheetRange),
    /// Union of references, e.g. `(A1:A3,C1)`.
    Refs(Vec<SheetRange>),
}

impl EvalValue {
    pub fn err(e: ErrorKind) -> Self {
        EvalValue::Scalar(Value::Error(e))
    }

    pub fn num(n: f64) -> Self {
        EvalValue::Scalar(num_value(n))
    }

    pub fn text(s: impl Into<Arc<str>>) -> Self {
        EvalValue::Scalar(Value::Text(s.into()))
    }

    pub fn bool(b: bool) -> Self {
        EvalValue::Scalar(Value::Bool(b))
    }

    pub fn as_error(&self) -> Option<ErrorKind> {
        match self {
            EvalValue::Scalar(Value::Error(e)) => Some(*e),
            _ => None,
        }
    }
}

/// A finite number, or `#NUM!` for NaN/infinite results.
pub fn num_value(n: f64) -> Value {
    if n.is_finite() {
        Value::Number(if n == 0.0 { 0.0 } else { n })
    } else {
        Value::Error(ErrorKind::Num)
    }
}

/// Data access the evaluator needs. Implemented by the recalc engine.
pub trait Host {
    /// Current value of a cell (computed value for formula cells).
    fn cell_value(&self, sheet: usize, a: CellAddr) -> Value;
    /// Visit stored (non-empty) cells in `range`, row-major.
    fn for_each_stored(&self, sheet: usize, range: CellRange, f: &mut dyn FnMut(CellAddr, &Value));
    fn cell_formula(&self, sheet: usize, a: CellAddr) -> Option<String>;
    fn sheet_index(&self, name: &str) -> Option<usize>;
    fn sheet_name(&self, i: usize) -> Option<String>;
    fn sheet_count(&self) -> usize;
    /// Bounding box of stored cells on a sheet.
    fn used_range(&self, sheet: usize) -> Option<CellRange>;
    /// Parsed defined name visible from `sheet`.
    fn defined_name(&self, name: &str, sheet: usize) -> Option<Arc<Expr>>;
    /// Whether rows are hidden (SUBTOTAL/AGGREGATE ignore-hidden variants).
    fn row_hidden(&self, _sheet: usize, _row: u32) -> bool {
        false
    }
    /// Seconds-resolution serial for NOW() (overridable for deterministic tests).
    fn now(&self) -> f64 {
        sp_core::date::now_serial()
    }
    fn random(&self) -> f64;
}

pub struct Ev<'h> {
    pub host: &'h dyn Host,
    pub sheet: usize,
    pub cell: CellAddr,
    pub registry: &'static Registry,
    depth: usize,
    locals: Vec<(String, EvalValue)>,
}

impl<'h> Ev<'h> {
    pub fn new(host: &'h dyn Host, sheet: usize, cell: CellAddr) -> Self {
        Ev {
            host,
            sheet,
            cell,
            registry: functions::registry(),
            depth: 0,
            locals: Vec::new(),
        }
    }

    pub fn eval(&mut self, e: &Expr) -> EvalValue {
        if self.depth >= MAX_EVAL_DEPTH {
            return EvalValue::err(ErrorKind::Calc);
        }
        self.depth += 1;
        let r = self.eval_inner(e);
        self.depth -= 1;
        r
    }

    fn eval_inner(&mut self, e: &Expr) -> EvalValue {
        match e {
            Expr::Number(n) => EvalValue::num(*n),
            Expr::Text(s) => EvalValue::text(s.as_str()),
            Expr::Bool(b) => EvalValue::bool(*b),
            Expr::Error(err) => EvalValue::err(*err),
            Expr::Missing => EvalValue::Scalar(Value::Empty),
            Expr::Paren(a) => self.eval(a),
            Expr::Ref(r) => self.eval_ref(r),
            Expr::Spill(a) => self.eval(a),
            Expr::Name(n) => self.eval_name(n),
            Expr::Array(rows) => {
                let nrows = rows.len();
                let ncols = rows.first().map_or(0, Vec::len);
                let mut data = Vec::with_capacity(nrows * ncols);
                for item in rows.iter().flatten() {
                    data.push(self.eval_scalar(item));
                }
                EvalValue::Array(Array2::new(nrows, ncols, data))
            }
            Expr::Unary(op, a) => {
                let v = self.eval(a);
                self.map_unary(v, *op)
            }
            Expr::Binary(op, a, b) => match op {
                BinOp::Range | BinOp::Union | BinOp::Isect => self.ref_op(*op, a, b),
                _ => {
                    let l = self.eval(a);
                    let r = self.eval(b);
                    self.binary(*op, l, r)
                }
            },
            Expr::Call(name, args) => self.call(name, args),
        }
    }

    fn eval_name(&mut self, n: &str) -> EvalValue {
        if let Some((_, v)) = self
            .locals
            .iter()
            .rev()
            .find(|(k, _)| k.eq_ignore_ascii_case(n))
        {
            return v.clone();
        }
        match self.host.defined_name(n, self.sheet) {
            Some(expr) => self.eval(&expr),
            None => EvalValue::err(ErrorKind::Name),
        }
    }

    pub fn push_local(&mut self, name: &str, v: EvalValue) {
        self.locals.push((name.to_string(), v));
    }

    pub fn pop_locals(&mut self, n: usize) {
        let keep = self.locals.len().saturating_sub(n);
        self.locals.truncate(keep);
    }

    pub fn resolve_sheet(&self, r: &RefExpr) -> Option<usize> {
        match &r.sheet {
            None => Some(self.sheet),
            Some(s) => self.host.sheet_index(s),
        }
    }

    fn eval_ref(&mut self, r: &RefExpr) -> EvalValue {
        let Some(sheet) = self.resolve_sheet(r) else {
            return EvalValue::err(ErrorKind::Ref);
        };
        let range = match &r.kind {
            RefKind::Cell(c) => CellRange::single(CellAddr::new(c.row, c.col)),
            RefKind::Area(a, b) => {
                CellRange::new(CellAddr::new(a.row, a.col), CellAddr::new(b.row, b.col))
            }
            RefKind::Cols { c0, c1, .. } => {
                CellRange::new(CellAddr::new(0, *c0), CellAddr::new(MAX_ROWS - 1, *c1))
            }
            RefKind::Rows { r0, r1, .. } => {
                CellRange::new(CellAddr::new(*r0, 0), CellAddr::new(*r1, MAX_COLS - 1))
            }
        };
        EvalValue::Ref(SheetRange { sheet, range })
    }

    fn ref_op(&mut self, op: BinOp, a: &Expr, b: &Expr) -> EvalValue {
        let l = self.eval(a);
        let r = self.eval(b);
        let refs = |v: &EvalValue| -> Option<Vec<SheetRange>> {
            match v {
                EvalValue::Ref(r) => Some(vec![*r]),
                EvalValue::Refs(rs) => Some(rs.clone()),
                _ => None,
            }
        };
        if let Some(e) = l.as_error().or(r.as_error()) {
            return EvalValue::err(e);
        }
        let (Some(lr), Some(rr)) = (refs(&l), refs(&r)) else {
            return EvalValue::err(ErrorKind::Value);
        };
        match op {
            BinOp::Union => EvalValue::Refs(lr.into_iter().chain(rr).collect()),
            BinOp::Range => {
                let all: Vec<SheetRange> = lr.into_iter().chain(rr).collect();
                let Some(first) = all.first() else {
                    return EvalValue::err(ErrorKind::Ref);
                };
                if all.iter().any(|x| x.sheet != first.sheet) {
                    return EvalValue::err(ErrorKind::Value);
                }
                let mut bb = first.range;
                for x in &all {
                    bb = CellRange::new(
                        CellAddr::new(
                            bb.start.row.min(x.range.start.row),
                            bb.start.col.min(x.range.start.col),
                        ),
                        CellAddr::new(
                            bb.end.row.max(x.range.end.row),
                            bb.end.col.max(x.range.end.col),
                        ),
                    );
                }
                EvalValue::Ref(SheetRange {
                    sheet: first.sheet,
                    range: bb,
                })
            }
            _ => {
                // Intersection of single areas.
                match (lr.as_slice(), rr.as_slice()) {
                    ([x], [y]) if x.sheet == y.sheet => match x.range.intersect(&y.range) {
                        Some(i) => EvalValue::Ref(SheetRange {
                            sheet: x.sheet,
                            range: i,
                        }),
                        None => EvalValue::err(ErrorKind::Null),
                    },
                    _ => EvalValue::err(ErrorKind::Null),
                }
            }
        }
    }

    fn call(&mut self, name: &str, args: &[Expr]) -> EvalValue {
        let Some(spec) = self.registry.get(name) else {
            return EvalValue::err(ErrorKind::Name);
        };
        if args.len() < spec.min_args || args.len() > spec.max_args {
            return EvalValue::err(ErrorKind::Value);
        }
        (spec.imp)(self, args)
    }

    // ── coercions ──────────────────────────────────────────────────────────────────────────

    /// Reduce to a single value: single-cell refs dereference, multi-cell refs use implicit
    /// intersection with the formula's row/column, arrays take their top-left element.
    pub fn to_scalar(&self, v: EvalValue) -> Value {
        match v {
            EvalValue::Scalar(v) => v,
            EvalValue::Array(a) => a.get(0, 0).clone(),
            EvalValue::Ref(r) => self.intersect_value(r),
            EvalValue::Refs(rs) => match rs.as_slice() {
                [r] => self.intersect_value(*r),
                _ => Value::Error(ErrorKind::Value),
            },
        }
    }

    fn intersect_value(&self, r: SheetRange) -> Value {
        let rg = r.range;
        if rg.is_single() {
            return self.host.cell_value(r.sheet, rg.start);
        }
        if rg.cols() == 1 && rg.start.row <= self.cell.row && self.cell.row <= rg.end.row {
            return self
                .host
                .cell_value(r.sheet, CellAddr::new(self.cell.row, rg.start.col));
        }
        if rg.rows() == 1 && rg.start.col <= self.cell.col && self.cell.col <= rg.end.col {
            return self
                .host
                .cell_value(r.sheet, CellAddr::new(rg.start.row, self.cell.col));
        }
        Value::Error(ErrorKind::Value)
    }

    pub fn eval_scalar(&mut self, e: &Expr) -> Value {
        let v = self.eval(e);
        self.to_scalar(v)
    }

    /// Materialise any result as an array (refs read their cells; capped size).
    pub fn to_array(&self, v: EvalValue) -> Result<Array2, ErrorKind> {
        match v {
            EvalValue::Scalar(v) => Ok(Array2::scalar(v)),
            EvalValue::Array(a) => Ok(a),
            EvalValue::Ref(r) => self.ref_to_array(r),
            EvalValue::Refs(rs) => match rs.as_slice() {
                [r] => self.ref_to_array(*r),
                _ => Err(ErrorKind::Value),
            },
        }
    }

    /// Clamp whole-row/column refs to the used range so they stay cheap.
    pub fn clamp_to_used(&self, r: SheetRange) -> CellRange {
        let rg = r.range;
        if !(rg.is_full_cols() || rg.is_full_rows()) {
            return rg;
        }
        match self.host.used_range(r.sheet).and_then(|u| rg.intersect(&u)) {
            Some(i) => CellRange::new(rg.start, i.end),
            None => CellRange::single(rg.start),
        }
    }

    pub fn ref_to_array(&self, r: SheetRange) -> Result<Array2, ErrorKind> {
        let rg = self.clamp_to_used(r);
        if rg.cell_count() > MAX_CELLS_PER_OP {
            return Err(ErrorKind::Num);
        }
        let (rows, cols) = (rg.rows() as usize, rg.cols() as usize);
        let mut a = Array2::filled(rows, cols, Value::Empty);
        self.host.for_each_stored(r.sheet, rg, &mut |addr, v| {
            let (rr, cc) = (
                (addr.row - rg.start.row) as usize,
                (addr.col - rg.start.col) as usize,
            );
            if let Some(slot) = a.data.get_mut(rr * cols + cc) {
                *slot = v.clone();
            }
        });
        Ok(a)
    }

    pub fn eval_array(&mut self, e: &Expr) -> Result<Array2, ErrorKind> {
        let v = self.eval(e);
        if let Some(err) = v.as_error() {
            return Err(err);
        }
        self.to_array(v)
    }

    pub fn num(&mut self, e: &Expr) -> Result<f64, ErrorKind> {
        let v = self.eval_scalar(e);
        to_number(&v)
    }

    /// Number argument that may be omitted (`Missing` → `default`).
    pub fn num_or(&mut self, args: &[Expr], i: usize, default: f64) -> Result<f64, ErrorKind> {
        match args.get(i) {
            None | Some(Expr::Missing) => Ok(default),
            Some(e) => self.num(e),
        }
    }

    pub fn text(&mut self, e: &Expr) -> Result<String, ErrorKind> {
        let v = self.eval_scalar(e);
        to_text(&v)
    }

    pub fn boolean(&mut self, e: &Expr) -> Result<bool, ErrorKind> {
        let v = self.eval_scalar(e);
        to_bool(&v)
    }

    pub fn bool_or(&mut self, args: &[Expr], i: usize, default: bool) -> Result<bool, ErrorKind> {
        match args.get(i) {
            None | Some(Expr::Missing) => Ok(default),
            Some(e) => self.boolean(e),
        }
    }

    // ── operators ──────────────────────────────────────────────────────────────────────────

    fn is_multi(&self, v: &EvalValue) -> bool {
        match v {
            EvalValue::Array(a) => a.rows * a.cols != 1,
            EvalValue::Ref(r) => !r.range.is_single(),
            EvalValue::Refs(_) => true,
            EvalValue::Scalar(_) => false,
        }
    }

    fn map_unary(&mut self, v: EvalValue, op: UnOp) -> EvalValue {
        let f = |x: &Value| -> Value {
            match to_number(x) {
                Err(e) => Value::Error(e),
                Ok(n) => num_value(match op {
                    UnOp::Neg => -n,
                    UnOp::Plus => n,
                    UnOp::Percent => n / 100.0,
                }),
            }
        };
        if op == UnOp::Plus && !self.is_multi(&v) {
            // Unary plus is a no-op on any scalar (keeps text as text).
            return EvalValue::Scalar(self.to_scalar(v));
        }
        if self.is_multi(&v) {
            return match self.to_array(v) {
                Ok(a) => EvalValue::Array(Array2 {
                    rows: a.rows,
                    cols: a.cols,
                    data: a.data.iter().map(f).collect(),
                }),
                Err(e) => EvalValue::err(e),
            };
        }
        let s = self.to_scalar(v);
        EvalValue::Scalar(f(&s))
    }

    fn binary(&mut self, op: BinOp, l: EvalValue, r: EvalValue) -> EvalValue {
        if self.is_multi(&l) || self.is_multi(&r) {
            let (a, b) = match (self.to_array(l), self.to_array(r)) {
                (Ok(a), Ok(b)) => (a, b),
                (Err(e), _) | (_, Err(e)) => return EvalValue::err(e),
            };
            let rows = if a.rows == 1 {
                b.rows
            } else if b.rows == 1 {
                a.rows
            } else {
                a.rows.max(b.rows)
            };
            let cols = if a.cols == 1 {
                b.cols
            } else if b.cols == 1 {
                a.cols
            } else {
                a.cols.max(b.cols)
            };
            if (rows as u64) * (cols as u64) > MAX_CELLS_PER_OP {
                return EvalValue::err(ErrorKind::Num);
            }
            let mut data = Vec::with_capacity(rows * cols);
            for i in 0..rows {
                for j in 0..cols {
                    data.push(scalar_binop(
                        op,
                        &a.get_broadcast(i, j),
                        &b.get_broadcast(i, j),
                    ));
                }
            }
            return EvalValue::Array(Array2 { rows, cols, data });
        }
        let a = self.to_scalar(l);
        let b = self.to_scalar(r);
        EvalValue::Scalar(scalar_binop(op, &a, &b))
    }
}

// ── free coercion helpers (shared with functions) ─────────────────────────────────────────

pub fn to_number(v: &Value) -> Result<f64, ErrorKind> {
    match v {
        Value::Number(n) => Ok(*n),
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        Value::Empty => Ok(0.0),
        Value::Text(s) => text_to_number(s).ok_or(ErrorKind::Value),
        Value::Error(e) => Err(*e),
    }
}

pub fn text_to_number(s: &str) -> Option<f64> {
    if s.trim().is_empty() {
        return None;
    }
    let year = sp_core::date::serial_to_date(sp_core::date::now_serial()).map_or(2000, |d| d.0);
    parse_numeric_input(s, year).map(|(n, _)| n)
}

pub fn to_text(v: &Value) -> Result<String, ErrorKind> {
    match v {
        Value::Error(e) => Err(*e),
        other => Ok(other.to_display()),
    }
}

pub fn to_bool(v: &Value) -> Result<bool, ErrorKind> {
    match v {
        Value::Bool(b) => Ok(*b),
        Value::Number(n) => Ok(*n != 0.0),
        Value::Empty => Ok(false),
        Value::Text(s) => {
            if s.eq_ignore_ascii_case("TRUE") {
                Ok(true)
            } else if s.eq_ignore_ascii_case("FALSE") {
                Ok(false)
            } else {
                Err(ErrorKind::Value)
            }
        }
        Value::Error(e) => Err(*e),
    }
}

/// Compare two scalars the way comparison operators do: numbers < text < logicals,
/// text case-insensitively, blanks equal to 0 / "" / FALSE depending on the other side.
pub fn compare(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let norm = |x: &Value, other: &Value| -> Value {
        match (x, other) {
            (Value::Empty, Value::Number(_)) => Value::Number(0.0),
            (Value::Empty, Value::Text(_)) => Value::text(""),
            (Value::Empty, Value::Bool(_)) => Value::Bool(false),
            (Value::Empty, Value::Empty) => Value::Number(0.0),
            _ => x.clone(),
        }
    };
    let (a, b) = (norm(a, b), norm(b, a));
    match (&a, &b) {
        (Value::Number(x), Value::Number(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        (Value::Text(x), Value::Text(y)) => x.to_lowercase().cmp(&y.to_lowercase()),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        _ => a.type_rank().cmp(&b.type_rank()),
    }
}

pub fn scalar_binop(op: BinOp, a: &Value, b: &Value) -> Value {
    use std::cmp::Ordering;
    if let Value::Error(e) = a {
        return Value::Error(*e);
    }
    if let Value::Error(e) = b {
        return Value::Error(*e);
    }
    match op {
        BinOp::Concat => match (to_text(a), to_text(b)) {
            (Ok(x), Ok(y)) => {
                let s = x + &y;
                if s.chars().count() > sp_core::MAX_TEXT_LEN {
                    Value::Error(ErrorKind::Value)
                } else {
                    Value::text(s)
                }
            }
            (Err(e), _) | (_, Err(e)) => Value::Error(e),
        },
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
            let o = compare(a, b);
            Value::Bool(match op {
                BinOp::Eq => o == Ordering::Equal,
                BinOp::Ne => o != Ordering::Equal,
                BinOp::Lt => o == Ordering::Less,
                BinOp::Le => o != Ordering::Greater,
                BinOp::Gt => o == Ordering::Greater,
                _ => o != Ordering::Less,
            })
        }
        _ => {
            let (x, y) = match (to_number(a), to_number(b)) {
                (Ok(x), Ok(y)) => (x, y),
                (Err(e), _) | (_, Err(e)) => return Value::Error(e),
            };
            match op {
                BinOp::Add => num_value(x + y),
                BinOp::Sub => num_value(x - y),
                BinOp::Mul => num_value(x * y),
                BinOp::Div => {
                    if y == 0.0 {
                        Value::Error(ErrorKind::Div0)
                    } else {
                        num_value(x / y)
                    }
                }
                BinOp::Pow => {
                    if x == 0.0 && y == 0.0 {
                        Value::Error(ErrorKind::Num)
                    } else if x == 0.0 && y < 0.0 {
                        Value::Error(ErrorKind::Div0)
                    } else {
                        num_value(x.powf(y))
                    }
                }
                _ => Value::Error(ErrorKind::Value),
            }
        }
    }
}
