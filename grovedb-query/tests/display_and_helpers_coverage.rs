use grovedb_query::{
    hex_to_ascii,
    proofs::{encode_into, Node, Op, TreeFeatureType},
    ProofItems, ProofStatus, Query, QueryItem, SubqueryBranch,
};

fn k(v: u8) -> Vec<u8> {
    vec![v]
}

// ─── hex_to_ascii ────────────────────────────────────────────────────

#[test]
fn hex_to_ascii_allowed_chars_returns_string() {
    assert_eq!(hex_to_ascii(b"abc123"), "abc123");
    assert_eq!(hex_to_ascii(b"A-Z_0/9"), "A-Z_0/9");
}

#[test]
fn hex_to_ascii_disallowed_chars_returns_hex() {
    // Space is not in ALLOWED_CHARS
    assert_eq!(hex_to_ascii(b"a b"), "0x612062");
    // Non-ASCII byte
    assert_eq!(hex_to_ascii(&[0xff]), "0xff");
}

// ─── Node Display ────────────────────────────────────────────────────

#[test]
fn node_display_all_variants() {
    let h = [0xab; 32];

    let display = format!("{}", Node::Hash(h));
    assert!(display.starts_with("Hash(HASH["));

    let display = format!("{}", Node::KVHash(h));
    assert!(display.starts_with("KVHash(HASH["));

    let display = format!("{}", Node::KV(b"key".to_vec(), b"val".to_vec()));
    assert!(display.contains("KV("));

    let display = format!("{}", Node::KVValueHash(b"k".to_vec(), b"v".to_vec(), h));
    assert!(display.contains("KVValueHash("));

    let display = format!("{}", Node::KVDigest(b"k".to_vec(), h));
    assert!(display.contains("KVDigest("));

    let display = format!("{}", Node::KVRefValueHash(b"k".to_vec(), b"v".to_vec(), h));
    assert!(display.contains("KVRefValueHash("));

    let display = format!(
        "{}",
        Node::KVValueHashFeatureType(
            b"k".to_vec(),
            b"v".to_vec(),
            h,
            TreeFeatureType::BasicMerkNode
        )
    );
    assert!(display.contains("KVValueHashFeatureType("));

    let display = format!("{}", Node::KVCount(b"k".to_vec(), b"v".to_vec(), 42));
    assert!(display.contains("KVCount("));
    assert!(display.contains("42"));

    let display = format!("{}", Node::KVHashCount(h, 7));
    assert!(display.contains("KVHashCount("));
    assert!(display.contains("7"));

    let display = format!(
        "{}",
        Node::KVRefValueHashCount(b"k".to_vec(), b"v".to_vec(), h, 99)
    );
    assert!(display.contains("KVRefValueHashCount("));
    assert!(display.contains("99"));

    let display = format!("{}", Node::KVDigestCount(b"k".to_vec(), h, 55));
    assert!(display.contains("KVDigestCount("));
    assert!(display.contains("55"));
}

// ─── Op KVDigestCount encode/decode ──────────────────────────────────

#[test]
fn encode_decode_push_kvdigestcount() {
    let op = Op::Push(Node::KVDigestCount(vec![1, 2, 3], [0xab; 32], 42));
    let mut bytes = vec![];
    ed::Encode::encode_into(&op, &mut bytes).unwrap();
    assert_eq!(bytes[0], 0x1a);

    let decoded = Op::decode(&bytes).unwrap();
    assert_eq!(decoded, op);
    assert_eq!(op.encoding_length(), bytes.len());
}

#[test]
fn encode_decode_push_inverted_kvdigestcount() {
    let op = Op::PushInverted(Node::KVDigestCount(vec![1, 2, 3], [0xab; 32], 42));
    let mut bytes = vec![];
    ed::Encode::encode_into(&op, &mut bytes).unwrap();
    assert_eq!(bytes[0], 0x1b);

    let decoded = Op::decode(&bytes).unwrap();
    assert_eq!(decoded, op);
    assert_eq!(op.encoding_length(), bytes.len());
}

// ─── encode_into standalone function ─────────────────────────────────

#[test]
fn encode_into_function_encodes_multiple_ops() {
    let ops = vec![Op::Parent, Op::Child, Op::ParentInverted, Op::ChildInverted];
    let mut output = vec![];
    encode_into(ops.iter(), &mut output);
    assert_eq!(output, vec![0x10, 0x11, 0x12, 0x13]);
}

// ─── Query Display ───────────────────────────────────────────────────

#[test]
fn query_display_covers_all_branches() {
    let mut q = Query::new_single_key(k(1));
    q.add_conditional_subquery(
        QueryItem::Key(k(2)),
        Some(vec![k(9)]),
        Some(Query::new_single_key(k(3))),
    );

    let display = format!("{}", q);
    assert!(display.contains("Query {"));
    assert!(display.contains("items:"));
    assert!(display.contains("conditional_subquery_branches:"));
    assert!(display.contains("left_to_right:"));
    assert!(display.contains("add_parent_tree_on_subquery:"));
}

