//! Database functions (DSUM, DCOUNT, DGET, ...): aggregate one field of the records in a
//! table that match a criteria table.
//!
//! The database is a range whose first row holds field names. The criteria range also has
//! field names in its first row; each row below is one alternative (rows are OR-ed, the
//! non-empty cells within a row are AND-ed). A text criterion without an operator matches
//! values that begin with it; `=text` matches exactly.

use super::Registry;
use super::criteria::Criterion;
use crate::ast::Expr;
use crate::eval::{Array2, Ev, EvalValue};
use sp_core::{ErrorKind, Value};

/// Header text used to match field names (case-insensitive).
fn header_key(v: &Value) -> String {
    v.to_display().to_lowercase()
}

/// Resolve the `field` argument to a 0-based column of `db`. `Ok(None)` = field omitted.
fn field_column(
    ev: &mut Ev<'_>,
    db: &Array2,
    field: Option<&Expr>,
) -> Result<Option<usize>, ErrorKind> {
    let Some(fe) = field else {
        return Ok(None);
    };
    if matches!(fe, Expr::Missing) {
        return Ok(None);
    }
    let v = match ev.eval(fe) {
        EvalValue::Ref(r) if !r.range.is_single() => return Err(ErrorKind::Value),
        EvalValue::Refs(_) => return Err(ErrorKind::Value),
        EvalValue::Array(a) if a.rows * a.cols != 1 => return Err(ErrorKind::Value),
        other => ev.to_scalar(other),
    };
    let by_index = |n: f64| -> Result<Option<usize>, ErrorKind> {
        let i = n.trunc();
        if i >= 1.0 && i <= db.cols as f64 {
            Ok(Some(i as usize - 1))
        } else {
            Err(ErrorKind::Value)
        }
    };
    match v {
        Value::Error(e) => Err(e),
        Value::Number(n) => by_index(n),
        Value::Bool(b) => by_index(if b { 1.0 } else { 0.0 }),
        Value::Empty => Err(ErrorKind::Value),
        Value::Text(s) => {
            let key = s.to_lowercase();
            db.row(0)
                .position(|h| !h.is_empty() && header_key(h) == key)
                .map(Some)
                .ok_or(ErrorKind::Value)
        }
    }
}

/// One criteria row: (database column or `None` for an unknown header, criterion).
type CritRow = Vec<(Option<usize>, Criterion)>;

fn criteria_rows(db: &Array2, crit: &Array2) -> Vec<CritRow> {
    let cols: Vec<Option<usize>> = crit
        .row(0)
        .map(|h| {
            if h.is_empty() {
                return None;
            }
            let k = header_key(h);
            db.row(0).position(|d| !d.is_empty() && header_key(d) == k)
        })
        .collect();
    (1..crit.rows)
        .map(|r| {
            crit.row(r)
                .zip(&cols)
                .filter(|(v, _)| !v.is_empty())
                .map(|(v, c)| (*c, Criterion::parse_database(v)))
                .collect()
        })
        .collect()
}

/// Indices (1-based database rows) of the records that satisfy the criteria.
fn matching_records(db: &Array2, crit: &Array2) -> Vec<usize> {
    let rows = criteria_rows(db, crit);
    (1..db.rows)
        .filter(|&r| {
            rows.iter().any(|conds| {
                conds.iter().all(|(c, k)| match c {
                    Some(c) => k.matches(db.get(r, *c)),
                    None => false,
                })
            })
        })
        .collect()
}

/// Evaluate the arguments and return the matching records' values of the field column
/// (or `None` per record when the field was omitted).
fn select(ev: &mut Ev<'_>, args: &[Expr]) -> Result<Vec<Option<Value>>, ErrorKind> {
    let (Some(de), Some(ce)) = (args.first(), args.get(2)) else {
        return Err(ErrorKind::Value);
    };
    let db = ev.eval_array(de)?;
    if db.rows < 2 || db.cols == 0 {
        return Err(ErrorKind::Value);
    }
    let col = field_column(ev, &db, args.get(1))?;
    let crit = ev.eval_array(ce)?;
    if crit.rows < 2 {
        return Err(ErrorKind::Value);
    }
    Ok(matching_records(&db, &crit)
        .into_iter()
        .map(|r| col.map(|c| db.get(r, c).clone()))
        .collect())
}

/// Numbers among the selected field values; errors propagate.
fn numbers(ev: &mut Ev<'_>, args: &[Expr]) -> Result<Vec<f64>, ErrorKind> {
    let mut out = Vec::new();
    for v in select(ev, args)?.into_iter().flatten() {
        match v {
            Value::Number(n) => out.push(n),
            Value::Error(e) => return Err(e),
            _ => {}
        }
    }
    Ok(out)
}

fn ret(r: Result<f64, ErrorKind>) -> EvalValue {
    super::ret(r)
}

