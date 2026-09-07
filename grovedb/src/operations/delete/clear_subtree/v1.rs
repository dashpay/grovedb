//! `clear_subtree_with_costs` version 1.
//!
//! Differs from [v0](super::v0) ONLY in how a non-Merk data tree (`MmrTree`,
//! `BulkAppendTree`, `DenseAppendOnlyFixedSizeTree`, `CommitmentTree`,
//! `PrivateDocumentStore`) is cleared: in addition to deleting the payload
//! from the data namespace, the parent element is rewritten to its canonical
//! empty state — count / mmr size zeroed, configuration, flags and any
//! `NonCounted`-style wrapper retained — with the child hash reset to the
//! same convention the insert path binds for an empty tree, and the rewrite
//! is propagated through ancestors (refreshing the entry's canonical
//! secondary row when the parent is an indexed primary). Payload clear,
//! element reset and propagation are staged into one storage batch, so they
//! commit atomically (issue #893).
//!
//! Selected by `GROVE_V4`+.

use std::collections::HashMap;

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use grovedb_merk::{
    element::{decode::ElementDecodeExtensions, insert::ElementInsertToStorageExtensions},
    proofs::Query,
    tree::hash::NULL_HASH,
    KVIterator, Merk,
};
use grovedb_path::SubtreePath;
use grovedb_storage::{
    rocksdb_storage::PrefixedRocksDbTransactionContext, Storage, StorageBatch, StorageContext,
};
use grovedb_version::version::GroveVersion;

use super::super::{ClearOptions, DeleteOptions};
use crate::{util::TxRef, Element, Error, GroveDb, TransactionArg};

impl GroveDb {
    /// Clear a subtree — version 1 (see the module documentation).
    pub(crate) fn clear_subtree_with_costs_v1<'b, B: AsRef<[u8]>>(
        &self,
        subtree_path: SubtreePath<'b, B>,
        options: Option<ClearOptions>,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<bool, Error> {
        let tx = TxRef::new(&self.db, transaction);

        let mut cost = OperationCost::default();
        let batch = StorageBatch::new();

        let options = options.unwrap_or_default();

        let mut merk_to_clear = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(
                subtree_path.clone(),
                tx.as_ref(),
                Some(&batch),
                grove_version,
            )
        );

        // Clearing an indexed primary would empty the primary Merk while
        // leaving every per-axis secondary Merk fully populated, so the
        // element's secondary root key / axes digest would still commit to
        // rows that no longer exist. Reject rather than corrupt; callers
        // should delete the indexed tree itself (which sweeps all axes) or
        // remove entries through the dedicated `delete_from_*` APIs.
        cost_return_on_error_no_add!(
            cost,
            crate::operations::indexed_tree::reject_generic_write_into_indexed_primary(
                merk_to_clear.tree_type,
                "clear_subtree",
            )
        );

