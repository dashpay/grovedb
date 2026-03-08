use grovedb_merk::element::insert::ElementInsertToStorageExtensions;
use grovedb_merk::proofs::query::AggregateSumQuery;
use grovedb_merk::proofs::query::QueryItem;
use grovedb_merk::tree::NULL_HASH;
use grovedb_path::SubtreePath;
use grovedb_storage::Storage;
use grovedb_version::version::GroveVersion;

use crate::element::aggregate_sum_query::{
    AggregateSumQueryOptions, ElementAggregateSumQueryExtensions,
};
use crate::merk_cache::MerkCache;
use crate::reference_path::ReferencePathType;
use crate::{
    tests::{make_test_sum_tree_grovedb, TEST_LEAF},
    AggregateSumPathQuery, Element, Error,
};

#[test]
fn test_get_aggregate_sum_query_full_range() {
    let grove_version = GroveVersion::latest();
    let db = make_test_sum_tree_grovedb(grove_version);

    db.insert(
        [TEST_LEAF].as_ref(),
        b"d",
        Element::new_sum_item(11),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"c",
        Element::new_sum_item(3),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"a",
        Element::new_sum_item(7),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"b",
        Element::new_sum_item(5),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");

    // Test queries by full range up to 10
    let aggregate_sum_query = AggregateSumQuery::new(10, None);

    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    assert_eq!(
        Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions::default(),
            None,
            grove_version
        )
        .unwrap()
        .expect("expected successful get_query")
        .results,
        vec![(b"a".to_vec(), 7), (b"b".to_vec(), 5)]
    );

    // Test queries by full range up to 12
    let aggregate_sum_query = AggregateSumQuery::new(12, None);

    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    assert_eq!(
        Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions::default(),
            None,
            grove_version
        )
        .unwrap()
        .expect("expected successful get_query")
        .results,
        vec![(b"a".to_vec(), 7), (b"b".to_vec(), 5)]
    );

    // Test queries by full range up to 13
    let aggregate_sum_query = AggregateSumQuery::new(13, None);

    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    assert_eq!(
        Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions::default(),
            None,
            grove_version
        )
        .unwrap()
        .expect("expected successful get_query")
        .results,
        vec![(b"a".to_vec(), 7), (b"b".to_vec(), 5), (b"c".to_vec(), 3)]
    );

    // Test queries by full range up to 0
    let aggregate_sum_query = AggregateSumQuery::new(0, None);

    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    assert_eq!(
        Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions::default(),
            None,
            grove_version
        )
        .unwrap()
        .expect("expected successful get_query")
        .results,
        vec![]
    );

    // Test queries by full range up to 100
    let aggregate_sum_query = AggregateSumQuery::new(100, None);

    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    assert_eq!(
        Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions::default(),
            None,
            grove_version
        )
        .unwrap()
        .expect("expected successful get_query")
        .results,
        vec![
            (b"a".to_vec(), 7),
            (b"b".to_vec(), 5),
            (b"c".to_vec(), 3),
            (b"d".to_vec(), 11),
        ]
    );
}

#[test]
fn test_get_aggregate_sum_query_full_range_descending() {
    let grove_version = GroveVersion::latest();
    let db = make_test_sum_tree_grovedb(grove_version);

    db.insert(
        [TEST_LEAF].as_ref(),
        b"a",
        Element::new_sum_item(7),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"b",
        Element::new_sum_item(5),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"c",
        Element::new_sum_item(3),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"d",
        Element::new_sum_item(11),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");

    // Test queries by full range up to 10
    let aggregate_sum_query = AggregateSumQuery::new_descending(10, None);

    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    assert_eq!(
        Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions::default(),
            None,
            grove_version
        )
        .unwrap()
        .expect("expected successful get_query")
        .results,
        vec![(b"d".to_vec(), 11)]
    );

    // Test queries by full range up to 12
    let aggregate_sum_query = AggregateSumQuery::new_descending(12, None);

    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    assert_eq!(
        Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions::default(),
            None,
            grove_version
        )
        .unwrap()
        .expect("expected successful get_query")
        .results,
        vec![(b"d".to_vec(), 11), (b"c".to_vec(), 3)]
    );

    // Test queries by full range up to 0
    let aggregate_sum_query = AggregateSumQuery::new_descending(0, None);

    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    assert_eq!(
        Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions::default(),
            None,
            grove_version
        )
        .unwrap()
        .expect("expected successful get_query")
        .results,
        vec![]
    );

    // Test queries by full range up to 100
    let aggregate_sum_query = AggregateSumQuery::new_descending(100, None);

    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    assert_eq!(
        Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions::default(),
            None,
            grove_version
        )
        .unwrap()
        .expect("expected successful get_query")
        .results,
        vec![
            (b"d".to_vec(), 11),
            (b"c".to_vec(), 3),
            (b"b".to_vec(), 5),
            (b"a".to_vec(), 7),
        ]
    );
}

#[test]
fn test_get_aggregate_sum_query_sub_ranges() {
    let grove_version = GroveVersion::latest();
    let db = make_test_sum_tree_grovedb(grove_version);

    db.insert(
        [TEST_LEAF].as_ref(),
        b"a",
        Element::new_sum_item(7),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"b",
        Element::new_sum_item(5),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"c",
        Element::new_sum_item(3),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"d",
        Element::new_sum_item(11),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"e",
        Element::new_sum_item(14),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"f",
        Element::new_sum_item(3),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");

    // Test queries by sub range up to 3
    let aggregate_sum_query = AggregateSumQuery::new_single_query_item(
        QueryItem::Range(b"b".to_vec()..b"e".to_vec()),
        3,
        None,
    );

    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    assert_eq!(
        Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions::default(),
            None,
            grove_version
        )
        .unwrap()
        .expect("expected successful get_query")
        .results,
        vec![(b"b".to_vec(), 5)]
    );

    // Test queries by sub range up to 0
    let aggregate_sum_query = AggregateSumQuery::new_single_query_item(
        QueryItem::Range(b"b".to_vec()..b"e".to_vec()),
        0,
        None,
    );

    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    assert_eq!(
        Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions::default(),
            None,
            grove_version
        )
        .unwrap()
        .expect("expected successful get_query")
        .results,
        vec![]
    );

    // Test queries by sub range up to 100
    let aggregate_sum_query = AggregateSumQuery::new_single_query_item(
        QueryItem::Range(b"b".to_vec()..b"e".to_vec()),
        100,
        None,
    );

    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    assert_eq!(
        Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions::default(),
            None,
            grove_version
        )
        .unwrap()
        .expect("expected successful get_query")
        .results,
        vec![(b"b".to_vec(), 5), (b"c".to_vec(), 3), (b"d".to_vec(), 11),]
    );

    // Test queries by sub range inclusive up to 100
    let aggregate_sum_query = AggregateSumQuery::new_single_query_item(
        QueryItem::RangeInclusive(b"b".to_vec()..=b"e".to_vec()),
        100,
        None,
    );

    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    assert_eq!(
        Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions::default(),
            None,
            grove_version
        )
        .unwrap()
        .expect("expected successful get_query")
        .results,
        vec![
            (b"b".to_vec(), 5),
            (b"c".to_vec(), 3),
            (b"d".to_vec(), 11),
            (b"e".to_vec(), 14),
        ]
    );
}

