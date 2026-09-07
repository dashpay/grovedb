//! The cross-segment consistency gate of `apply_partial_batch` (issue #842).
//!
//! A partial batch applies in two segments — the caller's initial ops, then
//! the ops its add-on closure returns — against one live Merk cache and one
//! storage batch, with a single atomic commit at the end. The per-batch
//! consistency check only sees one segment at a time, so the footprint
//! recorded here is what lets the flow refuse add-on shapes that are
//! individually consistent but incoherent across the segment boundary.

use grovedb_merk::element::tree_type::ElementTreeTypeExtensions;

use crate::{
    batch::{GroveOp, QualifiedGroveDbOp},
    Error,
};

/// What the initial segment of a partial batch wrote and deleted, for the
/// cross-segment consistency gate (issue #842).
///
/// The per-batch consistency check only sees one segment at a time, so a
/// continuation could otherwise write under a subtree the initial segment
/// deleted, or replace / delete a subtree the initial segment wrote into.
/// The continuation runs on the initial segment's live Merk cache, which
/// makes writes into the same subtrees coherent — but neither of those two
/// shapes has a coherent outcome: the first leaves rows under a deleted
/// prefix, the second orphans the initial segment's rows behind a fresh
/// root. Both are refused before the continuation applies.
pub(super) struct InitialSegmentFootprint {
    /// Qualified path (path ‖ key) of every element the segment deleted.
    deleted: Vec<Vec<Vec<u8>>>,
    /// Path of the Merk every op in the segment wrote into.
    written_paths: Vec<Vec<Vec<u8>>>,
    /// Qualified path of every non-Merk tree the segment appended into,
    /// created, or replaced. Add-on typed appends are preprocessed against
    /// COMMITTED state (see `preprocess_commitment_tree_ops` and friends),
    /// so a target whose data namespace or element only exists pending in
    /// the batch cannot be appended to coherently — and deleting or
    /// replacing such a tree would strand the segment's pending data-row
    /// writes behind a cleanup pass that cannot see them.
    non_merk_tree_targets: Vec<Vec<Vec<u8>>>,
}

impl InitialSegmentFootprint {
    pub(super) fn from_ops(ops: &[QualifiedGroveDbOp]) -> Self {
        let mut deleted = Vec::new();
        let mut written_paths = Vec::with_capacity(ops.len());
        let mut non_merk_tree_targets = Vec::new();
        for op in ops {
            let path = op.path.to_path();
            if matches!(op.op, GroveOp::Delete | GroveOp::DeleteTree(..))
                && let Some(key) = op.key.as_ref()
            {
                let mut qualified = path.clone();
                qualified.push(key.get_key_clone());
                deleted.push(qualified);
            }
            // Typed appends were rewritten into ReplaceNonMerkTreeRoot by
            // preprocessing before the footprint is taken; insert-family
            // ops can create or replace a non-Merk tree element directly.
            let non_merk_tree_target = match &op.op {
                GroveOp::ReplaceNonMerkTreeRoot { .. } => true,
                GroveOp::InsertOrReplace { element }
                | GroveOp::InsertWithKnownToNotAlreadyExist { element }
                | GroveOp::InsertIfNotExists { element, .. }
                | GroveOp::Replace { element }
                | GroveOp::Patch { element, .. } => element
                    .tree_type()
                    .is_some_and(|tree_type| tree_type.uses_non_merk_data_storage()),
                _ => false,
            };
            if non_merk_tree_target && let Some(key) = op.key.as_ref() {
                let mut qualified = path.clone();
                qualified.push(key.get_key_clone());
                non_merk_tree_targets.push(qualified);
            }
            written_paths.push(path);
        }
        Self {
            deleted,
            written_paths,
            non_merk_tree_targets,
        }
    }

    /// Refuse add-on ops that cannot be applied coherently after this
    /// segment (see the type docs).
    pub(super) fn verify_add_on_ops(&self, add_on_ops: &[QualifiedGroveDbOp]) -> Result<(), Error> {
        for op in add_on_ops {
            let path = op.path.to_path();
            if self.deleted.iter().any(|deleted| path.starts_with(deleted)) {
                return Err(Error::InvalidBatchOperation(
                    "add-on operation writes under a path the initial segment deleted",
                ));
            }
            // Add-on typed appends (keyless; the tree key is the path's
            // last segment) are preprocessed against committed state, so a
            // target this segment appended into, created, or replaced has
            // no coherent committed root to append onto.
            if matches!(
                op.op,
                GroveOp::CommitmentTreeInsert { .. }
                    | GroveOp::MmrTreeAppend { .. }
                    | GroveOp::BulkAppend { .. }
                    | GroveOp::DenseTreeInsert { .. }
                    | GroveOp::PrivateDocumentStoreInsert { .. }
            ) && self.non_merk_tree_targets.contains(&path)
            {
                return Err(Error::InvalidBatchOperation(
                    "add-on append targets a non-Merk tree the initial segment wrote",
                ));
            }
            // Whether an op orphans what the initial segment wrote depends
            // only on what was AT the target, never on what the new element
            // is: overwriting a written-into subtree with a plain item
            // strands the segment's pending child writes just as surely as
            // overwriting it with a fresh tree.
            let replaces_or_deletes_subtree = matches!(
                op.op,
                GroveOp::Delete
                    | GroveOp::DeleteTree(..)
                    | GroveOp::InsertOrReplace { .. }
                    | GroveOp::InsertWithKnownToNotAlreadyExist { .. }
                    | GroveOp::InsertIfNotExists { .. }
                    | GroveOp::Replace { .. }
                    | GroveOp::Patch { .. }
            );
            if replaces_or_deletes_subtree && let Some(key) = op.key.as_ref() {
                let mut qualified = path;
                qualified.push(key.get_key_clone());
                if self
                    .written_paths
                    .iter()
                    .any(|written| written.starts_with(&qualified))
                {
                    return Err(Error::InvalidBatchOperation(
                        "add-on op replaces or deletes a subtree the initial segment wrote into",
                    ));
                }
                if self.non_merk_tree_targets.contains(&qualified) {
                    return Err(Error::InvalidBatchOperation(
                        "add-on op replaces or deletes a non-Merk tree the initial segment wrote",
                    ));
                }
            }
        }
        Ok(())
    }
}
