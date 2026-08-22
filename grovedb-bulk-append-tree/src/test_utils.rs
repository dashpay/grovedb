//! Test utilities: in-memory StorageContext for BulkAppendTree tests.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
};

use grovedb_costs::{
    storage_cost::key_value_cost::KeyValueStorageCost, ChildrenSizesWithIsSumTree, CostContext,
    CostResult, CostsExt, OperationCost,
};
use grovedb_storage::{Batch, RawIterator, StorageContext};

/// In-memory storage context for testing.
///
/// Immediate reads and writes backed by a `HashMap`. Only `get` and `put`
/// (data storage) have real implementations; all other `StorageContext`
/// methods panic if called.
///
/// `fail_get` / `fail_put` inject storage faults. Tree code is full of arms
/// that can only be reached when the backing store errors mid-operation —
/// exactly the paths that decide whether a fault surfaces or is silently
/// swallowed — and without injection those arms are untestable.
#[derive(Default)]
pub struct MemStorageContext {
    pub data: RefCell<HashMap<Vec<u8>, Vec<u8>>>,
    pub fail_get: Cell<bool>,
    pub fail_put: Cell<bool>,
    /// Every data `put` in order, with the cost information it carried —
    /// what a real storage context would hand the commit path to bill.
    pub puts: RefCell<Vec<(Vec<u8>, Option<KeyValueStorageCost>)>>,
    /// Every data `get` in order, by key — what an append actually reads.
    pub gets: RefCell<Vec<Vec<u8>>>,
}

impl MemStorageContext {
    pub fn new() -> Self {
        Self::default()
    }

    /// Make every subsequent `get` return a storage error.
    pub fn fail_reads(&self) {
        self.fail_get.set(true);
    }

    /// Make every subsequent `put` return a storage error.
    pub fn fail_writes(&self) {
        self.fail_put.set(true);
    }

    /// Resume normal operation.
    pub fn heal(&self) {
        self.fail_get.set(false);
        self.fail_put.set(false);
    }
}

impl<'db> StorageContext<'db> for MemStorageContext {
    type Batch = MemBatch;
    type RawIterator = MemRawIterator;

    fn get<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, grovedb_storage::Error> {
        if self.fail_get.get() {
            return Err(grovedb_storage::Error::StorageError(
                "injected read failure".to_string(),
            ))
            .wrap_with_cost(OperationCost::default());
        }
        self.gets.borrow_mut().push(key.as_ref().to_vec());
        Ok(self.data.borrow().get(key.as_ref()).cloned()).wrap_with_cost(OperationCost::default())
    }

    fn put<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        _children_sizes: ChildrenSizesWithIsSumTree,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), grovedb_storage::Error> {
        if self.fail_put.get() {
            return Err(grovedb_storage::Error::StorageError(
                "injected write failure".to_string(),
            ))
            .wrap_with_cost(OperationCost::default());
        }
        self.puts
            .borrow_mut()
            .push((key.as_ref().to_vec(), cost_info));
        self.data
            .borrow_mut()
            .insert(key.as_ref().to_vec(), value.to_vec());
        Ok(()).wrap_with_cost(OperationCost::default())
    }

    fn put_aux<K: AsRef<[u8]>>(
        &self,
        _key: K,
        _value: &[u8],
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), grovedb_storage::Error> {
        unimplemented!("MemStorageContext::put_aux")
    }

    fn put_root<K: AsRef<[u8]>>(
        &self,
        _key: K,
        _value: &[u8],
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), grovedb_storage::Error> {
        unimplemented!("MemStorageContext::put_root")
    }

    fn put_meta<K: AsRef<[u8]>>(
        &self,
        _key: K,
        _value: &[u8],
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), grovedb_storage::Error> {
        unimplemented!("MemStorageContext::put_meta")
    }

    fn delete<K: AsRef<[u8]>>(
        &self,
        _key: K,
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), grovedb_storage::Error> {
        unimplemented!("MemStorageContext::delete")
    }

    fn delete_aux<K: AsRef<[u8]>>(
        &self,
        _key: K,
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), grovedb_storage::Error> {
        unimplemented!("MemStorageContext::delete_aux")
    }

    fn delete_root<K: AsRef<[u8]>>(
        &self,
        _key: K,
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), grovedb_storage::Error> {
        unimplemented!("MemStorageContext::delete_root")
    }

    fn delete_meta<K: AsRef<[u8]>>(
        &self,
        _key: K,
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), grovedb_storage::Error> {
        unimplemented!("MemStorageContext::delete_meta")
    }

    fn get_aux<K: AsRef<[u8]>>(
        &self,
        _key: K,
    ) -> CostResult<Option<Vec<u8>>, grovedb_storage::Error> {
        unimplemented!("MemStorageContext::get_aux")
    }

    fn get_root<K: AsRef<[u8]>>(
        &self,
        _key: K,
    ) -> CostResult<Option<Vec<u8>>, grovedb_storage::Error> {
        unimplemented!("MemStorageContext::get_root")
    }

    fn get_meta<K: AsRef<[u8]>>(
        &self,
        _key: K,
    ) -> CostResult<Option<Vec<u8>>, grovedb_storage::Error> {
        unimplemented!("MemStorageContext::get_meta")
    }

    fn new_batch(&self) -> Self::Batch {
        MemBatch
    }

    fn commit_batch(&self, _batch: Self::Batch) -> CostResult<(), grovedb_storage::Error> {
        Ok(()).wrap_with_cost(OperationCost::default())
    }

    fn raw_iter(&self) -> Self::RawIterator {
        unimplemented!("MemStorageContext::raw_iter")
    }
}

