//! L0 foundation: cell addresses, A1 parsing, values, errors and date serials.
//!
//! Everything here is pure and dependency-light. Addresses are 0-based internally.

pub mod addr;
pub mod date;
pub mod numfmt;
pub mod parse;
pub mod value;

pub use addr::{CellAddr, CellRange, col_to_letters, letters_to_col};
pub use value::{ErrorKind, Value};

/// Number of rows in a sheet (target app limit).
pub const MAX_ROWS: u32 = 1_048_576;
/// Number of columns in a sheet (target app limit, `XFD`).
pub const MAX_COLS: u32 = 16_384;
/// Upper bound on cells any single operation may materialise (paste, fill, array results).
pub const MAX_CELLS_PER_OP: u64 = 20_000_000;
/// Maximum length of a text value, in characters.
pub const MAX_TEXT_LEN: usize = 32_767;

/// Errors raised by foundation helpers.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CoreError {
    #[error("invalid cell reference: {0}")]
    BadRef(String),
    #[error("reference out of bounds: {0}")]
    OutOfBounds(String),
}
