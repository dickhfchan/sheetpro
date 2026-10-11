//! Data-analysis functions: GETPIVOTDATA (against the pivot model), GROUPBY, PIVOTBY and
//! the data-table cell function TABLE (volatile; see `crate::whatif`).

use super::Registry;
use super::arrays::MAX_ARRAY_CELLS;
use crate::ast::Expr;
use crate::eval::{Array2, Callable, Ev, EvalValue, is_omitted, to_number};
use crate::pivot::{ItemKey, cmp_items, item_key};
use sp_core::{CellRange, ErrorKind, Value};
use std::cmp::Ordering;
use std::collections::HashMap;

type R<T> = Result<T, ErrorKind>;

fn out(r: R<Array2>) -> EvalValue {
    match r {
        Ok(a) if a.rows * a.cols == 1 => {
            EvalValue::Scalar(a.data.into_iter().next().unwrap_or(Value::Empty))
        }
        Ok(a) => EvalValue::Array(a),
        Err(e) => EvalValue::err(e),
    }
}

fn array_of(ev: &mut Ev<'_>, e: &Expr) -> R<Array2> {
    let v = ev.eval(e);
    if let Some(err) = v.as_error() {
        return Err(err);
    }
    ev.to_array(v)
}

/// Optional integer argument (`None` when omitted or missing).
fn opt_int(ev: &mut Ev<'_>, args: &[Expr], i: usize) -> R<Option<i64>> {
    let Some(e) = args.get(i) else {
        return Ok(None);
    };
    if matches!(e, Expr::Missing) {
        return Ok(None);
    }
    let v = ev.eval(e);
    if is_omitted(&v) {
        return Ok(None);
    }
    let n = to_number(&ev.to_scalar(v))?.trunc();
    if !(-1e6..=1e6).contains(&n) {
        return Err(ErrorKind::Value);
    }
    Ok(Some(n as i64))
}

fn func(ev: &mut Ev<'_>, e: Option<&Expr>) -> R<Callable> {
    let f = e.and_then(|e| ev.callable(e)).ok_or(ErrorKind::Value)?;
    match ev.arity(f) {
        Some(k) if k != 1 => Err(ErrorKind::Value),
        _ => Ok(f),
    }
}

/// Apply the aggregation callable to a column of values.
fn apply_col(ev: &mut Ev<'_>, f: Callable, vals: Vec<Value>) -> Value {
    let n = vals.len();
    let r = ev.apply(f, vec![EvalValue::Array(Array2::new(n, 1, vals))]);
    match &r {
        EvalValue::Array(a) if a.rows * a.cols > 1 => Value::Error(ErrorKind::Calc),
        EvalValue::Ref(x) if !x.range.is_single() => Value::Error(ErrorKind::Calc),
        _ => match ev.to_scalar(r) {
            Value::Empty => Value::Number(0.0),
            x => x,
        },
    }
}

fn cmp_keys(a: &[Value], b: &[Value]) -> Ordering {
    for (x, y) in a.iter().zip(b) {
        let o = cmp_items(x, y);
        if o != Ordering::Equal {
            return o;
        }
    }
    a.len().cmp(&b.len())
}

/// Distinct keys of `rows` (each a slice of field values) with their member row indexes,
/// in first-seen order.
fn group(rows: &[(usize, Vec<Value>)]) -> Vec<(Vec<Value>, Vec<usize>)> {
    let mut idx: HashMap<Vec<ItemKey>, usize> = HashMap::new();
    let mut out: Vec<(Vec<Value>, Vec<usize>)> = Vec::new();
    for (i, k) in rows {
        let kk: Vec<ItemKey> = k.iter().map(item_key).collect();
        match idx.get(&kk) {
            Some(g) => {
                if let Some(slot) = out.get_mut(*g) {
                    slot.1.push(*i);
                }
            }
            None => {
                idx.insert(kk, out.len());
                out.push((k.clone(), vec![*i]));
            }
        }
    }
    out
}

