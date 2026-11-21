//! Reads that the storage refuses inside a scan.
//!
//! GroveDB walks the records of a subtree with a cursor: the range of a
//! query, the walk that adds sum items, the walk of elements that finds
//! the subtrees of a tree, the scan before a clear, the scans of a
//! verification and those of an indexed tree's secondary. A move of a
//! cursor returns no error. A cursor whose storage did not make the move
//! stands on no record, as a cursor at the end of its records does, and
//! says which of the two it is by its status. So each of these scans asks
//! the status before it takes its end for the end of the records.
//!
//! Each history here runs an operation once for every move of a cursor
//! that the operation makes, with that one move refused
//! ([`with_one_refused_move`]), and once with none refused. Then it does
//! the same for every record the operation asks a cursor for, with that
//! record withheld ([`with_one_unread_record`]): a cursor that stands on a
//! key and could not read the record under it. And for every time the
//! operation points a cursor at another subtree, with the cursor not
//! pointed there ([`with_one_refused_retarget`]), which is what a page
//! that is read by count does between the element of its tree and the
//! nodes it counts. With any of these faults the operation has to fail,
//! and where a cursor is all it reads with, by the storage's own error.
//!
//! An operation that returns no `OperationCost` (a clear, a verification)
//! has none in its history: the histories of the two engines are then
//! compared by its answers alone.

use std::{fmt::Debug, path::Path};

use grovedb_costs::{CostResult, OperationCost};
use grovedb_merk::proofs::query::AggregateSumQuery;
use grovedb_path::SubtreePath;
use grovedb_version::version::GroveVersion;

use super::faultable_reads::{
    with_one_refused_move, with_one_refused_retarget, with_one_unread_record, RefusedReads,
    INJECTED_READ_FAILURE,
};
use crate::{
    operations::delete::ClearOptions, query_result_type::QueryResultType, AggregateSumPathQuery,
    Element, Engine, Error, GroveDb, PathQuery, Query, QueryItem, SizedQuery,
};

/// One call of a history: its name, what it was charged where the call
/// returns that, and its answer as text.
pub(crate) type Answer = (String, Option<OperationCost>, String);

/// What a call returned: what it was charged, where it says, and its
/// answer.
type Charged<T> = (Option<OperationCost>, Result<T, Error>);

fn charged<T>(answer: CostResult<T, Error>) -> Charged<T> {
    (Some(answer.cost), answer.value)
}

/// The answer of a call that returns no cost.
fn uncharged<T>(answer: Result<T, Error>) -> Charged<T> {
    (None, answer)
}

/// The faults of a cursor, in the order a sweep injects them.
const FAULTS: [&str; 3] = [
    "the move refused",
    "its record withheld",
    "not pointed elsewhere",
];
const MOVES: usize = 0;
const RECORDS: usize = 1;
const RETARGETS: usize = 2;

const ROWS: &[u8] = b"rows";
const SUMS: &[u8] = b"sums";

fn key(n: u8) -> Vec<u8> {
    format!("key {n:02}").into_bytes()
}

fn insert<E: Engine>(
    db: &GroveDb<E>,
    at: &[&[u8]],
    key: &[u8],
    element: Element,
    version: &GroveVersion,
) {
    db.insert(at, key, element, None, None, version)
        .unwrap()
        .expect("an insert");
}

/// What an operation answered with one fault of a cursor injected, for
/// each read of a cursor it makes in turn and for each kind of fault, and
/// what it answers with none.
struct Swept<T> {
    refused: Vec<Result<T, Error>>,
    /// How many faults of each kind of [`FAULTS`] the operation came to.
    fired: [usize; 3],
    whole: Result<T, Error>,
}

