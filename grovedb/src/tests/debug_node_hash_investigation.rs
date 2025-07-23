use grovedb_version::version::GroveVersion;

use crate::{tests::make_test_grovedb, Element, GroveDb};

#[test]
fn debug_node_hash_investigation() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);

    // Create a ProvableCountTree
    db.insert(
        &[] as &[&[u8]],
        b"verified",
        Element::empty_provable_count_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert tree");

    // Insert one item
    db.insert(
        &[b"verified"],
        b"alice",
        Element::new_item(b"data1".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert item");

    // The fundamental issue is that for query proofs of ProvableCountTree:
    // 1. The proof generation uses to_kv_value_hash_feature_type_node
    // 2. This includes the value_hash, not the full node hash
    // 3. But the tree execution expects to compute node_hash_with_count
    // 4. This mismatch causes verification to fail

    // The solution is to create a new node type or modify the existing one
    // to include the full node hash for ProvableCountTree nodes

    println!("This test demonstrates the conceptual issue with ProvableCountTree proofs");
}
