use grovedb_merk::proofs::Query;
use grovedb_version::version::GroveVersion;

use crate::{tests::make_test_grovedb, Element, GroveDb, PathQuery};

#[test]
fn debug_kvcount_verification() {
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

    // Insert test data
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

    // Generate proof
    let mut query = Query::new();
    query.insert_key(b"alice".to_vec());

    let path_query = PathQuery::new_unsized(vec![b"verified".to_vec()], query);

    let proof = db
        .prove_query(&path_query, None, grove_version)
        .unwrap()
        .expect("should generate proof");

    // Let's manually calculate what the hash should be for KVCount node
    use grovedb_merk::tree::{kv_digest_to_kv_hash, node_hash_with_count, value_hash};

    // The KVCount node has key="alice", value="data1", count=1
    let key = b"alice";
    let value = Element::new_item(b"data1".to_vec())
        .serialize(grove_version)
        .unwrap();
    let count = 1u64;

    // Calculate value hash
    let val_hash = value_hash(&value).unwrap();
    println!("Value hash: {}", hex::encode(&val_hash));

    // Calculate KV hash
    let kv_hash = kv_digest_to_kv_hash(key, &val_hash).unwrap();
    println!("KV hash: {}", hex::encode(&kv_hash));

    // For ProvableCountTree, we should use node_hash_with_count
    // node_hash_with_count takes: kv_hash, left_hash, right_hash, count
    // For a leaf node, left and right are [0; 32]
    let node_hash = node_hash_with_count(&kv_hash, &[0; 32], &[0; 32], count).unwrap();
    println!("Node hash with count: {}", hex::encode(&node_hash));

    // Compare with just the kv_hash (what current verification uses)
    println!(
        "\nCurrent verification uses KV hash: {}",
        hex::encode(&kv_hash)
    );
    println!(
        "But should use node hash with count: {}",
        hex::encode(&node_hash)
    );

    // Try to verify
    match GroveDb::verify_query_raw(&proof, &path_query, grove_version) {
        Ok((root_hash, _)) => {
            println!(
                "\nVerification succeeded! Root hash: {}",
                hex::encode(&root_hash)
            );
        }
        Err(e) => {
            println!("\nVerification failed: {:?}", e);
        }
    }
}
