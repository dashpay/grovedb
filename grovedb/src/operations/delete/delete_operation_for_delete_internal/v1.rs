//! `delete_operation_for_delete_internal` version 1.
//!
//! Differs from [v0](super::v0) ONLY in which pending deletes at the tree's
//! own path count as removing a key: only those
//! [`GroveOp::always_removes_its_key`] accepts, the rule the apply path uses
//! for its own emptiness check. A pending `DeleteTree` with
//! `SubelementsDeletionBehavior::Skip` leaves its child counted as present,
//! so its parent is never deleted without cleanup on the strength of a delete
//! the batch may drop.
//!
//! Selected by `GROVE_V4`+.

use std::collections::BTreeSet;

use grovedb_costs::{cost_return_on_error, CostResult, CostsExt, OperationCost};
use grovedb_merk::{element::tree_type::ElementTreeTypeExtensions, MaybeTree};
use grovedb_path::SubtreePath;
use grovedb_version::version::GroveVersion;

use super::super::{DeleteOptions, PendingOperations};
use crate::{
    batch::{GroveOp, QualifiedGroveDbOp, SubelementsDeletionBehavior},
    util::{compat, TxRef},
    Error, GroveDb, TransactionArg,
};

impl GroveDb {
    /// Build the delete operation for one element — version 1 (see the
    /// module documentation).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn delete_operation_for_delete_internal_v1<
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
        let tx = TxRef::new(&self.db, transaction);

        let mut cost = OperationCost::default();

        if path.is_root() {
            // Attempt to delete a root tree leaf
            Err(Error::InvalidPath(
                "root tree leaves currently cannot be deleted".to_owned(),
            ))
            .wrap_with_cost(cost)
        } else {
            if options.validate_tree_at_path_exists {
                cost_return_on_error!(
                    &mut cost,
                    self.check_subtree_exists_path_not_found(
                        path.clone(),
                        tx.as_ref(),
                        grove_version
                    )
                );
            }
            // Fetch the element if not already known, so we can determine
            // tree type and (for non-Merk trees) entry count.
            let element = match is_known_to_be_subtree {
                None => Some(cost_return_on_error!(
                    &mut cost,
                    self.get_raw(path.clone(), key.as_ref(), Some(tx.as_ref()), grove_version)
                )),
                Some(_) => None,
            };
            let tree_type = match (&element, is_known_to_be_subtree) {
                (Some(el), _) => el.maybe_tree_type(),
                (None, Some(x)) => x,
                _ => unreachable!(),
            };

            if let MaybeTree::Tree(tree_type) = tree_type {
                let subtree_merk_path = path.derive_owned_with_child(key);
                let subtree_merk_path_vec = subtree_merk_path.to_vec();

                // The operations pending at this tree's own path: whether the
                // batch writes anything into it and, for a Merk tree, the keys
                // it deletes there. The path is re-checked, so an index that
                // answers with more than was asked cannot change the result.
                // Only the deletes `GroveOp::always_removes_its_key` accepts
                // count, as on the apply path: a `DeleteTree` with
                // `SubelementsDeletionBehavior::Skip` is dropped at apply when
                // its tree is not empty, so its child still counts as present.
                let merk_backed = !tree_type.uses_non_merk_data_storage();
                let mut batch_deleted_keys = BTreeSet::<&[u8]>::new();
                let mut batch_writes_into_tree = false;
                for op in current_batch_operations
                    .at_path(&subtree_merk_path_vec)
                    .filter(|op| op.path.eq_path_vec(&subtree_merk_path_vec))
                {
                    match op.op {
                        GroveOp::Delete
                        | GroveOp::DeleteDontCheckForBackwardsReferences
                        | GroveOp::DeleteTree(..)
                        | GroveOp::DeleteTreeDontCheckForBackwardsReferences(..) => {
                            if merk_backed
                                && let Some(deleted_key) =
                                    op.key_always_removed_at(&subtree_merk_path_vec)
                            {
                                batch_deleted_keys.insert(deleted_key);
                            }
                        }
                        _ => batch_writes_into_tree = true,
                    }
                }
                // The child the up-tree chain deletes at the level below: the
                // chain builds only deletes that always remove their key.
                if merk_backed && let Some(deleted_child_key) = deleted_child_key {
                    batch_deleted_keys.insert(deleted_child_key);
                }

                // Non-Merk data trees (CommitmentTree, MmrTree,
                // BulkAppendTree, DenseTree) never contain child subtrees
                // in the Merk sense, so is_empty_tree_except would
                // incorrectly see non-Merk keys.  We check their
                // element-level entry count instead.
                let mut is_empty = if tree_type.uses_non_merk_data_storage() {
                    // If we already fetched the element, use it; otherwise
                    // fetch it now to check the entry count.
                    let count = if let Some(ref el) = element {
                        el.non_merk_entry_count().unwrap_or(0)
                    } else {
                        let el = cost_return_on_error!(
                            &mut cost,
                            self.get_raw(
                                path.clone(),
                                key.as_ref(),
                                Some(tx.as_ref()),
                                grove_version,
                            )
                        );
                        el.non_merk_entry_count().unwrap_or(0)
                    };
                    count == 0
                } else {
                    let subtree = cost_return_on_error!(
                        &mut cost,
                        compat::merk_optional_tx_path_not_empty(
                            &self.db,
                            SubtreePath::from(&subtree_merk_path),
                            tx.as_ref(),
                            None,
                            grove_version,
                        )
                    );

                    subtree
                        .is_empty_tree_except(batch_deleted_keys)
                        .unwrap_add_cost(&mut cost)
                };

                // If there is any current batch operation that is inserting something in this
                // tree then it is not empty either
                is_empty &= !batch_writes_into_tree;

                let result = if !options.allow_deleting_non_empty_trees && !is_empty {
                    if options.deleting_non_empty_trees_returns_error {
                        Err(Error::DeletingNonEmptyTree(
                            "trying to do a delete operation for a non empty tree, but options \
                             not allowing this",
                        ))
                    } else {
                        Ok(None)
                    }
                } else if is_empty {
                    // Emptiness was already verified above — use
                    // DontCheckWithNoCleanup to avoid a redundant re-check
                    // and skip cleanup (the tree is empty, nothing to clean).
                    Ok(Some(
                        QualifiedGroveDbOp::delete_tree_op(
                            path.to_vec(),
                            key.to_vec(),
                            tree_type,
                            SubelementsDeletionBehavior::DontCheckWithNoCleanup,
                        )
                        .with_backwards_references(options.backwards_references),
                    ))
                } else {
                    Err(Error::NotSupported(
                        "deletion operation for non empty tree not currently supported".to_string(),
                    ))
                };
                result.wrap_with_cost(cost)
            } else {
                Ok(Some(
                    QualifiedGroveDbOp::delete_op(path.to_vec(), key.to_vec())
                        .with_backwards_references(options.backwards_references),
                ))
                .wrap_with_cost(cost)
            }
        }
    }
}
