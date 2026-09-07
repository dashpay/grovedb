//! `delete_internal_on_transaction` — **v0** (legacy, `GROVE_V1`..`GROVE_V3`).
//!
//! CONSENSUS-CRITICAL. When deleting a **non-empty child tree**, the parent
//! Merk is reopened via `Merk::open_layered_with_root_key` labeled with the
//! **deleted child's** tree type instead of the parent's (issue #686). The
//! label is wrong, and for the six Provable* types — whose link hash embeds
//! the aggregate via `hash_for_link` — a mismatched parent/child pairing
//! either:
//!
//! * commits a wrong link hash into the grandparent (Provable* parent,
//!   plain child; `verify_grovedb` reports the mismatch), or
//! * panics in `hash_for_link` with "feature_type is inconsistent with its
//!   tree_type" (plain parent, Provable* child, parent left non-empty).
//!
//! For non-Provable mismatches the resulting state is identical to
//! [`super::v1`] (aggregates derive from node feature types and
//! `hash_for_link` falls through to a plain node hash), but the reopen still
//! incurs its own seek/load costs, which are fee-relevant.
//!
//! `GROVE_V1`..`GROVE_V3` are live in production: any such delete that
//! already executed committed the resulting (possibly corrupt) link hash
//! into a consensus root, so replay must reproduce this path bug-for-bug —
//! including the wrong label, the extra reopen costs, and the panic.
//!
//! v0 differs from [`super::v1`] ONLY in that non-empty-child-tree branch:
//! there the already-open parent Merk (correctly labeled) is reused and the
//! reopen disappears. Everything else is identical. See the module docs in
//! [`super`][`mod@super`] for the version rationale.

use std::collections::HashMap;

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_into, cost_return_on_error_no_add,
    storage_cost::removal::StorageRemovedBytes, CostResult, CostsExt, OperationCost,
};
use grovedb_merk::{
    element::{
        costs::ElementCostExtensions, delete::ElementDeleteFromStorageExtensions,
        tree_type::ElementTreeTypeExtensions,
    },
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
    /// Legacy delete on a transaction — reopens the parent layer labeled
    /// with the deleted child's tree type when deleting a non-empty child
    /// tree. Frozen for `GROVE_V1`..`GROVE_V3`; see the module docs.
    pub(crate) fn delete_internal_on_transaction_v0<B: AsRef<[u8]>>(
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
                // Legacy behavior (frozen — see the module docs): reopen the
                // parent layer labeled with the DELETED CHILD's tree type
                // before deleting the tree element.
                let storage = self
                    .db
                    .get_transactional_storage_context(path.clone(), Some(batch), transaction)
                    .unwrap_add_cost(&mut cost);

                // The merk reopened here is the PARENT merk (at `path`), but
                // the historical code labels it with the DELETED CHILD's
                // tree type. For a PrivateDocumentStore child that label
                // would trip the merk-level "no ops on a PDS Merk"
                // chokepoint (the delete below applies to the parent), so
                // use the parent's actual tree type for PDS deletions.
                // Existing types keep the historical label byte-for-byte to
                // avoid any behavior change on released paths. (PDS is
                // v4-gated and GROVE_V4 dispatches to v1, so this carve-out
                // is unreachable here in practice — kept for exactness.)
                let reopen_tree_type =
                    if matches!(tree_type, grovedb_merk::TreeType::PrivateDocumentStore(_)) {
                        subtree_to_delete_from.tree_type
                    } else {
                        tree_type
                    };
                let mut merk_to_delete_tree_from = cost_return_on_error!(
                    &mut cost,
                    Merk::open_layered_with_root_key(
                        storage,
                        subtree_to_delete_from.root_key(),
                        reopen_tree_type,
                        Some(&Element::value_defined_cost_for_serialized_value),
                        grove_version,
                    )
                    .map_err(|e| {
                        Error::CorruptedData(format!(
                            "cannot open a subtree with given root key: {e}"
                        ))
                    })
                );
                // We are deleting a tree, a tree uses 3 bytes
                cost_return_on_error_into!(
                    &mut cost,
                    Element::delete_with_sectioned_removal_bytes(
                        &mut merk_to_delete_tree_from,
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
                merk_cache.insert(path.clone(), merk_to_delete_tree_from);
                cost_return_on_error!(
                    &mut cost,
                    self.propagate_changes_with_batch_transaction(
                        batch,
                        merk_cache,
                        &path,
                        transaction,
                        grove_version,
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
