//! AST → formula text (without the leading `=`).

use crate::ast::{CellPart, Expr, RefExpr, RefKind, UnOp};
use sp_core::addr::quote_sheet_name;
use sp_core::col_to_letters;

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
        if g.len() <= 17 {
            return g;
        }
        // A typed number keeps 15 significant digits (as the target does), not the
        // 10 of the General display format.
        let r: f64 = format!("{n:.14e}").parse().unwrap_or(n);
        if (1e-5..1e15).contains(&r.abs()) {
            format!("{r}")
        } else {
            format!("{r:e}").replace('e', "E")
        }
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
        Expr::Table(t) => out.push_str(&crate::structref::print(t)),
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

    /// Long literals keep 15 significant digits (they were cut to the 10 of the General
    /// display format, so `=NORM.S.INV(0.50000039894228032)` lost its last digits).
    #[test]
    fn long_numbers_keep_fifteen_digits() {
        let p = |f: &str| super::to_formula(&crate::parser::parse(f).unwrap());
        assert_eq!(p("0.50000039894228032-0.5"), "0.50000039894228-0.5");
        assert_eq!(p("7.0710678118654757E-7"), "7.07106781186548E-7");
        assert_eq!(p("1.2345678901234567E+20"), "1.23456789012346E20");
        assert_eq!(p("0.1+0.25"), "0.1+0.25");
        assert_eq!(p("1E-300"), "1E-300");
    }
}
