//! A storage read that fails while opening a subtree is not an absent
//! parent (#1030).
//!
//! Opening a subtree reads its parent element. When that read fails, the
//! readers that answer "parent not found" with absence (`get_raw_optional`,
//! key items and subquery hops of a path query) return the storage error
//! instead. A parent key that is not there, and a path below a tree that
//! keeps its own data where a child Merk would be, are still absence.

use grovedb_storage::{RawIterator, Storage, StorageContext};
use grovedb_version::version::{v4::GROVE_V4, GroveVersion, GROVE_VERSIONS};

use crate::{
    query_result_type::QueryResultType,
    tests::{common::EMPTY_PATH, make_empty_grovedb, make_test_grovedb, TempGroveDb, TEST_LEAF},
    util::compat::test_hooks::{with_failed_parent_read, INJECTED_READ_FAILURE},
    BackwardsReferences, Element, Error, PathQuery, Query,
};

const PARENT: &[u8] = b"parent";
const KEY: &[u8] = b"key";
const ELEMENTS: QueryResultType = QueryResultType::QueryElementResultType;

fn item() -> Element {
    Element::new_item(b"value".to_vec())
}

/// `TEST_LEAF / PARENT / KEY` holds an item.
fn db_with_item(grove_version: &GroveVersion) -> TempGroveDb {
    let db = make_test_grovedb(grove_version);
    db.insert(
        [TEST_LEAF].as_ref(),
        PARENT,
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert parent tree");
    db.insert(
        [TEST_LEAF, PARENT].as_ref(),
        KEY,
        item(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert item");
    db
}

fn assert_injected_failure<T: std::fmt::Debug>(result: Result<T, Error>, context: &str) {
    assert!(
        matches!(
            &result,
            Err(Error::StorageError(grovedb_storage::Error::StorageError(message)))
                if message == INJECTED_READ_FAILURE
        ),
        "{context}: expected the storage error, got {result:?}"
    );
}

/// A key item at `path`.
fn key_query(path: &[&[u8]], key: &[u8]) -> PathQuery {
    PathQuery::new_unsized(
        path.iter().map(|segment| segment.to_vec()).collect(),
        Query::new_single_key(key.to_vec()),
    )
}

/// `PARENT` selected in `TEST_LEAF`, then `KEY` below it as a subquery path.
fn hop_query() -> PathQuery {
    let mut query = Query::new_single_key(PARENT.to_vec());
    query.set_subquery_path(vec![KEY.to_vec()]);
    PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query)
}

#[test]
fn should_return_the_storage_error_when_the_parent_read_fails_on_a_single_read() {
    for grove_version in GROVE_VERSIONS {
        let db = db_with_item(grove_version);
        let path = [TEST_LEAF, PARENT];

        let healthy = db.get_raw_optional(path.as_ref().into(), KEY, None, grove_version);
        assert_eq!(healthy.value.expect("healthy read"), Some(item()));

        // What an absent parent costs: one seek that loads nothing.
        let absent = db.get_raw_optional(
            [TEST_LEAF, b"absent"].as_ref().into(),
            KEY,
            None,
            grove_version,
        );
        assert_eq!(absent.value.expect("absence"), None);

        with_failed_parent_read(PARENT, || {
            let read = db.get_raw_optional(path.as_ref().into(), KEY, None, grove_version);
            // The failed seek is still paid for.
            assert_eq!(read.cost, absent.cost);
            assert_eq!(read.cost.seek_count, 1);
            assert_injected_failure(read.value, "get_raw_optional");

            assert_injected_failure(
                db.get_raw(path.as_ref().into(), KEY, None, grove_version)
                    .unwrap(),
                "get_raw",
            );
            assert_injected_failure(
                db.get(path.as_ref(), KEY, None, grove_version).unwrap(),
                "get",
            );
        });

        // The seam is disarmed again.
        let healthy = db.get_raw_optional(path.as_ref().into(), KEY, None, grove_version);
        assert_eq!(healthy.value.expect("healthy read"), Some(item()));
    }
}

#[test]
fn should_return_the_storage_error_when_the_parent_read_fails_in_a_path_query() {
    for grove_version in GROVE_VERSIONS {
        let db = db_with_item(grove_version);
        let queries = [
            ("key item", key_query(&[TEST_LEAF, PARENT], KEY)),
            ("subquery hop", hop_query()),
        ];
        for (name, path_query) in &queries {
            for error_if_intermediate_path_tree_not_present in [false, true] {
                let run = || {
                    db.query_raw(
                        path_query,
                        true,
                        true,
                        error_if_intermediate_path_tree_not_present,
                        ELEMENTS,
                        None,
                        grove_version,
                    )
                    .unwrap()
                };
                let (healthy, _) = run().expect("healthy query");
                assert_eq!(healthy.to_elements(), vec![item()], "{name}");

                let faulted = with_failed_parent_read(PARENT, run);
                assert_injected_failure(faulted, name);
            }
        }
    }
}

#[test]
fn should_still_report_a_missing_parent_key_as_absence() {
    for grove_version in GROVE_VERSIONS {
        let db = db_with_item(grove_version);
        let path = [TEST_LEAF, b"missing".as_slice()];

        // Another key's read failing does not touch this one.
        with_failed_parent_read(PARENT, || {
            let read = db.get_raw_optional(path.as_ref().into(), KEY, None, grove_version);
            assert_eq!(
                (read.cost.seek_count, read.cost.storage_loaded_bytes),
                (1, 0)
            );
            assert_eq!(read.value.expect("absence, not an error"), None);

            assert!(matches!(
                db.get_raw(path.as_ref().into(), KEY, None, grove_version)
                    .unwrap(),
                Err(Error::PathParentLayerNotFound(_))
            ));

            let (elements, _) = db
                .query_raw(
                    &key_query(&path, KEY),
                    true,
                    true,
                    false,
                    ELEMENTS,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("an empty result, not an error");
            assert!(elements.is_empty());
            assert!(matches!(
                db.query_raw(
                    &key_query(&path, KEY),
                    true,
                    true,
                    true,
                    ELEMENTS,
                    None,
                    grove_version,
                )
                .unwrap(),
                Err(Error::PathParentLayerNotFound(_))
            ));
        });
    }
}

/// The first record a non-Merk tree at `path` keeps in its own data storage.
fn first_stored_record(db: &TempGroveDb, path: &[&[u8]]) -> (Vec<u8>, Vec<u8>) {
    let tx = db.start_transaction();
    let storage = db
        .db
        .get_transactional_storage_context(path.into(), None, &tx)
        .unwrap();
    let mut iter = storage.raw_iter();
    iter.seek_to_first().unwrap();
    let key = iter.key().unwrap().expect("a stored record").to_vec();
    let value = iter.value().unwrap().expect("a stored record").to_vec();
    (key, value)
}

#[test]
fn should_still_report_a_path_below_a_non_merk_tree_as_absence() {
    for grove_version in [&GROVE_V4, GroveVersion::latest()] {
        let db = make_empty_grovedb();
        db.insert(
            EMPTY_PATH,
            b"dense",
            Element::empty_dense_tree(3),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert dense tree");
        for i in 0..5u8 {
            db.dense_tree_insert(EMPTY_PATH, b"dense", vec![i; 4], None, grove_version)
                .unwrap()
                .expect("dense insert");
        }

        // The tree's own records sit where the elements of a child Merk
        // would: a read of a "parent element" under one of their keys finds
        // bytes that are not a Merk node.
        let (record_key, record) = first_stored_record(&db, &[b"dense"]);
        assert!(!record.is_empty());

        for below in [record_key.as_slice(), b"no such record".as_slice()] {
            let path = [b"dense".as_slice(), below];
            let read = db.get_raw_optional(path.as_ref().into(), KEY, None, grove_version);
            assert_eq!(read.value.expect("absence, not an error"), None);
            // The open itself reports the parent as not found, with the
            // record's bytes as with no bytes.
            assert!(matches!(
                db.get_raw(path.as_ref().into(), KEY, None, grove_version)
                    .unwrap(),
                Err(Error::PathParentLayerNotFound(_))
            ));

            let (elements, _) = db
                .query_raw(
                    &key_query(&path, KEY),
                    true,
                    true,
                    false,
                    ELEMENTS,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("an empty result, not an error");
            assert!(elements.is_empty());
        }
    }
}

#[test]
fn should_not_reclaim_a_dropped_path_whose_liveness_read_fails() {
    let grove_version = GroveVersion::latest();
    let db = db_with_item(grove_version);
    let path = [TEST_LEAF, PARENT];

    // Drop the tree in a caller transaction, which leaves its redo record
    // pending, then put a tree with an item back at the same path.
    let tx = db.start_transaction();
    db.drop_flat_subtree(
        [TEST_LEAF].as_ref(),
        PARENT,
        BackwardsReferences::DontCheck,
        Some(&tx),
        grove_version,
    )
    .unwrap()
    .expect("drop in a transaction");
    db.commit_transaction(tx).unwrap().expect("commit");
    db.insert(
        [TEST_LEAF].as_ref(),
        PARENT,
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("recreate the tree");
    db.insert(path.as_ref(), KEY, item(), None, None, grove_version)
        .unwrap()
        .expect("insert item");

    // The liveness check reads `TEST_LEAF / PARENT`, which opens `TEST_LEAF`.
    // With that open failing, the check has no answer: the record is counted
    // as unresolved and the path's storage is left alone.
    let report = with_failed_parent_read(TEST_LEAF, || {
        db.flush_pending_prefix_drops(grove_version).expect("flush")
    });
    assert_eq!(
        (
            report.reclaimed_records,
            report.skipped_live,
            report.unresolved_records
        ),
        (0, 0, 1)
    );
    assert!(matches!(
        report.first_unresolved_error.as_deref(),
        Some(message) if message.contains(INJECTED_READ_FAILURE)
    ));
    assert_eq!(
        db.get_raw(path.as_ref().into(), KEY, None, grove_version)
            .unwrap()
            .expect("the live item"),
        item()
    );

    // A healthy pass finds the path live and leaves it, as before.
    let report = db.flush_pending_prefix_drops(grove_version).expect("flush");
    assert_eq!(
        (
            report.reclaimed_records,
            report.skipped_live,
            report.unresolved_records
        ),
        (0, 1, 0)
    );
}
