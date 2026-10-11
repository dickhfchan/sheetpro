//! AST → formula text (without the leading `=`).

use crate::ast::{CellPart, Expr, RefExpr, RefKind, UnOp};
use sp_core::addr::quote_sheet_name;
use sp_core::col_to_letters;
use sp_core::value::format_general;

pub fn to_formula(e: &Expr) -> String {
    let mut s = String::new();
    write(e, &mut s);
    s
}

fn num(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        let g = format!("{n}");
        if g.len() <= 17 { g } else { format_general(n) }
    }
}

pub fn cell_part(c: &CellPart) -> String {
    format!(
        "{}{}{}{}",
        if c.col_abs { "$" } else { "" },
        col_to_letters(c.col),
        if c.row_abs { "$" } else { "" },
        u64::from(c.row) + 1
    )
}

pub fn ref_text(r: &RefExpr) -> String {
    let body = match &r.kind {
        RefKind::Cell(c) => cell_part(c),
        RefKind::Area(a, b) => format!("{}:{}", cell_part(a), cell_part(b)),
        RefKind::Cols {
            c0,
            c0_abs,
            c1,
            c1_abs,
        } => format!(
            "{}{}:{}{}",
            if *c0_abs { "$" } else { "" },
            col_to_letters(*c0),
            if *c1_abs { "$" } else { "" },
            col_to_letters(*c1)
        ),
        RefKind::Rows {
            r0,
            r0_abs,
            r1,
            r1_abs,
        } => format!(
            "{}{}:{}{}",
            if *r0_abs { "$" } else { "" },
            u64::from(*r0) + 1,
            if *r1_abs { "$" } else { "" },
            u64::from(*r1) + 1
        ),
    };
    match &r.sheet {
        Some(s) => format!("{}!{}", quote_sheet_name(s), body),
        None => body,
    }
}

fn write(e: &Expr, out: &mut String) {
    match e {
        Expr::Number(n) => out.push_str(&num(*n)),
        Expr::Text(t) => {
            out.push('"');
            out.push_str(&t.replace('"', "\"\""));
            out.push('"');
        }
        Expr::Bool(b) => out.push_str(if *b { "TRUE" } else { "FALSE" }),
        Expr::Error(err) => out.push_str(err.as_str()),
        Expr::Ref(r) => out.push_str(&ref_text(r)),
        Expr::Name(n) => out.push_str(n),
        Expr::Array(rows) => {
            out.push('{');
            for (i, row) in rows.iter().enumerate() {
                if i > 0 {
                    out.push(';');
                }
                for (j, item) in row.iter().enumerate() {
                    if j > 0 {
                        out.push(',');
                    }
                    write(item, out);
                }
            }
            out.push('}');
        }
        Expr::Unary(op, a) => match op {
            UnOp::Neg => {
                out.push('-');
                write(a, out);
            }
            UnOp::Plus => {
                out.push('+');
                write(a, out);
            }
            UnOp::Percent => {
                write(a, out);
                out.push('%');
            }
        },
        Expr::Binary(op, a, b) => {
            write(a, out);
            out.push_str(op.symbol());
            write(b, out);
        }
        Expr::Call(name, args) => {
            out.push_str(name);
            out.push('(');
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write(a, out);
            }
            out.push(')');
        }
        Expr::Missing => {}
        Expr::Spill(a) => {
            write(a, out);
            out.push('#');
        }
        Expr::Paren(a) => {
            out.push('(');
            write(a, out);
            out.push(')');
        }
        Expr::Implicit(a) => {
            out.push('@');
            if matches!(**a, Expr::Binary(..) | Expr::Unary(..)) {
                out.push('(');
                write(a, out);
                out.push(')');
            } else {
                write(a, out);
            }
        }
        Expr::Invoke(callee, args) => {
            write(callee, out);
            out.push('(');
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write(a, out);
            }
            out.push(')');
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn exponent_numbers_print_plain() {
        let e = crate::parser::parse("1E+3").unwrap();
        assert_eq!(super::to_formula(&e), "1000");
    }
}
