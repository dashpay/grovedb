//! Options

#[cfg(feature = "minimal")]
use grovedb_merk::MerkOptions;

#[cfg(feature = "minimal")]
use crate::operations::{delete::DeleteOptions, insert::InsertOptions};

/// Batch apply options
#[cfg(feature = "minimal")]
#[derive(Debug, Clone)]
pub struct BatchApplyOptions {
    /// Validate insertion does not override
    pub validate_insertion_does_not_override: bool,
    /// Validate insertion does not override tree
    pub validate_insertion_does_not_override_tree: bool,
    /// Allow deleting non-empty trees
    pub allow_deleting_non_empty_trees: bool,
    /// Deleting non empty trees returns error
    pub deleting_non_empty_trees_returns_error: bool,
    /// Disable the full operation consistency check performed by
    /// [`super::QualifiedGroveDbOp::verify_consistency_of_operations`].
    ///
    /// This check detects several conflict types including:
    /// - Duplicate operations targeting the same `(path, key)` pair
    /// - Inserts below paths that are being deleted in the same batch
    /// - Append/delete conflicts on the same tree
    ///
    /// When this is `false` (the default), the batch system calls
    /// [`super::QualifiedGroveDbOp::verify_consistency_of_operations`] before
    /// applying and rejects the batch if any conflicts are found.
    ///
    /// # Warning -- silent last-op-wins behavior
    ///
    /// When set to `true`, duplicate operations on the same `(path, key)` are
    /// **not** detected. Because the internal batch structure is a `BTreeMap`
    /// keyed by `(path, key)`, inserting a second operation for an already-seen
    /// key silently overwrites the first. The **last** operation encountered in
    /// the input `Vec` wins, and the earlier operation is lost without any
    /// error or warning.
    ///
    /// This is safe **only** when the caller has already guaranteed that the
    /// operation list contains no conflicting entries, or when the caller
    /// intentionally relies on last-op-wins semantics (e.g., an idempotent
    /// replay scenario). In all other cases, leave this set to `false` to
    /// catch accidental conflicts early.
    ///
    /// # Warning -- potential storage leak on insert-under-delete
    ///
    /// When set to `true`, the consistency check that rejects inserts below
    /// deleted paths is also skipped. If a batch both inserts a subtree and
    /// deletes an ancestor of that subtree, the post-`apply_body` cleanup
    /// uses [`GroveDb::find_subtrees`](crate::GroveDb::find_subtrees) to
    /// discover nested subtrees for storage removal. However,
    /// `find_subtrees` reads from the committed transaction state (without
    /// the pending `StorageBatch`), so subtrees created by the same batch
    /// are invisible to it. This means the newly-inserted subtree's storage
    /// prefix will not be cleaned up, resulting in orphaned data in the
    /// underlying store.
    ///
    /// With the consistency check enabled (the default), such batches are
    /// rejected before `apply_body` runs, so this stale-state window is
    /// never reachable.
    pub disable_operation_consistency_check: bool,
    /// Base root storage is free
    pub base_root_storage_is_free: bool,
    /// At what height do we want to pause applying batch operations
    /// Most of the time this should be not set
    pub batch_pause_height: Option<u8>,
}

#[cfg(feature = "minimal")]
impl Default for BatchApplyOptions {
    fn default() -> Self {
        BatchApplyOptions {
            validate_insertion_does_not_override: false,
            validate_insertion_does_not_override_tree: false,
            allow_deleting_non_empty_trees: false,
            deleting_non_empty_trees_returns_error: true,
            disable_operation_consistency_check: false,
            base_root_storage_is_free: true,
            batch_pause_height: None,
        }
    }
}

#[cfg(feature = "minimal")]
impl BatchApplyOptions {
    /// As insert options
    pub(crate) fn as_insert_options(&self) -> InsertOptions {
        InsertOptions {
            validate_insertion_does_not_override: self.validate_insertion_does_not_override,
            validate_insertion_does_not_override_tree: self
                .validate_insertion_does_not_override_tree,
            base_root_storage_is_free: self.base_root_storage_is_free,
        }
    }

    /// As delete options
    pub(crate) fn as_delete_options(&self) -> DeleteOptions where {
        DeleteOptions {
            allow_deleting_non_empty_trees: self.allow_deleting_non_empty_trees,
            deleting_non_empty_trees_returns_error: self.deleting_non_empty_trees_returns_error,
            base_root_storage_is_free: self.base_root_storage_is_free,
            validate_tree_at_path_exists: false,
        }
    }

    /// As Merk options
    pub(crate) fn as_merk_options(&self) -> MerkOptions {
        MerkOptions {
            base_root_storage_is_free: self.base_root_storage_is_free,
        }
    }
}
