// MIT LICENSE
//
// Copyright (c) 2021 Dash Core Group
//
// Permission is hereby granted, free of charge, to any
// person obtaining a copy of this software and associated
// documentation files (the "Software"), to deal in the
// Software without restriction, including without
// limitation the rights to use, copy, modify, merge,
// publish, distribute, sublicense, and/or sell copies of
// the Software, and to permit persons to whom the Software
// is furnished to do so, subject to the following
// conditions:
//
// The above copyright notice and this permission notice
// shall be included in all copies or substantial portions
// of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
// ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
// TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
// PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
// SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
// CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
// OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
// IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

//! Auxiliary operations

use grovedb_costs::{
    cost_return_on_error, storage_cost::key_value_cost::KeyValueStorageCost, CostResult, CostsExt,
    OperationCost,
};
use grovedb_element::indexed::IndexAxis;
use grovedb_path::SubtreePath;
use grovedb_storage::{rocksdb_storage::RocksDbStorage, Storage, StorageBatch, StorageContext};
use grovedb_version::version::GroveVersion;

use crate::{
    element::elements_iterator::ElementIteratorExtensions, util::TxRef, Element, Error, GroveDb,
    Transaction, TransactionArg,
};

impl GroveDb {
    /// Put op for aux storage
    pub fn put_aux<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
        transaction: TransactionArg,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        let tx = TxRef::new(&self.db, transaction);
        let batch = Default::default();

        let aux_storage = self
            .db
            .get_transactional_storage_context(SubtreePath::empty(), Some(&batch), tx.as_ref())
            .unwrap_add_cost(&mut cost);

        cost_return_on_error!(
            &mut cost,
            aux_storage
                .put_aux(key.as_ref(), value, cost_info)
                .map_err(Into::into)
        );

        cost_return_on_error!(
            &mut cost,
            self.db
                .commit_multi_context_batch(batch, Some(tx.as_ref()))
                .map_err(Into::into)
        );