#[test]
fn test_get_aggregate_sum_query_on_keys() {
    let grove_version = GroveVersion::latest();
    let db = make_test_sum_tree_grovedb(grove_version);

    db.insert(
        [TEST_LEAF].as_ref(),
        b"a",
        Element::new_sum_item(7),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"b",
        Element::new_sum_item(5),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"c",
        Element::new_sum_item(3),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"d",
        Element::new_sum_item(11),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"e",
        Element::new_sum_item(14),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"f",
        Element::new_sum_item(3),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");

    // Test queries by sub range up to 50
    let aggregate_sum_query = AggregateSumQuery::new_with_keys(
        vec![b"b".to_vec(), b"e".to_vec(), b"c".to_vec()],
        50,
        None,
    );

    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    // We should get them back in the same order we asked
    assert_eq!(
        Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions::default(),
            None,
            grove_version
        )
        .unwrap()
        .expect("expected successful get_query")
        .results,
        vec![(b"b".to_vec(), 5), (b"e".to_vec(), 14), (b"c".to_vec(), 3),]
    );

    // Test queries by sub range up to 6
    let aggregate_sum_query = AggregateSumQuery::new_with_keys(
        vec![b"b".to_vec(), b"e".to_vec(), b"c".to_vec()],
        6,
        None,
    );

    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    // We should get only the first 2
    assert_eq!(
        Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions::default(),
            None,
            grove_version
        )
        .unwrap()
        .expect("expected successful get_query")
        .results,
        vec![(b"b".to_vec(), 5), (b"e".to_vec(), 14),]
    );

    // Test queries by sub range up to 5
    let aggregate_sum_query = AggregateSumQuery::new_with_keys(
        vec![b"b".to_vec(), b"e".to_vec(), b"c".to_vec()],
        5,
        None,
    );

    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    // We should get only the first one
    assert_eq!(
        Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions::default(),
            None,
            grove_version
        )
        .unwrap()
        .expect("expected successful get_query")
        .results,
        vec![(b"b".to_vec(), 5),]
    );

    // Test queries by sub range up to 50, but we make sure to only allow two elements to come back
    let aggregate_sum_query = AggregateSumQuery::new_with_keys(
        vec![b"b".to_vec(), b"e".to_vec(), b"c".to_vec()],
        50,
        Some(2),
    );

    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    // We should get only the first two
    assert_eq!(
        Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions::default(),
            None,
            grove_version
        )
        .unwrap()
        .expect("expected successful get_query")
        .results,
        vec![(b"b".to_vec(), 5), (b"e".to_vec(), 14),]
    );

    // Test queries by sub range up to 50, but we make sure to only allow two elements to come back, descending
    let aggregate_sum_query = AggregateSumQuery::new_with_keys_reversed(
        vec![b"b".to_vec(), b"e".to_vec(), b"c".to_vec()],
        50,
        Some(2),
    );

    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    // We should get only the first two in reverse order
    assert_eq!(
        Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions::default(),
            None,
            grove_version
        )
        .unwrap()
        .expect("expected successful get_query")
        .results,
        vec![(b"c".to_vec(), 3), (b"e".to_vec(), 14),]
    );

    // Test queries by sub range up to 3, descending
    let aggregate_sum_query = AggregateSumQuery::new_with_keys_reversed(
        vec![b"b".to_vec(), b"e".to_vec(), b"c".to_vec()],
        3,
        None,
    );

    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    // We should get only the first one
    assert_eq!(
        Element::get_aggregate_sum_query(
            &db.db,
            &aggregate_sum_path_query,
            AggregateSumQueryOptions::default(),
            None,
            grove_version
        )
        .unwrap()
        .expect("expected successful get_query")
        .results,
        vec![(b"c".to_vec(), 3),]
    );
}

#[test]
fn display_aggregate_sum_query_options_default() {
    let opts = AggregateSumQueryOptions::default();
    let s = format!("{}", opts);
    assert!(s.contains("allow_cache: true"));
    assert!(s.contains("error_if_intermediate_path_tree_not_present: true"));
    assert!(s.contains("error_if_non_sum_item_found: true"));
    assert!(s.contains("ignore_references: false"));
}

#[test]
fn display_aggregate_sum_query_options_custom() {
    let opts = AggregateSumQueryOptions {
        allow_cache: false,
        error_if_intermediate_path_tree_not_present: false,
        error_if_non_sum_item_found: false,
        ignore_references: true,
    };
    let s = format!("{}", opts);
    assert!(s.contains("allow_cache: false"));
    assert!(s.contains("error_if_intermediate_path_tree_not_present: false"));
    assert!(s.contains("error_if_non_sum_item_found: false"));
    assert!(s.contains("ignore_references: true"));
}

#[test]
fn display_aggregate_sum_path_query_push_args() {
    use crate::element::aggregate_sum_query::AggregateSumPathQueryPushArgs;

    let grove_version = GroveVersion::latest();
    let db = make_test_sum_tree_grovedb(grove_version);

    let path: &[&[u8]] = &[TEST_LEAF];
    let mut results = vec![(b"prev".to_vec(), 42i64)];
    let mut limit = Some(5u16);
    let mut sum_limit_left = 100i64;
    let mut elements_scanned = 3u16;

    let args = AggregateSumPathQueryPushArgs {
        storage: &db.db,
        transaction: None,
        key: Some(b"mykey"),
        element: Element::new_sum_item(7),
        path,
        left_to_right: true,
        query_options: AggregateSumQueryOptions::default(),
        results: &mut results,
        limit: &mut limit,
        sum_limit_left: &mut sum_limit_left,
        elements_scanned: &mut elements_scanned,
        max_elements_scanned: 1024,
    };

    let s = format!("{}", args);
    assert!(s.contains("AggregateSumPathQueryPushArgs"));
    assert!(s.contains("key:"));
    assert!(s.contains("left_to_right: true"));
    assert!(s.contains("limit: Some(5)"));
    assert!(s.contains("sum_limit: 100"));
    assert!(s.contains("elements_scanned: 3"));
    assert!(s.contains("max_elements_scanned: 1024"));
    assert!(s.contains("0x70726576: 42")); // "prev" in hex
}

