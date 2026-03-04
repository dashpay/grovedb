use bincode::{config::standard, decode_from_slice, encode_to_vec};

use grovedb_query::{AggregateSumQuery, QueryItem};

// ---------------------------------------------------------------------------
// Constructor tests (mod.rs)
// ---------------------------------------------------------------------------

#[test]
fn new_and_new_range_full_are_equivalent() {
    let q = AggregateSumQuery::new(100, Some(5));
    assert_eq!(q.items, vec![QueryItem::RangeFull(..)]);
    assert!(q.left_to_right);
    assert_eq!(q.sum_limit, 100);
    assert_eq!(q.limit_of_items_to_check, Some(5));

    let q2 = AggregateSumQuery::new_range_full(100, Some(5));
    assert_eq!(q, q2);
}

#[test]
fn new_descending_and_new_range_full_descending_are_equivalent() {
    let q = AggregateSumQuery::new_descending(42, None);
    assert!(!q.left_to_right);
    assert_eq!(q.items, vec![QueryItem::RangeFull(..)]);
    assert_eq!(q.sum_limit, 42);
    assert_eq!(q.limit_of_items_to_check, None);

    let q2 = AggregateSumQuery::new_range_full_descending(42, None);
    assert_eq!(q, q2);
}

#[test]
fn new_single_key() {
    let q = AggregateSumQuery::new_single_key(vec![1, 2, 3], 99);
    assert_eq!(q.items, vec![QueryItem::Key(vec![1, 2, 3])]);
    assert!(q.left_to_right);
    assert_eq!(q.sum_limit, 99);
    assert_eq!(q.limit_of_items_to_check, Some(1));
}

#[test]
fn new_single_query_item() {
    let item = QueryItem::Range(vec![0]..vec![10]);
    let q = AggregateSumQuery::new_single_query_item(item.clone(), 50, Some(3));
    assert_eq!(q.items, vec![item]);
    assert!(q.left_to_right);
    assert_eq!(q.sum_limit, 50);
    assert_eq!(q.limit_of_items_to_check, Some(3));
}

#[test]
fn new_with_query_items() {
    let items = vec![
        QueryItem::Key(vec![1]),
        QueryItem::Key(vec![2]),
        QueryItem::Key(vec![3]),
    ];
    let q = AggregateSumQuery::new_with_query_items(items.clone(), 10, Some(10));
    assert_eq!(q.items, items);
    assert!(q.left_to_right);
}

#[test]
fn new_with_keys() {
    let keys = vec![vec![10], vec![20]];
    let q = AggregateSumQuery::new_with_keys(keys, 77, None);
    assert_eq!(
        q.items,
        vec![QueryItem::Key(vec![10]), QueryItem::Key(vec![20])]
    );
    assert!(q.left_to_right);
    assert_eq!(q.sum_limit, 77);
    assert_eq!(q.limit_of_items_to_check, None);
}

#[test]
fn new_with_keys_reversed() {
    let keys = vec![vec![10], vec![20]];
    let q = AggregateSumQuery::new_with_keys_reversed(keys, 55, Some(4));
    assert!(!q.left_to_right);
    assert_eq!(q.sum_limit, 55);
    assert_eq!(q.limit_of_items_to_check, Some(4));
}

#[test]
fn new_single_query_item_with_direction() {
    let item = QueryItem::Key(vec![7]);
    let q = AggregateSumQuery::new_single_query_item_with_direction(item.clone(), false, 8, None);
    assert!(!q.left_to_right);
    assert_eq!(q.items, vec![item]);

    let q2 = AggregateSumQuery::new_single_query_item_with_direction(
        QueryItem::Key(vec![7]),
        true,
        8,
        None,
    );
    assert!(q2.left_to_right);
}

// ---------------------------------------------------------------------------
// Iterator tests
// ---------------------------------------------------------------------------

