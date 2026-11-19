//! The write transaction.

mod arena;
mod check;
mod commits;
mod cursor;
mod error;
mod frames;
mod key;
mod pages;
mod path;
#[allow(clippy::module_inception)]
mod txn;
mod view;
mod write;
mod write_set;

#[cfg(all(test, not(loom)))]
mod tests;

pub use commits::Commits;
pub use cursor::{CommittedSide, Cursor, FlatSide};
pub use error::{BatchError, TxnError};
pub use key::{Column, KeySet, Subtree, Unit};
pub use path::{Directory, DirectoryError, FlatWriter, Keyspaces, WritePath};
pub use txn::{Scope, TxnStats, WriteTxn};
pub use view::{LinkView, NodeValue, NodeView};
pub use write::{
    Applied, Batch, Hosted, LinkPut, MerkWrite, NodePut, RecordWrite, RootPut, ValuePut,
};
pub use write_set::Mark;

/// The write half of a snapshot being read-only (`transactions.md` RD1,
/// T-RD5): the entry point that writes through a transaction answers
/// [`SnapshotError::ReadOnly`](crate::snapshot::SnapshotError::ReadOnly)
/// for a snapshot, and changes nothing.
impl crate::snapshot::Snapshot<'_> {
    /// Refused: a snapshot is read-only.
    pub fn apply(&self, batch: &Batch<'_>) -> Result<Applied, crate::snapshot::SnapshotError> {
        let _ = batch;
        Err(crate::snapshot::SnapshotError::ReadOnly)
    }
}
