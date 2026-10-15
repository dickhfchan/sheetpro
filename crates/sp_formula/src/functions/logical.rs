//! Logical functions.

use super::{Registry, for_each_value};
use crate::ast::Expr;
use crate::eval::{Array2, Ev, EvalValue, compare, to_bool};
use sp_core::{ErrorKind, Value};

fn fold_bools(ev: &mut Ev<'_>, args: &[Expr], init: bool, f: fn(bool, bool) -> bool) -> EvalValue {
    let mut acc = init;
    let mut seen = false;
    let r = for_each_value(ev, args, &mut |v, from_ref| {
        match (v, from_ref) {
            (Value::Error(e), _) => return Err(*e),
            (Value::Text(_), true) | (Value::Empty, true) => {}
            // An empty-string argument is skipped like a blank cell ("" alone is #VALUE!).
            (Value::Text(s), false) if s.is_empty() => {}
            (Value::Text(s), false) => {
                let b = to_bool(&Value::Text(s.clone()))?;
                acc = f(acc, b);
                seen = true;
            }
            (other, _) => {
                acc = f(acc, to_bool(other)?);
                seen = true;
            }
        }
        Ok(())
    });
    match r {
        Err(e) => EvalValue::err(e),
        Ok(()) if !seen => EvalValue::err(ErrorKind::Value),
        Ok(()) => EvalValue::bool(acc),
    }
}

fn if_fn(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let Some(c) = args.first() else {
        return EvalValue::err(ErrorKind::Value);
    };
    let cv = ev.eval(c);
    // Array condition: evaluate both branches and pick elementwise.
    let multi = matches!(&cv, EvalValue::Array(a) if a.rows * a.cols > 1)
        || matches!(&cv, EvalValue::Ref(r) if !r.range.is_single());
    if multi {
        let cond = match ev.to_array(cv) {
            Ok(a) => a,
            Err(e) => return EvalValue::err(e),
        };
        let t = match args.get(1) {
            Some(e) => ev
                .eval_array(e)
                .unwrap_or_else(|e| Array2::scalar(Value::Error(e))),
            None => Array2::scalar(Value::Bool(true)),
        };
        let f = match args.get(2) {
            Some(e) => ev
                .eval_array(e)
                .unwrap_or_else(|e| Array2::scalar(Value::Error(e))),
            None => Array2::scalar(Value::Bool(false)),
        };
        let mut data = Vec::with_capacity(cond.data.len());
        for r in 0..cond.rows {
            for c in 0..cond.cols {
                data.push(match to_bool(cond.get(r, c)) {
                    Ok(true) => t.get_broadcast(r, c),
                    Ok(false) => f.get_broadcast(r, c),
                    Err(e) => Value::Error(e),
                });
            }
        }
        return EvalValue::Array(Array2::new(cond.rows, cond.cols, data));
    }
    let cond = ev.to_scalar(cv);
    match to_bool(&cond) {
        Err(e) => EvalValue::err(e),
        Ok(true) => match args.get(1) {
            Some(Expr::Missing) => EvalValue::num(0.0),
            Some(e) => ev.eval(e),
            None => EvalValue::bool(true),
        },
        Ok(false) => match args.get(2) {
            Some(Expr::Missing) => EvalValue::num(0.0),
            Some(e) => ev.eval(e),
            None => EvalValue::bool(false),
        },
    }
}

fn ifs(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    if !args.len().is_multiple_of(2) {
        return EvalValue::err(ErrorKind::Value);
    }
    for pair in args.chunks(2) {
        let (Some(c), Some(v)) = (pair.first(), pair.get(1)) else {
            break;
        };
        match ev.boolean(c) {
            Err(e) => return EvalValue::err(e),
            Ok(true) => return ev.eval(v),
            Ok(false) => {}
        }
    }
    EvalValue::err(ErrorKind::NA)
}