/// Header handling: (data has a header row, show headers, generate headers).
fn headers(mode: Option<i64>, vals: &Array2) -> R<(bool, bool, bool)> {
    Ok(match mode {
        None => {
            let auto = vals.rows >= 2
                && matches!(vals.get(0, 0), Value::Text(_))
                && matches!(vals.get(1, 0), Value::Number(_));
            (auto, auto, false)
        }
        Some(0) => (false, false, false),
        Some(1) => (true, false, false),
        Some(2) => (false, false, true),
        Some(3) => (true, true, false),
        Some(_) => return Err(ErrorKind::Value),
    })
}

fn filter_mask(ev: &mut Ev<'_>, args: &[Expr], i: usize, n: usize, start: usize) -> R<Option<Vec<bool>>> {
    let Some(e) = args.get(i) else {
        return Ok(None);
    };
    if matches!(e, Expr::Missing) {
        return Ok(None);
    }
    let v = ev.eval(e);
    if is_omitted(&v) {
        return Ok(None);
    }
    let a = ev.to_array(v)?;
    let flat: Vec<&Value> = if a.cols == 1 {
        a.col(0).collect()
    } else if a.rows == 1 {
        a.row(0).collect()
    } else {
        return Err(ErrorKind::Value);
    };
    let skip = if flat.len() == n + start { start } else { 0 };
    if flat.len() != n + skip {
        return Err(ErrorKind::Value);
    }
    let mut m = Vec::with_capacity(n);
    for v in flat.into_iter().skip(skip) {
        m.push(match v {
            Value::Bool(b) => *b,
            Value::Number(x) => *x != 0.0,
            Value::Empty => false,
            Value::Error(e) => return Err(*e),
            Value::Text(_) => return Err(ErrorKind::Value),
        });
    }
    Ok(Some(m))
}

fn row_vals(a: &Array2, r: usize) -> Vec<Value> {
    a.row(r).cloned().collect()
}

