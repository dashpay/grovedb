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

//! Implementation for a storage abstraction over RocksDB.

use std::{ops::AddAssign, path::Path};

use costs::{
    cost_return_on_error, cost_return_on_error_no_add,
    storage_cost::removal::StorageRemovedBytes::BasicStorageRemoval, CostContext, CostResult,
    CostsExt, OperationCost,
};
use error::Error;
use integer_encoding::VarInt;
use lazy_static::lazy_static;
use rocksdb::{
    checkpoint::Checkpoint, ColumnFamily, ColumnFamilyDescriptor, OptimisticTransactionDB,
    Transaction, WriteBatchWithTransaction,
};

use super::{
    PrefixedRocksDbBatchStorageContext, PrefixedRocksDbBatchTransactionContext,
    PrefixedRocksDbStorageContext, PrefixedRocksDbTransactionContext,
};
use crate::{
    error,
    error::Error::{CostError, RocksDBError},
    worst_case_costs::WorstKeyLength,
    AbstractBatchOperation, Storage, StorageBatch,
};

const BLAKE_BLOCK_LEN: usize = 64;

fn blake_block_count(len: usize) -> usize {
    if len == 0 {
        1
    } else {
        1 + (len - 1) / BLAKE_BLOCK_LEN
    }
}

/// Name of column family used to store auxiliary data
pub(crate) const AUX_CF_NAME: &str = "aux";
/// Name of column family used to store subtrees roots data
pub(crate) const ROOTS_CF_NAME: &str = "roots";
/// Name of column family used to store metadata
pub(crate) const META_CF_NAME: &str = "meta";

lazy_static! {
    static ref DEFAULT_OPTS: rocksdb::Options = {
        let mut opts = rocksdb::Options::default();
        opts.create_if_missing(true);
        opts.increase_parallelism(num_cpus::get() as i32);
        opts.set_allow_mmap_writes(true);
        opts.set_allow_mmap_reads(true);
        opts.create_missing_column_families(true);
        opts.set_atomic_flush(true);
        opts
    };
}

/// Type alias for a database
pub(crate) type Db = OptimisticTransactionDB;

/// Type alias for a transaction
pub(crate) type Tx<'db> = Transaction<'db, Db>;

/// Storage which uses RocksDB as its backend.
pub struct RocksDbStorage {
    db: OptimisticTransactionDB,
}

impl RocksDbStorage {
    /// Create RocksDb storage with default parameters using `path`.
    pub fn default_rocksdb_with_path<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        let db = Db::open_cf_descriptors(
            &DEFAULT_OPTS,
            &path,
            [
                ColumnFamilyDescriptor::new(AUX_CF_NAME, DEFAULT_OPTS.clone()),
                ColumnFamilyDescriptor::new(ROOTS_CF_NAME, DEFAULT_OPTS.clone()),
                ColumnFamilyDescriptor::new(META_CF_NAME, DEFAULT_OPTS.clone()),
            ],
        )
        .map_err(RocksDBError)?;