/// Runs `operation` with its first move of a cursor refused, then with its
/// second, and so on until it makes no move more than were let through.
/// Then the same with each record withheld that it asks a cursor for, and
/// with each cursor not pointed elsewhere that it points elsewhere.
fn sweep<T: Debug>(
    log: &mut Vec<Answer>,
    name: &str,
    mut operation: impl FnMut() -> Charged<T>,
) -> Swept<T> {
    let mut refused = Vec::new();
    let mut fired = [0; 3];
    let mut whole = None;
    for (kind, fault) in FAULTS.into_iter().enumerate() {
        for passes in 0.. {
            assert!(
                passes < 10_000,
                "{name}: the operation makes no end of reads"
            );
            let ((cost, answer), came_to_it) = match kind {
                MOVES => with_one_refused_move(passes, &mut operation),
                RECORDS => with_one_unread_record(passes, &mut operation),
                _ => with_one_refused_retarget(passes, &mut operation),
            };
            let fault = match came_to_it {
                true => fault,
                false => "no fault",
            };
            let name = format!(
                "{name}: {} {passes}, {fault}",
                ["move", "record", "pointing"][kind]
            );
            log.push((name, cost, format!("{answer:?}")));
            if !came_to_it {
                whole = Some(answer);
                break;
            }
            fired[kind] += 1;
            refused.push(answer);
        }
    }
    Swept {
        refused,
        fired,
        whole: whole.expect("a run with no fault"),
    }
}

/// Whether `error` is the storage's error of the injected fault itself,
/// and no other error that carries its words.
fn is_the_refusal(error: &Error) -> bool {
    use grovedb_storage::Error as Storage;

    let of_the_storage = match error {
        Error::StorageError(error) | Error::MerkError(grovedb_merk::Error::StorageError(error)) => {
            error
        }
        _ => return false,
    };
    matches!(of_the_storage, Storage::StorageError(words) if words == INJECTED_READ_FAILURE)
}

impl<T: Debug> Swept<T> {
    /// The operation moved a cursor and asked it for a record, and no
    /// fault was taken for the end of the records: with any read of a
    /// cursor failed the operation failed, and with at least one of them
    /// by the refusal itself.
    fn no_refusal_is_an_answer(&self, name: &str) {
        assert!(
            self.fired[MOVES] > 0 && self.fired[RECORDS] > 0,
            "{name}: the operation moved no cursor, or read no record: {:?}",
            self.fired
        );
        for (at, answer) in self.refused.iter().enumerate() {
            assert!(
                answer.is_err(),
                "{name}: read {at} of a cursor failed, and the operation answered {answer:?}"
            );
        }
        let by_the_refusal =
            |answer: &&Result<T, Error>| answer.as_ref().is_err_and(is_the_refusal);
        assert!(
            self.refused.iter().any(|answer| by_the_refusal(&answer)),
            "{name}: no refusal came back as the storage's error: {:?}",
            self.refused
        );
    }

    /// As [`Self::no_refusal_is_an_answer`], for an operation that reads
    /// through a cursor nothing but what it answers from: each fault
    /// comes back as the storage's error, and with none the operation
    /// answers.
    fn each_refusal_is_the_error(&self, name: &str) {
        self.no_refusal_is_an_answer(name);
        assert!(self.whole.is_ok(), "{name}: {:?}", self.whole);
        for (at, answer) in self.refused.iter().enumerate() {
            assert!(
                answer.as_ref().is_err_and(is_the_refusal),
                "{name}: read {at} of a cursor failed, and the operation answered {answer:?}"
            );
        }
    }

    /// The operation pointed a cursor at another subtree `times` times.
    fn pointed_a_cursor_elsewhere(&self, name: &str, times: usize) {
        assert_eq!(self.fired[RETARGETS], times, "{name}: {:?}", self.fired);
    }
}