#[test]
fn iter_forward() {
    let q = AggregateSumQuery::new_with_keys(vec![vec![1], vec![2], vec![3]], 100, None);
    let collected: Vec<_> = q.iter().cloned().collect();
    assert_eq!(
        collected,
        vec![
            QueryItem::Key(vec![1]),
            QueryItem::Key(vec![2]),
            QueryItem::Key(vec![3]),
        ]
    );
}

#[test]
fn rev_iter_reverse() {
    let q = AggregateSumQuery::new_with_keys(vec![vec![1], vec![2], vec![3]], 100, None);
    let collected: Vec<_> = q.rev_iter().cloned().collect();
    assert_eq!(
        collected,
        vec![
            QueryItem::Key(vec![3]),
            QueryItem::Key(vec![2]),
            QueryItem::Key(vec![1]),
        ]
    );
}

#[test]
fn directional_iter_delegates_correctly() {
    let q = AggregateSumQuery::new_with_keys(vec![vec![1], vec![2]], 100, None);

    let fwd: Vec<_> = q.directional_iter(true).cloned().collect();
    let rev: Vec<_> = q.directional_iter(false).cloned().collect();

    assert_eq!(fwd, vec![QueryItem::Key(vec![1]), QueryItem::Key(vec![2])]);
    assert_eq!(rev, vec![QueryItem::Key(vec![2]), QueryItem::Key(vec![1])]);
}

#[test]
fn has_only_keys_true_for_keys() {
    let q = AggregateSumQuery::new_with_keys(vec![vec![1], vec![2]], 10, None);
    assert!(q.has_only_keys());
}

#[test]
fn has_only_keys_false_with_range() {
    let mut q = AggregateSumQuery::new_with_keys(vec![vec![1]], 10, None);
    q.insert_range(vec![5]..vec![10]);
    assert!(!q.has_only_keys());
}

// ---------------------------------------------------------------------------
// Display test
// ---------------------------------------------------------------------------

#[test]
fn display_includes_direction_and_sum_limit() {
    let q = AggregateSumQuery::new(42, None);
    let s = format!("{}", q);
    assert!(s.contains("→"), "ascending should have right arrow");
    assert!(s.contains("42"), "should contain sum_limit");

    let q2 = AggregateSumQuery::new_descending(99, None);
    let s2 = format!("{}", q2);
    assert!(s2.contains("←"), "descending should have left arrow");
    assert!(s2.contains("99"));
}

// ---------------------------------------------------------------------------
// Insert tests (insert.rs)
// ---------------------------------------------------------------------------

#[test]
fn insert_key() {
    let mut q = AggregateSumQuery::new_with_keys(vec![], 10, None);
    q.insert_key(vec![5]);
    assert_eq!(q.items, vec![QueryItem::Key(vec![5])]);
}

#[test]
fn insert_keys() {
    let mut q = AggregateSumQuery::new_with_keys(vec![], 10, None);
    q.insert_keys(vec![vec![1], vec![2], vec![3]]);
    assert_eq!(
        q.items,
        vec![
            QueryItem::Key(vec![1]),
            QueryItem::Key(vec![2]),
            QueryItem::Key(vec![3]),
        ]
    );
}

#[test]
fn insert_range() {
    let mut q = AggregateSumQuery::new_with_keys(vec![], 10, None);
    q.insert_range(vec![1]..vec![5]);
    assert_eq!(q.items, vec![QueryItem::Range(vec![1]..vec![5])]);
}

#[test]
fn insert_range_inclusive() {
    let mut q = AggregateSumQuery::new_with_keys(vec![], 10, None);
    q.insert_range_inclusive(vec![1]..=vec![5]);
    assert_eq!(q.items, vec![QueryItem::RangeInclusive(vec![1]..=vec![5])]);
}

#[test]
fn insert_range_from() {
    let mut q = AggregateSumQuery::new_with_keys(vec![], 10, None);
    q.insert_range_from(vec![3]..);
    assert_eq!(q.items, vec![QueryItem::RangeFrom(vec![3]..)]);
}