#[test]
fn test_key_not_found_returns_empty() {
    // Exercises line 417: Err(Error::PathKeyNotFound(_)) => Ok(())
    let grove_version = GroveVersion::latest();
    let db = make_test_sum_tree_grovedb(grove_version);

    db.insert(
        [TEST_LEAF].as_ref(),
        b"a",
        Element::new_sum_item(7),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");

    // Query for a key that doesn't exist
    let aggregate_sum_query = AggregateSumQuery::new_single_key(b"nonexistent".to_vec(), 100);

    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    let result = Element::get_aggregate_sum_query(
        &db.db,
        &aggregate_sum_path_query,
        AggregateSumQueryOptions::default(),
        None,
        grove_version,
    )
    .unwrap()
    .expect("expected successful get_query");

    assert!(result.results.is_empty());
}

#[test]
fn test_non_sum_item_in_range_query_errors() {
    // A range query encountering a non-SumItem element should error
    let grove_version = GroveVersion::latest();
    let db = make_test_sum_tree_grovedb(grove_version);

    db.insert(
        [TEST_LEAF].as_ref(),
        b"a",
        Element::new_sum_item(7),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"b",
        Element::new_item(b"not_a_sum_item".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");

    // Range query with error_if_non_sum_item_found=true (default) should error on the Item
    let aggregate_sum_query = AggregateSumQuery::new(100, None);
    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    let result = Element::get_aggregate_sum_query(
        &db.db,
        &aggregate_sum_path_query,
        AggregateSumQueryOptions::default(),
        None,
        grove_version,
    )
    .unwrap();

    assert!(
        result.is_err(),
        "expected error on non-SumItem in range query"
    );
}

#[test]
fn test_query_with_limit_of_items_to_check() {
    // Exercises line 256-258: limit == Some(0) break path in ascending
    let grove_version = GroveVersion::latest();
    let db = make_test_sum_tree_grovedb(grove_version);

    db.insert(
        [TEST_LEAF].as_ref(),
        b"a",
        Element::new_sum_item(1),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"b",
        Element::new_sum_item(2),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"c",
        Element::new_sum_item(3),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");

    // sum_limit is high but limit_of_items_to_check is 1
    let aggregate_sum_query = AggregateSumQuery::new(1000, Some(1));
    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    let result = Element::get_aggregate_sum_query(
        &db.db,
        &aggregate_sum_path_query,
        AggregateSumQueryOptions::default(),
        None,
        grove_version,
    )
    .unwrap()
    .expect("expected successful get_query");

    assert_eq!(result.results.len(), 1);
    assert_eq!(result.results[0], (b"a".to_vec(), 1));
}

#[test]
fn test_query_multiple_keys_with_some_missing() {
    // Exercises line 417 PathKeyNotFound for some keys, success for others
    let grove_version = GroveVersion::latest();
    let db = make_test_sum_tree_grovedb(grove_version);

    db.insert(
        [TEST_LEAF].as_ref(),
        b"a",
        Element::new_sum_item(7),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"c",
        Element::new_sum_item(3),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");

    // Query for 3 keys, but "b" doesn't exist
    let aggregate_sum_query = AggregateSumQuery::new_with_keys(
        vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()],
        100,
        None,
    );
    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    let result = Element::get_aggregate_sum_query(
        &db.db,
        &aggregate_sum_path_query,
        AggregateSumQueryOptions::default(),
        None,
        grove_version,
    )
    .unwrap()
    .expect("expected successful get_query");

    // Should return only a and c, skipping the missing b
    assert_eq!(result.results, vec![(b"a".to_vec(), 7), (b"c".to_vec(), 3)]);
}

#[test]
fn test_descending_query_with_limit_break() {
    // Exercises line 281-283: limit == Some(0) break path in descending branch
    let grove_version = GroveVersion::latest();
    let db = make_test_sum_tree_grovedb(grove_version);

    db.insert(
        [TEST_LEAF].as_ref(),
        b"a",
        Element::new_sum_item(1),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"b",
        Element::new_sum_item(2),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"c",
        Element::new_sum_item(3),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");

    // descending with items-to-check limit of 1
    let aggregate_sum_query = AggregateSumQuery::new_descending(1000, Some(1));
    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    let result = Element::get_aggregate_sum_query(
        &db.db,
        &aggregate_sum_path_query,
        AggregateSumQueryOptions::default(),
        None,
        grove_version,
    )
    .unwrap()
    .expect("expected successful get_query");

    assert_eq!(result.results.len(), 1);
    assert_eq!(result.results[0], (b"c".to_vec(), 3));
}

#[test]
fn test_range_query_skips_non_sum_items() {
    let grove_version = GroveVersion::latest();
    let db = make_test_sum_tree_grovedb(grove_version);

    // Insert a mix of Items and SumItems
    db.insert(
        [TEST_LEAF].as_ref(),
        b"a",
        Element::new_sum_item(7),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"b",
        Element::new_item(b"regular_item".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"c",
        Element::new_sum_item(3),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"d",
        Element::new_item(b"another_item".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"e",
        Element::new_sum_item(11),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");

    // Query with error_if_non_sum_item_found=false should return only SumItems
    let aggregate_sum_query = AggregateSumQuery::new(100, None);
    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    let result = Element::get_aggregate_sum_query(
        &db.db,
        &aggregate_sum_path_query,
        AggregateSumQueryOptions {
            error_if_non_sum_item_found: false,
            ..AggregateSumQueryOptions::default()
        },
        None,
        grove_version,
    )
    .unwrap()
    .expect("expected successful get_query");

    assert_eq!(
        result.results,
        vec![(b"a".to_vec(), 7), (b"c".to_vec(), 3), (b"e".to_vec(), 11),]
    );
}

#[test]
fn test_range_query_skipped_items_do_not_decrement_limit() {
    let grove_version = GroveVersion::latest();
    let db = make_test_sum_tree_grovedb(grove_version);

    db.insert(
        [TEST_LEAF].as_ref(),
        b"a",
        Element::new_sum_item(7),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"b",
        Element::new_item(b"regular_item".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"c",
        Element::new_sum_item(3),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");

    // With limit_of_items_to_check=2 and error_if_non_sum_item_found=false,
    // b (item) is skipped without consuming a limit slot.
    // a (sum_item, limit→1), b (item, skipped), c (sum_item, limit→0)
    let aggregate_sum_query = AggregateSumQuery::new(100, Some(2));
    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    let result = Element::get_aggregate_sum_query(
        &db.db,
        &aggregate_sum_path_query,
        AggregateSumQueryOptions {
            error_if_non_sum_item_found: false,
            ..AggregateSumQueryOptions::default()
        },
        None,
        grove_version,
    )
    .unwrap()
    .expect("expected successful get_query");

    // Both "a" and "c" returned — skipped "b" didn't consume a limit slot
    assert_eq!(result.results, vec![(b"a".to_vec(), 7), (b"c".to_vec(), 3)]);
}

#[test]
fn test_key_query_skips_non_sum_items() {
    let grove_version = GroveVersion::latest();
    let db = make_test_sum_tree_grovedb(grove_version);

    db.insert(
        [TEST_LEAF].as_ref(),
        b"a",
        Element::new_item(b"regular_item".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"b",
        Element::new_sum_item(5),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");

    // Key query with error_if_non_sum_item_found=false: Item key "a" silently produces no result
    let aggregate_sum_query =
        AggregateSumQuery::new_with_keys(vec![b"a".to_vec(), b"b".to_vec()], 100, None);
    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    let result = Element::get_aggregate_sum_query(
        &db.db,
        &aggregate_sum_path_query,
        AggregateSumQueryOptions {
            error_if_non_sum_item_found: false,
            ..AggregateSumQueryOptions::default()
        },
        None,
        grove_version,
    )
    .unwrap()
    .expect("expected successful get_query");

    assert_eq!(result.results, vec![(b"b".to_vec(), 5)]);
}

#[test]
fn test_hard_limit_returns_partial_results() {
    // Create a custom grove version with max_elements_scanned=3
    let mut custom_version = GroveVersion::latest().clone();
    custom_version
        .grovedb_versions
        .query_limits
        .max_aggregate_sum_query_elements_scanned = 3;

    let db = make_test_sum_tree_grovedb(&custom_version);

    db.insert(
        [TEST_LEAF].as_ref(),
        b"a",
        Element::new_sum_item(1),
        None,
        None,
        &custom_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"b",
        Element::new_sum_item(2),
        None,
        None,
        &custom_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"c",
        Element::new_sum_item(3),
        None,
        None,
        &custom_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"d",
        Element::new_sum_item(4),
        None,
        None,
        &custom_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"e",
        Element::new_sum_item(5),
        None,
        None,
        &custom_version,
    )
    .unwrap()
    .expect("cannot insert element");

    // Query with sum_limit high enough to get all, but hard limit is 3
    let aggregate_sum_query = AggregateSumQuery::new(1000, None);
    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    let result = Element::get_aggregate_sum_query(
        &db.db,
        &aggregate_sum_path_query,
        AggregateSumQueryOptions::default(),
        None,
        &custom_version,
    )
    .unwrap()
    .expect("expected successful get_query (partial results, not error)");

    // Should return only first 3 elements due to hard limit
    assert_eq!(
        result.results,
        vec![(b"a".to_vec(), 1), (b"b".to_vec(), 2), (b"c".to_vec(), 3),]
    );
    assert!(result.hard_limit_reached, "hard limit should be flagged");
}

#[test]
fn test_error_if_non_sum_item_found_default() {
    let grove_version = GroveVersion::latest();
    let db = make_test_sum_tree_grovedb(grove_version);

    db.insert(
        [TEST_LEAF].as_ref(),
        b"a",
        Element::new_item(b"regular_item".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");

    // Query with error_if_non_sum_item_found=true (default) should error on non-SumItem
    let aggregate_sum_query = AggregateSumQuery::new_single_key(b"a".to_vec(), 100);
    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    let result = Element::get_aggregate_sum_query(
        &db.db,
        &aggregate_sum_path_query,
        AggregateSumQueryOptions::default(),
        None,
        grove_version,
    )
    .unwrap();

    assert!(
        result.is_err(),
        "expected error on non-SumItem with error_if_non_sum_item_found=true"
    );
}

#[test]
fn test_zero_sum_limit_with_key_query_returns_empty() {
    let grove_version = GroveVersion::latest();
    let db = make_test_sum_tree_grovedb(grove_version);

    db.insert(
        [TEST_LEAF].as_ref(),
        b"a",
        Element::new_sum_item(7),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");

    // sum_limit = 0 with a single key query should return empty
    let aggregate_sum_query = AggregateSumQuery::new_with_keys(vec![b"a".to_vec()], 0, None);

    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    let result = Element::get_aggregate_sum_query(
        &db.db,
        &aggregate_sum_path_query,
        AggregateSumQueryOptions::default(),
        None,
        grove_version,
    )
    .unwrap()
    .expect("expected successful get_query");

    assert!(
        result.results.is_empty(),
        "sum_limit=0 should return no results, got: {:?}",
        result
    );
}

#[test]
fn test_item_with_sum_item_in_range_query() {
    let grove_version = GroveVersion::latest();
    let db = make_test_sum_tree_grovedb(grove_version);

    db.insert(
        [TEST_LEAF].as_ref(),
        b"a",
        Element::new_sum_item(7),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"b",
        Element::new_item_with_sum_item(b"payload".to_vec(), 10),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"c",
        Element::new_sum_item(3),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");

    // Full range query should include ItemWithSumItem using its sum value
    let aggregate_sum_query = AggregateSumQuery::new(100, None);
    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    let result = Element::get_aggregate_sum_query(
        &db.db,
        &aggregate_sum_path_query,
        AggregateSumQueryOptions::default(),
        None,
        grove_version,
    )
    .unwrap()
    .expect("expected successful get_query");

    assert_eq!(
        result.results,
        vec![(b"a".to_vec(), 7), (b"b".to_vec(), 10), (b"c".to_vec(), 3),]
    );
}

#[test]
fn test_item_with_sum_item_in_key_query() {
    let grove_version = GroveVersion::latest();
    let db = make_test_sum_tree_grovedb(grove_version);

    db.insert(
        [TEST_LEAF].as_ref(),
        b"a",
        Element::new_item_with_sum_item(b"data_a".to_vec(), 15),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"b",
        Element::new_sum_item(5),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");

    // Key query for both types
    let aggregate_sum_query =
        AggregateSumQuery::new_with_keys(vec![b"a".to_vec(), b"b".to_vec()], 100, None);
    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    let result = Element::get_aggregate_sum_query(
        &db.db,
        &aggregate_sum_path_query,
        AggregateSumQueryOptions::default(),
        None,
        grove_version,
    )
    .unwrap()
    .expect("expected successful get_query");

    assert_eq!(
        result.results,
        vec![(b"a".to_vec(), 15), (b"b".to_vec(), 5)]
    );
}

#[test]
fn test_mixed_item_with_sum_item_and_sum_items_with_sum_limit() {
    let grove_version = GroveVersion::latest();
    let db = make_test_sum_tree_grovedb(grove_version);

    db.insert(
        [TEST_LEAF].as_ref(),
        b"a",
        Element::new_sum_item(4),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"b",
        Element::new_item_with_sum_item(b"payload".to_vec(), 6),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"c",
        Element::new_sum_item(8),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"d",
        Element::new_item_with_sum_item(b"more_data".to_vec(), 12),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");

    // Sum limit of 10: a(4) + b(6) = 10, should stop after b
    let aggregate_sum_query = AggregateSumQuery::new(10, None);
    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    let result = Element::get_aggregate_sum_query(
        &db.db,
        &aggregate_sum_path_query,
        AggregateSumQueryOptions::default(),
        None,
        grove_version,
    )
    .unwrap()
    .expect("expected successful get_query");

    assert_eq!(result.results, vec![(b"a".to_vec(), 4), (b"b".to_vec(), 6)]);
}

#[test]
fn test_item_with_sum_item_not_skipped_when_error_disabled() {
    // error_if_non_sum_item_found=false should only skip basic Items, not ItemWithSumItem
    let grove_version = GroveVersion::latest();
    let db = make_test_sum_tree_grovedb(grove_version);

    db.insert(
        [TEST_LEAF].as_ref(),
        b"a",
        Element::new_item(b"plain_item".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"b",
        Element::new_item_with_sum_item(b"hybrid".to_vec(), 9),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"c",
        Element::new_sum_item(3),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");

    // error_if_non_sum_item_found=false should skip "a" (basic Item) but keep "b" (ItemWithSumItem)
    let aggregate_sum_query = AggregateSumQuery::new(100, None);
    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    let result = Element::get_aggregate_sum_query(
        &db.db,
        &aggregate_sum_path_query,
        AggregateSumQueryOptions {
            error_if_non_sum_item_found: false,
            ..AggregateSumQueryOptions::default()
        },
        None,
        grove_version,
    )
    .unwrap()
    .expect("expected successful get_query");

    assert_eq!(result.results, vec![(b"b".to_vec(), 9), (b"c".to_vec(), 3)]);
}

#[test]
fn test_reference_to_sum_item_followed() {
    // A reference to a SumItem should be followed and resolve to the target's sum value
    let grove_version = GroveVersion::latest();
    let db = make_test_sum_tree_grovedb(grove_version);

    db.insert(
        [TEST_LEAF].as_ref(),
        b"a",
        Element::new_sum_item(7),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    // Insert a reference pointing to the sum item "a"
    db.insert(
        [TEST_LEAF].as_ref(),
        b"ref_a",
        Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"a".to_vec(),
        ])),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert reference");

    // Query for the reference key - should follow it and return the target's sum value
    let aggregate_sum_query = AggregateSumQuery::new_single_key(b"ref_a".to_vec(), 100);
    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    let result = Element::get_aggregate_sum_query(
        &db.db,
        &aggregate_sum_path_query,
        AggregateSumQueryOptions::default(),
        None,
        grove_version,
    )
    .unwrap()
    .expect("expected successful get_query");

    assert_eq!(result.results, vec![(b"ref_a".to_vec(), 7)]);
}

#[test]
fn test_reference_to_item_with_sum_item_followed() {
    // A reference to an ItemWithSumItem should be followed and resolve to the target's sum value
    let grove_version = GroveVersion::latest();
    let db = make_test_sum_tree_grovedb(grove_version);

    db.insert(
        [TEST_LEAF].as_ref(),
        b"hybrid",
        Element::new_item_with_sum_item(b"data".to_vec(), 15),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"ref_hybrid",
        Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"hybrid".to_vec(),
        ])),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert reference");

    let aggregate_sum_query = AggregateSumQuery::new_single_key(b"ref_hybrid".to_vec(), 100);
    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    let result = Element::get_aggregate_sum_query(
        &db.db,
        &aggregate_sum_path_query,
        AggregateSumQueryOptions::default(),
        None,
        grove_version,
    )
    .unwrap()
    .expect("expected successful get_query");

    assert_eq!(result.results, vec![(b"ref_hybrid".to_vec(), 15)]);
}

#[test]
fn test_reference_to_regular_item_errors() {
    // A reference that resolves to a regular Item (not a sum item) should error
    let grove_version = GroveVersion::latest();
    let db = make_test_sum_tree_grovedb(grove_version);

    db.insert(
        [TEST_LEAF].as_ref(),
        b"item",
        Element::new_item(b"not_a_sum_item".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"ref_item",
        Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"item".to_vec(),
        ])),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert reference");

    let aggregate_sum_query = AggregateSumQuery::new_single_key(b"ref_item".to_vec(), 100);
    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    let result = Element::get_aggregate_sum_query(
        &db.db,
        &aggregate_sum_path_query,
        AggregateSumQueryOptions::default(),
        None,
        grove_version,
    )
    .unwrap();

    assert!(
        result.is_err(),
        "expected error when reference target is not a sum item"
    );
}

#[test]
fn test_reference_to_sum_item_skipped_with_ignore_references() {
    // With ignore_references=true, references are silently dropped
    let grove_version = GroveVersion::latest();
    let db = make_test_sum_tree_grovedb(grove_version);

    db.insert(
        [TEST_LEAF].as_ref(),
        b"a",
        Element::new_sum_item(7),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"b",
        Element::new_sum_item(3),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    // Reference to sum item "a"
    db.insert(
        [TEST_LEAF].as_ref(),
        b"ref_a",
        Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"a".to_vec(),
        ])),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert reference");

    // Range query with ignore_references=true
    let aggregate_sum_query = AggregateSumQuery::new(100, None);
    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    let result = Element::get_aggregate_sum_query(
        &db.db,
        &aggregate_sum_path_query,
        AggregateSumQueryOptions {
            ignore_references: true,
            ..AggregateSumQueryOptions::default()
        },
        None,
        grove_version,
    )
    .unwrap()
    .expect("expected successful get_query");

    // Only sum items returned, reference silently skipped
    assert_eq!(result.results, vec![(b"a".to_vec(), 7), (b"b".to_vec(), 3)]);
}

#[test]
fn test_reference_to_item_skipped_with_ignore_references() {
    // Reference to a regular Item is also skipped with ignore_references=true
    let grove_version = GroveVersion::latest();
    let db = make_test_sum_tree_grovedb(grove_version);

    db.insert(
        [TEST_LEAF].as_ref(),
        b"a",
        Element::new_sum_item(7),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"item",
        Element::new_item(b"regular".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    // Reference to the regular item
    db.insert(
        [TEST_LEAF].as_ref(),
        b"ref_item",
        Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"item".to_vec(),
        ])),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert reference");

    // Range query skipping both items and references
    let aggregate_sum_query = AggregateSumQuery::new(100, None);
    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    let result = Element::get_aggregate_sum_query(
        &db.db,
        &aggregate_sum_path_query,
        AggregateSumQueryOptions {
            error_if_non_sum_item_found: false,
            ignore_references: true,
            ..AggregateSumQueryOptions::default()
        },
        None,
        grove_version,
    )
    .unwrap()
    .expect("expected successful get_query");

    // Only sum item "a" returned
    assert_eq!(result.results, vec![(b"a".to_vec(), 7)]);
}

#[test]
fn test_reference_to_item_with_sum_item_skipped_with_ignore_references() {
    // Reference to an ItemWithSumItem is also skipped with ignore_references=true
    let grove_version = GroveVersion::latest();
    let db = make_test_sum_tree_grovedb(grove_version);

    db.insert(
        [TEST_LEAF].as_ref(),
        b"a",
        Element::new_sum_item(5),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"hybrid",
        Element::new_item_with_sum_item(b"data".to_vec(), 10),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    // Reference to the ItemWithSumItem
    db.insert(
        [TEST_LEAF].as_ref(),
        b"ref_hybrid",
        Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"hybrid".to_vec(),
        ])),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert reference");

    // Range query skipping references only
    let aggregate_sum_query = AggregateSumQuery::new(100, None);
    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    let result = Element::get_aggregate_sum_query(
        &db.db,
        &aggregate_sum_path_query,
        AggregateSumQueryOptions {
            ignore_references: true,
            ..AggregateSumQueryOptions::default()
        },
        None,
        grove_version,
    )
    .unwrap()
    .expect("expected successful get_query");

    // Sum item and ItemWithSumItem returned, reference skipped
    assert_eq!(
        result.results,
        vec![(b"a".to_vec(), 5), (b"hybrid".to_vec(), 10)]
    );
}

#[test]
fn test_key_query_reference_skipped_with_ignore_references() {
    // Key query targeting a reference key with ignore_references=true
    let grove_version = GroveVersion::latest();
    let db = make_test_sum_tree_grovedb(grove_version);

    db.insert(
        [TEST_LEAF].as_ref(),
        b"a",
        Element::new_sum_item(7),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"ref_a",
        Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"a".to_vec(),
        ])),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert reference");

    // Key query for both the sum item and the reference
    let aggregate_sum_query =
        AggregateSumQuery::new_with_keys(vec![b"ref_a".to_vec(), b"a".to_vec()], 100, None);
    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    let result = Element::get_aggregate_sum_query(
        &db.db,
        &aggregate_sum_path_query,
        AggregateSumQueryOptions {
            ignore_references: true,
            ..AggregateSumQueryOptions::default()
        },
        None,
        grove_version,
    )
    .unwrap()
    .expect("expected successful get_query");

    // Only sum item "a" returned, reference silently skipped
    assert_eq!(result.results, vec![(b"a".to_vec(), 7)]);
}

#[test]
fn test_reference_does_not_decrement_limit_when_skipped() {
    // Skipped references should NOT count against the user limit
    let grove_version = GroveVersion::latest();
    let db = make_test_sum_tree_grovedb(grove_version);

    db.insert(
        [TEST_LEAF].as_ref(),
        b"a",
        Element::new_sum_item(5),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"b",
        Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"a".to_vec(),
        ])),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert reference");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"c",
        Element::new_sum_item(3),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");

    // limit=2 with ignore_references: a (sum_item, limit→1), b (ref, skipped), c (sum_item, limit→0)
    let aggregate_sum_query = AggregateSumQuery::new(100, Some(2));
    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    let result = Element::get_aggregate_sum_query(
        &db.db,
        &aggregate_sum_path_query,
        AggregateSumQueryOptions {
            ignore_references: true,
            ..AggregateSumQueryOptions::default()
        },
        None,
        grove_version,
    )
    .unwrap()
    .expect("expected successful get_query");

    // Both "a" and "c" returned — skipped ref "b" didn't consume a limit slot
    assert_eq!(result.results, vec![(b"a".to_vec(), 5), (b"c".to_vec(), 3)]);
}

#[test]
fn test_reference_followed_in_range_query() {
    // References encountered during range iteration should be followed, not just in key queries
    let grove_version = GroveVersion::latest();
    let db = make_test_sum_tree_grovedb(grove_version);

    db.insert(
        [TEST_LEAF].as_ref(),
        b"a",
        Element::new_sum_item(7),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"b",
        Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"a".to_vec(),
        ])),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert reference");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"c",
        Element::new_sum_item(3),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");

    // Range query should follow the reference at "b" and resolve to sum value 7
    let aggregate_sum_query = AggregateSumQuery::new(100, None);
    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    let result = Element::get_aggregate_sum_query(
        &db.db,
        &aggregate_sum_path_query,
        AggregateSumQueryOptions::default(),
        None,
        grove_version,
    )
    .unwrap()
    .expect("expected successful get_query");

    assert_eq!(
        result.results,
        vec![(b"a".to_vec(), 7), (b"b".to_vec(), 7), (b"c".to_vec(), 3)]
    );
}

#[test]
fn test_multi_hop_reference_chain() {
    // ref_c → ref_b → sum_item_a: should follow 2 hops and resolve
    let grove_version = GroveVersion::latest();
    let db = make_test_sum_tree_grovedb(grove_version);

    db.insert(
        [TEST_LEAF].as_ref(),
        b"a",
        Element::new_sum_item(42),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    // ref_b points to sum_item "a"
    db.insert(
        [TEST_LEAF].as_ref(),
        b"ref_b",
        Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"a".to_vec(),
        ])),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert reference");
    // ref_c points to ref_b (chain: ref_c → ref_b → a)
    db.insert(
        [TEST_LEAF].as_ref(),
        b"ref_c",
        Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"ref_b".to_vec(),
        ])),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert reference");

    // Key query for the double-hop reference
    let aggregate_sum_query = AggregateSumQuery::new_single_key(b"ref_c".to_vec(), 100);
    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    let result = Element::get_aggregate_sum_query(
        &db.db,
        &aggregate_sum_path_query,
        AggregateSumQueryOptions::default(),
        None,
        grove_version,
    )
    .unwrap()
    .expect("expected successful get_query");

    assert_eq!(result.results, vec![(b"ref_c".to_vec(), 42)]);
}

#[test]
fn test_reference_limit_exceeded() {
    // Chain of 5 references exceeds MAX_AGGREGATE_REFERENCE_HOPS (3).
    // ref_a → ref_b → ref_c → ref_d → ref_e → target
    // The initial convert gives us ref_b's path. Then in the loop:
    // ref_b (hop 3→2), ref_c (2→1), ref_d (1→0), ref_e (hops_left==0 → error)
    let grove_version = GroveVersion::latest();
    let db = make_test_sum_tree_grovedb(grove_version);

    db.insert(
        [TEST_LEAF].as_ref(),
        b"target",
        Element::new_sum_item(99),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    // ref_e → target
    db.insert(
        [TEST_LEAF].as_ref(),
        b"ref_e",
        Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"target".to_vec(),
        ])),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert reference");
    // ref_d → ref_e
    db.insert(
        [TEST_LEAF].as_ref(),
        b"ref_d",
        Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"ref_e".to_vec(),
        ])),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert reference");
    // ref_c → ref_d
    db.insert(
        [TEST_LEAF].as_ref(),
        b"ref_c",
        Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"ref_d".to_vec(),
        ])),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert reference");
    // ref_b → ref_c
    db.insert(
        [TEST_LEAF].as_ref(),
        b"ref_b",
        Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"ref_c".to_vec(),
        ])),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert reference");
    // ref_a → ref_b
    db.insert(
        [TEST_LEAF].as_ref(),
        b"ref_a",
        Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"ref_b".to_vec(),
        ])),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert reference");

    let aggregate_sum_query = AggregateSumQuery::new_single_key(b"ref_a".to_vec(), 100);
    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    let result = Element::get_aggregate_sum_query(
        &db.db,
        &aggregate_sum_path_query,
        AggregateSumQueryOptions::default(),
        None,
        grove_version,
    )
    .unwrap();

    assert!(
        result.is_err(),
        "expected ReferenceLimit error for 5-reference chain (4 intermediate hops)"
    );
}

