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

/// CELL(info_type, [reference]): facts about the top-left cell of a reference (the
/// formula's own cell when omitted).
fn cell_fn(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let Some(t) = args.first() else {
        return EvalValue::err(ErrorKind::Value);
    };
    let kind = match ev.eval_scalar(t) {
        Value::Text(s) => s.to_ascii_lowercase(),
        Value::Error(e) => return EvalValue::err(e),
        _ => return EvalValue::err(ErrorKind::Value),
    };
    let (sheet, addr) = match args.get(1) {
        None | Some(Expr::Missing) => (ev.sheet, ev.cell),
        Some(e) => match ev.eval(e) {
            crate::eval::EvalValue::Ref(r) => (r.sheet, r.range.start),
            crate::eval::EvalValue::Refs(rs) => match rs.first() {
                Some(r) => (r.sheet, r.range.start),
                None => return EvalValue::err(ErrorKind::Value),
            },
            other => match other.as_error() {
                Some(e) => return EvalValue::err(e),
                None => return EvalValue::err(ErrorKind::Value),
            },
        },
    };
    let value = ev.host.cell_value(sheet, addr);
    match kind.as_str() {
        "address" => {
            let a1 = addr.to_a1();
            let col: String = a1.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
            let row: String = a1.chars().skip_while(|c| c.is_ascii_alphabetic()).collect();
            let local = format!("${col}${row}");
            if sheet == ev.sheet {
                EvalValue::text(local)
            } else {
                let name = ev.host.sheet_name(sheet).unwrap_or_default();
                EvalValue::text(format!("{name}!{local}"))
            }
        }
        "col" => EvalValue::num(f64::from(addr.col) + 1.0),
        "row" => EvalValue::num(f64::from(addr.row) + 1.0),
        "contents" => EvalValue::Scalar(if value.is_empty() {
            Value::Number(0.0)
        } else {
            value
        }),
        "type" => EvalValue::text(match value {
            Value::Empty => "b",
            Value::Text(_) => "l",
            _ => "v",
        }),
        "filename" => EvalValue::text(""),
        "format" => EvalValue::text("G"),
        "prefix" => EvalValue::text(match value {
            Value::Text(_) => "'",
            _ => "",
        }),
        "protect" => EvalValue::num(1.0),
        "parentheses" | "color" => EvalValue::num(0.0),
        "width" => EvalValue::num(8.0),
        _ => EvalValue::err(ErrorKind::Value),
    }
}

/// INFO(type_text): environment facts.
fn info_fn(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let Some(t) = args.first() else {
        return EvalValue::err(ErrorKind::Value);
    };
    let kind = match ev.eval_scalar(t) {
        Value::Text(s) => s.to_ascii_lowercase(),
        Value::Error(e) => return EvalValue::err(e),
        _ => return EvalValue::err(ErrorKind::Value),
    };
    match kind.as_str() {
        "directory" => EvalValue::text(""),
        "numfile" => EvalValue::num(ev.host.sheet_count() as f64),
        "origin" => EvalValue::text("$A:$A$1"),
        "osversion" => EvalValue::text(std::env::consts::OS),
        "recalc" => EvalValue::text("Automatic"),
        "release" => EvalValue::text("16.0"),
        "system" => EvalValue::text(if cfg!(target_os = "macos") {
            "mac"
        } else {
            "pcdos"
        }),
        _ => EvalValue::err(ErrorKind::Value),
    }
}

pub fn register(r: &mut Registry) {
    r.add("CELL", 1, 2, cell_fn);
    r.add("INFO", 1, 1, info_fn);
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
