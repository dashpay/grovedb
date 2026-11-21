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
//! the same with the record withheld that a move comes to
//! ([`with_one_unread_record`]): a cursor that stands on a key and could
//! not read the record under it. With either fault the operation has to
//! fail, and where the scan is all it does with the storage's own error.

use std::{fmt::Debug, path::Path};

use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_merk::proofs::query::AggregateSumQuery;
use grovedb_path::SubtreePath;
use grovedb_version::version::GroveVersion;

use super::faultable_reads::{
    with_one_refused_move, with_one_unread_record, RefusedReads, INJECTED_READ_FAILURE,
};
use crate::{
    operations::delete::ClearOptions, query_result_type::QueryResultType, AggregateSumPathQuery,
    Element, Engine, Error, GroveDb, PathQuery, Query, QueryItem, SizedQuery,
};

/// One call of a history: its name, what it was charged, and its answer
/// as text.
pub(crate) type Answer = (String, OperationCost, String);

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
/// each move it makes in turn and for both kinds of fault, and what it
/// answers with none.
struct Swept<T> {
    refused: Vec<Result<T, Error>>,
    whole: Result<T, Error>,
}

/// Runs `operation` with its first move of a cursor refused, then with its
/// second, and so on until it makes no move more than were let through.
/// Then the same with the record withheld that each move comes to.
fn sweep<T: Debug>(
    log: &mut Vec<Answer>,
    name: &str,
    mut operation: impl FnMut() -> CostResult<T, Error>,
) -> Swept<T> {
    let mut refused = Vec::new();
    let mut whole = None;
    for withholds in [false, true] {
        for passes in 0.. {
            assert!(
                passes < 10_000,
                "{name}: the operation makes no end of moves"
            );
            let (answer, fired) = match withholds {
                false => with_one_refused_move(passes, &mut operation),
                true => with_one_unread_record(passes, &mut operation),
            };
            let (cost, answer) = (answer.cost, answer.value);
            let fault = match (fired, withholds) {
                (false, _) => "no fault",
                (true, false) => "the move refused",
                (true, true) => "its record withheld",
            };
            let name = format!("{name}: move {passes}, {fault}");
            log.push((name, cost, format!("{answer:?}")));
            if !fired {
                whole = Some(answer);
                break;
            }
            refused.push(answer);
        }
    }
    Swept {
        refused,
        whole: whole.expect("a run with no fault"),
    }
}

fn is_the_refusal(error: &Error) -> bool {
    error.to_string().contains(INJECTED_READ_FAILURE)
}

impl<T: Debug> Swept<T> {
    /// The operation moved a cursor, and no refused move was taken for the
    /// end of the records: with any move refused the operation failed, and
    /// with at least one of them by the refusal itself.
    fn no_refusal_is_an_answer(&self, name: &str) {
        assert!(
            !self.refused.is_empty(),
            "{name}: the operation moved no cursor"
        );
        for (at, answer) in self.refused.iter().enumerate() {
            assert!(
                answer.is_err(),
                "{name}: move {at} was refused, and the operation answered {answer:?}"
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

    /// As [`Self::no_refusal_is_an_answer`], for an operation whose every
    /// move is a move of its scan: each refusal comes back as the
    /// storage's error, and with none the operation answers.
    fn each_refusal_is_the_error(&self, name: &str) {
        self.no_refusal_is_an_answer(name);
        assert!(self.whole.is_ok(), "{name}: {:?}", self.whole);
        for (at, answer) in self.refused.iter().enumerate() {
            assert!(
                answer.as_ref().is_err_and(is_the_refusal),
                "{name}: move {at} was refused, and the operation answered {answer:?}"
            );
        }
    }
}

/// The scans of GroveDB over trees of items, with one move refused at a
/// time: the range of a query in both directions, the walk that adds sum
/// items, the walk that finds the subtrees of a tree, the scan before a
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
            db.query_raw(
                &query,
                true,
                true,
                true,
                QueryResultType::QueryKeyElementPairResultType,
                None,
                version,
            )
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
            db.query_aggregate_sums(&walk, true, true, None, version)
        });
        swept.each_refusal_is_the_error(&name);
        assert_eq!(swept.whole.expect("the sums").results.len(), 6, "{name}");
    }

    let rows: [&[u8]; 1] = [ROWS];
    let at = SubtreePath::from(rows.as_ref());
    let name = "the subtrees of a tree";
    let swept = sweep(&mut log, name, || db.find_subtrees(&at, None, version));
    swept.each_refusal_is_the_error(name);
    assert_eq!(swept.whole.expect("the subtrees").len(), 2, "{name}");

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
        db.clear_subtree(rows.as_ref(), Some(options.clone()), None, version)
            .wrap_with_cost(OperationCost::default())
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
        db.verify_grovedb(None, true, false, version)
            .wrap_with_cost(OperationCost::default())
    });
    swept.no_refusal_is_an_answer(name);
    let issues = swept.whole.expect("a verification that ran");
    assert!(issues.is_empty(), "{name}: {issues:?}");
    log
}

/// The scans of an indexed tree with one move refused at a time: a range
/// of its secondary, the first rows of it, and the two scans of a
/// verification that compare the secondary with the primary.
pub(crate) fn refused_scans_of_an_indexed_tree<E: Engine>(
    dir: &Path,
    version: &'static GroveVersion,
) -> Vec<Answer> {
    const INDEXED: &[u8] = b"indexed";

    let db = GroveDb::<RefusedReads<E>>::open_with_engine(dir).expect("a database");
    let mut log = Vec::new();
    let tree = Element::empty_provable_count_indexed_tree();
    insert(&db, &[], INDEXED, tree, version);
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
            db.indexed_count_range(indexed.as_ref(), 0, u64::MAX, descending, 10, None, version)
        });
        swept.each_refusal_is_the_error(&name);
        assert_eq!(swept.whole.expect("the entries").len(), 4, "{name}");

        let name = format!("the first rows of the secondary, descending: {descending}");
        let swept = sweep(&mut log, &name, || {
            db.indexed_count_top_k_paginated(indexed.as_ref(), 3, 0, descending, None, version)
        });
        swept.each_refusal_is_the_error(&name);
        assert_eq!(swept.whole.expect("a page").entries.len(), 3, "{name}");
    }

    let name = "a verification of an indexed tree";
    let swept = sweep(&mut log, name, || {
        db.verify_grovedb(None, true, false, version)
            .wrap_with_cost(OperationCost::default())
    });
    swept.no_refusal_is_an_answer(name);
    let issues = swept.whole.expect("a verification that ran");
    assert!(issues.is_empty(), "{name}: {issues:?}");
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
}