/// The scans of GroveDB over trees of items, with one read of a cursor
/// failed at a time: the range of a query in both directions, the walk
/// that adds sum items, the walk that finds the subtrees of a tree, the
/// walk of state sync over the nodes it restored, the scan before a
/// clear, and the scan of a verification.
pub(crate) fn refused_scans<E: Engine>(dir: &Path, version: &'static GroveVersion) -> Vec<Answer> {
    let db = GroveDb::<RefusedReads<E>>::open_with_engine(dir).expect("a database");
    let mut log = Vec::new();
    insert(&db, &[], ROWS, Element::empty_tree(), version);
    insert(&db, &[], SUMS, Element::empty_sum_tree(), version);
    for n in 0..6 {
        insert(
            &db,
            &[ROWS],
            &key(n),
            Element::new_item(vec![n; 4]),
            version,
        );
        let sum = Element::new_sum_item(i64::from(n) + 1);
        insert(&db, &[SUMS], &key(n), sum, version);
    }
    // A tree after the rows, under the last key of their Merk, with a row
    // of its own.
    insert(&db, &[ROWS], b"tree", Element::empty_tree(), version);
    let item = Element::new_item(vec![9]);
    insert(&db, &[ROWS, b"tree"], &key(0), item, version);

    for left_to_right in [true, false] {
        let range =
            Query::new_single_query_item_with_direction(QueryItem::RangeFull(..), left_to_right);
        let query = PathQuery::new(vec![ROWS.to_vec()], SizedQuery::new(range, None, None));
        let name = format!("a range, left to right: {left_to_right}");
        let swept = sweep(&mut log, &name, || {
            charged(db.query_raw(
                &query,
                true,
                true,
                true,
                QueryResultType::QueryKeyElementPairResultType,
                None,
                version,
            ))
        });
        swept.each_refusal_is_the_error(&name);
        let (rows, _) = swept.whole.expect("the rows");
        assert_eq!(rows.len(), 7, "{name}");

        let walk = AggregateSumPathQuery {
            path: vec![SUMS.to_vec()],
            aggregate_sum_query: AggregateSumQuery {
                items: vec![QueryItem::RangeFull(..)],
                left_to_right,
                sum_limit: 1_000,
                limit_of_items_to_check: None,
            },
        };
        let name = format!("a walk of sums, left to right: {left_to_right}");
        let swept = sweep(&mut log, &name, || {
            charged(db.query_aggregate_sums(&walk, true, true, None, version))
        });
        swept.each_refusal_is_the_error(&name);
        assert_eq!(swept.whole.expect("the sums").results.len(), 6, "{name}");
    }

    let rows: [&[u8]; 1] = [ROWS];
    let at = SubtreePath::from(rows.as_ref());
    let name = "the subtrees of a tree";
    let swept = sweep(&mut log, name, || {
        charged(db.find_subtrees(&at, None, version))
    });
    swept.each_refusal_is_the_error(name);
    assert_eq!(swept.whole.expect("the subtrees").len(), 2, "{name}");

    // The walk over the nodes of every Merk that state sync makes before
    // it commits what it restored, here over a grove that was written.
    let name = "a verification of what was restored";
    let transaction = db.start_transaction();
    let swept = sweep(&mut log, name, || {
        uncharged(db.verify_restored_value_hashes(&transaction, version))
    });
    swept.each_refusal_is_the_error(name);
    drop(transaction);

    // A clear that may not take a tree with a tree in it. The scan that
    // looks for one comes to it at its last record: a scan that took a
    // refused move for its end would find none, and clear the tree.
    let root = db.root_hash(None, version).unwrap().expect("a root hash");
    let options = ClearOptions {
        check_for_subtrees: true,
        allow_deleting_subtrees: false,
        trying_to_clear_with_subtrees_returns_error: true,
        ..ClearOptions::default()
    };
    let name = "a clear of a tree that holds a tree";
    let swept = sweep(&mut log, name, || {
        uncharged(db.clear_subtree(rows.as_ref(), Some(options.clone()), None, version))
    });
    swept.no_refusal_is_an_answer(name);
    assert!(
        matches!(
            swept.whole,
            Err(Error::ClearingTreeWithSubtreesNotAllowed(_))
        ),
        "{name}: {:?}",
        swept.whole
    );
    let root_after = db.root_hash(None, version).unwrap().expect("a root hash");
    assert_eq!(root_after, root, "{name}: nothing was cleared");

    let name = "a verification";
    let swept = sweep(&mut log, name, || {
        uncharged(db.verify_grovedb(None, true, false, version))
    });
    swept.no_refusal_is_an_answer(name);
    let issues = swept.whole.expect("a verification that ran");
    assert!(issues.is_empty(), "{name}: {issues:?}");
    log
}

