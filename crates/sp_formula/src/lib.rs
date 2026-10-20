//! L2 formula engine: parser, printer, reference shifting, evaluator, functions, recalc.

pub mod ast;
pub mod deps;
pub mod eval;
pub mod functions;
pub mod parser;
pub mod pivot;
pub mod print;
pub mod shared;
pub mod recalc;
pub mod shift;
pub mod structref;
pub mod whatif;

pub use ast::Expr;
pub use parser::{ParseError, parse};
pub use print::to_formula;
pub use recalc::{AstCache, RecalcOptions, RecalcStats, eval_formula, recalc};

/// Names of every implemented worksheet function (sorted).
pub fn function_names() -> Vec<&'static str> {
    functions::registry().names()
}