#[test]
fn test_reference_at_max_hops_succeeds() {
    // Chain of exactly MAX_AGGREGATE_REFERENCE_HOPS (3) intermediate hops should succeed.
    // ref_a → ref_b → ref_c → ref_d → target
    // The initial convert gives us ref_b's path. Then:
    // ref_b (hop 3→2), ref_c (2→1), ref_d (1→0), target resolved → success
    let grove_version = GroveVersion::latest();
    let db = make_test_sum_tree_grovedb(grove_version);

    db.insert(
        [TEST_LEAF].as_ref(),
        b"target",
        Element::new_sum_item(99),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"ref_d",
        Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"target".to_vec(),
        ])),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert reference");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"ref_c",
        Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"ref_d".to_vec(),
        ])),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert reference");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"ref_b",
        Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"ref_c".to_vec(),
        ])),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert reference");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"ref_a",
        Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"ref_b".to_vec(),
        ])),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert reference");

    let aggregate_sum_query = AggregateSumQuery::new_single_key(b"ref_a".to_vec(), 100);
    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    let result = Element::get_aggregate_sum_query(
        &db.db,
        &aggregate_sum_path_query,
        AggregateSumQueryOptions::default(),
        None,
        grove_version,
    )
    .unwrap()
    .expect("4-reference chain (3 intermediate hops) should succeed");

    assert_eq!(result.results, vec![(b"ref_a".to_vec(), 99)]);
}