/// The reads of an indexed tree with one read of a cursor failed at a
/// time: a range of its secondary, the first rows of it, a page of it
/// from a place the read counts to, the same of a tree with no child, and
/// the two scans of a verification that compare the secondary with the
/// primary.
pub(crate) fn refused_scans_of_an_indexed_tree<E: Engine>(
    dir: &Path,
    version: &'static GroveVersion,
) -> Vec<Answer> {
    const INDEXED: &[u8] = b"indexed";
    const EMPTY: &[u8] = b"empty";

    let db = GroveDb::<RefusedReads<E>>::open_with_engine(dir).expect("a database");
    let mut log = Vec::new();
    for tree in [INDEXED, EMPTY] {
        let indexed = Element::empty_provable_count_indexed_tree();
        insert(&db, &[], tree, indexed, version);
    }
    let indexed: [&[u8]; 1] = [INDEXED];
    for (n, count) in [(0u8, 3u8), (1, 1), (2, 5), (3, 2)] {
        db.insert_into_count_indexed_tree(
            indexed.as_ref(),
            &key(n),
            Element::empty_count_tree(),
            None,
            version,
        )
        .unwrap()
        .expect("a tree that is counted");
        for row in 0..count {
            let item = Element::new_item(vec![row]);
            insert(&db, &[INDEXED, &key(n)], &[row], item, version);
        }
    }

    for descending in [false, true] {
        let name = format!("a range of the secondary, descending: {descending}");
        let swept = sweep(&mut log, &name, || {
            charged(db.indexed_count_range(
                indexed.as_ref(),
                0,
                u64::MAX,
                descending,
                10,
                None,
                version,
            ))
        });
        swept.each_refusal_is_the_error(&name);
        assert_eq!(swept.whole.expect("the entries").len(), 4, "{name}");

        let name = format!("the first rows of the secondary, descending: {descending}");
        let swept = sweep(&mut log, &name, || {
            charged(db.indexed_count_top_k_paginated(
                indexed.as_ref(),
                3,
                0,
                descending,
                None,
                version,
            ))
        });
        swept.each_refusal_is_the_error(&name);
        assert_eq!(swept.whole.expect("a page").entries.len(), 3, "{name}");

        // A page from a place the read counts to: one cursor reads the
        // element of the tree, is pointed at the secondary, and reads the
        // nodes it counts by their keys.
        let name = format!("a page of the secondary from its second row, descending: {descending}");
        let swept = sweep(&mut log, &name, || {
            charged(db.indexed_count_top_k_paginated(
                indexed.as_ref(),
                2,
                1,
                descending,
                None,
                version,
            ))
        });
        swept.each_refusal_is_the_error(&name);
        swept.pointed_a_cursor_elsewhere(&name, 1);
        let page = swept.whole.expect("a page");
        assert_eq!((page.entries.len(), page.skipped), (2, 1), "{name}");

        // The same of a tree with no child, whose secondary has no node
        // to read: the cursor is pointed there all the same, and a cursor
        // that was not is no empty secondary.
        let name = format!("a page of an empty secondary, descending: {descending}");
        let swept = sweep(&mut log, &name, || {
            charged(db.indexed_count_top_k_paginated(
                [EMPTY].as_ref(),
                2,
                1,
                descending,
                None,
                version,
            ))
        });
        swept.each_refusal_is_the_error(&name);
        swept.pointed_a_cursor_elsewhere(&name, 1);
        let page = swept.whole.expect("a page");
        assert_eq!((page.entries.len(), page.skipped), (0, 0), "{name}");
    }

    let name = "a verification of an indexed tree";
    let swept = sweep(&mut log, name, || {
        uncharged(db.verify_grovedb(None, true, false, version))
    });
    swept.no_refusal_is_an_answer(name);
    let issues = swept.whole.expect("a verification that ran");
    assert!(issues.is_empty(), "{name}: {issues:?}");
    log
}

