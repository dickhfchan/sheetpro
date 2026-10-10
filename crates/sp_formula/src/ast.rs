//! Formula AST.

use sp_core::ErrorKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CellPart {
    pub row: u32,
    pub col: u32,
    pub row_abs: bool,
    pub col_abs: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RefKind {
    Cell(CellPart),
    Area(CellPart, CellPart),
    /// Whole columns `A:C` (row fields unused).
    Cols {
        c0: u32,
        c0_abs: bool,
        c1: u32,
        c1_abs: bool,
    },
    /// Whole rows `2:5`.
    Rows {
        r0: u32,
        r0_abs: bool,
        r1: u32,
        r1_abs: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct RefExpr {
    pub sheet: Option<String>,
    pub kind: RefKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Plus,
    Percent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
    Concat,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Range,
    Union,
    Isect,
}

impl BinOp {
    pub fn symbol(self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Pow => "^",
            BinOp::Concat => "&",
            BinOp::Eq => "=",
            BinOp::Ne => "<>",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::Range => ":",
            BinOp::Union => ",",
            BinOp::Isect => " ",
        }
    }

    /// Binding power (higher binds tighter), following the target's operator precedence.
    pub fn precedence(self) -> u8 {
        match self {
            BinOp::Range => 90,
            BinOp::Isect => 85,
            BinOp::Union => 80,
            BinOp::Pow => 60,
            BinOp::Mul | BinOp::Div => 50,
            BinOp::Add | BinOp::Sub => 40,
            BinOp::Concat => 30,
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => 20,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Number(f64),
    Text(String),
    Bool(bool),
    Error(ErrorKind),
    Ref(RefExpr),
    Name(String),
    /// `{1,2;3,4}` constant array, row-major.
    Array(Vec<Vec<Expr>>),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    /// Function call; name is upper-case without `_xlfn.` prefixes.
    Call(String, Vec<Expr>),
    /// Omitted argument, e.g. the middle of `IF(A1,,2)`.
    Missing,
    /// `A1#` spilled-range reference.
    Spill(Box<Expr>),
    Paren(Box<Expr>),
}

impl Expr {
    /// Visit every node depth-first.
    pub fn walk(&self, f: &mut impl FnMut(&Expr)) {
        f(self);
        match self {
            Expr::Unary(_, a) | Expr::Spill(a) | Expr::Paren(a) => a.walk(f),
            Expr::Binary(_, a, b) => {
                a.walk(f);
                b.walk(f);
            }
            Expr::Call(_, args) => args.iter().for_each(|a| a.walk(f)),
            Expr::Array(rows) => rows.iter().flatten().for_each(|a| a.walk(f)),
            _ => {}
        }
    }

    pub fn walk_mut(&mut self, f: &mut impl FnMut(&mut Expr)) {
        f(self);
        match self {
            Expr::Unary(_, a) | Expr::Spill(a) | Expr::Paren(a) => a.walk_mut(f),
            Expr::Binary(_, a, b) => {
                a.walk_mut(f);
                b.walk_mut(f);
            }
            Expr::Call(_, args) => args.iter_mut().for_each(|a| a.walk_mut(f)),
            Expr::Array(rows) => rows.iter_mut().flatten().for_each(|a| a.walk_mut(f)),
            _ => {}
        }
    }

    /// True if any called function is volatile (recomputed on every recalc).
    pub fn is_volatile(&self) -> bool {
        let mut v = false;
        self.walk(&mut |e| {
            if let Expr::Call(n, _) = e {
                v |= matches!(
                    n.as_str(),
                    "NOW"
                        | "TODAY"
                        | "RAND"
                        | "RANDBETWEEN"
                        | "RANDARRAY"
                        | "OFFSET"
                        | "INDIRECT"
                        | "INFO"
                        | "CELL"
                );
            }
        });
        v
    }
}
