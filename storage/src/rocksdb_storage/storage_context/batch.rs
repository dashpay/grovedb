//! Prefixed storage batch implementation for RocksDB backend.
use costs::{cost_return_on_error_no_add, CostContext, CostsExt, OperationCost, StorageCost};
use rocksdb::{ColumnFamily, WriteBatchWithTransaction};

use super::make_prefixed_key;
use crate::{rocksdb_storage::storage::Db, Batch, StorageBatch};

/// Wrapper to RocksDB batch.
/// All calls go to RocksDB batch, but wrapper handles prefixes and column
/// families. Also accumulates costs before commit.
pub struct PrefixedRocksDbBatch<'db> {
    pub(crate) prefix: Vec<u8>,
    pub(crate) batch: WriteBatchWithTransaction<true>,
    pub(crate) cf_aux: &'db ColumnFamily,
    pub(crate) cf_roots: &'db ColumnFamily,

    /// As a batch to be commited is a RocksDB batch and there is no way to get
    /// what it will do, we collect costs at the moment we append something to
    /// the batch.
    pub(crate) cost_acc: OperationCost,
    /// ... However, computation of deletion costs is still hacky as we need to
    /// do additional `get` which possibly could fail, but batch append should
    /// not fail, we defer these `get`s to commit stage. Hopefully all
    /// operations will return costs (or written data numbers) naturally in the
    /// future.
    pub(crate) delete_keys_for_costs: Vec<Vec<u8>>,
    pub(crate) delete_keys_for_costs_aux: Vec<Vec<u8>>,
    pub(crate) delete_keys_for_costs_roots: Vec<Vec<u8>>,
}

impl<'db> PrefixedRocksDbBatch<'db> {
    /// Populate costs accumulator with deferred deletion costs.
    pub(crate) fn finalize_deletion_costs(
        &mut self,
        db: &'db Db,
    ) -> CostContext<Result<(), rocksdb::Error>> {
        // Comutation of deletion cost has it's own... cost.
        let mut cost = OperationCost::default();

        for key in self.delete_keys_for_costs.iter() {
            let value = cost_return_on_error_no_add!(&cost, db.get(key));
            cost.seek_count += 1;
            if let Some(v) = value {
                cost.storage_loaded_bytes += v.len() as u32;
                self.cost_acc.storage_removed_bytes += v.len() as u32;
            }
        }

        for key in self.delete_keys_for_costs_aux.iter() {
            let value = cost_return_on_error_no_add!(&cost, db.get_cf(self.cf_aux, key));
            cost.seek_count += 1;
            if let Some(v) = value {
                cost.storage_loaded_bytes += v.len() as u32;
                self.cost_acc.storage_removed_bytes += v.len() as u32;
            }
        }

        for key in self.delete_keys_for_costs_roots.iter() {
            let value = cost_return_on_error_no_add!(&cost, db.get_cf(self.cf_roots, key));
            cost.seek_count += 1;
            if let Some(v) = value {
                cost.storage_loaded_bytes += v.len() as u32;
                self.cost_acc.storage_removed_bytes += v.len() as u32;
            }
        }

        Ok(()).wrap_with_cost(cost)
    }
}

/// Batch with no backing storage (it's not a RocksDB batch, but our own way to
/// represent a set of operations) that eventually will be merged into
/// multi-context batch.
pub struct PrefixedMultiContextBatchPart {
    pub(crate) prefix: Vec<u8>,
    pub(crate) batch: StorageBatch,
    pub(crate) acc_cost: OperationCost,
}

/// Implementation of a batch ouside a transaction
impl<'db> Batch for PrefixedRocksDbBatch<'db> {
    fn put<K: AsRef<[u8]>>(
        &mut self,
        key: K,
        value: &[u8],
        value_cost_info: Option<StorageCost>,
    ) {
        let prefixed_key = make_prefixed_key(self.prefix.clone(), key);

        self.cost_acc.seek_count += 1;
        self.cost_acc.storage_added_bytes += prefixed_key.len() as u32 + value.len() as u32;
        // dbg!(prefixed_key.len());

        self.batch.put(prefixed_key, value);
    }

    fn put_aux<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) {
        let prefixed_key = make_prefixed_key(self.prefix.clone(), key);

        self.cost_acc.seek_count += 1;
        self.cost_acc.storage_added_bytes += prefixed_key.len() as u32 + value.len() as u32;

        self.batch.put_cf(self.cf_aux, prefixed_key, value);
    }

    fn put_root<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) {
        let prefixed_key = make_prefixed_key(self.prefix.clone(), key);

        self.cost_acc.seek_count += 1;
        self.cost_acc.storage_added_bytes += prefixed_key.len() as u32 + value.len() as u32;

        self.batch.put_cf(self.cf_roots, prefixed_key, value);
    }

    fn delete<K: AsRef<[u8]>>(&mut self, key: K) {
        let prefixed_key = make_prefixed_key(self.prefix.clone(), key);

        self.cost_acc.seek_count += 1;
        self.delete_keys_for_costs.push(prefixed_key.clone());

        self.batch.delete(prefixed_key);
    }

    fn delete_aux<K: AsRef<[u8]>>(&mut self, key: K) {
        let prefixed_key = make_prefixed_key(self.prefix.clone(), key);

        self.cost_acc.seek_count += 1;
        self.delete_keys_for_costs_aux.push(prefixed_key.clone());

        self.batch.delete_cf(self.cf_aux, prefixed_key);
    }

    fn delete_root<K: AsRef<[u8]>>(&mut self, key: K) {
        let prefixed_key = make_prefixed_key(self.prefix.clone(), key);

        self.cost_acc.seek_count += 1;
        self.delete_keys_for_costs_roots.push(prefixed_key.clone());

        self.batch.delete_cf(self.cf_roots, prefixed_key);
    }
}

/// Implementation of a rocksdb batch ouside a transaction for multi-context
/// batch.
impl Batch for PrefixedMultiContextBatchPart {
    fn put<K: AsRef<[u8]>>(
        &mut self,
        key: K,
        value: &[u8],
        value_cost_info: Option<StorageCost>,
    ) {
        self.batch
            .put(
                make_prefixed_key(self.prefix.clone(), key),
                value.to_vec(),
                value_cost_info,
            )
            .unwrap_add_cost(&mut self.acc_cost);
    }

    fn put_aux<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) {
        self.batch
            .put_aux(make_prefixed_key(self.prefix.clone(), key), value.to_vec())
            .unwrap_add_cost(&mut self.acc_cost);
    }

    fn put_root<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) {
        self.batch
            .put_root(make_prefixed_key(self.prefix.clone(), key), value.to_vec())
            .unwrap_add_cost(&mut self.acc_cost);
    }

    fn delete<K: AsRef<[u8]>>(&mut self, key: K) {
        self.batch
            .delete(make_prefixed_key(self.prefix.clone(), key))
            .unwrap_add_cost(&mut self.acc_cost);
    }

    fn delete_aux<K: AsRef<[u8]>>(&mut self, key: K) {
        self.batch
            .delete_aux(make_prefixed_key(self.prefix.clone(), key))
            .unwrap_add_cost(&mut self.acc_cost);
    }

    fn delete_root<K: AsRef<[u8]>>(&mut self, key: K) {
        self.batch
            .delete_root(make_prefixed_key(self.prefix.clone(), key))
            .unwrap_add_cost(&mut self.acc_cost);
    }
}