#[test]
fn insert_range_to() {
    let mut q = AggregateSumQuery::new_with_keys(vec![], 10, None);
    q.insert_range_to(..vec![7]);
    assert_eq!(q.items, vec![QueryItem::RangeTo(..vec![7])]);
}

#[test]
fn insert_range_to_inclusive() {
    let mut q = AggregateSumQuery::new_with_keys(vec![], 10, None);
    q.insert_range_to_inclusive(..=vec![7]);
    assert_eq!(q.items, vec![QueryItem::RangeToInclusive(..=vec![7])]);
}

#[test]
fn insert_range_after() {
    let mut q = AggregateSumQuery::new_with_keys(vec![], 10, None);
    q.insert_range_after(vec![2]..);
    assert_eq!(q.items, vec![QueryItem::RangeAfter(vec![2]..)]);
}

#[test]
fn insert_range_after_to() {
    let mut q = AggregateSumQuery::new_with_keys(vec![], 10, None);
    q.insert_range_after_to(vec![2]..vec![5]);
    assert_eq!(q.items, vec![QueryItem::RangeAfterTo(vec![2]..vec![5])]);
}

#[test]
fn insert_range_after_to_inclusive() {
    let mut q = AggregateSumQuery::new_with_keys(vec![], 10, None);
    q.insert_range_after_to_inclusive(vec![2]..=vec![5]);
    assert_eq!(
        q.items,
        vec![QueryItem::RangeAfterToInclusive(vec![2]..=vec![5])]
    );
}

#[test]
fn insert_all_replaces_items_with_range_full() {
    let mut q = AggregateSumQuery::new_with_keys(vec![vec![1], vec![2]], 10, None);
    q.insert_all();
    assert_eq!(q.items, vec![QueryItem::RangeFull(..)]);
}

#[test]
fn insert_item_merges_overlapping_ranges() {
    let mut q = AggregateSumQuery::new_with_keys(vec![], 10, None);
    q.insert_range(vec![1]..vec![5]);
    q.insert_range(vec![3]..vec![8]);
    // The two overlapping ranges should be merged into one
    assert_eq!(q.items.len(), 1);
    assert_eq!(q.items[0], QueryItem::Range(vec![1]..vec![8]));
}

#[test]
fn insert_items_batch() {
    let mut q = AggregateSumQuery::new_with_keys(vec![], 10, None);
    q.insert_items(vec![
        QueryItem::Key(vec![1]),
        QueryItem::Key(vec![2]),
        QueryItem::Key(vec![3]),
    ]);
    assert_eq!(q.items.len(), 3);
}

// ---------------------------------------------------------------------------
// Merge tests (merge.rs)
// ---------------------------------------------------------------------------

#[test]
fn merge_multiple_empty_returns_noop() {
    let result = AggregateSumQuery::merge_multiple(vec![]).unwrap();
    assert_eq!(result.sum_limit, 0);
    assert_eq!(result.limit_of_items_to_check, Some(0));
}

#[test]
fn merge_multiple_single_returns_equivalent() {
    let q = AggregateSumQuery::new_with_keys(vec![vec![1]], 42, Some(3));
    let result = AggregateSumQuery::merge_multiple(vec![q.clone()]).unwrap();
    assert_eq!(result.items, q.items);
    assert_eq!(result.sum_limit, q.sum_limit);
    assert_eq!(result.limit_of_items_to_check, q.limit_of_items_to_check);
    assert_eq!(result.left_to_right, q.left_to_right);
}

#[test]
fn merge_multiple_two_queries() {
    let q1 = AggregateSumQuery::new_with_keys(vec![vec![1]], 10, Some(2));
    let q2 = AggregateSumQuery::new_with_keys(vec![vec![2]], 20, Some(3));
    let result = AggregateSumQuery::merge_multiple(vec![q1, q2]).unwrap();
    assert_eq!(result.sum_limit, 30);
    assert_eq!(result.limit_of_items_to_check, Some(5));
    assert_eq!(result.items.len(), 2);
}

