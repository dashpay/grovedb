//! A seam for failing a storage read, the way a storage fault would,
//! without touching the database underneath.
//!
//! In test builds `open_merk` reads a subtree's parent element through
//! [`FaultableReads`], and a test can open a Merk over one to fail the read
//! of one of its nodes.

use std::cell::RefCell;

use grovedb_costs::{
    storage_cost::key_value_cost::KeyValueStorageCost, ChildrenSizesWithIsSumTree, CostResult,
    CostsExt, OperationCost,
};
use grovedb_storage::{Error, StorageContext};

/// The message of the injected failure.
pub(crate) const INJECTED_READ_FAILURE: &str = "injected read failure";

/// Which reads of a key fail.
struct ArmedFault {
    key: Vec<u8>,
    /// Reads of `key` let through before the first failure.
    passes_left: usize,
    /// Failures still to inject; `None` fails every read from then on.
    failures_left: Option<usize>,
    /// Whether a read has failed.
    fired: bool,
}

impl ArmedFault {
    fn fails(&mut self, key: &[u8]) -> bool {
        if key != self.key {
            return false;
        }
        if self.passes_left > 0 {
            self.passes_left -= 1;
            return false;
        }
        match &mut self.failures_left {
            Some(0) => return false,
            Some(left) => *left -= 1,
            None => {}
        }
        self.fired = true;
        true
    }
}

thread_local! {
    static ARMED: RefCell<Option<ArmedFault>> = const { RefCell::new(None) };
}

/// Runs `body` with `fault` armed and returns whether a read failed. The
/// fault armed before is put back afterwards, even when `body` panics, so a
/// scope nested in another leaves the outer one armed.
fn with_fault<T>(fault: ArmedFault, body: impl FnOnce() -> T) -> (T, bool) {
    struct Disarm(Option<ArmedFault>);

    impl Drop for Disarm {
        fn drop(&mut self) {
            ARMED.with(|armed| armed.replace(self.0.take()));
        }
    }

    let outer = Disarm(ARMED.with(|armed| armed.replace(Some(fault))));
    let result = body();
    let fired = ARMED.with(|armed| armed.borrow().as_ref().is_some_and(|fault| fault.fired));
    drop(outer);
    (result, fired)
}

/// Runs `body` with every read of `key` through a [`FaultableReads`] context
/// failing with a storage error. For `open_merk` that is the read of a
/// parent element stored under `key`.
pub(crate) fn with_failed_parent_read<T>(key: &[u8], body: impl FnOnce() -> T) -> T {
    with_fault(
        ArmedFault {
            key: key.to_vec(),
            passes_left: 0,
            failures_left: None,
            fired: false,
        },
        body,
    )
    .0
}

/// Runs `body` with one read of `key` failing: the one after `passes` reads
/// of it went through. Reads after the failed one succeed again. Also
/// returns whether `body` got as far as the failing read.
pub(crate) fn with_one_failed_parent_read<T>(
    key: &[u8],
    passes: usize,
    body: impl FnOnce() -> T,
) -> (T, bool) {
    with_fault(
        ArmedFault {
            key: key.to_vec(),
            passes_left: passes,
            failures_left: Some(1),
            fired: false,
        },
        body,
    )
}

/// A storage context that is the one it wraps, except that a data read of
/// an armed key fails.
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
        let fails = ARMED.with(|armed| {
            armed
                .borrow_mut()
                .as_mut()
                .is_some_and(|fault| fault.fails(key.as_ref()))
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
