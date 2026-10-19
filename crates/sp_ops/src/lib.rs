//! L2 operations: transactional history plus editing algorithms.

pub mod autofill;
pub mod chart_shift;
pub mod clipboard;
pub mod edit;
pub mod find;
pub mod flash_fill;
pub mod history;
pub mod object_shift;
pub mod pivot_shift;
pub mod table_shift;

pub use clipboard::{ClipData, PasteOp, PasteOpts, PasteWhat};
pub use edit::{ClearWhat, FillDir, OpError, SortKey};
pub use history::{History, Patch, Tx, Txn};
