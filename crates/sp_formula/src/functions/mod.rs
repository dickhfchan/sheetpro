//! Worksheet-function registry and shared argument helpers.
//!
//! Each category module exposes `register(&mut Registry)`. A function counts as live for
//! parity (`fn.<NAME>`) as soon as it is registered here.

pub mod arrays;
pub mod bonds;
pub mod criteria;
pub mod database;
pub mod datetime;
pub mod distributions;
pub mod engineering;
pub mod financial;
pub mod info;
pub mod lambda;
pub mod logical;
pub mod lookup;
pub mod math;
pub mod math_ext;
pub mod regex;
pub mod special;
pub mod stats;
pub mod stats_ext;
pub mod text;
pub mod text_ext;

use crate::ast::Expr;
use crate::eval::{Array2, Ev, EvalValue, num_value, to_number};
use sp_core::{ErrorKind, Value};
use std::collections::HashMap;
use std::sync::OnceLock;

pub type FnImp = fn(&mut Ev<'_>, &[Expr]) -> EvalValue;

#[derive(Clone, Copy)]
pub struct FnSpec {
    pub name: &'static str,
    pub min_args: usize,
    pub max_args: usize,
    pub imp: FnImp,
}

#[derive(Default)]
pub struct Registry {
    map: HashMap<&'static str, FnSpec>,
}

impl Registry {
    pub fn add(&mut self, name: &'static str, min_args: usize, max_args: usize, imp: FnImp) {
        self.map.insert(
            name,
            FnSpec {
                name,
                min_args,
                max_args,
                imp,
            },
        );
    }

    /// Register `alias` with the same implementation as an already-registered function.
    pub fn alias(&mut self, alias: &'static str, of: &str) {
        if let Some(s) = self.map.get(of).copied() {
            self.map.insert(alias, FnSpec { name: alias, ..s });
        }
    }

    pub fn get(&self, name: &str) -> Option<&FnSpec> {
        self.map.get(name)
    }

    pub fn names(&self) -> Vec<&'static str> {
        let mut v: Vec<_> = self.map.keys().copied().collect();
        v.sort_unstable();
        v
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

pub fn registry() -> &'static Registry {
    static R: OnceLock<Registry> = OnceLock::new();
    R.get_or_init(|| {
        let mut r = Registry::default();
        math::register(&mut r);
        logical::register(&mut r);
        text::register(&mut r);
        info::register(&mut r);
        lookup::register(&mut r);
        stats::register(&mut r);
        datetime::register(&mut r);
        financial::register(&mut r);
        distributions::register(&mut r);
        stats_ext::register(&mut r);
        engineering::register(&mut r);
        criteria::register(&mut r);
        database::register(&mut r);
        bonds::register(&mut r);
        text_ext::register(&mut r);
        regex::register(&mut r);
        math_ext::register(&mut r);
        arrays::register(&mut r);
        lambda::register(&mut r);
        r
    })
}

pub const MAX_ARGS: usize = 255;

// ── shared helpers ───────────────────────────────────────────────────────────────────────

/// Visit every value an argument list supplies. `from_ref` is true for values that came
/// from a reference or array (these follow "ignore text/logicals" rules in aggregates).
/// Empty cells inside references are skipped.
pub fn for_each_value(
    ev: &mut Ev<'_>,
    args: &[Expr],
    f: &mut dyn FnMut(&Value, bool) -> Result<(), ErrorKind>,
) -> Result<(), ErrorKind> {
    for a in args {
        let v = ev.eval(a);
        visit_value(ev, v, f)?;
    }
    Ok(())
}

pub fn visit_value(
    ev: &Ev<'_>,
    v: EvalValue,
    f: &mut dyn FnMut(&Value, bool) -> Result<(), ErrorKind>,
) -> Result<(), ErrorKind> {
    match v {
        EvalValue::Scalar(v) => f(&v, false),
        EvalValue::Array(a) => {
            for x in &a.data {
                f(x, true)?;
            }
            Ok(())
        }
        EvalValue::Ref(r) => visit_ref(ev, r, f),
        EvalValue::Refs(rs) => {
            for r in rs {
                visit_ref(ev, r, f)?;
            }
            Ok(())
        }
    }
}

fn visit_ref(
    ev: &Ev<'_>,
    r: crate::eval::SheetRange,
    f: &mut dyn FnMut(&Value, bool) -> Result<(), ErrorKind>,
) -> Result<(), ErrorKind> {
    let rg = ev.clamp_to_used(r);
    let mut res = Ok(());
    ev.host.for_each_stored(r.sheet, rg, &mut |_, v| {
        if res.is_ok() && !v.is_empty() {
            res = f(v, true);
        }
    });
    res
}

/// How aggregates treat non-numeric values.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Agg {
    /// SUM/AVERAGE/MAX: refs → numbers only; direct args coerce text/bools.
    Numbers,
    /// AVERAGEA/MAXA: refs → text counts 0, bools 1/0.
    All,
}

