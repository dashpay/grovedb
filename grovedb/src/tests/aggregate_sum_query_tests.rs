//! Tests for AggregateSumPathQuery methods and the GroveDb::query_aggregate_sums public API.

use grovedb_merk::proofs::aggregate_sum_query::AggregateSumQuery;
use grovedb_merk::proofs::query::QueryItem;
use grovedb_version::version::GroveVersion;

use crate::tests::{make_test_sum_tree_grovedb, TempGroveDb, TEST_LEAF};
use crate::{AggregateSumPathQuery, Element};

// =========================================================================
// Group C: Public API query_aggregate_sums
// =========================================================================

/// Helper: create sum tree with keys a..d
fn setup_sum_tree(grove_version: &GroveVersion) -> TempGroveDb {
    let db = make_test_sum_tree_grovedb(grove_version);
    for (key, val) in [(b"a" as &[u8], 2), (b"b", 3), (b"c", 4), (b"d", 5)] {
        db.insert(
            [TEST_LEAF].as_ref(),
            key,
            Element::new_sum_item(val),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert element");
    }
    db
}

#[test]
fn test_query_aggregate_sums_basic() {
    let grove_version = GroveVersion::latest();
    let db = setup_sum_tree(grove_version);

    let aggregate_sum_path_query =
        AggregateSumPathQuery::new(vec![TEST_LEAF.to_vec()], AggregateSumQuery::new(100, None));

    let results = db
        .query_aggregate_sums(
            &aggregate_sum_path_query,
            true, // allow_cache
            true, // error_if_intermediate_path_tree_not_present
            None, // no transaction
            grove_version,
        )
        .unwrap()
        .expect("expected successful query");

    assert_eq!(
        results,
        vec![
            (b"a".to_vec(), 2),
            (b"b".to_vec(), 3),
            (b"c".to_vec(), 4),
            (b"d".to_vec(), 5),
        ]
    );
}

#[test]
fn test_query_aggregate_sums_with_transaction() {
    let grove_version = GroveVersion::latest();
    let db = setup_sum_tree(grove_version);

    let transaction = db.start_transaction();

    // Insert an extra element within the transaction
    db.insert(
        [TEST_LEAF].as_ref(),
        b"e",
        Element::new_sum_item(6),
        None,
        Some(&transaction),
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");

    let aggregate_sum_path_query =
        AggregateSumPathQuery::new(vec![TEST_LEAF.to_vec()], AggregateSumQuery::new(100, None));

    // Query within the transaction — should see the new element
    let results = db
        .query_aggregate_sums(
            &aggregate_sum_path_query,
            true,
            true,
            Some(&transaction),
            grove_version,
        )
        .unwrap()
        .expect("expected successful query");

    assert_eq!(results.len(), 5);
    assert!(results.contains(&(b"e".to_vec(), 6)));

    // Query without the transaction — should NOT see the new element
    let results_no_tx = db
        .query_aggregate_sums(&aggregate_sum_path_query, true, true, None, grove_version)
        .unwrap()
        .expect("expected successful query");

    assert_eq!(results_no_tx.len(), 4);
    assert!(!results_no_tx.contains(&(b"e".to_vec(), 6)));
}

#[test]
fn test_query_aggregate_sums_intermediate_path_missing_no_error() {
    let grove_version = GroveVersion::latest();
    let db = setup_sum_tree(grove_version);

    let aggregate_sum_path_query = AggregateSumPathQuery::new(
        vec![b"nonexistent".to_vec()],
        AggregateSumQuery::new_with_keys(vec![b"a".to_vec()], 100, None),
    );

    // With error_if_intermediate_path_tree_not_present = false → empty, no error
    let results = db
        .query_aggregate_sums(
            &aggregate_sum_path_query,
            true,
            false, // don't error on missing path
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected successful query (empty)");

    assert!(results.is_empty());
}

// =========================================================================
// Group D: AggregateSumPathQuery methods
// =========================================================================

#[test]
fn test_aggregate_sum_path_query_merge_success() {
    let grove_version = GroveVersion::latest();

    let q1 = AggregateSumPathQuery::new(
        vec![TEST_LEAF.to_vec()],
        AggregateSumQuery::new_with_keys(vec![b"a".to_vec()], 10, Some(2)),
    );
    let q2 = AggregateSumPathQuery::new(
        vec![TEST_LEAF.to_vec()],
        AggregateSumQuery::new_with_keys(vec![b"b".to_vec()], 20, Some(3)),
    );

    let merged =
        AggregateSumPathQuery::merge(vec![&q1, &q2], grove_version).expect("merge should succeed");

    assert_eq!(merged.path, vec![TEST_LEAF.to_vec()]);
    // Sum limits are added
    assert_eq!(merged.aggregate_sum_query.sum_limit, 30);
    // Item limits are added
    assert_eq!(merged.aggregate_sum_query.limit_of_items_to_check, Some(5));
    // Items from both queries are present
    assert_eq!(merged.aggregate_sum_query.items.len(), 2);
}

#[test]
fn test_aggregate_sum_path_query_merge_single() {
    let grove_version = GroveVersion::latest();

    let q = AggregateSumPathQuery::new(
        vec![TEST_LEAF.to_vec()],
        AggregateSumQuery::new(42, Some(5)),
    );

    let merged =
        AggregateSumPathQuery::merge(vec![&q], grove_version).expect("merge should succeed");

    assert_eq!(merged.path, q.path);
    assert_eq!(
        merged.aggregate_sum_query.sum_limit,
        q.aggregate_sum_query.sum_limit
    );
    assert_eq!(
        merged.aggregate_sum_query.limit_of_items_to_check,
        q.aggregate_sum_query.limit_of_items_to_check
    );
}

#[test]
fn test_aggregate_sum_path_query_merge_empty_errors() {
    let grove_version = GroveVersion::latest();

    let result = AggregateSumPathQuery::merge(vec![], grove_version);
    assert!(result.is_err());
    match result {
        Err(crate::Error::InvalidInput(msg)) => {
            assert!(msg.contains("at least 1"));
        }
        other => panic!("expected InvalidInput error, got {:?}", other),
    }
}

#[test]
fn test_aggregate_sum_path_query_merge_path_mismatch_errors() {
    let grove_version = GroveVersion::latest();

    let q1 = AggregateSumPathQuery::new(vec![b"path_a".to_vec()], AggregateSumQuery::new(10, None));
    let q2 = AggregateSumPathQuery::new(vec![b"path_b".to_vec()], AggregateSumQuery::new(10, None));

    let result = AggregateSumPathQuery::merge(vec![&q1, &q2], grove_version);
    assert!(result.is_err());
    match result {
        Err(crate::Error::InvalidInput(msg)) => {
            assert!(msg.contains("same path"));
        }
        other => panic!("expected InvalidInput error, got {:?}", other),
    }
}

#[test]
fn test_aggregate_sum_path_query_constructors() {
    // new()
    let q = AggregateSumPathQuery::new(vec![b"p".to_vec()], AggregateSumQuery::new(50, Some(10)));
    assert_eq!(q.path, vec![b"p".to_vec()]);
    assert_eq!(q.aggregate_sum_query.sum_limit, 50);
    assert_eq!(q.aggregate_sum_query.limit_of_items_to_check, Some(10));

    // new_single_key()
    let q = AggregateSumPathQuery::new_single_key(vec![b"p".to_vec()], b"mykey".to_vec(), 99);
    assert_eq!(q.path, vec![b"p".to_vec()]);
    assert_eq!(q.aggregate_sum_query.sum_limit, 99);
    assert_eq!(q.aggregate_sum_query.limit_of_items_to_check, Some(1));
    assert_eq!(q.aggregate_sum_query.items.len(), 1);
    assert_eq!(
        q.aggregate_sum_query.items[0],
        QueryItem::Key(b"mykey".to_vec())
    );

    // new_single_query_item()
    let qi = QueryItem::RangeFrom(b"start".to_vec()..);
    let q =
        AggregateSumPathQuery::new_single_query_item(vec![b"p".to_vec()], qi.clone(), 200, Some(5));
    assert_eq!(q.aggregate_sum_query.sum_limit, 200);
    assert_eq!(q.aggregate_sum_query.limit_of_items_to_check, Some(5));
    assert_eq!(q.aggregate_sum_query.items.len(), 1);
    assert_eq!(q.aggregate_sum_query.items[0], qi);
}

#[test]
fn test_aggregate_sum_path_query_display() {
    let q = AggregateSumPathQuery::new(
        vec![b"test".to_vec(), b"path".to_vec()],
        AggregateSumQuery::new(100, None),
    );

    let display = format!("{}", q);
    assert!(display.contains("AggregateSumPathQuery"));
    assert!(display.contains("test"));
    assert!(display.contains("path"));
}
