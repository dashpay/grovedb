use grovedb_query::{ProofItems, QueryItem};

fn k(v: u8) -> Vec<u8> {
    vec![v]
}

#[test]
fn proof_items_partition_and_process_key_with_boundaries() {
    let query_items = vec![QueryItem::RangeAfter(k(5)..), QueryItem::Range(k(3)..k(7))];

    let (proof_items, params) = ProofItems::new_with_query_items(&query_items, false);
    assert!(!params.left_to_right);
    assert!(!proof_items.has_no_query_items());

    let target = k(5);
    let (present, on_boundary, left, right) = proof_items.process_key(&target);

    assert!(present);
    assert!(on_boundary);
    assert!(!left.has_no_query_items());
    assert!(!right.has_no_query_items());
}
