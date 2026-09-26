//! `delete_operation_for_delete_internal` — versioned dispatch.
//!
//! Builds the batch operation that deletes one element. When the element is
//! a tree it is deleted only if empty, and the operations already pending in
//! the batch take part in that check: the keys the batch deletes at the tree's
//! own path count as removed, and any other operation there makes the tree
//! non-empty. An empty tree is deleted with
//! [`SubelementsDeletionBehavior::DontCheckWithNoCleanup`](crate::batch::SubelementsDeletionBehavior::DontCheckWithNoCleanup),
//! so the apply path neither re-checks it nor cleans up after it.
//!
//! The versions differ only in which pending deletes count as removing a key.
//! That decides whether a tree is empty, and so which operation is built and
//! the cost of the `is_empty_tree_except` walk, so the difference is
//! **consensus-critical** and version-gated:
//!
//! * **[v0]** — legacy behaviour, frozen for `GROVE_V1`..`GROVE_V3` (live in
//!   production). Every pending delete counts, a `DeleteTree` with
//!   [`SubelementsDeletionBehavior::Skip`](crate::batch::SubelementsDeletionBehavior::Skip)
//!   included. The apply path drops a `Skip` delete whose tree is not empty,
//!   so a parent emptied only by such a delete is deleted without cleanup while
//!   the child is still in it, orphaning the child and its contents. Kept
//!   bug-for-bug for replay compatibility.
//! * **[v1]** — `GROVE_V4`+. Only the deletes
//!   [`GroveOp::always_removes_its_key`](crate::batch::GroveOp::always_removes_its_key)
//!   accepts count, the same rule the apply path uses for its own emptiness
//!   check, so a pending `Skip` delete leaves its child counted as present.
//!
//! See [v0] / [v1].
//!
//! [v0]: self::v0
//! [v1]: self::v1

mod v0;
mod v1;

use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_merk::MaybeTree;
use grovedb_path::SubtreePath;
use grovedb_version::version::GroveVersion;

use super::{DeleteOptions, PendingOperations};
use crate::{batch::QualifiedGroveDbOp, Error, GroveDb, TransactionArg};

impl GroveDb {
    /// Delete operation for delete internal.
    ///
    /// # Dangling references
    ///
    /// This builds a batch operation; it performs no reference check itself.
    /// Ordinary [`Reference`](crate::Element::Reference) elements pointing at
    /// the deleted element become dangling when the batch applies. The op
    /// carries `options.backwards_references`: [`BackwardsReferences::DontCheck`](crate::BackwardsReferences::DontCheck)
    /// builds the `DontCheckForBackwardsReferences` twin, so whether the batch maintains or refuses
    /// a backward-reference participant is decided here. See the
    /// [module-level documentation](super) for details.
    ///
    /// # Pending operations
    ///
    /// `current_batch_operations` are the operations already in the batch the
    /// delete is built into. They are read only when the deleted element is a
    /// tree, and then only those at the tree's own path (`path` followed by
    /// `key`): deletes there count as removing those children, and anything
    /// else there makes the tree non-empty. From `GROVE_V4` a `DeleteTree`
    /// with `SubelementsDeletionBehavior::Skip` does not count as removing its
    /// child, since the batch drops it when that child is not empty. See
    /// [`PendingOperations`] for what can be passed: a slice, `&Vec`, an
    /// iterator or adapter over the caller's own operations, or an index by
    /// path.
    pub fn delete_operation_for_delete_internal<'a, B: AsRef<[u8]>>(
        &self,
        path: SubtreePath<B>,
        key: &[u8],
        options: &DeleteOptions,
        is_known_to_be_subtree: Option<MaybeTree>,
        current_batch_operations: impl PendingOperations<'a>,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Option<QualifiedGroveDbOp>, Error> {
        self.delete_operation_for_delete_internal_with_deleted_child(
            path,
            key,
            options,
            is_known_to_be_subtree,
            &current_batch_operations,
            None,
            transaction,
            grove_version,
        )
    }

    /// [`delete_operation_for_delete_internal`](Self::delete_operation_for_delete_internal),
    /// also counting `deleted_child_key` as removed from the tree when the
    /// element is a Merk tree: the key of the tree's child that the up-tree
    /// chain being built deletes at the level below.
    ///
    /// Version dispatch (consensus-critical) — see the module documentation.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn delete_operation_for_delete_internal_with_deleted_child<
        'a,
        B: AsRef<[u8]>,
        P: PendingOperations<'a>,
    >(
        &self,
        path: SubtreePath<B>,
        key: &[u8],
        options: &DeleteOptions,
        is_known_to_be_subtree: Option<MaybeTree>,
        current_batch_operations: &P,
        deleted_child_key: Option<&[u8]>,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Option<QualifiedGroveDbOp>, Error> {
        match grove_version
            .grovedb_versions
            .operations
            .delete
            .delete_operation_for_delete_internal
        {
            0 => self.delete_operation_for_delete_internal_v0(
                path,
                key,
                options,
                is_known_to_be_subtree,
                current_batch_operations,
                deleted_child_key,
                transaction,
                grove_version,
            ),
            1 => self.delete_operation_for_delete_internal_v1(
                path,
                key,
                options,
                is_known_to_be_subtree,
                current_batch_operations,
                deleted_child_key,
                transaction,
                grove_version,
            ),
            version => Err(
                grovedb_version::error::GroveVersionError::UnknownVersionMismatch {
                    method: "delete_operation_for_delete_internal".to_string(),
                    known_versions: vec![0, 1],
                    received: version,
                }
                .into(),
            )
            .wrap_with_cost(OperationCost::default()),
        }
    }
}
