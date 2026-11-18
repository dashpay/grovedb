//! A seam for failing a storage read, the way a storage fault would,
//! without touching the database underneath.
//!
//! In test builds `open_merk` reads a subtree's parent element through
//! [`FaultableReads`], and a test can open a Merk over one to fail the read
//! of one of its nodes.

use std::cell::RefCell;

use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_merk::{
    store::{NodeChange, NodeStore},
    tree::{kv::ValueDefinedCostType, TreeNode, TreeNodeInner},
    Error, Link,
};
use grovedb_storage::Error as StorageError;
use grovedb_version::version::GroveVersion;

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

/// A subtree handle that is the one it wraps, except that a node read of an
/// armed key fails.
pub(crate) struct FaultableReads<S>(pub(crate) S);

/// The failed read of `key`, if one is armed: the seek is paid for, as on
/// the real handle, and nothing is loaded.
fn injected_failure<T>(key: &[u8]) -> Option<CostResult<T, Error>> {
    ARMED
        .with(|armed| {
            armed
                .borrow_mut()
                .as_mut()
                .is_some_and(|fault| fault.fails(key))
        })
        .then(|| {
            Err(Error::StorageError(StorageError::StorageError(
                INJECTED_READ_FAILURE.to_owned(),
            )))
            .wrap_with_cost(OperationCost::with_seek_count(1))
        })
}

impl<'db, S: NodeStore<'db>> NodeStore<'db> for FaultableReads<S> {
    type ChangeSet = S::ChangeSet;
    type Cursor = S::Cursor;

    fn get_node(
        &self,
        key: &[u8],
        value_defined_cost_fn: Option<
            impl Fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>,
        >,
        grove_version: &GroveVersion,
    ) -> CostResult<Option<TreeNode>, Error> {
        injected_failure(key)
            .unwrap_or_else(|| self.0.get_node(key, value_defined_cost_fn, grove_version))
    }

    fn get_node_for_read(&self, key: &[u8]) -> CostResult<Option<TreeNodeInner>, Error> {
        injected_failure(key).unwrap_or_else(|| self.0.get_node_for_read(key))
    }

    fn has_node(&self, key: &[u8]) -> CostResult<bool, Error> {
        injected_failure(key).unwrap_or_else(|| self.0.has_node(key))
    }

    fn fetch_node(
        &self,
        link: &Link,
        value_defined_cost_fn: Option<
            &impl Fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>,
        >,
        grove_version: &GroveVersion,
    ) -> CostResult<TreeNode, Error> {
        injected_failure(link.key()).unwrap_or_else(|| {
            self.0
                .fetch_node(link, value_defined_cost_fn, grove_version)
        })
    }

    fn write_node(&self, node: &TreeNode, change: NodeChange) -> CostResult<(), Error> {
        self.0.write_node(node, change)
    }

    fn root_key(&self) -> CostResult<Option<Vec<u8>>, Error> {
        self.0.root_key()
    }

    fn set_root_key(&self, root_key: Option<&[u8]>) -> CostResult<(), Error> {
        self.0.set_root_key(root_key)
    }

    fn cursor(&self) -> Self::Cursor {
        self.0.cursor()
    }

    fn change_set(&self) -> Self::ChangeSet {
        self.0.change_set()
    }

    fn commit(&self, change_set: Self::ChangeSet) -> CostResult<(), Error> {
        self.0.commit(change_set)
    }

    fn get_aux_record(&self, key: &[u8]) -> CostResult<Option<Vec<u8>>, Error> {
        self.0.get_aux_record(key)
    }

    fn get_meta_record(&self, key: &[u8]) -> CostResult<Option<Vec<u8>>, Error> {
        self.0.get_meta_record(key)
    }

    fn put_meta_record(&self, key: &[u8], value: &[u8]) -> CostResult<(), Error> {
        self.0.put_meta_record(key, value)
    }

    fn delete_meta_record(&self, key: &[u8]) -> CostResult<(), Error> {
        self.0.delete_meta_record(key)
    }
}
