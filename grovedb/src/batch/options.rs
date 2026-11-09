//! Options

#[cfg(feature = "minimal")]
use crate::BackwardsReferences;
use grovedb_merk::MerkOptions;

#[cfg(feature = "minimal")]
use crate::operations::{delete::DeleteOptions, insert::InsertOptions};
#[cfg(feature = "minimal")]
use crate::Error;
#[cfg(feature = "minimal")]
use grovedb_version::version::GroveVersion;

/// Batch apply options
#[cfg(feature = "minimal")]
#[derive(Debug, Clone)]
pub struct BatchApplyOptions {
    /// Validate insertion does not override
    pub validate_insertion_does_not_override: bool,
    /// Validate insertion does not override tree
    pub validate_insertion_does_not_override_tree: bool,
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
    ///
    /// # Emptiness check limitation
    ///
    /// The pre-deletion emptiness check for `DeleteTree` ops uses
    /// `is_empty_tree_except` which only accounts for Delete/DeleteTree ops
    /// in the same batch -- it does not consider Insert ops that would add
    /// new keys to the subtree. With the consistency check enabled (default),
    /// such conflicting batches are rejected before the emptiness check runs.
    /// When this flag is `true`, the caller must ensure no batch contains
    /// both inserts into a subtree and a `DeleteTree` of that subtree.
    ///
    /// Partial batches always validate conflicts between the initial and
    /// continuation segments. This flag disables only per-segment checks;
    /// it cannot make pending state safe for committed-state cleanup.
    pub disable_operation_consistency_check: bool,
    /// Base root storage is free
    pub base_root_storage_is_free: bool,
    /// At what height do we want to pause applying batch operations
    /// Most of the time this should be not set
    pub batch_pause_height: Option<u8>,
    /// Account an update in place whose flags update answers
    /// [`ElementFlagsUpdate::AccountAsReplacement`](grovedb_costs::storage_cost::transition::ElementFlagsUpdate::AccountAsReplacement)
    /// as the removal of the old element plus the insertion of the new one.
    ///
    /// The old element's bytes, key included, count as removed and are
    /// sectioned by the removal-bytes callback as a deletion sections them;
    /// the new element's bytes, key included, count as added, and it keeps
    /// the flags the callback leaves it with. A batch that does not set this
    /// refuses a flags update that answers `AccountAsReplacement` with
    /// [`Error::InvalidBatchOperation`](crate::Error::InvalidBatchOperation).
    ///
    /// The estimators read it too: an estimate with it set charges every
    /// write that may be accounted so (`InsertOrReplace`, `Replace`,
    /// `Patch` and a trusted `RefreshReference` of an element with flags, and
    /// the write of a flagged tree the batch also writes under) at least the
    /// bytes its element adds when inserted. The referrer entries a
    /// backward-references element carries over are counted, in the worst
    /// case, at its declared capacity, each at the largest entry
    /// registration admits, so a worst-case estimate is never below what the
    /// apply records; the average case counts the typical shape (the
    /// average item fan-out, each entry the size of one from the write's own
    /// position).
    ///
    /// Off by default, and every cost, estimate, stored element and hash is
    /// then what it was before the option existed. Setting it needs
    /// `apply_batch.replacement_accounting` (GROVE_V5+); on an earlier grove
    /// version the batch and its estimate fail with
    /// [`Error::InvalidBatchOperation`](crate::Error::InvalidBatchOperation).
    pub allow_replacement_accounting: bool,
}

#[cfg(feature = "minimal")]
impl Default for BatchApplyOptions {
    fn default() -> Self {
        BatchApplyOptions {
            validate_insertion_does_not_override: false,
            validate_insertion_does_not_override_tree: false,
            disable_operation_consistency_check: false,
            base_root_storage_is_free: true,
            batch_pause_height: None,
            allow_replacement_accounting: false,
        }
    }
}

#[cfg(feature = "minimal")]
impl BatchApplyOptions {
    /// Whether the batch options `options`, when given, set
    /// [`Self::allow_replacement_accounting`].
    pub(crate) fn allow_replacement_accounting_in(options: &Option<BatchApplyOptions>) -> bool {
        options
            .as_ref()
            .is_some_and(|options| options.allow_replacement_accounting)
    }

    /// Refuses options that set [`Self::allow_replacement_accounting`] on a
    /// grove version that does not have it.
    pub(crate) fn check_replacement_accounting_version(
        options: &Option<BatchApplyOptions>,
        grove_version: &GroveVersion,
    ) -> Result<(), Error> {
        if Self::allow_replacement_accounting_in(options)
            && grove_version
                .grovedb_versions
                .apply_batch
                .replacement_accounting
                == 0
        {
            return Err(Error::InvalidBatchOperation(
                "replacement accounting needs grove version 5 or later",
            ));
        }
        Ok(())
    }

    /// As insert options
    pub(crate) fn as_insert_options(&self) -> InsertOptions {
        InsertOptions {
            validate_insertion_does_not_override: self.validate_insertion_does_not_override,
            validate_insertion_does_not_override_tree: self
                .validate_insertion_does_not_override_tree,
            base_root_storage_is_free: self.base_root_storage_is_free,
            // Per operation: the caller overrides this with the op's own
            // declaration.
            backwards_references: BackwardsReferences::Check,
        }
    }

    /// As delete options
    pub(crate) fn as_delete_options(&self) -> DeleteOptions {
        DeleteOptions {
            allow_deleting_non_empty_trees: false,
            deleting_non_empty_trees_returns_error: true,
            base_root_storage_is_free: self.base_root_storage_is_free,
            validate_tree_at_path_exists: false,
            // Per operation: the caller overrides this with the op's own
            // declaration.
            backwards_references: BackwardsReferences::Check,
        }
    }

    /// As Merk options
    pub(crate) fn as_merk_options(&self) -> MerkOptions {
        MerkOptions {
            base_root_storage_is_free: self.base_root_storage_is_free,
        }
    }
}
