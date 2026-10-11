//! LAMBDA and its helpers: MAP, REDUCE, SCAN, BYROW, BYCOL, MAKEARRAY, ISOMITTED.

use super::Registry;
use super::arrays::MAX_ARRAY_CELLS;
use crate::ast::Expr;
use crate::eval::{Array2, Callable, Ev, EvalValue, is_omitted, omitted, to_number};
use sp_core::{ErrorKind, Value};

type R<T> = Result<T, ErrorKind>;

fn lambda(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let Some((body, params)) = args.split_last() else {
        return EvalValue::err(ErrorKind::Value);
    };
    let mut names: Vec<String> = Vec::with_capacity(params.len());
    for p in params {
        match p {
            Expr::Name(n) if !names.iter().any(|x| x.eq_ignore_ascii_case(n)) => {
                names.push(n.clone())
            }
            _ => return EvalValue::err(ErrorKind::Value),
        }
    }
    ev.make_lambda(names, body)
}

/// The function argument (last) of a lambda helper, checked to take `n` parameters.
fn func(ev: &mut Ev<'_>, e: Option<&Expr>, n: usize) -> R<Callable> {
    let f = e.and_then(|e| ev.callable(e)).ok_or(ErrorKind::Value)?;
    match ev.arity(f) {
        Some(k) if k != n => Err(ErrorKind::Value),
        _ => Ok(f),
    }
}

fn array_of(ev: &mut Ev<'_>, e: &Expr) -> R<Array2> {
    let v = ev.eval(e);
    if let Some(err) = v.as_error() {
        return Err(err);
    }
    ev.to_array(v)
}

/// A lambda result that must be a single value (nested arrays are `#CALC!`).
fn single(ev: &Ev<'_>, v: EvalValue) -> Value {
    match &v {
        EvalValue::Array(a) if a.rows * a.cols > 1 => Value::Error(ErrorKind::Calc),
        EvalValue::Ref(r) if !r.range.is_single() => Value::Error(ErrorKind::Calc),
        _ => match ev.to_scalar(v) {
            Value::Empty => Value::Number(0.0),
            x => x,
        },
    }
}

fn out(r: R<Array2>) -> EvalValue {
    match r {
        Ok(a) if a.rows * a.cols == 1 => {
            EvalValue::Scalar(a.data.into_iter().next().unwrap_or(Value::Empty))
        }
        Ok(a) => EvalValue::Array(a),
        Err(e) => EvalValue::err(e),
    }
}

fn map(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    out((|| {
        let (fe, arrs) = args.split_last().ok_or(ErrorKind::Value)?;
        let f = func(ev, Some(fe), arrs.len())?;
        let arrays = arrs
            .iter()
            .map(|e| array_of(ev, e))
            .collect::<R<Vec<Array2>>>()?;
        let rows = arrays.iter().map(|a| a.rows).max().unwrap_or(0);
        let cols = arrays.iter().map(|a| a.cols).max().unwrap_or(0);
        let mut data = Vec::with_capacity(rows * cols);
        for r in 0..rows {
            for c in 0..cols {
                let vals = arrays
                    .iter()
                    .map(|a| EvalValue::Scalar(a.get_broadcast(r, c)))
                    .collect();
                let v = ev.apply(f, vals);
                data.push(single(ev, v));
            }
        }
        Ok(Array2::new(rows, cols, data))
    })())
}

fn reduce_scan(ev: &mut Ev<'_>, args: &[Expr], scan: bool) -> EvalValue {
    let r = (|| -> R<EvalValue> {
        let (init, a, f) = match args {
            [a, f] => (omitted(), a, f),
            [i, a, f] => (
                match i {
                    Expr::Missing => omitted(),
                    e => ev.eval(e),
                },
                a,
                f,
            ),
            _ => return Err(ErrorKind::Value),
        };
        let f = func(ev, Some(f), 2)?;
        let arr = array_of(ev, a)?;
        let mut acc = init;
        let mut steps = Vec::with_capacity(if scan { arr.data.len() } else { 0 });
        for v in &arr.data {
            acc = ev.apply(f, vec![acc, EvalValue::Scalar(v.clone())]);
            if scan {
                steps.push(single(ev, acc.clone()));
            }
        }
        Ok(if scan {
            out(Ok(Array2::new(arr.rows, arr.cols, steps)))
        } else {
            acc
        })
    })();
    r.unwrap_or_else(EvalValue::err)
}

fn by_line(ev: &mut Ev<'_>, args: &[Expr], rows: bool) -> EvalValue {
    out((|| {
        let a = array_of(ev, args.first().ok_or(ErrorKind::Value)?)?;
        let f = func(ev, args.get(1), 1)?;
        let t = if rows { a } else { a.transpose() };
        let mut data = Vec::with_capacity(t.rows);
        for r in 0..t.rows {
            let line: Vec<Value> = t.row(r).cloned().collect();
            let arg = Array2::new(1, t.cols, line);
            let arg = if rows { arg } else { arg.transpose() };
            let v = ev.apply(f, vec![EvalValue::Array(arg)]);
            data.push(single(ev, v));
        }
        let n = data.len();
        Ok(if rows {
            Array2::new(n, 1, data)
        } else {
            Array2::new(1, n, data)
        })
    })())
}

fn makearray(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    out((|| {
        let dim = |ev: &mut Ev<'_>, i: usize| -> R<usize> {
            let n = to_number(&ev.eval_scalar(args.get(i).ok_or(ErrorKind::Value)?))?.trunc();
            if !(1.0..=MAX_ARRAY_CELLS as f64).contains(&n) {
                return Err(ErrorKind::Value);
            }
            Ok(n as usize)
        };
        let rows = dim(ev, 0)?;
        let cols = dim(ev, 1)?;
        if rows.saturating_mul(cols) > MAX_ARRAY_CELLS {
            return Err(ErrorKind::Num);
        }
        let f = func(ev, args.get(2), 2)?;
        let mut data = Vec::with_capacity(rows * cols);
        for r in 0..rows {
            for c in 0..cols {
                let v = ev.apply(
                    f,
                    vec![
                        EvalValue::num(r as f64 + 1.0),
                        EvalValue::num(c as f64 + 1.0),
                    ],
                );
                data.push(single(ev, v));
            }
        }
        Ok(Array2::new(rows, cols, data))
    })())
}

fn isomitted(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    match args.first() {
        Some(e) => {
            let v = ev.eval(e);
            EvalValue::bool(is_omitted(&v))
        }
        None => EvalValue::err(ErrorKind::Value),
    }
}

pub fn register(r: &mut Registry) {
    r.add("LAMBDA", 1, 254, lambda);
    r.add("MAP", 2, 254, map);
    r.add("REDUCE", 2, 3, |ev, a| reduce_scan(ev, a, false));
    r.add("SCAN", 2, 3, |ev, a| reduce_scan(ev, a, true));
    r.add("BYROW", 2, 2, |ev, a| by_line(ev, a, true));
    r.add("BYCOL", 2, 2, |ev, a| by_line(ev, a, false));
    r.add("MAKEARRAY", 3, 3, makearray);
    r.add("ISOMITTED", 1, 1, isomitted);
}