/// GROUPBY(row_fields, values, function, [field_headers], [total_depth], [sort_order],
/// [filter_array], [field_relationship])
fn groupby(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    out((|| {
        let keys = array_of(ev, args.first().ok_or(ErrorKind::Value)?)?;
        let vals = array_of(ev, args.get(1).ok_or(ErrorKind::Value)?)?;
        let f = func(ev, args.get(2))?;
        if keys.rows != vals.rows || keys.rows == 0 {
            return Err(ErrorKind::Value);
        }
        let (has_h, show_h, gen_h) = headers(opt_int(ev, args, 3)?, &vals)?;
        let depth = opt_int(ev, args, 4)?.unwrap_or(1);
        if !(-2..=2).contains(&depth) {
            return Err(ErrorKind::Value);
        }
        let sort = opt_int(ev, args, 5)?;
        let start = usize::from(has_h);
        let n = keys.rows - start;
        let mask = filter_mask(ev, args, 6, n, start)?;
        let k = keys.cols;
        let m = vals.cols;
        let rows: Vec<(usize, Vec<Value>)> = (start..keys.rows)
            .filter(|r| mask.as_ref().is_none_or(|mk| mk.get(r - start).copied().unwrap_or(false)))
            .map(|r| (r, row_vals(&keys, r)))
            .collect();
        let mut groups = group(&rows);
        groups.sort_by(|a, b| cmp_keys(&a.0, &b.0));
        // Aggregate each group.
        let mut body: Vec<(Vec<Value>, Vec<Value>)> = Vec::with_capacity(groups.len());
        for (key, members) in &groups {
            let mut res = Vec::with_capacity(m);
            for j in 0..m {
                let col: Vec<Value> = members.iter().map(|r| vals.get(*r, j).clone()).collect();
                res.push(apply_col(ev, f, col));
            }
            body.push((key.clone(), res));
        }
        if let Some(s) = sort
            && s != 0
        {
            let col = s.unsigned_abs() as usize - 1;
            if col >= k + m {
                return Err(ErrorKind::Value);
            }
            body.sort_by(|a, b| {
                let va = if col < k { a.0.get(col) } else { a.1.get(col - k) };
                let vb = if col < k { b.0.get(col) } else { b.1.get(col - k) };
                let o = match (va, vb) {
                    (Some(x), Some(y)) => cmp_items(x, y),
                    _ => Ordering::Equal,
                };
                if s < 0 { o.reverse() } else { o }
            });
        }
        let width = k + m;
        let mut data: Vec<Value> = Vec::new();
        let mut nrows = 0usize;
        let mut push_row = |data: &mut Vec<Value>, r: Vec<Value>| {
            data.extend(r);
            nrows += 1;
        };
        if show_h || gen_h {
            let mut h = Vec::with_capacity(width);
            for c in 0..k {
                h.push(if show_h {
                    keys.get(0, c).clone()
                } else {
                    Value::text(format!("Row Field {}", c + 1))
                });
            }
            for c in 0..m {
                h.push(if show_h {
                    vals.get(0, c).clone()
                } else {
                    Value::text(format!("Value {}", c + 1))
                });
            }
            push_row(&mut data, h);
        }
        let totals_row = |ev: &mut Ev<'_>, label: Vec<Value>, members: Vec<usize>| -> Vec<Value> {
            let mut r = label;
            r.resize(k, Value::Empty);
            for j in 0..m {
                let col: Vec<Value> = members.iter().map(|x| vals.get(*x, j).clone()).collect();
                r.push(apply_col(ev, f, col));
            }
            r
        };
        let all: Vec<usize> = rows.iter().map(|(r, _)| *r).collect();
        if depth < 0 {
            let t = totals_row(ev, vec![Value::text("Grand Total")], all.clone());
            push_row(&mut data, t);
        }
        let subtotals = depth.abs() >= 2 && k >= 2;
        let mut i = 0usize;
        while i < body.len() {
            // One level-0 group at a time.
            let first = body.get(i).and_then(|b| b.0.first()).cloned().unwrap_or(Value::Empty);
            let mut j = i;
            while j < body.len()
                && body
                    .get(j)
                    .and_then(|b| b.0.first())
                    .is_some_and(|v| item_key(v) == item_key(&first))
            {
                j += 1;
            }
            if !subtotals {
                j = i + 1;
            }
            let members: Vec<usize> = if subtotals {
                rows.iter()
                    .filter(|(_, kv)| kv.first().is_some_and(|v| item_key(v) == item_key(&first)))
                    .map(|(r, _)| *r)
                    .collect()
            } else {
                Vec::new()
            };
            if subtotals && depth < 0 {
                let t = totals_row(ev, vec![first.clone()], members.clone());
                push_row(&mut data, t);
            }
            for b in body.get(i..j).unwrap_or(&[]) {
                let mut r = b.0.clone();
                r.extend(b.1.iter().cloned());
                push_row(&mut data, r);
            }
            if subtotals && depth > 0 {
                let t = totals_row(ev, vec![first.clone()], members);
                push_row(&mut data, t);
            }
            i = j.max(i + 1);
            if data.len() > MAX_ARRAY_CELLS {
                return Err(ErrorKind::Num);
            }
        }
        if depth > 0 {
            let label = if subtotals { "Grand Total" } else { "Total" };
            let t = totals_row(ev, vec![Value::text(label)], all);
            push_row(&mut data, t);
        }
        if nrows == 0 {
            return Err(ErrorKind::Calc);
        }
        Ok(Array2::new(nrows, width, data))
    })())
}