fn variance(v: &[f64], sample: bool) -> Result<f64, ErrorKind> {
    let n = v.len() as f64;
    let d = if sample { n - 1.0 } else { n };
    if d <= 0.0 {
        return Err(ErrorKind::Div0);
    }
    let m = v.iter().sum::<f64>() / n;
    Ok(v.iter().map(|x| (x - m) * (x - m)).sum::<f64>() / d)
}

fn dcount(ev: &mut Ev<'_>, args: &[Expr], all: bool) -> EvalValue {
    match select(ev, args) {
        Ok(vals) => EvalValue::num(
            vals.iter()
                .filter(|v| match v {
                    None => true,
                    Some(Value::Number(_)) => true,
                    Some(x) => all && !x.is_empty(),
                })
                .count() as f64,
        ),
        Err(e) => EvalValue::err(e),
    }
}

fn dget(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    match select(ev, args) {
        Ok(vals) => match vals.as_slice() {
            [Some(v)] => EvalValue::Scalar(v.clone()),
            [None] | [] => EvalValue::err(ErrorKind::Value),
            _ => EvalValue::err(ErrorKind::Num),
        },
        Err(e) => EvalValue::err(e),
    }
}

fn agg(ev: &mut Ev<'_>, args: &[Expr], f: fn(&[f64]) -> Result<f64, ErrorKind>) -> EvalValue {
    ret(numbers(ev, args).and_then(|v| f(&v)))
}

pub fn register(r: &mut Registry) {
    r.add("DAVERAGE", 3, 3, |ev, a| {
        agg(ev, a, |v| {
            if v.is_empty() {
                Err(ErrorKind::Div0)
            } else {
                Ok(v.iter().sum::<f64>() / v.len() as f64)
            }
        })
    });
    r.add("DCOUNT", 3, 3, |ev, a| dcount(ev, a, false));
    r.add("DCOUNTA", 3, 3, |ev, a| dcount(ev, a, true));
    r.add("DGET", 3, 3, dget);
    r.add("DMAX", 3, 3, |ev, a| {
        agg(ev, a, |v| {
            Ok(v.iter().copied().reduce(f64::max).unwrap_or(0.0))
        })
    });
    r.add("DMIN", 3, 3, |ev, a| {
        agg(ev, a, |v| {
            Ok(v.iter().copied().reduce(f64::min).unwrap_or(0.0))
        })
    });
    r.add("DPRODUCT", 3, 3, |ev, a| {
        agg(ev, a, |v| {
            Ok(if v.is_empty() {
                0.0
            } else {
                v.iter().product()
            })
        })
    });
    r.add("DSTDEV", 3, 3, |ev, a| {
        agg(ev, a, |v| variance(v, true).map(f64::sqrt))
    });
    r.add("DSTDEVP", 3, 3, |ev, a| {
        agg(ev, a, |v| variance(v, false).map(f64::sqrt))
    });
    r.add("DSUM", 3, 3, |ev, a| agg(ev, a, |v| Ok(v.iter().sum())));
    r.add("DVAR", 3, 3, |ev, a| agg(ev, a, |v| variance(v, true)));
    r.add("DVARP", 3, 3, |ev, a| agg(ev, a, |v| variance(v, false)));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Value {
        Value::text(s)
    }

    fn db() -> Array2 {
        Array2::new(
            4,
            2,
            vec![
                t("Name"),
                t("Qty"),
                t("Apple"),
                Value::Number(10.0),
                t("Pear"),
                Value::Number(5.0),
                t("Applesauce"),
                Value::Number(2.0),
            ],
        )
    }

    #[test]
    fn prefix_and_exact_text() {
        let c = Array2::new(2, 1, vec![t("name"), t("Apple")]);
        assert_eq!(matching_records(&db(), &c), vec![1, 3]);
        let c = Array2::new(2, 1, vec![t("Name"), t("=Apple")]);
        assert_eq!(matching_records(&db(), &c), vec![1]);
    }

    #[test]
    fn or_rows_and_empty_row() {
        let c = Array2::new(
            3,
            2,
            vec![
                t("Name"),
                t("Qty"),
                t("P"),
                Value::Empty,
                Value::Empty,
                t(">9"),
            ],
        );
        assert_eq!(matching_records(&db(), &c), vec![1, 2]);
        let c = Array2::new(2, 1, vec![t("Qty"), Value::Empty]);
        assert_eq!(matching_records(&db(), &c), vec![1, 2, 3]);
    }

    #[test]
    fn unknown_header_with_value_never_matches() {
        let c = Array2::new(2, 1, vec![t("Nope"), t("x")]);
        assert!(matching_records(&db(), &c).is_empty());
        let c = Array2::new(2, 1, vec![Value::Empty, t("Apple")]);
        assert!(matching_records(&db(), &c).is_empty());
    }

    #[test]
    fn variance_needs_points() {
        assert_eq!(variance(&[1.0], true), Err(ErrorKind::Div0));
        assert_eq!(variance(&[1.0, 3.0], false), Ok(1.0));
    }
}
