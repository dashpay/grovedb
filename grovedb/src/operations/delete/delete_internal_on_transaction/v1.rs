//! `delete_internal_on_transaction` — **v1** (`GROVE_V4`+).
//!
//! Fixes issue #686: when deleting a **non-empty child tree**, the
//! already-open parent Merk (`subtree_to_delete_from`) is reused for the
//! tree-element delete and the upward propagation. It carries the parent's
//! true tree type, so `hash_for_link` and aggregate propagation run with the
//! parent type — where [`super::v0`] reopened the parent labeled with the
//! deleted **child's** tree type, corrupting the grandparent link hash for
//! Provable* parents and panicking in `hash_for_link` for Provable* children
//! under plain parents. The redundant reopen (an extra storage-context open
//! plus `Merk::open_layered_with_root_key`) disappears with it, which also
//! changes the operation's cost profile — hence the version gate.
//!
//! The same branch also propagates through
//! `propagate_changes_with_transaction` (the full indexed-aware walk used
//! by the other two branches) instead of the legacy
//! `propagate_changes_with_batch_transaction`, whose basic parent update
//! cannot climb through an indexed-tree primary: deleting a non-empty tree
//! inside one of a primary's child subtrees erred with "can only propagate
//! on tree items" for PSIT / PCPSIT and silently desynced the count index
//! for PCIT.
//!
//! v1 differs from [`super::v0`] ONLY in that non-empty-child-tree branch.
//! Everything else is identical. See the module docs in
//! [`super`][`mod@super`] for the version rationale.

use std::collections::HashMap;

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_into, cost_return_on_error_no_add,
    storage_cost::removal::StorageRemovedBytes, CostResult, CostsExt, OperationCost,
};
use grovedb_merk::{
    element::{delete::ElementDeleteFromStorageExtensions, tree_type::ElementTreeTypeExtensions},
    Error as MerkError, Merk,
};
use grovedb_path::SubtreePath;
use grovedb_storage::{
    rocksdb_storage::{PrefixedRocksDbTransactionContext, RocksDbStorage},
    Storage, StorageBatch,
};
use grovedb_version::version::GroveVersion;

use super::super::DeleteOptions;
use crate::{Element, Error, GroveDb, Transaction};

impl GroveDb {
    /// Delete on a transaction, reusing the already-open (correctly labeled)
    /// parent Merk when deleting a non-empty child tree. `GROVE_V4`+; see
    /// the module docs.
    pub(crate) fn delete_internal_on_transaction_v1<B: AsRef<[u8]>>(
        &self,
        path: SubtreePath<B>,
        key: &[u8],
        options: &DeleteOptions,
        transaction: &Transaction,
        sectioned_removal: &mut impl FnMut(
            &Vec<u8>,
            u32,
            u32,
        ) -> Result<
            (StorageRemovedBytes, StorageRemovedBytes),
            MerkError,
        >,
        batch: &StorageBatch,
        grove_version: &GroveVersion,
    ) -> CostResult<bool, Error> {
        let mut cost = OperationCost::default();

        let element = cost_return_on_error!(
            &mut cost,
            self.get_raw(path.clone(), key.as_ref(), Some(transaction), grove_version)
        );
        let mut subtree_to_delete_from = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(
                path.clone(),
                transaction,
                Some(batch),
                grove_version
            )
        );
        // A generic delete cannot mirror the removed child's ordering value
        // out of an indexed primary's secondary index. Reject before any
        // mutation.
        cost_return_on_error_no_add!(
            cost,
            crate::operations::indexed_tree::reject_generic_write_into_indexed_primary(
                subtree_to_delete_from.tree_type,
                "delete",
            )
        );

