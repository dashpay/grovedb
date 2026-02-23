//! Test utilities: in-memory StorageContext implementations.

use std::{cell::RefCell, collections::HashMap};

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
pub(crate) struct MemStorageContext {
    pub data: RefCell<HashMap<Vec<u8>, Vec<u8>>>,
}

impl MemStorageContext {
    pub fn new() -> Self {
        Self {
            data: RefCell::new(HashMap::new()),
        }
    }
}

impl<'db> StorageContext<'db> for MemStorageContext {
    type Batch = MemBatch;
    type RawIterator = MemRawIterator;

    fn get<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, grovedb_storage::Error> {
        Ok(self.data.borrow().get(key.as_ref()).cloned()).wrap_with_cost(OperationCost::default())
    }

    fn put<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        _children_sizes: ChildrenSizesWithIsSumTree,
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), grovedb_storage::Error> {
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

/// Storage context that can be configured to fail on specific positions.
///
/// Uses `RefCell` for fail conditions so they can be set after the storage
/// is moved into a tree.
pub(crate) struct FailingStorageContext {
    pub data: RefCell<HashMap<Vec<u8>, Vec<u8>>>,
    pub fail_on_get_key: RefCell<Option<[u8; 2]>>,
    pub fail_on_put_key: RefCell<Option<[u8; 2]>>,
}

impl FailingStorageContext {
    pub fn new() -> Self {
        Self {
            data: RefCell::new(HashMap::new()),
            fail_on_get_key: RefCell::new(None),
            fail_on_put_key: RefCell::new(None),
        }
    }
}

impl<'db> StorageContext<'db> for FailingStorageContext {
    type Batch = MemBatch;
    type RawIterator = MemRawIterator;

    fn get<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, grovedb_storage::Error> {
        let key_bytes = key.as_ref();
        if let Some(fail_key) = *self.fail_on_get_key.borrow() {
            if key_bytes == fail_key {
                return Err(grovedb_storage::Error::StorageError(
                    "simulated get failure".to_string(),
                ))
                .wrap_with_cost(OperationCost::default());
            }
        }
        Ok(self.data.borrow().get(key_bytes).cloned()).wrap_with_cost(OperationCost::default())
    }

    fn put<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        _children_sizes: ChildrenSizesWithIsSumTree,
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), grovedb_storage::Error> {
        let key_bytes = key.as_ref();
        if let Some(fail_key) = *self.fail_on_put_key.borrow() {
            if key_bytes == fail_key {
                return Err(grovedb_storage::Error::StorageError(
                    "simulated put failure".to_string(),
                ))
                .wrap_with_cost(OperationCost::default());
            }
        }
        self.data
            .borrow_mut()
            .insert(key_bytes.to_vec(), value.to_vec());
        Ok(()).wrap_with_cost(OperationCost::default())
    }

    fn put_aux<K: AsRef<[u8]>>(
        &self,
        _key: K,
        _value: &[u8],
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), grovedb_storage::Error> {
        unimplemented!()
    }

    fn put_root<K: AsRef<[u8]>>(
        &self,
        _key: K,
        _value: &[u8],
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), grovedb_storage::Error> {
        unimplemented!()
    }

    fn put_meta<K: AsRef<[u8]>>(
        &self,
        _key: K,
        _value: &[u8],
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), grovedb_storage::Error> {
        unimplemented!()
    }

    fn delete<K: AsRef<[u8]>>(
        &self,
        _key: K,
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), grovedb_storage::Error> {
        unimplemented!()
    }

    fn delete_aux<K: AsRef<[u8]>>(
        &self,
        _key: K,
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), grovedb_storage::Error> {
        unimplemented!()
    }

    fn delete_root<K: AsRef<[u8]>>(
        &self,
        _key: K,
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), grovedb_storage::Error> {
        unimplemented!()
    }

    fn delete_meta<K: AsRef<[u8]>>(
        &self,
        _key: K,
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), grovedb_storage::Error> {
        unimplemented!()
    }

    fn get_aux<K: AsRef<[u8]>>(
        &self,
        _key: K,
    ) -> CostResult<Option<Vec<u8>>, grovedb_storage::Error> {
        unimplemented!()
    }

    fn get_root<K: AsRef<[u8]>>(
        &self,
        _key: K,
    ) -> CostResult<Option<Vec<u8>>, grovedb_storage::Error> {
        unimplemented!()
    }

    fn get_meta<K: AsRef<[u8]>>(
        &self,
        _key: K,
    ) -> CostResult<Option<Vec<u8>>, grovedb_storage::Error> {
        unimplemented!()
    }

    fn new_batch(&self) -> Self::Batch {
        MemBatch
    }

    fn commit_batch(&self, _batch: Self::Batch) -> CostResult<(), grovedb_storage::Error> {
        Ok(()).wrap_with_cost(OperationCost::default())
    }

    fn raw_iter(&self) -> Self::RawIterator {
        unimplemented!()
    }
}

// ── Batch and RawIterator stubs ───────────────────────────────────────

/// No-op batch (never used — MemStorageContext does immediate writes).
pub(crate) struct MemBatch;

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

/// Stub iterator (never used by the dense tree).
pub(crate) struct MemRawIterator;

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