#[test]
fn merge_multiple_differing_left_to_right_errors() {
    let q1 = AggregateSumQuery::new(10, None);
    let q2 = AggregateSumQuery::new_descending(20, None);
    let err = AggregateSumQuery::merge_multiple(vec![q1, q2]).unwrap_err();
    assert!(err.to_string().contains("left_to_right"));
}

#[test]
fn merge_multiple_sum_limit_overflow_errors() {
    let q1 = AggregateSumQuery::new(u64::MAX, None);
    let q2 = AggregateSumQuery::new(1, None);
    let err = AggregateSumQuery::merge_multiple(vec![q1, q2]).unwrap_err();
    assert!(err.to_string().contains("overflow") || err.to_string().contains("Overflow"));
}

#[test]
fn merge_multiple_none_limit_plus_some_limit_gives_none() {
    let q1 = AggregateSumQuery::new(10, None);
    let q2 = AggregateSumQuery::new(10, Some(5));
    let result = AggregateSumQuery::merge_multiple(vec![q1, q2]).unwrap();
    assert_eq!(result.limit_of_items_to_check, None);
}

#[test]
fn merge_multiple_limit_overflow_errors() {
    let q1 = AggregateSumQuery::new(1, Some(u16::MAX));
    let q2 = AggregateSumQuery::new(1, Some(1));
    let err = AggregateSumQuery::merge_multiple(vec![q1, q2]).unwrap_err();
    assert!(err.to_string().contains("overflow") || err.to_string().contains("Overflow"));
}

#[test]
fn merge_with_adds_items_and_limits() {
    let mut q1 = AggregateSumQuery::new_with_keys(vec![vec![1]], 10, Some(2));
    let q2 = AggregateSumQuery::new_with_keys(vec![vec![2]], 20, Some(3));
    q1.merge_with(q2).unwrap();
    assert_eq!(q1.sum_limit, 30);
    assert_eq!(q1.limit_of_items_to_check, Some(5));
    assert_eq!(q1.items.len(), 2);
}

#[test]
fn merge_with_differing_direction_errors() {
    let mut q1 = AggregateSumQuery::new(10, None);
    let q2 = AggregateSumQuery::new_descending(20, None);
    let err = q1.merge_with(q2).unwrap_err();
    assert!(err.to_string().contains("left_to_right"));
}

#[test]
fn merge_with_sum_limit_overflow_errors() {
    let mut q1 = AggregateSumQuery::new(u64::MAX, None);
    let q2 = AggregateSumQuery::new(1, None);
    let err = q1.merge_with(q2).unwrap_err();
    assert!(err.to_string().contains("overflow") || err.to_string().contains("Overflow"));
}

#[test]
fn merge_with_limit_overflow_errors() {
    let mut q1 = AggregateSumQuery::new(1, Some(u16::MAX));
    let q2 = AggregateSumQuery::new(1, Some(1));
    let err = q1.merge_with(q2).unwrap_err();
    assert!(err.to_string().contains("overflow") || err.to_string().contains("Overflow"));
}

#[test]
fn merge_with_none_limit_gives_none() {
    let mut q1 = AggregateSumQuery::new(5, Some(3));
    let q2 = AggregateSumQuery::new(5, None);
    q1.merge_with(q2).unwrap();
    assert_eq!(q1.limit_of_items_to_check, None);
}

// ---------------------------------------------------------------------------
// Encode / decode round-trip
// ---------------------------------------------------------------------------

#[test]
fn encode_decode_round_trip() {
    let q = AggregateSumQuery::new_with_keys(vec![vec![1], vec![2]], 42, Some(7));
    let encoded = encode_to_vec(&q, standard()).expect("encode");
    let (decoded, consumed): (AggregateSumQuery, usize) =
        decode_from_slice(&encoded, standard()).expect("decode");
    assert_eq!(consumed, encoded.len());
    assert_eq!(decoded, q);
}