#[test]
fn query_display_without_conditionals() {
    let q = Query::new_single_key(k(1));
    let display = format!("{}", q);
    assert!(display.contains("Query {"));
    // Should NOT contain conditional_subquery_branches section
    assert!(!display.contains("conditional_subquery_branches:"));
}

// ─── Query constructors ──────────────────────────────────────────────

#[test]
fn query_constructors_cover_all_variants() {
    let q = Query::new_range_full();
    assert_eq!(q.items.len(), 1);
    assert!(matches!(q.items[0], QueryItem::RangeFull(_)));

    let q = Query::new_single_query_item(QueryItem::RangeFrom(k(5)..));
    assert_eq!(q.items.len(), 1);

    let q = Query::new_single_query_item_with_direction(QueryItem::Key(k(3)), false);
    assert!(!q.left_to_right);

    let q = Query::new_with_direction(false);
    assert!(!q.left_to_right);
    assert!(q.items.is_empty());
}

// ─── Query iteration ─────────────────────────────────────────────────

#[test]
fn query_directional_iter_and_into_iter() {
    let mut q = Query::new();
    q.insert_key(k(1));
    q.insert_key(k(2));
    q.insert_key(k(3));

    // left_to_right
    let ltr: Vec<_> = q.directional_iter(true).collect();
    assert_eq!(ltr.len(), 3);
    assert_eq!(ltr[0], &QueryItem::Key(k(1)));

    // right_to_left
    let rtl: Vec<_> = q.directional_iter(false).collect();
    assert_eq!(rtl.len(), 3);
    assert_eq!(rtl[0], &QueryItem::Key(k(3)));

    // len/is_empty
    assert_eq!(q.len(), 3);
    assert!(!q.is_empty());

    // Into<Vec<QueryItem>>
    let items: Vec<QueryItem> = q.into();
    assert_eq!(items.len(), 3);
}

// ─── Query max_depth ─────────────────────────────────────────────────

#[test]
fn query_max_depth_basic_and_nested() {
    // No subquery — depth 1
    let q = Query::new_single_key(k(1));
    assert_eq!(q.max_depth(), Some(1));

    // With subquery — depth 2
    let mut q = Query::new_single_key(k(1));
    q.set_subquery(Query::new_single_key(k(2)));
    assert_eq!(q.max_depth(), Some(2));

    // With subquery path — depth = 1 + path_len + subquery_depth
    let mut q = Query::new_single_key(k(1));
    q.set_subquery_path(vec![k(2), k(3)]);
    q.set_subquery(Query::new_single_key(k(4)));
    assert_eq!(q.max_depth(), Some(4)); // 1 + 2 + 1

    // With conditional deeper than default
    let mut q = Query::new_single_key(k(1));
    let mut deep = Query::new_single_key(k(2));
    deep.set_subquery(Query::new_single_key(k(3)));
    q.add_conditional_subquery(QueryItem::Key(k(1)), None, Some(deep));
    assert_eq!(q.max_depth(), Some(3)); // 1 + max(0, 2)
}

// ─── SubqueryBranch Display and max_depth ────────────────────────────

#[test]
fn subquery_branch_display_with_and_without_path() {
    let branch = SubqueryBranch {
        subquery_path: Some(vec![k(1), k(2)]),
        subquery: Some(Box::new(Query::new_single_key(k(3)))),
    };
    let display = format!("{}", branch);
    assert!(display.contains("SubqueryBranch"));
    assert!(display.contains("subquery_path:"));
    assert!(display.contains("subquery:"));

    let branch = SubqueryBranch {
        subquery_path: None,
        subquery: None,
    };
    let display = format!("{}", branch);
    assert!(display.contains("subquery_path: None"));
    assert!(display.contains("subquery: None"));
}

#[test]
fn subquery_branch_max_depth_recursion_limit() {
    // Build a deeply recursive structure
    let mut inner = Query::new_single_key(k(1));
    for _ in 0..300 {
        let mut wrapper = Query::new_single_key(k(1));
        wrapper.set_subquery(inner);
        inner = wrapper;
    }

    let branch = SubqueryBranch {
        subquery_path: None,
        subquery: Some(Box::new(inner)),
    };
    // max_depth uses u8::MAX (255) recursion limit so it should return None
    // for a chain deeper than 255
    assert_eq!(branch.max_depth(), None);
}

// ─── ProofStatus ─────────────────────────────────────────────────────