        // Non-Merk data trees store data in the data namespace as non-Element
        // entries.  We cannot iterate them with Element::iterator, so clear
        // the storage directly — and, because their count and state
        // commitment live in the parent element rather than in the (always
        // empty) Merk, reset that element to its canonical empty state and
        // propagate, all staged into the same storage batch so payload
        // clear, element reset and propagation commit atomically.
        if merk_to_clear.tree_type.uses_non_merk_data_storage() {
            let (parent_path, parent_key) = cost_return_on_error_no_add!(
                cost,
                subtree_path.derive_parent().ok_or(Error::CorruptedPath(
                    "a non-Merk data tree cannot be the root tree".to_string()
                ))
            );

            // The stored element, wrapper and flags included.
            let stored_element = cost_return_on_error!(
                &mut cost,
                self.get_raw_caching_optional(
                    parent_path.clone(),
                    parent_key,
                    true,
                    Some(tx.as_ref()),
                    grove_version,
                )
            );

            // Zero the count / mmr size, keeping configuration, flags and
            // any wrapper. The child hash must match what the insert path
            // binds for a freshly inserted empty tree of the same type —
            // see `add_element_on_transaction`.
            let mut emptied_element = stored_element.clone();
            let empty_child_hash = match emptied_element.underlying_mut() {
                Element::MmrTree(mmr_size, ..) => {
                    *mmr_size = 0;
                    NULL_HASH
                }
                Element::BulkAppendTree(total_count, ..) => {
                    *total_count = 0;
                    NULL_HASH
                }
                Element::DenseAppendOnlyFixedSizeTree(count, ..) => {
                    *count = 0;
                    NULL_HASH
                }
                Element::CommitmentTree(total_count, ..) => {
                    *total_count = 0;
                    grovedb_commitment_tree::EMPTY_COMMITMENT_TREE_STATE_ROOT
                }
                Element::PrivateDocumentStore(total_count, entry_size, chunk_power, _) => {
                    let empty_root =
                        grovedb_private_document_store::empty_private_document_store_state_root(
                            *entry_size,
                            *chunk_power,
                        );
                    *total_count = 0;
                    empty_root
                }
                _ => {
                    return Err(Error::CorruptedData(
                        "tree type uses non-merk data storage but the stored element is not a \
                         non-Merk data tree"
                            .to_string(),
                    ))
                    .wrap_with_cost(cost);
                }
            };

            // Clear the payload from the data namespace.
            let mut storage = self
                .db
                .get_transactional_storage_context(subtree_path.clone(), Some(&batch), tx.as_ref())
                .unwrap_add_cost(&mut cost);
            cost_return_on_error!(
                &mut cost,
                storage.clear().map_err(|e| {
                    Error::CorruptedData(format!(
                        "unable to clear non-merk tree data from storage: {e}",
                    ))
                })
            );

            drop(merk_to_clear);

            // Rewrite the parent element and propagate the new child hash,
            // refreshing the entry's canonical secondary row when the
            // parent is an indexed primary — the same walk the typed
            // append paths use.
            let mut parent_merk = cost_return_on_error!(
                &mut cost,
                self.open_transactional_merk_at_path(
                    parent_path.clone(),
                    tx.as_ref(),
                    Some(&batch),
                    grove_version,
                )
            );

            let old_indexed_state = cost_return_on_error!(
                &mut cost,
                GroveDb::capture_indexed_entry_state(
                    &parent_merk,
                    parent_key,
                    &stored_element,
                    grove_version,
                )
            );

            cost_return_on_error!(
                &mut cost,
                emptied_element
                    .insert_subtree(
                        &mut parent_merk,
                        parent_key,
                        empty_child_hash,
                        None,
                        grove_version,
                    )
                    .map_err(|e| e.into())
            );

            let mut merk_cache: HashMap<SubtreePath<B>, Merk<PrefixedRocksDbTransactionContext>> =
                HashMap::default();
            merk_cache.insert(parent_path.clone(), parent_merk);
            cost_return_on_error!(
                &mut cost,
                self.propagate_changes_with_transaction_refreshing_indexed_row(
                    merk_cache,
                    parent_path,
                    parent_key,
                    old_indexed_state,
                    tx.as_ref(),
                    &batch,
                    grove_version,
                )
            );

            cost_return_on_error!(
                &mut cost,
                self.db
                    .commit_multi_context_batch(batch, Some(tx.as_ref()))
                    .map_err(Into::into)
            );

            return tx.commit_local().map(|_| true).wrap_with_cost(cost);
        }

        if options.check_for_subtrees {
            let mut all_query = Query::new();
            all_query.insert_all();

            let mut element_iterator =
                KVIterator::new(merk_to_clear.storage.raw_iter(), &all_query).unwrap();

            // delete all nested subtrees
            while let Some((key, element_value)) =
                element_iterator.next_kv().unwrap_add_cost(&mut cost)
            {
                let element = match Element::raw_decode(&element_value, grove_version) {
                    Ok(e) => e,
                    Err(e) => {
                        return Err(Error::CorruptedData(format!(
                            "unable to decode element while clearing subtree: {e}"
                        )))
                        .wrap_with_cost(cost);
                    }
                };
                if element.is_any_tree() {
                    if options.allow_deleting_subtrees {
                        cost_return_on_error!(
                            &mut cost,
                            self.delete(
                                subtree_path.clone(),
                                key.as_slice(),
                                Some(DeleteOptions {
                                    allow_deleting_non_empty_trees: true,
                                    deleting_non_empty_trees_returns_error: false,
                                    ..Default::default()
                                }),
                                Some(tx.as_ref()),
                                grove_version,
                            )
                        );
                    } else if options.trying_to_clear_with_subtrees_returns_error {
                        return Err(Error::ClearingTreeWithSubtreesNotAllowed(
                            "options do not allow to clear this merk tree as it contains subtrees",
                        ))
                        .wrap_with_cost(cost);
                    } else {
                        return Ok(false).wrap_with_cost(cost);
                    }
                }
            }
        }

        // delete non subtree values
        cost_return_on_error!(&mut cost, merk_to_clear.clear().map_err(Error::MerkError));

        // propagate changes
        let mut merk_cache: HashMap<SubtreePath<B>, Merk<PrefixedRocksDbTransactionContext>> =
            HashMap::default();
        merk_cache.insert(subtree_path.clone(), merk_to_clear);
        cost_return_on_error!(
            &mut cost,
            self.propagate_changes_with_transaction(
                merk_cache,
                subtree_path.clone(),
                tx.as_ref(),
                &batch,
                grove_version,
            )
        );

        cost_return_on_error!(
            &mut cost,
            self.db
                .commit_multi_context_batch(batch, Some(tx.as_ref()))
                .map_err(Into::into)
        );

        tx.commit_local().map(|_| true).wrap_with_cost(cost)
    }
}
