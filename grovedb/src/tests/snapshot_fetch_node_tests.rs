//! A failed seek on a pinned iterator view is not an absent node.

use grovedb_costs::{CostContext, CostsExt, OperationCost};
use grovedb_storage::RawIterator;
use grovedb_version::version::GroveVersion;

use crate::{operations::indexed_tree::snapshot_fetch_node, Error};

/// Holds no record at any position, as an iterator past its records and one
/// stopped on an error both report. Only `status` tells them apart.
struct NoRecordIterator {
    failure: Option<&'static str>,
}

impl RawIterator for NoRecordIterator {
    fn seek_to_first(&mut self) -> CostContext<()> {
        ().wrap_with_cost(OperationCost::with_seek_count(1))
    }

    fn seek_to_last(&mut self) -> CostContext<()> {
        ().wrap_with_cost(OperationCost::with_seek_count(1))
    }

    fn seek<K: AsRef<[u8]>>(&mut self, _key: K) -> CostContext<()> {
        ().wrap_with_cost(OperationCost::with_seek_count(1))
    }

    fn seek_for_prev<K: AsRef<[u8]>>(&mut self, _key: K) -> CostContext<()> {
        ().wrap_with_cost(OperationCost::with_seek_count(1))
    }

    fn next(&mut self) -> CostContext<()> {
        ().wrap_with_cost(OperationCost::with_seek_count(1))
    }

    fn prev(&mut self) -> CostContext<()> {
        ().wrap_with_cost(OperationCost::with_seek_count(1))
    }

    fn value(&self) -> CostContext<Option<&[u8]>> {
        None.wrap_with_cost(OperationCost::default())
    }

    fn key(&self) -> CostContext<Option<&[u8]>> {
        None.wrap_with_cost(OperationCost::default())
    }

    fn valid(&self) -> CostContext<bool> {
        false.wrap_with_cost(OperationCost::default())
    }

    fn status(&self) -> Result<(), grovedb_storage::Error> {
        match self.failure {
            Some(failure) => Err(grovedb_storage::Error::StorageError(failure.to_owned())),
            None => Ok(()),
        }
    }
}

#[test]
fn should_report_a_failed_seek_rather_than_an_absent_node() {
    let mut failed = NoRecordIterator {
        failure: Some("injected read failure"),
    };
    let fetched = snapshot_fetch_node(&mut failed, b"host", GroveVersion::latest());
    assert_eq!(
        fetched.cost.seek_count, 1,
        "the seek's cost must be retained"
    );
    assert!(matches!(
        fetched.value,
        Err(Error::StorageError(grovedb_storage::Error::StorageError(message)))
            if message == "injected read failure"
    ));
}

#[test]
fn should_report_a_clean_miss_as_an_absent_node() {
    let mut empty = NoRecordIterator { failure: None };
    let fetched = snapshot_fetch_node(&mut empty, b"host", GroveVersion::latest());
    assert_eq!(fetched.cost.seek_count, 1);
    assert!(matches!(fetched.value, Ok(None)));
}