        Ok(RocksDbStorage { db })
    }

    fn build_prefix_body<'a, P>(path: P) -> (Vec<u8>, usize)
    where
        P: IntoIterator<Item = &'a [u8]>,
    {
        let segments_iter = path.into_iter();
        let mut segments_count: usize = 0;
        let mut res = Vec::new();
        let mut lengthes = Vec::new();

        for s in segments_iter {
            segments_count += 1;
            res.extend_from_slice(s);
            lengthes.push(s.len() as u8); // if the key len is under 255 bytes
        }

        res.extend(segments_count.to_ne_bytes());
        res.extend(lengthes);
        (res, segments_count)
    }

    fn worst_case_body_size<L: WorstKeyLength>(path: &[L]) -> usize {
        path.len() + path.iter().map(|a| a.max_length() as usize).sum::<usize>()
    }

    /// A helper method to build a prefix to rocksdb keys or identify a subtree
    /// in `subtrees` map by tree path;
    pub fn build_prefix<'a, P>(path: P) -> CostContext<Vec<u8>>
    where
        P: IntoIterator<Item = &'a [u8]>,
    {
        let (body, segments_count) = Self::build_prefix_body(path);
        if segments_count == 0 {
            [0; 32].to_vec().wrap_with_cost(OperationCost::default())
        } else {
            let blocks_count = blake_block_count(body.len());

            blake3::hash(&body)
                .as_bytes()
                .to_vec()
                .wrap_with_cost(OperationCost::with_hash_node_calls(blocks_count as u32))
        }
    }

    /// Returns the write batch, with costs and pending costs
    /// Pending costs are costs that should only be applied after successful
    /// write of the write batch.
    pub fn build_write_batch(
        &self,
        storage_batch: StorageBatch,
    ) -> CostResult<(WriteBatchWithTransaction<true>, OperationCost), Error> {
        let mut db_batch = WriteBatchWithTransaction::<true>::default();
        self.continue_write_batch(&mut db_batch, storage_batch)
            .map_ok(|operation_cost| (db_batch, operation_cost))
    }

    /// Continues the write batch, returning pending costs
    /// Pending costs are costs that should only be applied after successful
    /// write of the write batch.
    pub fn continue_write_batch(
        &self,
        db_batch: &mut WriteBatchWithTransaction<true>,
        storage_batch: StorageBatch,
    ) -> CostResult<OperationCost, Error> {
        let mut cost = OperationCost::default();
        // Until batch is committed these costs are pending (should not be added in case
        // of early termination).
        let mut pending_costs = OperationCost::default();

        for op in storage_batch.into_iter() {
            match op {
                AbstractBatchOperation::Put {
                    key,
                    value,
                    children_sizes,
                    cost_info,
                } => {
                    db_batch.put(&key, &value);
                    cost.seek_count += 1;
                    cost_return_on_error_no_add!(
                        &cost,
                        pending_costs
                            .add_key_value_storage_costs(
                                key.len() as u32,
                                value.len() as u32,
                                children_sizes,
                                cost_info
                            )
                            .map_err(CostError)
                    );
                }
                AbstractBatchOperation::PutAux {
                    key,
                    value,
                    cost_info,
                } => {
                    db_batch.put_cf(cf_aux(&self.db), &key, &value);
                    cost.seek_count += 1;
                    cost_return_on_error_no_add!(
                        &cost,
                        pending_costs
                            .add_key_value_storage_costs(
                                key.len() as u32,
                                value.len() as u32,
                                None,
                                cost_info
                            )
                            .map_err(CostError)
                    );
                }
                AbstractBatchOperation::PutRoot {
                    key,
                    value,
                    cost_info,
                } => {
                    db_batch.put_cf(cf_roots(&self.db), &key, &value);
                    cost.seek_count += 1;
                    // We only add costs for put root if they are set, otherwise it is free
                    if cost_info.is_some() {
                        cost_return_on_error_no_add!(
                            &cost,
                            pending_costs
                                .add_key_value_storage_costs(
                                    key.len() as u32,
                                    value.len() as u32,
                                    None,
                                    cost_info
                                )
                                .map_err(CostError)
                        );
                    }
                }
                AbstractBatchOperation::PutMeta {
                    key,
                    value,
                    cost_info,
                } => {
                    db_batch.put_cf(cf_meta(&self.db), &key, &value);
                    cost.seek_count += 1;
                    cost_return_on_error_no_add!(
                        &cost,
                        pending_costs
                            .add_key_value_storage_costs(
                                key.len() as u32,
                                value.len() as u32,
                                None,
                                cost_info
                            )
                            .map_err(CostError)
                    );
                }
                AbstractBatchOperation::Delete { key, cost_info } => {
                    db_batch.delete(&key);

                    // TODO: fix not atomic freed size computation

                    if let Some(key_value_removed_bytes) = cost_info {
                        cost.seek_count += 1;
                        pending_costs.storage_cost.removed_bytes +=
                            key_value_removed_bytes.combined_removed_bytes();
                    } else {
                        cost.seek_count += 2;
                        // lets get the values
                        let value_len = cost_return_on_error_no_add!(
                            &cost,
                            self.db.get(&key).map_err(RocksDBError)
                        )
                        .map(|x| x.len() as u32)
                        .unwrap_or(0);
                        cost.storage_loaded_bytes += value_len;
                        let key_len = key.len() as u32;
                        // todo: improve deletion
                        pending_costs.storage_cost.removed_bytes += BasicStorageRemoval(
                            key_len
                                + value_len
                                + key_len.required_space() as u32
                                + value_len.required_space() as u32,
                        );
                    }
                }
                AbstractBatchOperation::DeleteAux { key, cost_info } => {
                    db_batch.delete_cf(cf_aux(&self.db), &key);

                    // TODO: fix not atomic freed size computation
                    if let Some(key_value_removed_bytes) = cost_info {
                        cost.seek_count += 1;
                        pending_costs.storage_cost.removed_bytes +=
                            key_value_removed_bytes.combined_removed_bytes();
                    } else {
                        cost.seek_count += 2;
                        let value_len = cost_return_on_error_no_add!(
                            &cost,
                            self.db.get_cf(cf_aux(&self.db), &key).map_err(RocksDBError)
                        )
                        .map(|x| x.len() as u32)
                        .unwrap_or(0);
                        cost.storage_loaded_bytes += value_len;

                        let key_len = key.len() as u32;
                        // todo: improve deletion
                        pending_costs.storage_cost.removed_bytes += BasicStorageRemoval(
                            key_len
                                + value_len
                                + key_len.required_space() as u32
                                + value_len.required_space() as u32,
                        );
                    }
                }
                AbstractBatchOperation::DeleteRoot { key, cost_info } => {
                    db_batch.delete_cf(cf_roots(&self.db), &key);

                    // TODO: fix not atomic freed size computation
                    if let Some(key_value_removed_bytes) = cost_info {
                        cost.seek_count += 1;
                        pending_costs.storage_cost.removed_bytes +=
                            key_value_removed_bytes.combined_removed_bytes();
                    } else {
                        cost.seek_count += 2;
                        let value_len = cost_return_on_error_no_add!(
                            &cost,
                            self.db
                                .get_cf(cf_roots(&self.db), &key)
                                .map_err(RocksDBError)
                        )
                        .map(|x| x.len() as u32)
                        .unwrap_or(0);
                        cost.storage_loaded_bytes += value_len;

                        let key_len = key.len() as u32;
                        // todo: improve deletion
                        pending_costs.storage_cost.removed_bytes += BasicStorageRemoval(
                            key_len
                                + value_len
                                + key_len.required_space() as u32
                                + value_len.required_space() as u32,
                        );
                    }
                }
                AbstractBatchOperation::DeleteMeta { key, cost_info } => {
                    db_batch.delete_cf(cf_meta(&self.db), &key);

                    // TODO: fix not atomic freed size computation
                    if let Some(key_value_removed_bytes) = cost_info {
                        cost.seek_count += 1;
                        pending_costs.storage_cost.removed_bytes +=
                            key_value_removed_bytes.combined_removed_bytes();
                    } else {
                        cost.seek_count += 2;
                        let value_len = cost_return_on_error_no_add!(
                            &cost,
                            self.db
                                .get_cf(cf_meta(&self.db), &key)
                                .map_err(RocksDBError)
                        )
                        .map(|x| x.len() as u32)
                        .unwrap_or(0);
                        cost.storage_loaded_bytes += value_len;

                        let key_len = key.len() as u32;
                        // todo: improve deletion
                        pending_costs.storage_cost.removed_bytes += BasicStorageRemoval(
                            key_len
                                + value_len
                                + key_len.required_space() as u32
                                + value_len.required_space() as u32,
                        );
                    }
                }
            }
        }
        Ok(pending_costs).wrap_with_cost(cost)
    }

    /// Commits a write batch
    pub fn commit_db_write_batch(
        &self,
        db_batch: WriteBatchWithTransaction<true>,
        pending_costs: OperationCost,
        transaction: Option<&<RocksDbStorage as Storage>::Transaction>,
    ) -> CostResult<(), Error> {
        let result = match transaction {
            None => self.db.write(db_batch),
            Some(transaction) => transaction.rebuild_from_writebatch(&db_batch),
        };

        if result.is_ok() {
            result.map_err(RocksDBError).wrap_with_cost(pending_costs)
        } else {
            result
                .map_err(RocksDBError)
                .wrap_with_cost(OperationCost::default())
        }
    }
}

