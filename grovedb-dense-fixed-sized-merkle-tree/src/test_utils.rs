//! Test utilities: in-memory `FlatStore` implementations.

use std::{cell::RefCell, collections::HashMap};

use grovedb_costs::{
    storage_cost::key_value_cost::KeyValueStorageCost, CostContext, CostResult, CostsExt,
    OperationCost,
};
use grovedb_storage::{FlatStore, RawIterator};

/// In-memory storage context for testing.
///
/// Immediate reads and writes backed by a `HashMap`. Only `get` and `put`
/// have real implementations; the other `FlatStore` methods panic if
/// called.
#[derive(Default)]
pub(crate) struct MemStorageContext {
    pub data: RefCell<HashMap<Vec<u8>, Vec<u8>>>,
    /// Every data `put` in order, with the cost information it carried.
    pub puts: RefCell<Vec<(Vec<u8>, Option<KeyValueStorageCost>)>>,
    /// Every data `get` in order (the keys asked for) — what the tree read
    /// back out of storage, as opposed to served from its caches.
    pub gets: RefCell<Vec<Vec<u8>>>,
    /// When set, every `put` of a hash record (3-byte key) fails, leaving
    /// slot puts working: exercises the insert's rollback after a partial
    /// ancestor-path rewrite.
    pub fail_record_puts: std::cell::Cell<bool>,
    /// When set, every `get` of a hash record (3-byte key) fails, leaving
    /// value reads working: exercises the record-read fault arms.
    pub fail_record_gets: std::cell::Cell<bool>,
}

impl MemStorageContext {
    pub fn new() -> Self {
        Self::default()
    }
}

impl FlatStore for MemStorageContext {
    type Cursor<'a>
        = MemRawIterator
    where
        Self: 'a;

    /// Charged like a real storage read — one seek, the bytes loaded — so
    /// tests can compare a cold read against the tree's deterministic
    /// cache-hit charge.
    fn get<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, grovedb_storage::Error> {
        self.gets.borrow_mut().push(key.as_ref().to_vec());
        if self.fail_record_gets.get() && key.as_ref().len() == 3 {
            // The lookup was attempted: charge its seek like a real store.
            return Err(grovedb_storage::Error::StorageError(
                "injected hash record read failure".to_string(),
            ))
            .wrap_with_cost(OperationCost {
                seek_count: 1,
                ..Default::default()
            });
        }
        let value = self.data.borrow().get(key.as_ref()).cloned();
        let cost = OperationCost {
            seek_count: 1,
            storage_loaded_bytes: value.as_ref().map_or(0, |v| v.len() as u64),
            ..Default::default()
        };
        Ok(value).wrap_with_cost(cost)
    }

    fn put<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), grovedb_storage::Error> {
        if self.fail_record_puts.get() && key.as_ref().len() == 3 {
            return Err(grovedb_storage::Error::StorageError(
                "injected hash record write failure".to_string(),
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

    fn delete<K: AsRef<[u8]>>(
        &self,
        _key: K,
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), grovedb_storage::Error> {
        unimplemented!("MemStorageContext::delete")
    }

    fn cursor(&self) -> Self::Cursor<'_> {
        unimplemented!("MemStorageContext::cursor")
    }
}

/// Storage context that can be configured to fail on specific positions.
///
/// Uses `RefCell` for fail conditions so they can be set after the storage
/// is moved into a tree.
#[derive(Default)]
pub(crate) struct FailingStorageContext {
    pub data: RefCell<HashMap<Vec<u8>, Vec<u8>>>,
    pub fail_on_get_key: RefCell<Option<[u8; 2]>>,
    pub fail_on_put_key: RefCell<Option<[u8; 2]>>,
}

impl FailingStorageContext {
    pub fn new() -> Self {
        Self::default()
    }
}

impl FlatStore for FailingStorageContext {
    type Cursor<'a>
        = MemRawIterator
    where
        Self: 'a;

    fn get<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, grovedb_storage::Error> {
        let key_bytes = key.as_ref();
        if let Some(fail_key) = *self.fail_on_get_key.borrow()
            && key_bytes == fail_key
        {
            return Err(grovedb_storage::Error::StorageError(
                "simulated get failure".to_string(),
            ))
            .wrap_with_cost(OperationCost::default());
        }
        Ok(self.data.borrow().get(key_bytes).cloned()).wrap_with_cost(OperationCost::default())
    }

    fn put<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), grovedb_storage::Error> {
        let key_bytes = key.as_ref();
        if let Some(fail_key) = *self.fail_on_put_key.borrow()
            && key_bytes == fail_key
        {
            return Err(grovedb_storage::Error::StorageError(
                "simulated put failure".to_string(),
            ))
            .wrap_with_cost(OperationCost::default());
        }
        self.data
            .borrow_mut()
            .insert(key_bytes.to_vec(), value.to_vec());
        Ok(()).wrap_with_cost(OperationCost::default())
    }

    fn delete<K: AsRef<[u8]>>(
        &self,
        _key: K,
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), grovedb_storage::Error> {
        unimplemented!()
    }

    fn cursor(&self) -> Self::Cursor<'_> {
        unimplemented!()
    }
}

// ── RawIterator stub ──────────────────────────────────────────────────

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