#[test]
fn test_reference_to_item_skipped_after_following_when_error_disabled() {
    // Reference → regular Item with error_if_non_sum_item_found=false should silently skip
    let grove_version = GroveVersion::latest();
    let db = make_test_sum_tree_grovedb(grove_version);

    db.insert(
        [TEST_LEAF].as_ref(),
        b"a",
        Element::new_sum_item(5),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"item",
        Element::new_item(b"not_a_sum".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    // Reference to the regular Item
    db.insert(
        [TEST_LEAF].as_ref(),
        b"ref_item",
        Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"item".to_vec(),
        ])),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert reference");

    // With error_if_non_sum_item_found=false: following the reference resolves to a regular Item,
    // which should be silently skipped
    let aggregate_sum_query =
        AggregateSumQuery::new_with_keys(vec![b"a".to_vec(), b"ref_item".to_vec()], 100, None);
    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    let result = Element::get_aggregate_sum_query(
        &db.db,
        &aggregate_sum_path_query,
        AggregateSumQueryOptions {
            error_if_non_sum_item_found: false,
            ..AggregateSumQueryOptions::default()
        },
        None,
        grove_version,
    )
    .unwrap()
    .expect("expected successful get_query");

    // Only "a" returned; reference to Item was followed then skipped
    assert_eq!(result.results, vec![(b"a".to_vec(), 5)]);
}