impl<'db> Storage<'db> for RocksDbStorage {
    type BatchStorageContext = PrefixedRocksDbBatchStorageContext<'db>;
    type BatchTransactionalStorageContext = PrefixedRocksDbBatchTransactionContext<'db>;
    type StorageContext = PrefixedRocksDbStorageContext<'db>;
    type Transaction = Tx<'db>;
    type TransactionalStorageContext = PrefixedRocksDbTransactionContext<'db>;

    fn start_transaction(&'db self) -> Self::Transaction {
        self.db.transaction()
    }

    fn commit_transaction(&self, transaction: Self::Transaction) -> CostResult<(), Error> {
        // All transaction costs were provided on method calls
        transaction
            .commit()
            .map_err(RocksDBError)
            .wrap_with_cost(Default::default())
    }

    fn rollback_transaction(&self, transaction: &Self::Transaction) -> Result<(), Error> {
        transaction.rollback().map_err(RocksDBError)
    }

    fn flush(&self) -> Result<(), Error> {
        self.db.flush().map_err(RocksDBError)
    }

    fn get_storage_context<'p, P>(&'db self, path: P) -> CostContext<Self::StorageContext>
    where
        P: IntoIterator<Item = &'p [u8]>,
    {
        Self::build_prefix(path).map(|prefix| PrefixedRocksDbStorageContext::new(&self.db, prefix))
    }

    fn get_transactional_storage_context<'p, P>(
        &'db self,
        path: P,
        transaction: &'db Self::Transaction,
    ) -> CostContext<Self::TransactionalStorageContext>
    where
        P: IntoIterator<Item = &'p [u8]>,
    {
        Self::build_prefix(path)
            .map(|prefix| PrefixedRocksDbTransactionContext::new(&self.db, transaction, prefix))
    }

    fn get_batch_storage_context<'p, P>(
        &'db self,
        path: P,
        batch: &'db StorageBatch,
    ) -> CostContext<Self::BatchStorageContext>
    where
        P: IntoIterator<Item = &'p [u8]>,
    {
        Self::build_prefix(path)
            .map(|prefix| PrefixedRocksDbBatchStorageContext::new(&self.db, prefix, batch))
    }

    fn get_batch_transactional_storage_context<'p, P>(
        &'db self,
        path: P,
        batch: &'db StorageBatch,
        transaction: &'db Self::Transaction,
    ) -> CostContext<Self::BatchTransactionalStorageContext>
    where
        P: IntoIterator<Item = &'p [u8]>,
    {
        Self::build_prefix(path).map(|prefix| {
            PrefixedRocksDbBatchTransactionContext::new(&self.db, transaction, prefix, batch)
        })
    }

    fn commit_multi_context_batch(
        &self,
        batch: StorageBatch,
        transaction: Option<&'db Self::Transaction>,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        let (db_batch, pending_costs) =
            cost_return_on_error!(&mut cost, self.build_write_batch(batch));

        self.commit_db_write_batch(db_batch, pending_costs, transaction)
            .add_cost(cost)
    }

    fn get_storage_context_cost<L: WorstKeyLength>(path: &[L]) -> OperationCost {
        if path.is_empty() {
            OperationCost::default()
        } else {
            let body_size = Self::worst_case_body_size(path);
            // the block size of blake3 is 64
            let blocks_num = blake_block_count(body_size) as u32;
            OperationCost::with_hash_node_calls(blocks_num)
        }
    }

    fn create_checkpoint<P: AsRef<Path>>(&self, path: P) -> Result<(), Error> {
        Checkpoint::new(&self.db)
            .and_then(|x| x.create_checkpoint(path))
            .map_err(RocksDBError)
    }
}