/// A page that a query reads by count in a tree that commits to its
/// count (a local offset, from grove version 5 on), with one read of a
/// cursor failed at a time: one cursor reads the element of the tree in
/// the Merk above, is pointed at the tree, and reads the nodes it counts
/// by their keys. Of a tree with rows in both directions, of a tree with
/// none, and of each tree under a key of the tree above.
pub(crate) fn refused_reads_of_a_page_by_count<E: Engine>(
    dir: &Path,
    version: &'static GroveVersion,
) -> Vec<Answer> {
    const COUNTED: &[u8] = b"counted";
    const NONE: &[u8] = b"none";

    let db = GroveDb::<RefusedReads<E>>::open_with_engine(dir).expect("a database");
    let mut log = Vec::new();
    insert(&db, &[], ROWS, Element::empty_tree(), version);
    for tree in [COUNTED, NONE] {
        let counted = Element::empty_provable_count_tree();
        insert(&db, &[ROWS], tree, counted, version);
    }
    for n in 0..6 {
        let item = Element::new_item(vec![n; 4]);
        insert(&db, &[ROWS, COUNTED], &key(n), item, version);
    }
    let read = |query: &PathQuery| {
        charged(db.query_raw(
            query,
            true,
            true,
            true,
            QueryResultType::QueryKeyElementPairResultType,
            None,
            version,
        ))
    };
    let page = |left_to_right| {
        let mut page =
            Query::new_single_query_item_with_direction(QueryItem::RangeFull(..), left_to_right);
        page.offset = Some(2);
        page.limit = Some(3);
        page
    };
    let keys_of = |rows: crate::query_result_type::QueryResultElements| -> Vec<Vec<u8>> {
        let pairs = rows.to_key_elements();
        pairs.into_iter().map(|(key, _)| key).collect()
    };
    for left_to_right in [true, false] {
        let at = |tree: &[u8]| vec![ROWS.to_vec(), tree.to_vec()];
        let query = PathQuery::new(
            at(COUNTED),
            SizedQuery::new(page(left_to_right), None, None),
        );
        let name = format!("a page by count, left to right: {left_to_right}");
        let swept = sweep(&mut log, &name, || read(&query));
        swept.each_refusal_is_the_error(&name);
        // At each of the two Merks along the path of the query, which
        // the read walks first, and at the tree of the page.
        swept.pointed_a_cursor_elsewhere(&name, 3);
        let expected = match left_to_right {
            true => vec![key(2), key(3), key(4)],
            false => vec![key(3), key(2), key(1)],
        };
        assert_eq!(keys_of(swept.whole.expect("a page").0), expected, "{name}");

        let query = PathQuery::new(at(NONE), SizedQuery::new(page(left_to_right), None, None));
        let name = format!("a page by count of an empty tree, left to right: {left_to_right}");
        let swept = sweep(&mut log, &name, || read(&query));
        swept.each_refusal_is_the_error(&name);
        swept.pointed_a_cursor_elsewhere(&name, 3);
        assert!(swept.whole.expect("a page").0.is_empty(), "{name}");

        let mut each = Query::new_single_query_item(QueryItem::RangeFull(..));
        each.set_subquery(page(left_to_right));
        let query = PathQuery::new(vec![ROWS.to_vec()], SizedQuery::new(each, None, None));
        let name = format!("a page by count of each tree, left to right: {left_to_right}");
        let swept = sweep(&mut log, &name, || read(&query));
        swept.no_refusal_is_an_answer(&name);
        swept.pointed_a_cursor_elsewhere(&name, 3);
        assert_eq!(keys_of(swept.whole.expect("a page").0), expected, "{name}");
    }
    log
}

#[cfg(grovedb_test_engine)]
mod on_the_engine_of_the_suite {
    use grovedb_version::version::GROVE_VERSIONS;

    use super::*;
    use crate::tests::SuiteEngine;

    #[test]
    fn a_move_refused_inside_a_scan_fails_the_operation_on_every_grove_version() {
        for version in GROVE_VERSIONS {
            let dir = tempfile::tempdir().expect("a directory");
            let log = refused_scans::<SuiteEngine>(dir.path(), version);
            assert!(log.len() > 60, "{} calls", log.len());
        }
    }

    #[test]
    fn a_move_refused_inside_a_scan_of_an_indexed_tree_fails_the_operation() {
        let dir = tempfile::tempdir().expect("a directory");
        let log =
            refused_scans_of_an_indexed_tree::<SuiteEngine>(dir.path(), GroveVersion::latest());
        assert!(log.len() > 40, "{} calls", log.len());
    }

    #[test]
    fn a_read_refused_inside_a_page_by_count_fails_the_operation() {
        let dir = tempfile::tempdir().expect("a directory");
        let log =
            refused_reads_of_a_page_by_count::<SuiteEngine>(dir.path(), GroveVersion::latest());
        assert!(log.len() > 40, "{} calls", log.len());
    }
}