/// PIVOTBY(row_fields, col_fields, values, function, [field_headers], [row_total_depth],
/// [row_sort_order], [col_total_depth], [col_sort_order], [filter_array], [relative_to])
fn pivotby(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    out((|| {
        let rk = array_of(ev, args.first().ok_or(ErrorKind::Value)?)?;
        let ck = array_of(ev, args.get(1).ok_or(ErrorKind::Value)?)?;
        let vals = array_of(ev, args.get(2).ok_or(ErrorKind::Value)?)?;
        let f = func(ev, args.get(3))?;
        if rk.rows != vals.rows || ck.rows != vals.rows || vals.rows == 0 {
            return Err(ErrorKind::Value);
        }
        let (has_h, show_h, gen_h) = headers(opt_int(ev, args, 4)?, &vals)?;
        let rdepth = opt_int(ev, args, 5)?.unwrap_or(1);
        let rsort = opt_int(ev, args, 6)?.unwrap_or(1);
        let cdepth = opt_int(ev, args, 7)?.unwrap_or(1);
        let csort = opt_int(ev, args, 8)?.unwrap_or(1);
        let start = usize::from(has_h);
        let n = vals.rows - start;
        let mask = filter_mask(ev, args, 9, n, start)?;
        let keep: Vec<usize> = (start..vals.rows)
            .filter(|r| mask.as_ref().is_none_or(|mk| mk.get(r - start).copied().unwrap_or(false)))
            .collect();
        let mut rgroups = group(&keep.iter().map(|r| (*r, row_vals(&rk, *r))).collect::<Vec<_>>());
        let mut cgroups = group(&keep.iter().map(|r| (*r, row_vals(&ck, *r))).collect::<Vec<_>>());
        rgroups.sort_by(|a, b| {
            let o = cmp_keys(&a.0, &b.0);
            if rsort < 0 { o.reverse() } else { o }
        });
        cgroups.sort_by(|a, b| {
            let o = cmp_keys(&a.0, &b.0);
            if csort < 0 { o.reverse() } else { o }
        });
        let m = vals.cols.max(1);
        let kr = rk.cols;
        let kc = ck.cols;
        // Column lines: (group index or None for the total).
        let mut clines: Vec<Option<usize>> = (0..cgroups.len()).map(Some).collect();
        if cdepth > 0 {
            clines.push(None);
        } else if cdepth < 0 {
            clines.insert(0, None);
        }
        let mut rlines: Vec<Option<usize>> = (0..rgroups.len()).map(Some).collect();
        if rdepth > 0 {
            rlines.push(None);
        } else if rdepth < 0 {
            rlines.insert(0, None);
        }
        let value_hdr = m > 1 || show_h || gen_h;
        let hrows = kc + usize::from(value_hdr);
        let width = kr + clines.len() * m;
        let height = hrows + rlines.len();
        if width.saturating_mul(height) > MAX_ARRAY_CELLS {
            return Err(ErrorKind::Num);
        }
        let mut a = Array2::filled(height, width, Value::Empty);
        // The corner shows blank (an empty anchor would read as 0).
        if let Some(x) = a.data.first_mut() {
            *x = Value::text("");
        }
        let set = |a: &mut Array2, r: usize, c: usize, v: Value| {
            if r < a.rows
                && c < a.cols
                && let Some(x) = a.data.get_mut(r * a.cols + c)
            {
                *x = v;
            }
        };
        for (li, cl) in clines.iter().enumerate() {
            let c0 = kr + li * m;
            match cl {
                Some(g) => {
                    if let Some((key, _)) = cgroups.get(*g) {
                        for (l, v) in key.iter().enumerate() {
                            set(&mut a, l, c0, v.clone());
                        }
                    }
                }
                None => set(&mut a, 0, c0, Value::text("Total")),
            }
            if value_hdr {
                for j in 0..m {
                    let h = if show_h {
                        vals.get(0, j).clone()
                    } else {
                        Value::text(format!("Value {}", j + 1))
                    };
                    set(&mut a, kc, c0 + j, h);
                }
            }
        }
        if show_h && value_hdr {
            for c in 0..kr {
                set(&mut a, kc, c, rk.get(0, c).clone());
            }
        }
        let members = |g: Option<usize>, groups: &[(Vec<Value>, Vec<usize>)]| -> Vec<usize> {
            match g.and_then(|g| groups.get(g)) {
                Some((_, m)) => m.clone(),
                None => keep.clone(),
            }
        };
        for (ri, rl) in rlines.iter().enumerate() {
            let r = hrows + ri;
            match rl {
                Some(g) => {
                    if let Some((key, _)) = rgroups.get(*g) {
                        for (l, v) in key.iter().enumerate() {
                            set(&mut a, r, l, v.clone());
                        }
                    }
                }
                None => set(&mut a, r, 0, Value::text("Total")),
            }
            let rm = members(*rl, &rgroups);
            for (li, cl) in clines.iter().enumerate() {
                let cm = members(*cl, &cgroups);
                let both: Vec<usize> = rm.iter().copied().filter(|x| cm.binary_search(x).is_ok()).collect();
                if both.is_empty() {
                    continue;
                }
                for j in 0..m {
                    let col: Vec<Value> = both.iter().map(|x| vals.get(*x, j).clone()).collect();
                    let v = apply_col(ev, f, col);
                    set(&mut a, r, kr + li * m + j, v);
                }
            }
        }
        Ok(a)
    })())
}