/// Get auxiliary data column family
fn cf_aux(storage: &Db) -> &ColumnFamily {
    storage
        .cf_handle(AUX_CF_NAME)
        .expect("aux column family must exist")
}

/// Get trees roots data column family
fn cf_roots(storage: &Db) -> &ColumnFamily {
    storage
        .cf_handle(ROOTS_CF_NAME)
        .expect("roots column family must exist")
}

/// Get metadata column family
fn cf_meta(storage: &Db) -> &ColumnFamily {
    storage
        .cf_handle(META_CF_NAME)
        .expect("meta column family must exist")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        rocksdb_storage::{test_utils::TempStorage, RocksDbStorage},
        RawIterator, Storage, StorageContext,
    };

    #[test]
    fn test_build_prefix() {
        let path_a = [b"aa".as_ref(), b"b"];
        let path_b = [b"a".as_ref(), b"ab"];
        assert_ne!(
            RocksDbStorage::build_prefix(path_a),
            RocksDbStorage::build_prefix(path_b),
        );
        assert_eq!(
            RocksDbStorage::build_prefix(path_a),
            RocksDbStorage::build_prefix(path_a),
        );
    }

    #[test]
    fn rocksdb_layout_not_affect_iteration_costs() {
        // The test checks that key lengthes of seemingly unrelated subtrees
        // won't affect iteration costs. To achieve this we'll have two subtrees
        // and see that nothing nasty will happen if key lengths of the next subtree
        // change.
        let storage = TempStorage::new();

        let path_a = [b"ayya" as &[u8]];
        let path_b = [b"ayyb" as &[u8]];
        let prefix_a = RocksDbStorage::build_prefix(path_a).unwrap();
        let prefix_b = RocksDbStorage::build_prefix(path_b).unwrap();

        let context_a = storage.get_storage_context(path_a).unwrap();
        let context_b = storage.get_storage_context(path_b).unwrap();

        // Here by "left" I mean a subtree that goes first in RocksDB.
        let (left, right) = if prefix_a < prefix_b {
            (&context_a, &context_b)
        } else {
            (&context_b, &context_a)
        };

        left.put(b"a", b"a", None, None).unwrap().unwrap();
        left.put(b"b", b"b", None, None).unwrap().unwrap();
        left.put(b"c", b"c", None, None).unwrap().unwrap();

        right.put(b"a", b"a", None, None).unwrap().unwrap();
        right.put(b"b", b"b", None, None).unwrap().unwrap();
        right.put(b"c", b"c", None, None).unwrap().unwrap();

        // Iterate over left subtree while right subtree contains 1 byte keys:
        let mut iteration_cost_before = OperationCost::default();
        let mut iter = left.raw_iter();
        iter.seek_to_first().unwrap();
        // Collect sum of `valid` and `key` to check both ways to mess things up
        while iter.valid().unwrap_add_cost(&mut iteration_cost_before)
            && iter
                .key()
                .unwrap_add_cost(&mut iteration_cost_before)
                .is_some()
        {
            iter.next().unwrap_add_cost(&mut iteration_cost_before);
        }

        // Update right subtree to have keys of different size
        right.delete(b"a", None).unwrap().unwrap();
        right.delete(b"b", None).unwrap().unwrap();
        right.delete(b"c", None).unwrap().unwrap();
        right
            .put(b"aaaaaaaaaaaa", b"a", None, None)
            .unwrap()
            .unwrap();
        right
            .put(b"bbbbbbbbbbbb", b"b", None, None)
            .unwrap()
            .unwrap();
        right
            .put(b"cccccccccccc", b"c", None, None)
            .unwrap()
            .unwrap();

        // Iterate over left subtree once again
        let mut iteration_cost_after = OperationCost::default();
        let mut iter = left.raw_iter();
        iter.seek_to_first().unwrap();
        while iter.valid().unwrap_add_cost(&mut iteration_cost_after)
            && iter
                .key()
                .unwrap_add_cost(&mut iteration_cost_after)
                .is_some()
        {
            iter.next().unwrap_add_cost(&mut iteration_cost_after);
        }

        assert_eq!(iteration_cost_before, iteration_cost_after);
    }
}
