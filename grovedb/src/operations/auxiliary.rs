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

mod find_subtrees;

use std::collections::HashSet;

use grovedb_costs::{
    cost_return_on_error, storage_cost::key_value_cost::KeyValueStorageCost, CostResult, CostsExt,
    OperationCost,
};
use grovedb_element::indexed::IndexAxis;
use grovedb_path::SubtreePath;
use grovedb_storage::{
    rocksdb_storage::RocksDbStorage, RawIterator, Storage, StorageBatch, StorageContext,
};
use grovedb_version::version::GroveVersion;

use crate::{util::TxRef, Error, GroveDb, Transaction, TransactionArg};

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

    /// Every aux entry whose key starts with `key_prefix`, in key order, as
    /// `(key, value)` pairs with the key exactly as it was given to
    /// [`Self::put_aux`]. An empty prefix lists every aux entry.
    ///
    /// Aux storage is otherwise reachable only by exact key, so a caller that
    /// keeps a collection as one entry per member under a shared key prefix
    /// reads the collection back with this. Through a transaction the listing
    /// includes that transaction's own uncommitted puts and deletes.
    pub fn get_aux_by_key_prefix<K: AsRef<[u8]>>(
        &self,
        key_prefix: K,
        transaction: TransactionArg,
    ) -> CostResult<Vec<(Vec<u8>, Vec<u8>)>, Error> {
        let mut cost = OperationCost::default();
        let tx = TxRef::new(&self.db, transaction);
        let key_prefix = key_prefix.as_ref();

        let aux_storage = self
            .db
            .get_transactional_storage_context(SubtreePath::empty(), None, tx.as_ref())
            .unwrap_add_cost(&mut cost);

        let mut iter = aux_storage.raw_iter_aux();
        iter.seek(key_prefix).unwrap_add_cost(&mut cost);

        let mut entries = Vec::new();
        while let Some(key) = iter.key().unwrap_add_cost(&mut cost) {
            if !key.starts_with(key_prefix) {
                break;
            }
            let key = key.to_vec();
            // A key was read, so the iterator is valid and the value is present.
            let Some(value) = iter.value().unwrap_add_cost(&mut cost) else {
                break;
            };
            entries.push((key, value.to_vec()));
            iter.next().unwrap_add_cost(&mut cost);
        }

        Ok(entries).wrap_with_cost(cost)
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
    /// With `accounted_deletes`, the V4+ walk also checks the removal's
    /// claim about its contents on the elements it decodes anyway: a
    /// backward-reference participant not deleted explicitly by the caller
    /// (a position in the set) refuses the removal. Earlier versions never
    /// hold participants and keep their historical walk.
    ///
    /// `context` names the calling operation in error messages.
    pub(crate) fn clear_subtree_storage_recursively<'db, B: AsRef<[u8]>>(
        &'db self,
        path: &SubtreePath<B>,
        transaction: &'db Transaction,
        batch: &'db StorageBatch,
        sweep_secondary_namespaces: bool,
        accounted_deletes: Option<&HashSet<Vec<Vec<u8>>>>,
        context: &str,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        let subtrees_paths = match accounted_deletes {
            Some(accounted_deletes)
                if grove_version
                    .grovedb_versions
                    .operations
                    .non_merk_tree
                    .subtree_discovery
                    >= 1 =>
            {
                cost_return_on_error!(
                    &mut cost,
                    self.find_subtrees_refusing_participants_v1(
                        path,
                        Some(transaction),
                        grove_version,
                        accounted_deletes,
                    )
                )
            }
            _ => cost_return_on_error!(
                &mut cost,
                self.find_subtrees(path, Some(transaction), grove_version)
            ),
        };
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

#[cfg(test)]
mod tests {
    use crate::{tests::make_empty_grovedb, GroveDb, TransactionArg};

    fn put(db: &GroveDb, key: &[u8], value: &[u8], transaction: TransactionArg) {
        db.put_aux(key, value, None, transaction)
            .unwrap()
            .expect("put aux");
    }

    fn listing(
        db: &GroveDb,
        key_prefix: &[u8],
        transaction: TransactionArg,
    ) -> Vec<(Vec<u8>, Vec<u8>)> {
        db.get_aux_by_key_prefix(key_prefix, transaction)
            .unwrap()
            .expect("aux listing")
    }

    #[test]
    fn get_aux_by_key_prefix_lists_only_that_prefix_in_key_order() {
        let db = make_empty_grovedb();
        put(&db, b"mn/b", b"2", None);
        put(&db, b"mn/a", b"1", None);
        put(&db, b"mn/c", b"3", None);
        // Equal to the prefix: included. Shorter, or diverging: excluded.
        put(&db, b"mn/", b"exact prefix", None);
        put(&db, b"mn", b"shorter", None);
        put(&db, b"mo/a", b"other prefix", None);
        put(&db, b"m", b"much shorter", None);

        assert_eq!(
            listing(&db, b"mn/", None),
            vec![
                (b"mn/".to_vec(), b"exact prefix".to_vec()),
                (b"mn/a".to_vec(), b"1".to_vec()),
                (b"mn/b".to_vec(), b"2".to_vec()),
                (b"mn/c".to_vec(), b"3".to_vec()),
            ]
        );
    }

    #[test]
    fn get_aux_by_key_prefix_is_empty_when_nothing_matches() {
        let db = make_empty_grovedb();
        put(&db, b"mn/a", b"1", None);

        assert!(listing(&db, b"vs/", None).is_empty());
        assert!(listing(&db, b"zz", None).is_empty());
        assert!(listing(&make_empty_grovedb(), b"", None).is_empty());
    }

    #[test]
    fn get_aux_by_key_prefix_with_an_empty_prefix_lists_every_entry() {
        let db = make_empty_grovedb();
        put(&db, b"vs/x", b"3", None);
        put(&db, b"mn/a", b"1", None);
        put(&db, b"mn/b", b"2", None);

        assert_eq!(
            listing(&db, b"", None),
            vec![
                (b"mn/a".to_vec(), b"1".to_vec()),
                (b"mn/b".to_vec(), b"2".to_vec()),
                (b"vs/x".to_vec(), b"3".to_vec()),
            ]
        );
    }

    #[test]
    fn get_aux_by_key_prefix_sees_the_transactions_own_writes() {
        let db = make_empty_grovedb();
        put(&db, b"mn/a", b"committed", None);
        put(&db, b"mn/b", b"to delete", None);

        let transaction = db.start_transaction();
        put(&db, b"mn/c", b"pending", Some(&transaction));
        put(&db, b"mn/a", b"rewritten", Some(&transaction));
        db.delete_aux(b"mn/b", None, Some(&transaction))
            .unwrap()
            .expect("delete aux");

        // Through the transaction: its own puts and deletes.
        assert_eq!(
            listing(&db, b"mn/", Some(&transaction)),
            vec![
                (b"mn/a".to_vec(), b"rewritten".to_vec()),
                (b"mn/c".to_vec(), b"pending".to_vec()),
            ]
        );
        // Outside it: committed state only.
        assert_eq!(
            listing(&db, b"mn/", None),
            vec![
                (b"mn/a".to_vec(), b"committed".to_vec()),
                (b"mn/b".to_vec(), b"to delete".to_vec()),
            ]
        );

        db.commit_transaction(transaction).unwrap().expect("commit");
        assert_eq!(
            listing(&db, b"mn/", None),
            vec![
                (b"mn/a".to_vec(), b"rewritten".to_vec()),
                (b"mn/c".to_vec(), b"pending".to_vec()),
            ]
        );
    }
}