/// GETPIVOTDATA(data_field, pivot_table, [field1, item1], …)
pub fn getpivotdata(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let r: R<Value> = (|| {
        let data = ev.text(args.first().ok_or(ErrorKind::Value)?)?;
        let target = match ev.eval(args.get(1).ok_or(ErrorKind::Value)?) {
            EvalValue::Ref(r) => r,
            EvalValue::Refs(rs) => *rs.first().ok_or(ErrorKind::Ref)?,
            EvalValue::Scalar(Value::Error(e)) => return Err(e),
            _ => return Err(ErrorKind::Ref),
        };
        let rest = args.get(2..).unwrap_or(&[]);
        if rest.len() % 2 != 0 {
            return Err(ErrorKind::Ref);
        }
        let mut pairs = Vec::with_capacity(rest.len() / 2);
        for p in rest.chunks(2) {
            let (Some(fe), Some(ie)) = (p.first(), p.get(1)) else {
                return Err(ErrorKind::Ref);
            };
            let field = ev.text(fe)?;
            let item = ev.eval_scalar(ie);
            if let Value::Error(e) = item {
                return Err(e);
            }
            pairs.push((field, item));
        }
        let wb = ev.host.workbook().ok_or(ErrorKind::Ref)?;
        let sheet = wb.sheet(target.sheet).ok_or(ErrorKind::Ref)?;
        let at = target.range.start;
        let pivot = sheet
            .analysis
            .pivots
            .iter()
            .find(|p| {
                let area = |t: &str| {
                    let body = t.rsplit('!').next().unwrap_or(t);
                    CellRange::parse(body).ok()
                };
                area(&p.output)
                    .or_else(|| area(&p.location))
                    .is_some_and(|r| r.contains(at))
            })
            .ok_or(ErrorKind::Ref)?;
        Ok(crate::pivot::query(pivot, &data, &pairs))
    })();
    match r {
        Ok(v) => EvalValue::Scalar(v),
        Err(e) => EvalValue::err(e),
    }
}

/// TABLE(row_input, col_input): a what-if data-table cell.
fn table(ev: &mut Ev<'_>, args: &[Expr]) -> EvalValue {
    let mut cell = |i: usize| -> R<Option<(usize, sp_core::CellAddr)>> {
        let Some(e) = args.get(i) else {
            return Ok(None);
        };
        if matches!(e, Expr::Missing) {
            return Ok(None);
        }
        match ev.eval(e) {
            EvalValue::Ref(r) if r.range.is_single() => Ok(Some((r.sheet, r.range.start))),
            v if is_omitted(&v) => Ok(None),
            _ => Err(ErrorKind::Ref),
        }
    };
    let (ri, ci) = match (cell(0), cell(1)) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => return EvalValue::err(e),
    };
    EvalValue::Scalar(crate::whatif::table_value(ev.host, ev.sheet, ev.cell, ri, ci))
}