#[test]
fn test_negative_sum_values() {
    // Negative SumItem values should work correctly with sum_limit
    let grove_version = GroveVersion::latest();
    let db = make_test_sum_tree_grovedb(grove_version);

    db.insert(
        [TEST_LEAF].as_ref(),
        b"a",
        Element::new_sum_item(10),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"b",
        Element::new_sum_item(-3),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"c",
        Element::new_sum_item(5),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");

    // sum_limit=12: a(10) leaves 2, b(-3) increases remaining to 5, c(5) leaves 0
    // All three should be returned
    let aggregate_sum_query = AggregateSumQuery::new(12, None);
    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    let result = Element::get_aggregate_sum_query(
        &db.db,
        &aggregate_sum_path_query,
        AggregateSumQueryOptions::default(),
        None,
        grove_version,
    )
    .unwrap()
    .expect("expected successful get_query");

    assert_eq!(
        result.results,
        vec![(b"a".to_vec(), 10), (b"b".to_vec(), -3), (b"c".to_vec(), 5),]
    );

    // sum_limit=8: a(10) leaves -2, which is <= 0, so stop after a
    let aggregate_sum_query = AggregateSumQuery::new(8, None);
    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    let result = Element::get_aggregate_sum_query(
        &db.db,
        &aggregate_sum_path_query,
        AggregateSumQueryOptions::default(),
        None,
        grove_version,
    )
    .unwrap()
    .expect("expected successful get_query");

    assert_eq!(result.results, vec![(b"a".to_vec(), 10)]);
}

#[test]
fn test_hard_limit_with_key_queries() {
    // Hard limit should also work with key queries
    let mut custom_version = GroveVersion::latest().clone();
    custom_version
        .grovedb_versions
        .query_limits
        .max_aggregate_sum_query_elements_scanned = 2;

    let db = make_test_sum_tree_grovedb(&custom_version);

    db.insert(
        [TEST_LEAF].as_ref(),
        b"a",
        Element::new_sum_item(1),
        None,
        None,
        &custom_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"b",
        Element::new_sum_item(2),
        None,
        None,
        &custom_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"c",
        Element::new_sum_item(3),
        None,
        None,
        &custom_version,
    )
    .unwrap()
    .expect("cannot insert element");

    // Key query for 3 keys with hard limit of 2
    let aggregate_sum_query = AggregateSumQuery::new_with_keys(
        vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()],
        100,
        None,
    );
    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    let result = Element::get_aggregate_sum_query(
        &db.db,
        &aggregate_sum_path_query,
        AggregateSumQueryOptions::default(),
        None,
        &custom_version,
    )
    .unwrap()
    .expect("expected successful get_query (partial results)");

    // Should return only first 2 elements due to hard limit
    assert_eq!(result.results, vec![(b"a".to_vec(), 1), (b"b".to_vec(), 2)]);
    assert!(result.hard_limit_reached, "hard limit should be flagged");
}