#[test]
fn proof_status_hit_limit_and_update_limit() {
    let status = ProofStatus::new_with_limit(Some(0));
    assert!(status.hit_limit());

    let status = ProofStatus::new_with_limit(Some(5));
    assert!(!status.hit_limit());

    let status = ProofStatus::new_with_limit(None);
    assert!(!status.hit_limit());

    // update_limit with Some replaces
    let updated = status.update_limit(Some(3));
    assert_eq!(updated.limit, Some(3));

    // update_limit with None preserves
    let preserved = updated.update_limit(None);
    assert_eq!(preserved.limit, Some(3));
}

// ─── QueryItem Display ──────────────────────────────────────────────

#[test]
fn query_item_display_all_variants() {
    assert!(format!("{}", QueryItem::Key(k(5))).contains("Key("));
    assert!(format!("{}", QueryItem::Range(k(1)..k(5))).contains("Range("));
    assert!(format!("{}", QueryItem::RangeInclusive(k(1)..=k(5))).contains("RangeInclusive("));
    assert!(format!("{}", QueryItem::RangeFull(..)).contains("RangeFull"));
    assert!(format!("{}", QueryItem::RangeFrom(k(3)..)).contains("RangeFrom("));
    assert!(format!("{}", QueryItem::RangeTo(..k(8))).contains("RangeTo("));
    assert!(format!("{}", QueryItem::RangeToInclusive(..=k(8))).contains("RangeToInclusive("));
    assert!(format!("{}", QueryItem::RangeAfter(k(3)..)).contains("RangeAfter("));
    assert!(format!("{}", QueryItem::RangeAfterTo(k(1)..k(5))).contains("RangeAfterTo("));
    assert!(format!("{}", QueryItem::RangeAfterToInclusive(k(1)..=k(5)))
        .contains("RangeAfterToInclusive("));
}

// ─── QueryItem processing_footprint ──────────────────────────────────

#[test]
fn query_item_processing_footprint_varies_by_variant() {
    assert_eq!(QueryItem::Key(vec![1, 2, 3]).processing_footprint(), 3);
    assert_eq!(QueryItem::RangeFull(..).processing_footprint(), 0);
    assert!(QueryItem::Range(k(1)..k(5)).processing_footprint() > 0);
    assert!(QueryItem::RangeInclusive(k(1)..=k(5)).processing_footprint() > 0);
    assert!(QueryItem::RangeAfterToInclusive(k(1)..=k(5)).processing_footprint() > 0);
}

// ─── QueryItem keys_consume ──────────────────────────────────────────

#[test]
fn query_item_keys_consume_for_key_and_ranges() {
    let keys = QueryItem::Key(vec![5]).keys_consume().unwrap();
    assert_eq!(keys, vec![vec![5]]);

    let keys = QueryItem::Range(vec![2]..vec![5]).keys_consume().unwrap();
    assert_eq!(keys, vec![vec![2], vec![3], vec![4]]);

    let keys = QueryItem::RangeInclusive(vec![2]..=vec![5])
        .keys_consume()
        .unwrap();
    assert_eq!(keys, vec![vec![2], vec![3], vec![4], vec![5]]);

    // Unbounded range errors
    assert!(QueryItem::RangeFull(..).keys_consume().is_err());
    assert!(QueryItem::RangeFrom(vec![1]..).keys_consume().is_err());
}

#[test]
fn query_item_keys_consume_multi_byte_errors() {
    // Multi-byte start
    assert!(QueryItem::Range(vec![1, 2]..vec![5])
        .keys_consume()
        .is_err());
    assert!(QueryItem::RangeInclusive(vec![1, 2]..=vec![5])
        .keys_consume()
        .is_err());
}

#[test]
fn query_item_keys_empty_start() {
    // Empty start with Range — exercises the unwrap_or_else path
    let keys = QueryItem::Range(vec![]..vec![3]).keys().unwrap();
    assert_eq!(keys, vec![vec![], vec![0], vec![1], vec![2]]);

    let keys = QueryItem::RangeInclusive(vec![]..=vec![2]).keys().unwrap();
    assert_eq!(keys, vec![vec![], vec![0], vec![1], vec![2]]);
}

// ─── QueryItem PartialEq and PartialOrd with &[u8] ──────────────────

#[test]
fn query_item_partial_eq_and_cmp_with_byte_slice() {
    let key = QueryItem::Key(vec![5]);
    assert!(key == &[5u8][..]);
    assert!(key != &[6u8][..]);

    let range = QueryItem::Range(vec![3]..vec![7]);
    assert!(range != &[3u8][..]);
    assert!(range != &[5u8][..]);

    // Ordering
    assert!(key.partial_cmp(&&[3u8][..]).is_some());
}

// ─── QueryItem enum_value, is_key, is_range, is_single, is_unbounded_range ──