pub fn register(r: &mut Registry) {
    r.add("GETPIVOTDATA", 2, 254, getpivotdata);
    r.add("GROUPBY", 3, 8, groupby);
    r.add("PIVOTBY", 4, 11, pivotby);
    r.add("TABLE", 1, 2, table);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use crate::recalc::{AstCache, RecalcOptions, recalc};
    use sp_core::{CellAddr, Value};
    use sp_model::{Cell, Workbook};

    fn book(cells: &[(&str, &str)]) -> Workbook {
        let mut wb = Workbook::new();
        for (a, input) in cells {
            let addr = CellAddr::parse(a).unwrap();
            let cell = match input.strip_prefix('=') {
                Some(f) => Cell::formula(f),
                None => match input.parse::<f64>() {
                    Ok(n) => Cell::constant(Value::Number(n)),
                    Err(_) => Cell::constant(Value::text(*input)),
                },
            };
            wb.sheets[0].set(addr, cell);
        }
        let mut cache = AstCache::default();
        recalc(&mut wb, &mut cache, RecalcOptions::default());
        wb
    }

    fn txt(wb: &Workbook, a: &str) -> String {
        wb.sheets[0].value(CellAddr::parse(a).unwrap()).to_display()
    }

    fn data() -> Vec<(&'static str, &'static str)> {
        vec![
            ("A1", "Region"),
            ("B1", "Qtr"),
            ("C1", "Sales"),
            ("A2", "West"),
            ("B2", "Q1"),
            ("C2", "20"),
            ("A3", "East"),
            ("B3", "Q1"),
            ("C3", "10"),
            ("A4", "East"),
            ("B4", "Q2"),
            ("C4", "12"),
            ("A5", "West"),
            ("B5", "Q2"),
            ("C5", "8"),
        ]
    }

    #[test]
    fn groupby_sums_with_headers_and_total() {
        let mut cells = data();
        cells.push(("E1", "=GROUPBY(A1:A5,C1:C5,SUM)"));
        let wb = book(&cells);
        let got: Vec<String> = ["E1", "F1", "E2", "F2", "E3", "F3", "E4", "F4"]
            .iter()
            .map(|a| txt(&wb, a))
            .collect();
        assert_eq!(got, ["Region", "Sales", "East", "22", "West", "28", "Total", "50"]);
    }

    #[test]
    fn groupby_options() {
        let mut cells = data();
        // No headers, no totals, sort descending by value, AVERAGE.
        cells.push(("E1", "=GROUPBY(A2:A5,C2:C5,AVERAGE,0,0,-2)"));
        cells.push(("H1", "=GROUPBY(A2:B5,C2:C5,SUM,0,2)"));
        let wb = book(&cells);
        assert_eq!(txt(&wb, "E1"), "West");
        assert_eq!(txt(&wb, "F1"), "14");
        assert_eq!(txt(&wb, "E2"), "East");
        assert_eq!(txt(&wb, "E3"), "");
        // Subtotals: East Q1, East Q2, East subtotal, West…, Grand Total.
        assert_eq!(txt(&wb, "H3"), "East");
        assert_eq!(txt(&wb, "J3"), "22");
        assert_eq!(txt(&wb, "H7"), "Grand Total");
        assert_eq!(txt(&wb, "J7"), "50");
    }

    #[test]
    fn pivotby_grid() {
        let mut cells = data();
        cells.push(("E1", "=PIVOTBY(A2:A5,B2:B5,C2:C5,SUM)"));
        let wb = book(&cells);
        let row = |r: u32| -> Vec<String> {
            ["E", "F", "G", "H"]
                .iter()
                .map(|c| txt(&wb, &format!("{c}{r}")))
                .collect()
        };
        assert_eq!(row(1), ["", "Q1", "Q2", "Total"]);
        assert_eq!(row(2), ["East", "10", "12", "22"]);
        assert_eq!(row(3), ["West", "20", "8", "28"]);
        assert_eq!(row(4), ["Total", "30", "20", "50"]);
    }

    #[test]
    fn getpivotdata_without_pivot_is_ref() {
        let wb = book(&[("A1", "=GETPIVOTDATA(\"Sales\",B2)")]);
        assert_eq!(txt(&wb, "A1"), "#REF!");
    }
}