pub fn collect_numbers(ev: &mut Ev<'_>, args: &[Expr], mode: Agg) -> Result<Vec<f64>, ErrorKind> {
    let mut out = Vec::new();
    for_each_value(ev, args, &mut |v, from_ref| {
        match (v, from_ref) {
            (Value::Number(n), _) => out.push(*n),
            (Value::Error(e), _) => return Err(*e),
            (Value::Bool(b), false) => out.push(if *b { 1.0 } else { 0.0 }),
            (Value::Bool(b), true) if mode == Agg::All => out.push(if *b { 1.0 } else { 0.0 }),
            (Value::Text(_), true) if mode == Agg::All => out.push(0.0),
            (Value::Text(s), false) => {
                out.push(crate::eval::text_to_number(s).ok_or(ErrorKind::Value)?)
            }
            (Value::Empty, false) => out.push(0.0),
            _ => {}
        }
        Ok(())
    })?;
    Ok(out)
}

/// One-number function, lifted elementwise over arrays/ranges.
pub fn m1(ev: &mut Ev<'_>, args: &[Expr], f: impl Fn(f64) -> Result<f64, ErrorKind>) -> EvalValue {
    let Some(a) = args.first() else {
        return EvalValue::err(ErrorKind::Value);
    };
    let v = ev.eval(a);
    lift1(ev, v, |x| match to_number(x) {
        Ok(n) => match f(n) {
            Ok(r) => num_value(r),
            Err(e) => Value::Error(e),
        },
        Err(e) => Value::Error(e),
    })
}

/// Apply `f` to a scalar, or to every element if `v` is multi-valued.
pub fn lift1(ev: &Ev<'_>, v: EvalValue, f: impl Fn(&Value) -> Value) -> EvalValue {
    let multi = match &v {
        EvalValue::Array(a) => a.rows * a.cols != 1,
        EvalValue::Ref(r) => !r.range.is_single(),
        EvalValue::Refs(_) => true,
        EvalValue::Scalar(_) => false,
    };
    if multi {
        return match ev.to_array(v) {
            Ok(a) => EvalValue::Array(Array2 {
                rows: a.rows,
                cols: a.cols,
                data: a.data.iter().map(f).collect(),
            }),
            Err(e) => EvalValue::err(e),
        };
    }
    EvalValue::Scalar(f(&ev.to_scalar(v)))
}

/// Two-number function (second optional with default), scalar only.
pub fn m2(
    ev: &mut Ev<'_>,
    args: &[Expr],
    default_b: Option<f64>,
    f: impl Fn(f64, f64) -> Result<f64, ErrorKind>,
) -> EvalValue {
    let Some(a) = args.first() else {
        return EvalValue::err(ErrorKind::Value);
    };
    let x = match ev.num(a) {
        Ok(x) => x,
        Err(e) => return EvalValue::err(e),
    };
    let y = match (args.get(1), default_b) {
        (None | Some(Expr::Missing), Some(d)) => d,
        (Some(b), _) => match ev.num(b) {
            Ok(y) => y,
            Err(e) => return EvalValue::err(e),
        },
        (None, None) => return EvalValue::err(ErrorKind::Value),
    };
    match f(x, y) {
        Ok(r) => EvalValue::num(r),
        Err(e) => EvalValue::err(e),
    }
}

/// Shorthand to turn a `Result<f64>` into an `EvalValue`.
pub fn ret(r: Result<f64, ErrorKind>) -> EvalValue {
    match r {
        Ok(n) => EvalValue::num(n),
        Err(e) => EvalValue::err(e),
    }
}

/// Evaluate an argument that must be a reference or array (for lookup/aggregate ranges).
pub fn arg_array(ev: &mut Ev<'_>, args: &[Expr], i: usize) -> Result<Array2, ErrorKind> {
    match args.get(i) {
        Some(e) => ev.eval_array(e),
        None => Err(ErrorKind::Value),
    }
}

/// Truncate toward zero, rejecting values outside i64.
pub fn trunc_i64(n: f64) -> Result<i64, ErrorKind> {
    if !n.is_finite() || n.abs() > 9.0e15 {
        return Err(ErrorKind::Num);
    }
    Ok(n.trunc() as i64)
}