fn iferror(ev: &mut Ev<'_>, args: &[Expr], only_na: bool) -> EvalValue {
    let (Some(a), Some(b)) = (args.first(), args.get(1)) else {
        return EvalValue::err(ErrorKind::Value);
    };
    let v = ev.eval(a);
    let fallback = |ev: &mut Ev<'_>| ev.eval(b);
    match &v {
        EvalValue::Scalar(Value::Error(e)) if !only_na || *e == ErrorKind::NA => fallback(ev),
        EvalValue::Scalar(_) => v,
        _ => {
            let arr = match ev.to_array(v.clone()) {
                Ok(a) => a,
                Err(_) => return fallback(ev),
            };
            if arr.rows * arr.cols == 1 {
                return match arr.get(0, 0) {
                    Value::Error(e) if !only_na || *e == ErrorKind::NA => fallback(ev),
                    Value::Empty => EvalValue::num(0.0),
                    x => EvalValue::Scalar(x.clone()),
                };
            }
            let fb = ev.eval_scalar(b);
            let data = arr
                .data
                .iter()
                .map(|x| match x {
                    Value::Error(e) if !only_na || *e == ErrorKind::NA => fb.clone(),
                    other => other.clone(),
                })
                .collect();
            EvalValue::Array(Array2::new(arr.rows, arr.cols, data))
        }
    }
}

fn switch(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let Some(e) = args.first() else {
        return EvalValue::err(ErrorKind::Value);
    };
    let target = ev.eval_scalar(e);
    if let Value::Error(err) = target {
        return EvalValue::err(err);
    }
    let rest = args.get(1..).unwrap_or(&[]);
    let mut i = 0;
    while i + 1 < rest.len() {
        if let (Some(k), Some(v)) = (rest.get(i), rest.get(i + 1)) {
            let kv = ev.eval_scalar(k);
            if let Value::Error(err) = kv {
                return EvalValue::err(err);
            }
            if std::mem::discriminant(&kv) == std::mem::discriminant(&target)
                && compare(&kv, &target).is_eq()
            {
                return ev.eval(v);
            }
        }
        i += 2;
    }
    if rest.len() % 2 == 1
        && let Some(d) = rest.last()
    {
        return ev.eval(d);
    }
    EvalValue::err(ErrorKind::NA)
}

fn let_fn(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    if args.len() < 3 || args.len().is_multiple_of(2) {
        return EvalValue::err(ErrorKind::Value);
    }
    let mut pushed = 0;
    let mut result = EvalValue::err(ErrorKind::Value);
    for (i, pair) in args.chunks(2).enumerate() {
        match pair {
            [Expr::Name(n), value] => {
                let v = ev.eval(value);
                ev.push_local(n, v);
                pushed += 1;
            }
            [body] if i == args.len() / 2 => {
                result = ev.eval(body);
            }
            _ => {
                ev.pop_locals(pushed);
                return EvalValue::err(ErrorKind::Value);
            }
        }
    }
    ev.pop_locals(pushed);
    result
}

pub fn register(r: &mut Registry) {
    r.add("TRUE", 0, 0, |_, _| EvalValue::bool(true));
    r.add("FALSE", 0, 0, |_, _| EvalValue::bool(false));
    r.add("AND", 1, 255, |ev, a| {
        fold_bools(ev, a, true, |x, y| x && y)
    });
    r.add("OR", 1, 255, |ev, a| {
        fold_bools(ev, a, false, |x, y| x || y)
    });
    r.add("XOR", 1, 255, |ev, a| {
        fold_bools(ev, a, false, |x, y| x ^ y)
    });
    r.add("NOT", 1, 1, |ev, a| {
        match a.first().map(|e| ev.boolean(e)) {
            Some(Ok(b)) => EvalValue::bool(!b),
            Some(Err(e)) => EvalValue::err(e),
            None => EvalValue::err(ErrorKind::Value),
        }
    });
    r.add("IF", 1, 3, if_fn);
    r.add("IFS", 2, 254, ifs);
    r.add("IFERROR", 2, 2, |ev, a| iferror(ev, a, false));
    r.add("IFNA", 2, 2, |ev, a| iferror(ev, a, true));
    r.add("SWITCH", 3, 254, switch);
    r.add("LET", 3, 253, let_fn);
}
