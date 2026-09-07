//! `clear_subtree_with_costs` version 0.
//!
//! Differs from [v1](super::v1) ONLY in how a non-Merk data tree (`MmrTree`,
//! `BulkAppendTree`, `DenseAppendOnlyFixedSizeTree`, `CommitmentTree`,
//! `PrivateDocumentStore`) is cleared: the payload is deleted from the data
//! namespace and the operation returns success, but the parent element keeps
//! its old count / state commitment and nothing is propagated, leaving the
//! retained commitment describing data that no longer exists (issue #893).
//!
//! Selected by `GROVE_V1`, `GROVE_V2` and `GROVE_V3` — all live in
//! production, so the incoherent behaviour is kept bug-for-bug for replay
//! compatibility.

use std::collections::HashMap;

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use grovedb_merk::{element::decode::ElementDecodeExtensions, proofs::Query, KVIterator, Merk};
use grovedb_path::SubtreePath;
use grovedb_storage::{
    rocksdb_storage::PrefixedRocksDbTransactionContext, Storage, StorageBatch, StorageContext,
};
use grovedb_version::version::GroveVersion;

use super::super::{ClearOptions, DeleteOptions};
use crate::{util::TxRef, Element, Error, GroveDb, TransactionArg};

impl GroveDb {
    /// Clear a subtree — version 0 (frozen, see the module documentation).
    pub(crate) fn clear_subtree_with_costs_v0<'b, B: AsRef<[u8]>>(
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
        // entries.  We cannot iterate them with Element::iterator, so just
        // clear the storage directly.
        if merk_to_clear.tree_type.uses_non_merk_data_storage() {
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
