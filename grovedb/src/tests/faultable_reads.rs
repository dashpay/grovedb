//! A seam for failing the read of a subtree's parent element, the way a
//! storage fault would, without touching the database underneath.

use std::cell::RefCell;

use grovedb_costs::{
    storage_cost::key_value_cost::KeyValueStorageCost, ChildrenSizesWithIsSumTree, CostResult,
    CostsExt, OperationCost,
};
use grovedb_storage::{Error, StorageContext};

/// The message of the injected failure.
pub(crate) const INJECTED_READ_FAILURE: &str = "injected read failure";

thread_local! {
    /// The parent key whose element read fails while set.
    static FAILED_PARENT_READ: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
}

/// Runs `body` with every `open_merk` read of a parent element stored under
/// `parent_key` failing with a storage error.
pub(crate) fn with_failed_parent_read<T>(parent_key: &[u8], body: impl FnOnce() -> T) -> T {
    /// Puts back the key that was armed before, even when `body` panics, so
    /// a scope nested in another leaves the outer one armed.
    struct Disarm(Option<Vec<u8>>);

    impl Drop for Disarm {
        fn drop(&mut self) {
            FAILED_PARENT_READ.with(|failed| failed.replace(self.0.take()));
        }
    }

    let _disarm =
        Disarm(FAILED_PARENT_READ.with(|failed| failed.replace(Some(parent_key.to_vec()))));
    body()
}

/// The parent storage context `open_merk` reads through in tests: the real
/// context, except that a data read of the key armed by
/// [`with_failed_parent_read`] fails.
pub(crate) struct FaultableReads<S>(pub(crate) S);

impl<'db, S: StorageContext<'db>> StorageContext<'db> for FaultableReads<S> {
    type Batch = S::Batch;
    type RawIterator = S::RawIterator;

    fn put<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        children_sizes: ChildrenSizesWithIsSumTree,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        self.0.put(key, value, children_sizes, cost_info)
    }

    fn put_aux<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        self.0.put_aux(key, value, cost_info)
    }

    fn put_root<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        self.0.put_root(key, value, cost_info)
    }

    fn put_meta<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        self.0.put_meta(key, value, cost_info)
    }

    fn delete<K: AsRef<[u8]>>(
        &self,
        key: K,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        self.0.delete(key, cost_info)
    }

    fn delete_aux<K: AsRef<[u8]>>(
        &self,
        key: K,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        self.0.delete_aux(key, cost_info)
    }

    fn delete_root<K: AsRef<[u8]>>(
        &self,
        key: K,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        self.0.delete_root(key, cost_info)
    }

    fn delete_meta<K: AsRef<[u8]>>(
        &self,
        key: K,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        self.0.delete_meta(key, cost_info)
    }

    fn get<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, Error> {
        let fails = FAILED_PARENT_READ.with(|failed| {
            failed
                .borrow()
                .as_deref()
                .is_some_and(|k| k == key.as_ref())
        });
        if fails {
            // The seek is paid for, as on the real context; nothing is loaded.
            return Err(Error::StorageError(INJECTED_READ_FAILURE.to_owned()))
                .wrap_with_cost(OperationCost::with_seek_count(1));
        }
        self.0.get(key)
    }

    fn get_aux<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, Error> {
        self.0.get_aux(key)
    }

    fn get_root<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, Error> {
        self.0.get_root(key)
    }

    fn get_meta<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, Error> {
        self.0.get_meta(key)
    }

    fn new_batch(&self) -> Self::Batch {
        self.0.new_batch()
    }

    fn commit_batch(&self, batch: Self::Batch) -> CostResult<(), Error> {
        self.0.commit_batch(batch)
    }

    fn raw_iter(&self) -> Self::RawIterator {
        self.0.raw_iter()
    }

    fn raw_iter_aux(&self) -> Self::RawIterator {
        self.0.raw_iter_aux()
    }
}