        tx.commit_local().wrap_with_cost(cost)
    }

    /// Delete op for aux storage
    pub fn delete_aux<K: AsRef<[u8]>>(
        &self,
        key: K,
        cost_info: Option<KeyValueStorageCost>,
        transaction: TransactionArg,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        let tx = TxRef::new(&self.db, transaction);
        let batch = Default::default();

        let aux_storage = self
            .db
            .get_transactional_storage_context(SubtreePath::empty(), Some(&batch), tx.as_ref())
            .unwrap_add_cost(&mut cost);

        cost_return_on_error!(
            &mut cost,
            aux_storage
                .delete_aux(key.as_ref(), cost_info)
                .map_err(|e| e.into())
        );

        cost_return_on_error!(
            &mut cost,
            self.db
                .commit_multi_context_batch(batch, Some(tx.as_ref()))
                .map_err(Into::into)
        );

        tx.commit_local().wrap_with_cost(cost)
    }

    /// Get op for aux storage
    pub fn get_aux<K: AsRef<[u8]>>(
        &self,
        key: K,
        transaction: TransactionArg,
    ) -> CostResult<Option<Vec<u8>>, Error> {
        let mut cost = OperationCost::default();
        let tx = TxRef::new(&self.db, transaction);

        let aux_storage = self
            .db
            .get_transactional_storage_context(SubtreePath::empty(), None, tx.as_ref())
            .unwrap_add_cost(&mut cost);

        aux_storage
            .get_aux(key.as_ref())
            .map_err(|e| e.into())
            .add_cost(cost)
    }

    // TODO: dumb traversal should not be tolerated
    /// Finds keys which are trees for a given subtree recursively.
    /// One element means a key of a `merk`, n > 1 elements mean relative path
    /// for a deeply nested subtree.
    ///
    /// # Storage batch visibility
    ///
    /// This method reads directly from the transaction (passing `None` for
    /// the storage batch parameter), so it only sees data that has been
    /// **committed** to the transaction. Any writes staged in a pending
    /// `StorageBatch` (e.g., from `apply_body` during batch processing)
    /// are invisible.
    ///
    /// In practice this is safe because the batch consistency check
    /// ([`crate::batch::QualifiedGroveDbOp::verify_consistency_of_operations`])
    /// rejects batches that insert subtrees under paths being deleted.
    /// The stale-state window only matters if the consistency check is
    /// bypassed via
    /// [`BatchApplyOptions::disable_operation_consistency_check`](crate::batch::BatchApplyOptions::disable_operation_consistency_check).
    pub fn find_subtrees<B: AsRef<[u8]>>(
        &self,
        path: &SubtreePath<B>,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<Vec<Vec<u8>>>, Error> {
        let mut cost = OperationCost::default();

        // TODO: remove conversion to vec;
        // However, it's not easy for a reason:
        // new keys to enqueue are taken from raw iterator which returns Vec<u8>;
        // changing that to slice is hard as cursor should be moved for next iteration
        // which requires exclusive (&mut) reference, also there is no guarantee that
        // slice which points into storage internals will remain valid if raw
        // iterator got altered so why that reference should be exclusive;
        //
        // Update: there are pinned views into RocksDB to return slices of data, perhaps
        // there is something for iterators

        let mut queue: Vec<Vec<Vec<u8>>> = vec![path.to_vec()];
        let mut result: Vec<Vec<Vec<u8>>> = queue.clone();

        let tx = TxRef::new(&self.db, transaction);

        while let Some(q) = queue.pop() {
            let subtree_path: SubtreePath<Vec<u8>> = q.as_slice().into();
            // Get the correct subtree with q_ref as path
            let storage = self
                .db
                .get_transactional_storage_context(subtree_path, None, tx.as_ref())
                .unwrap_add_cost(&mut cost);
            let mut raw_iter = Element::iterator(storage.raw_iter()).unwrap_add_cost(&mut cost);
            while let Some((key, value)) =
                cost_return_on_error!(&mut cost, raw_iter.next_element(grove_version))
            {
                if value.is_any_tree() {
                    let mut sub_path = q.clone();
                    sub_path.push(key.to_vec());
                    queue.push(sub_path.clone());
                    result.push(sub_path);
                }
            }
        }
        Ok(result).wrap_with_cost(cost)
    }

    /// Recursively clear storage owned by the subtree at `path`: the
    /// primary namespace of every nested subtree discovered by
    /// [`Self::find_subtrees`], plus — when `sweep_secondary_namespaces` is
    /// enabled, for EVERY discovered subtree — the
    /// per-axis indexed-tree secondary namespaces at
    /// `Blake3(subtree_prefix ‖ axis_tag)` (S2-B derivation).
    ///
    /// This is the single recursive ownership-cleanup routine (issue #888)
    /// shared by the direct delete (`delete_internal_on_transaction`
    /// v0/v1), the batch `DeleteTree` post-apply pass (full and partial),
    /// the batch cidx safe-subset overwrite pass, and the dedicated
    /// indexed-tree child overwrite. `find_subtrees` only walks
    /// path-derived prefixes, so a nested indexed-tree primary's secondary
    /// namespaces are invisible to it; without the per-descendant sweep
    /// they would be orphaned, and a later re-creation of the same
    /// deterministic path (prefixes are path-derived) would resurrect the
    /// stale secondary rows, breaking primary-secondary agreement.
    ///
    /// When enabled, all three axis tags are swept for every discovered
    /// subtree rather than decoding each subtree's element to check its
    /// tree type: clearing an empty namespace is a no-op, so the
    /// redundancy is intentional defense-in-depth (it also removes a class
    /// of missed-decoding bugs).
    ///
    /// Full and partial batch deletion disable the secondary sweep on
    /// V1..V3 to preserve historical costs, including empty-namespace seeks
    /// and hashes for ordinary trees. Direct deletion keeps it enabled on
    /// every version because its legacy loop already included the sweep.
    ///
    /// `context` names the calling operation in error messages.
    pub(crate) fn clear_subtree_storage_recursively<'db, B: AsRef<[u8]>>(
        &'db self,
        path: &SubtreePath<B>,
        transaction: &'db Transaction,
        batch: &'db StorageBatch,
        sweep_secondary_namespaces: bool,
        context: &str,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        let subtrees_paths = cost_return_on_error!(
            &mut cost,
            self.find_subtrees(path, Some(transaction), grove_version)
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
                        "unable to clean up subtree storage in {context}: {e}",
                    ))
                })
            );

            if !sweep_secondary_namespaces {
                continue;
            }
            let primary_prefix = RocksDbStorage::build_prefix(p).unwrap_add_cost(&mut cost);
            for axis in [IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg] {
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
                            "unable to clean up indexed-tree secondary (axis {axis:?}) in \
                             {context}: {e}",
                        ))
                    })
                );
            }
        }
        Ok(()).wrap_with_cost(cost)
    }
}
