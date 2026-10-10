//! Information functions.

use super::Registry;
use crate::ast::Expr;
use crate::eval::{Ev, EvalValue, to_number};
use sp_core::{ErrorKind, Value};

fn is(ev: &mut Ev<'_>, args: &[Expr], f: fn(&Value) -> bool) -> EvalValue {
    let Some(a) = args.first() else {
        return EvalValue::err(ErrorKind::Value);
    };
    let v = ev.eval(a);
    super::lift1(ev, v, |x| Value::Bool(f(x)))
}

fn is_parity(ev: &mut Ev<'_>, args: &[Expr], even: bool) -> EvalValue {
    let Some(a) = args.first() else {
        return EvalValue::err(ErrorKind::Value);
    };
    let v = ev.eval_scalar(a);
    if matches!(v, Value::Bool(_)) {
        return EvalValue::err(ErrorKind::Value);
    }
    match to_number(&v) {
        Ok(n) => EvalValue::bool(((n.trunc() as i64) % 2 == 0) == even),
        Err(e) => EvalValue::err(e),
    }
}

fn type_fn(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let Some(a) = args.first() else {
        return EvalValue::err(ErrorKind::Value);
    };
    let v = ev.eval(a);
    let code = match &v {
        EvalValue::Array(_) => 64.0,
        EvalValue::Ref(r) if !r.range.is_single() => 16.0_f64.min(64.0),
        _ => match ev.to_scalar(v) {
            Value::Number(_) | Value::Empty => 1.0,
            Value::Text(_) => 2.0,
            Value::Bool(_) => 4.0,
            Value::Error(_) => 16.0,
        },
    };
    EvalValue::num(code)
}

fn sheet_fn(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    match args.first() {
        None => EvalValue::num(ev.sheet as f64 + 1.0),
        Some(e) => match ev.eval(e) {
            EvalValue::Ref(r) => EvalValue::num(r.sheet as f64 + 1.0),
            EvalValue::Scalar(Value::Text(name)) => match ev.host.sheet_index(&name) {
                Some(i) => EvalValue::num(i as f64 + 1.0),
                None => EvalValue::err(ErrorKind::NA),
            },
            EvalValue::Scalar(Value::Error(e)) => EvalValue::err(e),
            _ => EvalValue::err(ErrorKind::NA),
        },
    }
}

fn sheets_fn(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    match args.first() {
        None => EvalValue::num(ev.host.sheet_count() as f64),
        Some(e) => match ev.eval(e) {
            EvalValue::Ref(_) => EvalValue::num(1.0),
            EvalValue::Refs(rs) => {
                let mut s: Vec<usize> = rs.iter().map(|r| r.sheet).collect();
                s.dedup();
                EvalValue::num(s.len() as f64)
            }
            _ => EvalValue::err(ErrorKind::NA),
        },
    }
}

pub fn register(r: &mut Registry) {
    r.add("ISBLANK", 1, 1, |ev, a| {
        is(ev, a, |v| matches!(v, Value::Empty))
    });
    r.add("ISERR", 1, 1, |ev, a| {
        is(
            ev,
            a,
            |v| matches!(v, Value::Error(e) if *e != ErrorKind::NA),
        )
    });
    r.add("ISERROR", 1, 1, |ev, a| {
        is(ev, a, |v| matches!(v, Value::Error(_)))
    });
    r.add("ISNA", 1, 1, |ev, a| {
        is(ev, a, |v| matches!(v, Value::Error(ErrorKind::NA)))
    });
    r.add("ISNUMBER", 1, 1, |ev, a| {
        is(ev, a, |v| matches!(v, Value::Number(_)))
    });
    r.add("ISTEXT", 1, 1, |ev, a| {
        is(ev, a, |v| matches!(v, Value::Text(_)))
    });
    r.add("ISNONTEXT", 1, 1, |ev, a| {
        is(ev, a, |v| !matches!(v, Value::Text(_)))
    });
    r.add("ISLOGICAL", 1, 1, |ev, a| {
        is(ev, a, |v| matches!(v, Value::Bool(_)))
    });
    r.add("ISEVEN", 1, 1, |ev, a| is_parity(ev, a, true));
    r.add("ISODD", 1, 1, |ev, a| is_parity(ev, a, false));
    r.add("ISREF", 1, 1, |ev, a| match a.first().map(|e| ev.eval(e)) {
        Some(EvalValue::Ref(_) | EvalValue::Refs(_)) => EvalValue::bool(true),
        _ => EvalValue::bool(false),
    });
    r.add("ISFORMULA", 1, 1, |ev, a| {
        match a.first().map(|e| ev.eval(e)) {
            Some(EvalValue::Ref(r)) => {
                EvalValue::bool(ev.host.cell_formula(r.sheet, r.range.start).is_some())
            }
            Some(EvalValue::Scalar(Value::Error(e))) => EvalValue::err(e),
            _ => EvalValue::err(ErrorKind::Value),
        }
    });
    r.add("N", 1, 1, |ev, a| {
        match a.first().map(|e| ev.eval_scalar(e)) {
            Some(Value::Number(n)) => EvalValue::num(n),
            Some(Value::Bool(b)) => EvalValue::num(if b { 1.0 } else { 0.0 }),
            Some(Value::Error(e)) => EvalValue::err(e),
            _ => EvalValue::num(0.0),
        }
    });
    r.add("NA", 0, 0, |_, _| EvalValue::err(ErrorKind::NA));
    r.add("TYPE", 1, 1, type_fn);
    r.add("ERROR.TYPE", 1, 1, |ev, a| {
        match a.first().map(|e| ev.eval_scalar(e)) {
            Some(Value::Error(e)) => EvalValue::num(e.type_code()),
            _ => EvalValue::err(ErrorKind::NA),
        }
    });
    r.add("SHEET", 0, 1, sheet_fn);
    r.add("SHEETS", 0, 1, sheets_fn);
}