#[test]
fn query_item_type_predicates() {
    assert_eq!(QueryItem::Key(k(1)).enum_value(), 0);
    assert!(QueryItem::Key(k(1)).is_key());
    assert!(QueryItem::Key(k(1)).is_single());
    assert!(!QueryItem::Key(k(1)).is_range());
    assert!(!QueryItem::Key(k(1)).is_unbounded_range());

    assert_eq!(QueryItem::Range(k(1)..k(5)).enum_value(), 1);
    assert!(QueryItem::Range(k(1)..k(5)).is_range());
    assert!(!QueryItem::Range(k(1)..k(5)).is_unbounded_range());

    assert_eq!(QueryItem::RangeFull(..).enum_value(), 3);
    assert!(QueryItem::RangeFull(..).is_unbounded_range());

    assert_eq!(
        QueryItem::RangeAfterToInclusive(k(1)..=k(5)).enum_value(),
        9
    );
}

// ─── QueryItem lower/upper bound helpers ─────────────────────────────

#[test]
fn query_item_lower_upper_bound_all_variants() {
    // Test that lower/upper bound methods work for all variants
    let items: Vec<QueryItem> = vec![
        QueryItem::Key(k(5)),
        QueryItem::Range(k(1)..k(5)),
        QueryItem::RangeInclusive(k(1)..=k(5)),
        QueryItem::RangeFull(..),
        QueryItem::RangeFrom(k(3)..),
        QueryItem::RangeTo(..k(8)),
        QueryItem::RangeToInclusive(..=k(8)),
        QueryItem::RangeAfter(k(3)..),
        QueryItem::RangeAfterTo(k(1)..k(5)),
        QueryItem::RangeAfterToInclusive(k(1)..=k(5)),
    ];

    let expected_lower_unbounded = [
        false, false, false, true, false, true, true, false, false, false,
    ];
    let expected_upper_unbounded = [
        false, false, false, true, true, false, false, true, false, false,
    ];

    for (i, item) in items.iter().enumerate() {
        assert_eq!(
            item.lower_unbounded(),
            expected_lower_unbounded[i],
            "lower_unbounded mismatch for {:?}",
            item
        );
        assert_eq!(
            item.upper_unbounded(),
            expected_upper_unbounded[i],
            "upper_unbounded mismatch for {:?}",
            item
        );
        // lower_bound and upper_bound should not panic
        let _ = item.lower_bound();
        let _ = item.upper_bound();
    }

    // Check exclusive start for RangeAfter variants
    let (_, exclusive) = QueryItem::RangeAfter(k(3)..).lower_bound();
    assert!(exclusive);
    let (_, exclusive) = QueryItem::RangeAfterTo(k(1)..k(5)).lower_bound();
    assert!(exclusive);
    let (_, exclusive) = QueryItem::RangeAfterToInclusive(k(1)..=k(5)).lower_bound();
    assert!(exclusive);
}

// ─── ProofItems Display ──────────────────────────────────────────────

#[test]
fn proof_items_display_covers_format() {
    let items = vec![QueryItem::Key(k(1)), QueryItem::Range(k(3)..k(7))];
    let (proof_items, _) = ProofItems::new_with_query_items(&items, true);
    let display = format!("{}", proof_items);
    assert!(display.contains("ProofItems:"));
    assert!(display.contains("Key Queries:"));
    assert!(display.contains("Range Queries:"));
}

// ─── QueryItem::contains ─────────────────────────────────────────────

#[test]
fn query_item_contains_for_various_types() {
    assert!(QueryItem::Key(k(5)).contains(&[5]));
    assert!(!QueryItem::Key(k(5)).contains(&[6]));

    assert!(QueryItem::Range(k(3)..k(7)).contains(&[5]));
    assert!(!QueryItem::Range(k(3)..k(7)).contains(&[7]));
    assert!(QueryItem::Range(k(3)..k(7)).contains(&[3]));

    assert!(QueryItem::RangeInclusive(k(3)..=k(7)).contains(&[7]));

    assert!(QueryItem::RangeFull(..).contains(&[0]));
    assert!(QueryItem::RangeFull(..).contains(&[255]));

    assert!(QueryItem::RangeAfter(k(3)..).contains(&[4]));
    assert!(!QueryItem::RangeAfter(k(3)..).contains(&[3]));

    assert!(QueryItem::RangeTo(..k(5)).contains(&[4]));
    assert!(!QueryItem::RangeTo(..k(5)).contains(&[5]));

    assert!(QueryItem::RangeToInclusive(..=k(5)).contains(&[5]));
}

// ─── QueryItem From<Vec<u8>> ─────────────────────────────────────────

#[test]
fn query_item_from_vec() {
    let item: QueryItem = vec![1, 2, 3].into();
    assert_eq!(item, QueryItem::Key(vec![1, 2, 3]));
}