        let parent_tree_type = subtree_to_delete_from.tree_type;
        if let Some(tree_type) = element.tree_type() {
            let subtree_merk_path = path.derive_owned_with_child(key);
            let subtree_merk_path_ref = SubtreePath::from(&subtree_merk_path);

            // Tree types that store data in the data namespace as non-Merk
            // entries (CommitmentTree, MmrTree, BulkAppendTree, DenseTree)
            // have an always-empty Merk but may have data.  We cannot iterate
            // their storage with find_subtrees because the entries are not
            // valid Element serializations.
            let non_merk_data = element.uses_non_merk_data_storage();

            let subtree_of_tree_we_are_deleting = cost_return_on_error!(
                &mut cost,
                self.open_transactional_merk_at_path(
                    subtree_merk_path_ref.clone(),
                    transaction,
                    Some(batch),
                    grove_version,
                )
            );

            // For non-Merk data trees the raw_iter check would see non-Merk
            // keys and wrongly report the tree as non-empty.  Use the
            // element's own count instead.
            let is_empty = if non_merk_data {
                element.non_merk_entry_count().unwrap_or(0) == 0
            } else {
                subtree_of_tree_we_are_deleting
                    .is_empty_tree()
                    .unwrap_add_cost(&mut cost)
            };

            if !options.allow_deleting_non_empty_trees && !is_empty {
                return if options.deleting_non_empty_trees_returns_error {
                    Err(Error::DeletingNonEmptyTree(
                        "trying to do a delete operation for a non empty tree, but options not \
                         allowing this",
                    ))
                    .wrap_with_cost(cost)
                } else {
                    Ok(false).wrap_with_cost(cost)
                };
            }

            // Indexed-tree primaries own one or more secondary storage
            // namespaces (the axis-ordered secondary indexes) at prefixes
            // derived from the primary's prefix via S2-B
            // (`Blake3(primary_prefix ‖ axis_tag)`) — one per axis: PCIT
            // has Count, PSIT has Sum, and PCPSIT has up to all three
            // (Count/Sum/Avg). `find_subtrees` only walks the primary's
            // namespace, so without this explicit clear the secondary's
            // storage would be orphaned: future inserts under the same
            // path could collide with stale entries (the derived prefix
            // is identical for a recreated tree at the same path), and
            // the secondary index would be unreachable but would still
            // consume disk. Run unconditionally on every indexed-tree
            // primary delete — including the `is_empty` branch below,
            // since a stale (drifted) secondary can co-exist with an
            // empty primary (e.g. a bug that mirrors deletions into the
            // primary but fails to mirror into the secondary would leave
            // orphans here; defend against it by clearing the namespace
            // at delete time). We sweep all three axis tags
            // unconditionally rather than decoding the axes TLV, matching
            // the nested-subtree sweep inside the `find_subtrees` loop
            // below: clearing an unused axis prefix is idempotent on an
            // empty namespace, so the redundancy is intentional
            // defense-in-depth. The per-prefix cleanup inside that loop
            // also clears these same namespaces for the target prefix,
            // but both clears are idempotent so the redundancy is fine.
            if tree_type.is_indexed_primary() {
                let primary_prefix = RocksDbStorage::build_prefix(subtree_merk_path_ref.clone())
                    .unwrap_add_cost(&mut cost);
                for axis in [
                    grovedb_element::indexed::IndexAxis::Count,
                    grovedb_element::indexed::IndexAxis::Sum,
                    grovedb_element::indexed::IndexAxis::Avg,
                ] {
                    let secondary_prefix =
                        RocksDbStorage::secondary_prefix_for(&primary_prefix, axis.tag())
                            .unwrap_add_cost(&mut cost);
                    let mut secondary_storage = self
                        .db
                        .get_transactional_storage_context_by_subtree_prefix(
                            secondary_prefix,
                            Some(batch),
                            transaction,
                        )
                        .unwrap_add_cost(&mut cost);
                    cost_return_on_error!(
                        &mut cost,
                        secondary_storage.clear().map_err(|e| {
                            Error::CorruptedData(format!(
                                "unable to cleanup indexed-tree secondary (axis {:?}) from \
                                 storage: {e}",
                                axis
                            ))
                        })
                    );
                }
            }

            if !is_empty {
                if non_merk_data {
                    // Non-Merk data trees: clear the subtree storage directly.
                    // These trees never contain child subtrees so we only need
                    // to clear the one storage context.
                    let mut storage = self
                        .db
                        .get_transactional_storage_context(
                            subtree_merk_path_ref.clone(),
                            Some(batch),
                            transaction,
                        )
                        .unwrap_add_cost(&mut cost);
                    cost_return_on_error!(
                        &mut cost,
                        storage.clear().map_err(|e| {
                            Error::CorruptedData(format!(
                                "unable to cleanup non-merk tree data from storage: {e}",
                            ))
                        })
                    );
                } else {
                    let subtrees_paths = cost_return_on_error!(
                        &mut cost,
                        self.find_subtrees(
                            &subtree_merk_path_ref,
                            Some(transaction),
                            grove_version
                        )
                    );
                    for subtree_path in subtrees_paths {
                        let p: SubtreePath<_> = subtree_path.as_slice().into();
                        let mut storage = self
                            .db
                            .get_transactional_storage_context(p.clone(), Some(batch), transaction)
                            .unwrap_add_cost(&mut cost);

                        cost_return_on_error!(
                            &mut cost,
                            storage.clear().map_err(|e| {
                                Error::CorruptedData(format!(
                                    "unable to cleanup tree from storage: {e}",
                                ))
                            })
                        );

                        // NESTED INDEXED-TREE SECONDARY CLEANUP.
                        // find_subtrees enumerates every nested subtree
                        // under the deletion target, but only the
                        // primary's storage namespace is reachable via
                        // the path-prefix walk. Any nested indexed-tree
                        // primary inside the deleted subtree has its own
                        // per-axis secondary namespaces at
                        // Blake3(its_prefix ‖ axis_tag) — one for PCIT
                        // (count), one for PSIT (sum), up to three for
                        // PCPSIT — that find_subtrees cannot see; clear
                        // them all too.
                        //
                        // Clearing the secondary prefix is idempotent on
                        // non-indexed subtrees (their secondary namespace
                        // is empty), so we sweep all three axis tags
                        // unconditionally rather than decoding each
                        // subtree's root element to check the tree type —
                        // cheaper and removes a class of missed-decoding
                        // bugs.
                        let primary_prefix =
                            RocksDbStorage::build_prefix(p).unwrap_add_cost(&mut cost);
                        for axis in [
                            grovedb_element::indexed::IndexAxis::Count,
                            grovedb_element::indexed::IndexAxis::Sum,
                            grovedb_element::indexed::IndexAxis::Avg,
                        ] {
                            let secondary_prefix =
                                RocksDbStorage::secondary_prefix_for(&primary_prefix, axis.tag())
                                    .unwrap_add_cost(&mut cost);
                            let mut secondary_storage = self
                                .db
                                .get_transactional_storage_context_by_subtree_prefix(
                                    secondary_prefix,
                                    Some(batch),
                                    transaction,
                                )
                                .unwrap_add_cost(&mut cost);
                            cost_return_on_error!(
                                &mut cost,
                                secondary_storage.clear().map_err(|e| {
                                    Error::CorruptedData(format!(
                                        "unable to cleanup nested indexed-tree secondary \
                                         (axis {:?}) in delete: {e}",
                                        axis
                                    ))
                                })
                            );
                        }
                    }
                }
                // Fixed behavior (issue #686): reuse the already-open parent
                // Merk, which carries the parent's true tree type, so delete
                // propagation hashes and aggregates with the parent type
                // instead of the deleted child's.
                // We are deleting a tree, a tree uses 3 bytes
                cost_return_on_error_into!(
                    &mut cost,
                    Element::delete_with_sectioned_removal_bytes(
                        &mut subtree_to_delete_from,
                        key,
                        Some(options.as_merk_options()),
                        true,
                        parent_tree_type,
                        sectioned_removal,
                        grove_version,
                    )
                );
                let mut merk_cache: HashMap<
                    SubtreePath<B>,
                    Merk<PrefixedRocksDbTransactionContext>,
                > = HashMap::default();
                merk_cache.insert(path.clone(), subtree_to_delete_from);
                // Propagate through the full indexed-aware walk, matching
                // the empty-tree and plain-element branches below. The
                // legacy batch propagation performed only the basic parent
                // update, so a walk climbing through an indexed-tree
                // primary (a non-empty tree deleted INSIDE one of the
                // primary's child subtrees) left the primary's canonical
                // secondary row stale — erroring with "can only propagate
                // on tree items" for PSIT / PCPSIT elements and silently
                // desyncing the count index for PCIT.
                cost_return_on_error!(
                    &mut cost,
                    self.propagate_changes_with_transaction(
                        merk_cache,
                        path,
                        transaction,
                        batch,
                        grove_version
                    )
                );
            } else {
                // We are deleting a tree, a tree uses 3 bytes
                cost_return_on_error_into!(
                    &mut cost,
                    Element::delete_with_sectioned_removal_bytes(
                        &mut subtree_to_delete_from,
                        key,
                        Some(options.as_merk_options()),
                        true,
                        parent_tree_type,
                        sectioned_removal,
                        grove_version,
                    )
                );
                let mut merk_cache: HashMap<
                    SubtreePath<B>,
                    Merk<PrefixedRocksDbTransactionContext>,
                > = HashMap::default();
                merk_cache.insert(path.clone(), subtree_to_delete_from);
                cost_return_on_error!(
                    &mut cost,
                    self.propagate_changes_with_transaction(
                        merk_cache,
                        path,
                        transaction,
                        batch,
                        grove_version
                    )
                );
            }
        } else {
            cost_return_on_error_into!(
                &mut cost,
                Element::delete_with_sectioned_removal_bytes(
                    &mut subtree_to_delete_from,
                    key,
                    Some(options.as_merk_options()),
                    false,
                    parent_tree_type,
                    sectioned_removal,
                    grove_version,
                )
            );
            let mut merk_cache: HashMap<SubtreePath<B>, Merk<PrefixedRocksDbTransactionContext>> =
                HashMap::default();
            merk_cache.insert(path.clone(), subtree_to_delete_from);
            cost_return_on_error!(
                &mut cost,
                self.propagate_changes_with_transaction(
                    merk_cache,
                    path,
                    transaction,
                    batch,
                    grove_version
                )
            );
        }

        Ok(true).wrap_with_cost(cost)
    }
}
