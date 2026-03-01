use grovedb_query::{
    hex_to_ascii,
    proofs::{Node, NULL_HASH},
    ProofItems, ProofStatus, Query, QueryItem, SubqueryBranch,
};

fn k(v: u8) -> Vec<u8> {
    vec![v]
}

#[test]
fn hex_to_ascii_returns_ascii_or_hex() {
    let ascii = b"Abc_-/\\[]@09";
    assert_eq!(hex_to_ascii(ascii), "Abc_-/\\[]@09");

    let non_ascii = [0_u8, 255_u8];
    assert_eq!(hex_to_ascii(&non_ascii), "0x00ff");
}

#[test]
fn proof_status_limit_update_and_hit_limit() {
    let status = ProofStatus::new_with_limit(Some(2));
    assert!(!status.hit_limit());

    let unchanged = status.update_limit(None);
    assert_eq!(unchanged.limit, Some(2));

    let exhausted = unchanged.update_limit(Some(0));
    assert!(exhausted.hit_limit());
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

#[test]
fn subquery_branch_max_depth_and_display_are_stable() {
    let mut nested = Query::new_single_key(k(1));
    nested.set_subquery_path(vec![k(2)]);

    let branch = SubqueryBranch {
        subquery_path: Some(vec![k(9)]),
        subquery: Some(Box::new(nested)),
    };

    assert_eq!(SubqueryBranch::default().max_depth(), Some(0));
    assert_eq!(branch.max_depth(), Some(3));

    let rendered = format!("{}", branch);
    assert!(rendered.contains("subquery_path"));
    assert!(rendered.contains("subquery"));
}

#[test]
fn display_impls_cover_key_rendering() {
    let node = Node::KV(b"key".to_vec(), b"value".to_vec());
    let rendered = format!("{}", node);
    assert!(rendered.contains("KV(key, value)"));

    // Keep at least one hash-based display path exercised.
    let hash_rendered = format!("{}", Node::Hash(NULL_HASH));
    assert!(hash_rendered.contains("Hash(HASH["));
}
