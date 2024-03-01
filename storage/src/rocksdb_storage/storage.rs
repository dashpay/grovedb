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

use std::path::Path;

use crate::error::Error;
use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add,
    storage_cost::removal::StorageRemovedBytes::BasicStorageRemoval, CostContext, CostResult,
    CostsExt, OperationCost,
};
use grovedb_path::SubtreePath;
use integer_encoding::VarInt;
use lazy_static::lazy_static;
use rocksdb::{
    checkpoint::Checkpoint, ColumnFamily, ColumnFamilyDescriptor, OptimisticTransactionDB,
    Transaction, WriteBatchWithTransaction, DEFAULT_COLUMN_FAMILY_NAME,
};

use super::{
    PrefixedRocksDbImmediateStorageContext, PrefixedRocksDbStorageContext,
    PrefixedRocksDbTransactionContext,
};
use crate::{
    storage::AbstractBatchOperation,
    worst_case_costs::WorstKeyLength,
    Storage, StorageBatch,
};

const BLAKE_BLOCK_LEN: usize = 64;

pub(crate) type SubtreePrefix = [u8; blake3::OUT_LEN];

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

/// Non-transactional database that supports secondary instance
pub(crate) type NonTransactionalDb = rocksdb::DB;

/// Type alias for a transaction
pub(crate) type Tx<'db> = Transaction<'db, OptimisticTransactionDB>;

/// Storage which uses RocksDB as its backend.
pub enum RocksDbStorage {
    /// Primary storage
    Primary(OptimisticTransactionDB),
    /// Secondary storage
    Secondary(NonTransactionalDb),
}

macro_rules! call_with_db {
    ($self:ident, $db:ident, $code:block) => {
        match $self {
            RocksDbStorage::Primary($db) => $code,
            RocksDbStorage::Secondary($db) => $code,
        }
    };
}

impl RocksDbStorage {
    /// Create RocksDb primary storage with default parameters using `path`.
    pub fn default_primary_rocksdb<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        let db = OptimisticTransactionDB::open_cf_descriptors(
            &DEFAULT_OPTS,
            &path,
            [
                ColumnFamilyDescriptor::new(AUX_CF_NAME, DEFAULT_OPTS.clone()),
                ColumnFamilyDescriptor::new(ROOTS_CF_NAME, DEFAULT_OPTS.clone()),
                ColumnFamilyDescriptor::new(META_CF_NAME, DEFAULT_OPTS.clone()),
            ],
        )
        .map_err(Error::RocksDBError)?;

