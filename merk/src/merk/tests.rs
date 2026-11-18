//! Tests of the key-value iterator against a storage iterator that fails.

use grovedb_costs::{CostContext, CostsExt, OperationCost};
use grovedb_storage::{Error as StorageError, RawIterator};

use crate::{merk::KVIterator, proofs::Query};

/// A storage iterator over fixed rows that fails once, on the step that
/// would reach `fails_reaching`. As a RocksDB iterator does, it is invalid
/// from then on and reports the error as its status until the next seek,
/// which clears it.
struct FailingIterator {
    rows: Vec<(Vec<u8>, Vec<u8>)>,
    position: Option<usize>,
    fails_reaching: usize,
    failed: bool,
    has_failed_once: bool,
}

impl FailingIterator {
    fn new(keys: &[&[u8]], fails_reaching: usize) -> Self {
        Self {
            rows: keys.iter().map(|key| (key.to_vec(), vec![0])).collect(),
            position: None,
            fails_reaching,
            failed: false,
            has_failed_once: false,
        }
    }

    fn settle(&mut self, position: Option<usize>) -> CostContext<()> {
        self.failed = false;
        self.position = position;
        ().wrap_with_cost(OperationCost::default())
    }
}

impl RawIterator for FailingIterator {
    fn seek_to_first(&mut self) -> CostContext<()> {
        let first = (!self.rows.is_empty()).then_some(0);
        self.settle(first)
    }

    fn seek_to_last(&mut self) -> CostContext<()> {
        let last = self.rows.len().checked_sub(1);
        self.settle(last)
    }

    fn seek<K: AsRef<[u8]>>(&mut self, key: K) -> CostContext<()> {
        let at = self
            .rows
            .iter()
            .position(|(row_key, _)| row_key.as_slice() >= key.as_ref());
        self.settle(at)
    }

    fn seek_for_prev<K: AsRef<[u8]>>(&mut self, key: K) -> CostContext<()> {
        let at = self
            .rows
            .iter()
            .rposition(|(row_key, _)| row_key.as_slice() <= key.as_ref());
        self.settle(at)
    }

    fn next(&mut self) -> CostContext<()> {
        self.position = match self.position {
            Some(at) if at + 1 == self.fails_reaching && !self.has_failed_once => {
                self.failed = true;
                self.has_failed_once = true;
                None
            }
            Some(at) if at + 1 < self.rows.len() => Some(at + 1),
            _ => None,
        };
        ().wrap_with_cost(OperationCost::default())
    }

    fn prev(&mut self) -> CostContext<()> {
        self.position = self.position.and_then(|at| at.checked_sub(1));
        ().wrap_with_cost(OperationCost::default())
    }

    fn value(&self) -> CostContext<Option<&[u8]>> {
        self.position
            .map(|at| self.rows[at].1.as_slice())
            .wrap_with_cost(OperationCost::default())
    }

    fn key(&self) -> CostContext<Option<&[u8]>> {
        self.position
            .map(|at| self.rows[at].0.as_slice())
            .wrap_with_cost(OperationCost::default())
    }

    fn valid(&self) -> CostContext<bool> {
        self.position
            .is_some()
            .wrap_with_cost(OperationCost::default())
    }

    fn status(&self) -> Result<(), StorageError> {
        if self.failed {
            Err(StorageError::StorageError("a read failed".to_string()))
        } else {
            Ok(())
        }
    }
}

const KEYS: [&[u8]; 5] = [b"a1", b"a2", b"a3", b"x1", b"x2"];

/// Two ranges that do not touch: the scan seeks once for each.
fn two_ranges() -> Query {
    let mut query = Query::new();
    query.insert_range(b"a".to_vec()..b"b".to_vec());
    query.insert_range(b"x".to_vec()..b"y".to_vec());
    query
}

fn scan(iterator: FailingIterator, query: &Query) -> (Vec<Vec<u8>>, Result<(), StorageError>) {
    let mut rows = KVIterator::new(iterator, query).unwrap();
    let mut keys = Vec::new();
    while let Some((key, _)) = rows.next_kv().unwrap() {
        keys.push(key);
    }
    (keys, rows.status())
}

#[test]
fn a_scan_that_meets_no_error_returns_every_row_of_every_item() {
    let query = two_ranges();
    let (keys, status) = scan(FailingIterator::new(&KEYS, usize::MAX), &query);
    assert_eq!(keys, KEYS.map(|key| key.to_vec()));
    assert!(status.is_ok());
}

/// The first range fails on its second row. The seek into the second range
/// would succeed and clear the error, and the scan would then end as a
/// complete one that happens to lack two rows.
#[test]
fn an_error_within_one_query_item_is_still_the_status_when_the_scan_ends() {
    let query = two_ranges();
    let (keys, status) = scan(FailingIterator::new(&KEYS, 1), &query);
    assert_eq!(keys, vec![b"a1".to_vec()]);
    assert!(
        status.is_err(),
        "the scan was cut short and must say so, got {keys:?}"
    );
}