#[test]
fn test_error_if_intermediate_path_tree_not_present_false() {
    // With error_if_intermediate_path_tree_not_present=false, a missing path
    // should be treated as empty rather than erroring
    let grove_version = GroveVersion::latest();
    let db = make_test_sum_tree_grovedb(grove_version);

    // Query a path that doesn't exist
    let aggregate_sum_query = AggregateSumQuery::new_single_key(b"a".to_vec(), 100);
    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![b"nonexistent_path".to_vec()],
        aggregate_sum_query,
    };

    // With default (true), this should error
    let result = Element::get_aggregate_sum_query(
        &db.db,
        &aggregate_sum_path_query,
        AggregateSumQueryOptions::default(),
        None,
        grove_version,
    )
    .unwrap();
    assert!(result.is_err(), "expected error with default options");

    // With error_if_intermediate_path_tree_not_present=false, should return empty
    let result = Element::get_aggregate_sum_query(
        &db.db,
        &aggregate_sum_path_query,
        AggregateSumQueryOptions {
            error_if_intermediate_path_tree_not_present: false,
            ..AggregateSumQueryOptions::default()
        },
        None,
        grove_version,
    )
    .unwrap()
    .expect("expected successful get_query with missing path treated as empty");

    assert!(result.results.is_empty());
}

#[test]
fn test_descending_hard_limit() {
    // Hard limit should work in descending (right-to-left) queries
    let mut custom_version = GroveVersion::latest().clone();
    custom_version
        .grovedb_versions
        .query_limits
        .max_aggregate_sum_query_elements_scanned = 2;

    let db = make_test_sum_tree_grovedb(&custom_version);

    db.insert(
        [TEST_LEAF].as_ref(),
        b"a",
        Element::new_sum_item(1),
        None,
        None,
        &custom_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"b",
        Element::new_sum_item(2),
        None,
        None,
        &custom_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"c",
        Element::new_sum_item(3),
        None,
        None,
        &custom_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"d",
        Element::new_sum_item(4),
        None,
        None,
        &custom_version,
    )
    .unwrap()
    .expect("cannot insert element");

    // Descending range query with hard limit of 2
    let mut aggregate_sum_query = AggregateSumQuery::new(100, None);
    aggregate_sum_query.left_to_right = false;

    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    let result = Element::get_aggregate_sum_query(
        &db.db,
        &aggregate_sum_path_query,
        AggregateSumQueryOptions::default(),
        None,
        &custom_version,
    )
    .unwrap()
    .expect("expected successful get_query (partial results)");

    // Descending: d(4), c(3) — hard limit reached after 2 elements
    assert_eq!(result.results, vec![(b"d".to_vec(), 4), (b"c".to_vec(), 3)]);
    assert!(result.hard_limit_reached, "hard limit should be flagged");
}