        Ok(Self::Primary(db))
    }

    /// Create RocksDb secondary storage with default parameters using primary and secondary db paths.
    pub fn default_secondary_rocksdb<P: AsRef<Path>>(
        primary_path: P,
        secondary_path: P,
    ) -> Result<Self, Error> {
        // Limitation for secondary indices https://github.com/facebook/rocksdb/wiki/Read-only-and-Secondary-instances
        let mut opts = DEFAULT_OPTS.clone();
        opts.set_max_open_files(-1);

        let db = NonTransactionalDb::open_cf_descriptors_as_secondary(
            &DEFAULT_OPTS,
            &primary_path,
            &secondary_path,
            [
                ColumnFamilyDescriptor::new(AUX_CF_NAME, DEFAULT_OPTS.clone()),
                ColumnFamilyDescriptor::new(ROOTS_CF_NAME, DEFAULT_OPTS.clone()),
                ColumnFamilyDescriptor::new(META_CF_NAME, DEFAULT_OPTS.clone()),
            ],
        )
        .map_err(Error::RocksDBError)?;

        Ok(Self::Secondary(db))
    }

    /// Replicate recent changes from primary database
    /// Available only for a secondary storage
    pub fn try_to_catch_up_from_primary(&self) -> Result<(), Error> {
        match self {
            RocksDbStorage::Primary(_) => {
                Err(Error::StorageError("primary storage doesn't catchup".to_string()))
            }
            RocksDbStorage::Secondary(db) => db.try_catch_up_with_primary().map_err(Error::RocksDBError),
        }
    }

    fn build_prefix_body<B>(path: SubtreePath<B>) -> (Vec<u8>, usize)
    where
        B: AsRef<[u8]>,
    {
        let segments_iter = path.into_reverse_iter();
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

    /// A helper method to build a prefix to rocksdb keys or identify a subtree
    /// in `subtrees` map by tree path;
    pub fn build_prefix<B>(path: SubtreePath<B>) -> CostContext<SubtreePrefix>
    where
        B: AsRef<[u8]>,
    {
        let (body, segments_count) = Self::build_prefix_body(path);
        if segments_count == 0 {
            SubtreePrefix::default().wrap_with_cost(OperationCost::default())
        } else {
            let blocks_count = blake_block_count(body.len());
            SubtreePrefix::from(blake3::hash(&body))
                .wrap_with_cost(OperationCost::with_hash_node_calls(blocks_count as u32))
        }
    }

    fn worst_case_body_size<L: WorstKeyLength>(path: &[L]) -> usize {
        path.len() + path.iter().map(|a| a.max_length() as usize).sum::<usize>()
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
                            .map_err(Error::CostError)
                    );
                }
                AbstractBatchOperation::PutAux {
                    key,
                    value,
                    cost_info,
                } => {
                    db_batch.put_cf(self.cf_aux(), &key, &value);
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
                            .map_err(Error::CostError)
                    );
                }
                AbstractBatchOperation::PutRoot {
                    key,
                    value,
                    cost_info,
                } => {
                    db_batch.put_cf(self.cf_roots(), &key, &value);
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
                                .map_err(Error::CostError)
                        );
                    }
                }
                AbstractBatchOperation::PutMeta {
                    key,
                    value,
                    cost_info,
                } => {
                    db_batch.put_cf(self.cf_meta(), &key, &value);
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
                            .map_err(Error::CostError)
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
                            call_with_db!(self, db, { db.get(&key).map_err(Error::RocksDBError) })
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
                    db_batch.delete_cf(self.cf_aux(), &key);

                    // TODO: fix not atomic freed size computation
                    if let Some(key_value_removed_bytes) = cost_info {
                        cost.seek_count += 1;
                        pending_costs.storage_cost.removed_bytes +=
                            key_value_removed_bytes.combined_removed_bytes();
                    } else {
                        cost.seek_count += 2;
                        let value_len = cost_return_on_error_no_add!(
                            &cost,
                            call_with_db!(self, db, {
                                db.get_cf(self.cf_aux(), &key).map_err(Error::RocksDBError)
                            })
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
                    db_batch.delete_cf(self.cf_roots(), &key);

                    // TODO: fix not atomic freed size computation
                    if let Some(key_value_removed_bytes) = cost_info {
                        cost.seek_count += 1;
                        pending_costs.storage_cost.removed_bytes +=
                            key_value_removed_bytes.combined_removed_bytes();
                    } else {
                        cost.seek_count += 2;
                        let value_len = cost_return_on_error_no_add!(
                            &cost,
                            call_with_db!(self, db, {
                                db.get_cf(self.cf_roots(), &key).map_err(Error::RocksDBError)
                            })
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
                    db_batch.delete_cf(self.cf_meta(), &key);

                    // TODO: fix not atomic freed size computation
                    if let Some(key_value_removed_bytes) = cost_info {
                        cost.seek_count += 1;
                        pending_costs.storage_cost.removed_bytes +=
                            key_value_removed_bytes.combined_removed_bytes();
                    } else {
                        cost.seek_count += 2;
                        let value_len = cost_return_on_error_no_add!(
                            &cost,
                            call_with_db!(self, db, {
                                db.get_cf(self.cf_meta(), &key).map_err(Error::RocksDBError)
                            })
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
        match self {
            RocksDbStorage::Primary(db) => {
                let result = match transaction {
                    None => db.write(db_batch),
                    Some(transaction) => transaction.rebuild_from_writebatch(&db_batch),
                };

                if result.is_ok() {
                    result.map_err(Error::RocksDBError).wrap_with_cost(pending_costs)
                } else {
                    result
                        .map_err(Error::RocksDBError)
                        .wrap_with_cost(OperationCost::default())
                }
            }
            RocksDbStorage::Secondary(_) => {
                unimplemented!("secondary storage does not support WriteBatchWithTransaction<true>")
            }
        }
    }

    /// Destroys the OptimisticTransactionDB and drops instance
    pub fn wipe(&self) -> Result<(), Error> {
        // TODO: fix this
        // very inefficient way of doing this, time complexity is O(n)
        // we can do O(1)
        self.wipe_column_family(DEFAULT_COLUMN_FAMILY_NAME)?;
        self.wipe_column_family(ROOTS_CF_NAME)?;
        self.wipe_column_family(AUX_CF_NAME)?;
        self.wipe_column_family(META_CF_NAME)?;
        Ok(())
    }

    fn wipe_column_family(&self, column_family_name: &str) -> Result<(), Error> {
        call_with_db!(self, db, {
            let cf_handle = db.cf_handle(column_family_name).ok_or(Error::StorageError(
                "failed to get column family handle".to_string(),
            ))?;
            let mut iter = db.raw_iterator_cf(&cf_handle);
            iter.seek_to_first();
            while iter.valid() {
                db.delete(iter.key().expect("should have key"))?;
                iter.next()
            }
            Ok(())
        })
    }

    /// Get auxiliary data column family
    fn cf_aux(&self) -> &ColumnFamily {
        call_with_db!(self, db, {
            db.cf_handle(AUX_CF_NAME)
                .expect("meta column family must exist")
        })
    }

    /// Get trees roots data column family
    fn cf_roots(&self) -> &ColumnFamily {
        call_with_db!(self, db, {
            db.cf_handle(ROOTS_CF_NAME)
                .expect("meta column family must exist")
        })
    }

    /// Get metadata column family
    fn cf_meta(&self) -> &ColumnFamily {
        call_with_db!(self, db, {
            db.cf_handle(META_CF_NAME)
                .expect("meta column family must exist")
        })
    }
}

impl<'db> Storage<'db> for RocksDbStorage {
    type Transaction = Tx<'db>;
    type BatchStorageContext = PrefixedRocksDbStorageContext<'db>;
    type BatchTransactionalStorageContext = PrefixedRocksDbTransactionContext<'db>;
    type ImmediateStorageContext = PrefixedRocksDbImmediateStorageContext<'db>;

    fn start_transaction(&'db self) -> Self::Transaction {
        match self {
            RocksDbStorage::Primary(db) => db.transaction(),
            RocksDbStorage::Secondary(_) => {
                unimplemented!("secondary storage does not support transactions")
            }
        }
    }

    fn commit_transaction(&self, transaction: Self::Transaction) -> CostResult<(), Error> {
        // All transaction costs were provided on method calls
        transaction
            .commit()
            .map_err(Error::RocksDBError)
            .wrap_with_cost(Default::default())
    }

    fn rollback_transaction(&self, transaction: &Self::Transaction) -> Result<(), Error> {
        transaction.rollback().map_err(Error::RocksDBError)
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

    fn flush(&self) -> Result<(), Error> {
        if let RocksDbStorage::Primary(db) = self {
            return db.flush().map_err(Error::RocksDBError)
        }

        // Flush is not implemented for secondary storage but still can be called by
        // GroveDB, so we just do nothing in this case

        Ok(())
    }

    fn get_storage_context<'b, B>(
        &'db self,
        path: SubtreePath<'b, B>,
        batch: Option<&'db StorageBatch>,
    ) -> CostContext<Self::BatchStorageContext>
    where
        B: AsRef<[u8]> + 'b,
    {
        Self::build_prefix(path).map(|prefix| match self {
            RocksDbStorage::Primary(db) => {
                PrefixedRocksDbStorageContext::new_primary(db, prefix, batch)
            }
            RocksDbStorage::Secondary(db) => {
                PrefixedRocksDbStorageContext::new_secondary(db, prefix, batch)
            }
        })
    }

    fn get_transactional_storage_context<'b, B>(
        &'db self,
        path: SubtreePath<'b, B>,
        batch: Option<&'db StorageBatch>,
        transaction: &'db Self::Transaction,
    ) -> CostContext<Self::BatchTransactionalStorageContext>
    where
        B: AsRef<[u8]> + 'b,
    {
        Self::build_prefix(path).map(|prefix| match self {
            RocksDbStorage::Primary(db) => {
                PrefixedRocksDbTransactionContext::new_primary(db, transaction, prefix, batch)
            }
            RocksDbStorage::Secondary(db) => {
                PrefixedRocksDbTransactionContext::new_secondary(db, prefix, batch)
            }
        })
    }

    fn get_immediate_storage_context<'b, B>(
        &'db self,
        path: SubtreePath<'b, B>,
        transaction: &'db Self::Transaction,
    ) -> CostContext<Self::ImmediateStorageContext>
    where
        B: AsRef<[u8]> + 'b,
    {
        Self::build_prefix(path).map(|prefix| match self {
            RocksDbStorage::Primary(db) => {
                PrefixedRocksDbImmediateStorageContext::new_primary(db, transaction, prefix)
            }
            RocksDbStorage::Secondary(db) => {
                PrefixedRocksDbImmediateStorageContext::new_secondary(db, prefix)
            }
        })
    }

    fn create_checkpoint<P: AsRef<Path>>(&self, path: P) -> Result<(), Error> {
        call_with_db!(self, db, {
            Checkpoint::new(db)
                .and_then(|x| x.create_checkpoint(path))
                .map_err(Error::RocksDBError)
        })
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
            RocksDbStorage::build_prefix(path_a.as_ref().into()),
            RocksDbStorage::build_prefix(path_b.as_ref().into()),
        );
        assert_eq!(
            RocksDbStorage::build_prefix(path_a.as_ref().into()),
            RocksDbStorage::build_prefix(path_a.as_ref().into()),
        );
    }

    #[test]
    fn rocksdb_layout_not_affect_iteration_costs() {
        // The test checks that key lengthes of seemingly unrelated subtrees
        // won't affect iteration costs. To achieve this we'll have two subtrees
        // and see that nothing nasty will happen if key lengths of the next subtree
        // change.
        let storage = TempStorage::new();

        let path_a = SubtreePath::from(&[b"ayya" as &[u8]]);
        let path_b = SubtreePath::from(&[b"ayyb" as &[u8]]);
        let prefix_a = RocksDbStorage::build_prefix(path_a.clone()).unwrap();
        let prefix_b = RocksDbStorage::build_prefix(path_b.clone()).unwrap();

        // Here by "left" I mean a subtree that goes first in RocksDB.
        let (left_path, right_path) = if prefix_a < prefix_b {
            (path_a, path_b)
        } else {
            (path_b, path_a)
        };

        let batch = StorageBatch::new();
        let left = storage
            .get_storage_context(left_path.clone(), Some(&batch))
            .unwrap();
        let right = storage
            .get_storage_context(right_path.clone(), Some(&batch))
            .unwrap();

        left.put(b"a", b"a", None, None).unwrap().unwrap();
        left.put(b"b", b"b", None, None).unwrap().unwrap();
        left.put(b"c", b"c", None, None).unwrap().unwrap();

        right.put(b"a", b"a", None, None).unwrap().unwrap();
        right.put(b"b", b"b", None, None).unwrap().unwrap();
        right.put(b"c", b"c", None, None).unwrap().unwrap();

        storage
            .commit_multi_context_batch(batch, None)
            .unwrap()
            .expect("cannot commit batch");

        let batch = StorageBatch::new();
        let left = storage
            .get_storage_context(left_path.clone(), Some(&batch))
            .unwrap();
        let right = storage
            .get_storage_context(right_path.clone(), Some(&batch))
            .unwrap();

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

        drop(iter);

        storage
            .commit_multi_context_batch(batch, None)
            .unwrap()
            .expect("cannot commit batch");

        let left = storage.get_storage_context(left_path, None).unwrap();
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