// ── Batch and RawIterator stubs ───────────────────────────────────────

/// No-op batch (never used — MemStorageContext does immediate writes).
pub struct MemBatch;

impl Batch for MemBatch {
    fn put<K: AsRef<[u8]>>(
        &mut self,
        _key: K,
        _value: &[u8],
        _children_sizes: ChildrenSizesWithIsSumTree,
        _cost_info: Option<KeyValueStorageCost>,
    ) -> Result<(), grovedb_costs::error::Error> {
        unimplemented!("MemBatch::put")
    }

    fn put_aux<K: AsRef<[u8]>>(
        &mut self,
        _key: K,
        _value: &[u8],
        _cost_info: Option<KeyValueStorageCost>,
    ) -> Result<(), grovedb_costs::error::Error> {
        unimplemented!("MemBatch::put_aux")
    }

    fn put_root<K: AsRef<[u8]>>(
        &mut self,
        _key: K,
        _value: &[u8],
        _cost_info: Option<KeyValueStorageCost>,
    ) -> Result<(), grovedb_costs::error::Error> {
        unimplemented!("MemBatch::put_root")
    }

    fn delete<K: AsRef<[u8]>>(&mut self, _key: K, _cost_info: Option<KeyValueStorageCost>) {
        unimplemented!("MemBatch::delete")
    }

    fn delete_aux<K: AsRef<[u8]>>(&mut self, _key: K, _cost_info: Option<KeyValueStorageCost>) {
        unimplemented!("MemBatch::delete_aux")
    }

    fn delete_root<K: AsRef<[u8]>>(&mut self, _key: K, _cost_info: Option<KeyValueStorageCost>) {
        unimplemented!("MemBatch::delete_root")
    }
}

/// Stub iterator (never used by the bulk append tree).
pub struct MemRawIterator;

impl RawIterator for MemRawIterator {
    fn seek_to_first(&mut self) -> CostContext<()> {
        unimplemented!()
    }

    fn seek_to_last(&mut self) -> CostContext<()> {
        unimplemented!()
    }

    fn seek<K: AsRef<[u8]>>(&mut self, _key: K) -> CostContext<()> {
        unimplemented!()
    }

    fn seek_for_prev<K: AsRef<[u8]>>(&mut self, _key: K) -> CostContext<()> {
        unimplemented!()
    }

    fn next(&mut self) -> CostContext<()> {
        unimplemented!()
    }

    fn prev(&mut self) -> CostContext<()> {
        unimplemented!()
    }

    fn value(&self) -> CostContext<Option<&[u8]>> {
        unimplemented!()
    }

    fn key(&self) -> CostContext<Option<&[u8]>> {
        unimplemented!()
    }

    fn valid(&self) -> CostContext<bool> {
        unimplemented!()
    }
}