#[test]
fn test_descending_range_skip_non_sum_items() {
    // Descending range query should skip non-sum items correctly
    let grove_version = GroveVersion::latest();
    let db = make_test_sum_tree_grovedb(grove_version);

    db.insert(
        [TEST_LEAF].as_ref(),
        b"a",
        Element::new_sum_item(1),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"b",
        Element::new_item(b"regular".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"c",
        Element::new_sum_item(3),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");

    let mut aggregate_sum_query = AggregateSumQuery::new(100, None);
    aggregate_sum_query.left_to_right = false;

    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    let result = Element::get_aggregate_sum_query(
        &db.db,
        &aggregate_sum_path_query,
        AggregateSumQueryOptions {
            error_if_non_sum_item_found: false,
            ..AggregateSumQueryOptions::default()
        },
        None,
        grove_version,
    )
    .unwrap()
    .expect("expected successful get_query");

    // Descending: c(3), b(item, skipped), a(1)
    assert_eq!(result.results, vec![(b"c".to_vec(), 3), (b"a".to_vec(), 1)]);
}

#[test]
fn test_key_query_skip_with_limit() {
    // Key query with error_if_non_sum_item_found=false and limit:
    // skipped elements should not consume limit slots
    let grove_version = GroveVersion::latest();
    let db = make_test_sum_tree_grovedb(grove_version);

    db.insert(
        [TEST_LEAF].as_ref(),
        b"a",
        Element::new_item(b"regular".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"b",
        Element::new_sum_item(5),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"c",
        Element::new_sum_item(3),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");

    // Key query for all three with limit=1
    let aggregate_sum_query = AggregateSumQuery::new_with_keys(
        vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()],
        100,
        Some(1),
    );
    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    let result = Element::get_aggregate_sum_query(
        &db.db,
        &aggregate_sum_path_query,
        AggregateSumQueryOptions {
            error_if_non_sum_item_found: false,
            ..AggregateSumQueryOptions::default()
        },
        None,
        grove_version,
    )
    .unwrap()
    .expect("expected successful get_query");

    // "a" is skipped (Item, no limit consumed), "b" returned (limit→0), "c" not reached
    assert_eq!(result.results, vec![(b"b".to_vec(), 5)]);
}

#[test]
fn test_descending_reference_followed() {
    // References should be followed in descending range queries too
    let grove_version = GroveVersion::latest();
    let db = make_test_sum_tree_grovedb(grove_version);

    db.insert(
        [TEST_LEAF].as_ref(),
        b"target",
        Element::new_sum_item(42),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"a",
        Element::new_sum_item(1),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert element");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"r",
        Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"target".to_vec(),
        ])),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("cannot insert reference");

    let mut aggregate_sum_query = AggregateSumQuery::new(100, None);
    aggregate_sum_query.left_to_right = false;

    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    let result = Element::get_aggregate_sum_query(
        &db.db,
        &aggregate_sum_path_query,
        AggregateSumQueryOptions::default(),
        None,
        grove_version,
    )
    .unwrap()
    .expect("expected successful get_query");

    // Descending: target(42), r(ref→target=42), a(1)
    assert_eq!(
        result.results,
        vec![
            (b"target".to_vec(), 42),
            (b"r".to_vec(), 42),
            (b"a".to_vec(), 1),
        ]
    );
}

#[test]
fn test_cyclic_reference_detected_in_aggregate_sum_query() {
    // Two references form a cycle: ref_a -> ref_b -> ref_a.
    // Before the fix, this would waste reads cycling through hops until hitting
    // MAX_AGGREGATE_REFERENCE_HOPS and returning ReferenceLimit.
    // After the fix, the visited set detects the cycle immediately and returns
    // CyclicReference with a more accurate error.
    let grove_version = GroveVersion::latest();
    let db = make_test_sum_tree_grovedb(grove_version);

    let tx = db.start_transaction();

    // Use MerkCache to insert cyclic references at the Merk level,
    // bypassing GroveDB-level validation that would reject them.
    {
        let cache = MerkCache::new(&db, &tx, grove_version);
        let path: SubtreePath<&[u8]> = SubtreePath::from(&[TEST_LEAF] as &[&[u8]]);

        // ref_a points to [TEST_LEAF, "ref_b"]
        let ref_a = Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"ref_b".to_vec(),
        ]));

        // ref_b points to [TEST_LEAF, "ref_a"]
        let ref_b = Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"ref_a".to_vec(),
        ]));

        let mut merk = cache
            .get_merk(path.derive_owned())
            .unwrap()
            .expect("should open merk");

        merk.for_merk(|m| {
            ref_a
                .insert_reference(m, b"ref_a", NULL_HASH, None, grove_version)
                .unwrap()
                .expect("should insert ref_a at merk level");
        });

        merk.for_merk(|m| {
            ref_b
                .insert_reference(m, b"ref_b", NULL_HASH, None, grove_version)
                .unwrap()
                .expect("should insert ref_b at merk level");
        });

        drop(merk);

        // Commit the batch to make the writes visible in the transaction
        let batch = cache.into_batch().unwrap().expect("should produce batch");
        db.db
            .commit_multi_context_batch(*batch, Some(&tx))
            .unwrap()
            .expect("should commit batch");
    }

    // Query for ref_a which forms a cycle: ref_a -> ref_b -> ref_a -> ...
    let aggregate_sum_query = AggregateSumQuery::new_single_key(b"ref_a".to_vec(), 100);
    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    let result = Element::get_aggregate_sum_query(
        &db.db,
        &aggregate_sum_path_query,
        AggregateSumQueryOptions::default(),
        Some(&tx),
        grove_version,
    )
    .unwrap();

    assert!(
        matches!(result, Err(Error::CyclicReference)),
        "expected CyclicReference error for cyclic ref_a -> ref_b -> ref_a, got: {:?}",
        result
    );
}

#[test]
fn test_self_referencing_element_detected_in_aggregate_sum_query() {
    // A reference that points to itself: ref_self -> ref_self.
    // The visited set should detect this immediately on the second iteration.
    let grove_version = GroveVersion::latest();
    let db = make_test_sum_tree_grovedb(grove_version);

    let tx = db.start_transaction();

    {
        let cache = MerkCache::new(&db, &tx, grove_version);
        let path: SubtreePath<&[u8]> = SubtreePath::from(&[TEST_LEAF] as &[&[u8]]);

        // ref_self points to itself: [TEST_LEAF, "ref_self"]
        let ref_self = Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"ref_self".to_vec(),
        ]));

        let mut merk = cache
            .get_merk(path.derive_owned())
            .unwrap()
            .expect("should open merk");

        merk.for_merk(|m| {
            ref_self
                .insert_reference(m, b"ref_self", NULL_HASH, None, grove_version)
                .unwrap()
                .expect("should insert ref_self at merk level");
        });

        drop(merk);

        let batch = cache.into_batch().unwrap().expect("should produce batch");
        db.db
            .commit_multi_context_batch(*batch, Some(&tx))
            .unwrap()
            .expect("should commit batch");
    }

    let aggregate_sum_query = AggregateSumQuery::new_single_key(b"ref_self".to_vec(), 100);
    let aggregate_sum_path_query = AggregateSumPathQuery {
        path: vec![TEST_LEAF.to_vec()],
        aggregate_sum_query,
    };

    let result = Element::get_aggregate_sum_query(
        &db.db,
        &aggregate_sum_path_query,
        AggregateSumQueryOptions::default(),
        Some(&tx),
        grove_version,
    )
    .unwrap();

    assert!(
        matches!(result, Err(Error::CyclicReference)),
        "expected CyclicReference error for self-referencing element, got: {:?}",
        result
    );
}
