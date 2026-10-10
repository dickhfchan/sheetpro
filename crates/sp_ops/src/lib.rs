//! L2 operations: transactional history plus editing algorithms.

pub mod edit;
pub mod history;

pub use edit::{ClearWhat, ClipData, FillDir, OpError, PasteMode, SortKey};
pub use history::{History, Patch, Tx, Txn};
