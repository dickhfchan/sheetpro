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
    /// `A1#` spilled-range reference (`ANCHORARRAY` in files).
    Spill(Box<Expr>),
    Paren(Box<Expr>),
    /// `@x` implicit intersection (`SINGLE` in files).
    Implicit(Box<Expr>),
    /// Call of a computed function value, e.g. `LAMBDA(x,x+1)(5)`.
    Invoke(Box<Expr>, Vec<Expr>),
    /// Structured table reference, e.g. `Table1[Col]`, `[@Col]`.
    Table(crate::structref::StructRef),
}

impl Expr {
    /// Visit every node depth-first.
    pub fn walk(&self, f: &mut impl FnMut(&Expr)) {
        f(self);
        match self {
            Expr::Unary(_, a) | Expr::Spill(a) | Expr::Paren(a) | Expr::Implicit(a) => a.walk(f),
            Expr::Invoke(c, args) => {
                c.walk(f);
                args.iter().for_each(|a| a.walk(f));
            }
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
            Expr::Unary(_, a) | Expr::Spill(a) | Expr::Paren(a) | Expr::Implicit(a) => {
                a.walk_mut(f)
            }
            Expr::Invoke(c, args) => {
                c.walk_mut(f);
                args.iter_mut().for_each(|a| a.walk_mut(f));
            }
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
                        | "TABLE"
                );
            }
        });
        v
    }
}

/// Functions whose result is always a single value, whatever their arguments.
const SCALAR_RESULT: &[&str] = &[
    "AND",
    "AREAS",
    "AVEDEV",
    "AVERAGE",
    "AVERAGEA",
    "AVERAGEIF",
    "AVERAGEIFS",
    "CONCAT",
    "CONCATENATE",
    "CORREL",
    "COUNT",
    "COUNTA",
    "COUNTBLANK",
    "COUNTIF",
    "COUNTIFS",
    "COVAR",
    "COVARIANCE.P",
    "COVARIANCE.S",
    "DAVERAGE",
    "DCOUNT",
    "DCOUNTA",
    "DEVSQ",
    "DGET",
    "DMAX",
    "DMIN",
    "DPRODUCT",
    "DSTDEV",
    "DSTDEVP",
    "DSUM",
    "DVAR",
    "DVARP",
    "GEOMEAN",
    "HARMEAN",
    "IRR",
    "ISREF",
    "KURT",
    "MAX",
    "MAXA",
    "MAXIFS",
    "MDETERM",
    "MEDIAN",
    "MIN",
    "MINA",
    "MINIFS",
    "MIRR",
    "NPV",
    "OR",
    "PEARSON",
    "PRODUCT",
    "ROWS",
    "COLUMNS",
    "RSQ",
    "SKEW",
    "SLOPE",
    "INTERCEPT",
    "STDEV",
    "STDEV.P",
    "STDEV.S",
    "STDEVA",
    "STDEVP",
    "STDEVPA",
    "STEYX",
    "SUBTOTAL",
    "SUM",
    "SUMIF",
    "SUMIFS",
    "SUMPRODUCT",
    "SUMSQ",
    "SUMX2MY2",
    "SUMX2PY2",
    "SUMXMY2",
    "TEXTJOIN",
    "VAR",
    "VAR.P",
    "VAR.S",
    "VARA",
    "VARP",
    "VARPA",
    "XIRR",
    "XNPV",
    "XOR",
    "ARRAYTOTEXT",
    "ISOMITTED",
    "SHEETS",
    "NOW",
    "TODAY",
    "RAND",
    "PI",
    "TRUE",
    "FALSE",
    "NA",
    "FORMULATEXT",
    "CELL",
    "SHEET",
];

impl Expr {
    /// Could this expression yield more than one value (a multi-cell reference or array)?
    /// Conservative: unknown function calls are multi-valued when any argument is.
    pub fn may_be_multi(&self) -> bool {
        match self {
            Expr::Number(_)
            | Expr::Text(_)
            | Expr::Bool(_)
            | Expr::Error(_)
            | Expr::Missing
            | Expr::Name(_)
            | Expr::Implicit(_) => false,
            Expr::Array(rows) => rows.len() > 1 || rows.iter().any(|r| r.len() > 1),
            Expr::Ref(r) => !matches!(r.kind, RefKind::Cell(_)),
            Expr::Spill(_) | Expr::Invoke(..) => true,
            Expr::Table(t) => {
                !(t.areas == [crate::structref::TableArea::ThisRow]
                    && t.col0.is_some()
                    && t.col1.is_none())
            }
            Expr::Paren(a) | Expr::Unary(_, a) => a.may_be_multi(),
            Expr::Binary(BinOp::Range | BinOp::Union | BinOp::Isect, ..) => true,
            Expr::Binary(_, a, b) => a.may_be_multi() || b.may_be_multi(),
            Expr::Call(name, args) => {
                if SCALAR_RESULT.contains(&name.as_str()) {
                    return false;
                }
                matches!(name.as_str(), "INDEX" | "OFFSET" | "INDIRECT")
                    || crate::functions::arrays::ARRAY_RESULT.contains(&name.as_str())
                    || args.iter().any(Expr::may_be_multi)
            }
        }
    }

    /// Rewrite a formula saved with pre-dynamic-array semantics so it evaluates the same way
    /// now: every operand that could be multi-valued at the top level (outside function
    /// arguments) gets the `@` implicit-intersection operator, as the target app shows it.
    pub fn into_legacy(self) -> Expr {
        match self {
            Expr::Paren(a) => Expr::Paren(Box::new(a.into_legacy())),
            Expr::Unary(op, a) => Expr::Unary(op, Box::new(a.into_legacy())),
            Expr::Binary(op, a, b) if !matches!(op, BinOp::Range | BinOp::Union | BinOp::Isect) => {
                Expr::Binary(op, Box::new(a.into_legacy()), Box::new(b.into_legacy()))
            }
            e if e.may_be_multi() => Expr::Implicit(Box::new(e)),
            e => e,
        }
    }

    /// Inverse of `into_legacy`: drop top-level `@` operators (where a legacy formula has
    /// them implicitly). `None` if the formula needs dynamic-array semantics, i.e. it has a
    /// top-level operand that could spill.
    pub fn from_legacy(self) -> Option<Expr> {
        Some(match self {
            Expr::Paren(a) => Expr::Paren(Box::new(a.from_legacy()?)),
            Expr::Unary(op, a) => Expr::Unary(op, Box::new(a.from_legacy()?)),
            Expr::Binary(op, a, b) if !matches!(op, BinOp::Range | BinOp::Union | BinOp::Isect) => {
                Expr::Binary(op, Box::new(a.from_legacy()?), Box::new(b.from_legacy()?))
            }
            Expr::Implicit(a) if a.may_be_multi() => *a,
            e if e.may_be_multi() => return None,
            e => e,
        })
    }
}
