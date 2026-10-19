//! L2 operations: transactional history plus editing algorithms.

pub mod chart_shift;
pub mod edit;
pub mod flash_fill;
pub mod history;
pub mod object_shift;
pub mod pivot_shift;
pub mod table_shift;

pub use edit::{ClearWhat, ClipData, FillDir, OpError, PasteMode, SortKey};
pub use history::{History, Patch, Tx, Txn};
