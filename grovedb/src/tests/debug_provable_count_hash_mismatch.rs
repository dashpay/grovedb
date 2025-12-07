use grovedb_merk::proofs::Query;
use grovedb_version::version::GroveVersion;

use crate::{tests::make_test_grovedb, Element, GroveDb, PathQuery};

#[test]
fn debug_provable_count_hash_mismatch() {
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
    let test_data = vec![
        (b"alice" as &[u8], b"data1" as &[u8]),
        (b"bob" as &[u8], b"data2" as &[u8]),
        (b"carol" as &[u8], b"data3" as &[u8]),
        (b"dave" as &[u8], b"data4" as &[u8]),
    ];

    for (key, value) in &test_data {
        db.insert(
            &[b"verified"],
            *key,
            Element::new_item(value.to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");
    }

    // Get the tree element in parent
    let tree_element = db
        .get(&[] as &[&[u8]], b"verified", None, grove_version)
        .unwrap()
        .expect("should get element");

    println!("\nTree element in parent: {:?}", tree_element);

    // Get the subtree root hash by opening the merk
    let transaction = db.start_transaction();
    let merk = db
        .open_transactional_merk_at_path(
            [b"verified"].as_ref().into(),
            &transaction,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should open merk");

    let subtree_root_hash = merk.root_hash().unwrap();
    println!("Subtree root hash: {}", hex::encode(&subtree_root_hash));

    // Calculate what the combined hash should be
    use grovedb_merk::tree::{combine_hash, value_hash};
    let tree_value_bytes = tree_element.serialize(grove_version).unwrap();
    let tree_value_hash = value_hash(&tree_value_bytes).unwrap();
    let combined_hash = combine_hash(&tree_value_hash, &subtree_root_hash).unwrap();

    println!("\nCalculated hashes:");
    println!("  Tree value bytes: {}", hex::encode(&tree_value_bytes));
    println!("  Tree value hash: {}", hex::encode(&tree_value_hash));
    println!("  Subtree root hash: {}", hex::encode(&subtree_root_hash));
    println!("  Combined hash: {}", hex::encode(&combined_hash));

    // Generate proof and look at what's in it
    let mut query = Query::new();
    query.insert_key(b"alice".to_vec());
    query.insert_key(b"carol".to_vec());

    let path_query = PathQuery::new_unsized(vec![b"verified".to_vec()], query);

    let proof = db
        .prove_query(&path_query, None, grove_version)
        .unwrap()
        .expect("should generate proof");

    // Decode the proof
    use grovedb_merk::proofs::{Decoder, Node, Op};

    use crate::operations::proof::GroveDBProof;

    let config = bincode::config::standard()
        .with_big_endian()
        .with_no_limit();
    let decoded_proof: GroveDBProof = bincode::decode_from_slice(&proof, config)
        .expect("should decode proof")
        .0;

    if let GroveDBProof::V0(proof_v0) = decoded_proof {
        // Look at root layer
        println!("\n=== ROOT LAYER PROOF ===");
        let ops = Decoder::new(&proof_v0.root_layer.merk_proof);
        for op in ops {
            if let Ok(Op::Push(node)) = op {
                match node {
                    Node::KVValueHash(k, v, h) => {
                        if k == b"verified" {
                            println!("\nFound 'verified' in root layer:");
                            println!("  Value bytes: {}", hex::encode(&v));
                            println!("  Hash in proof: {}", hex::encode(&h));

                            let elem = Element::deserialize(&v, grove_version).unwrap();
                            println!("  Element: {:?}", elem);
                        }
                    }
                    _ => {}
                }
            }
        }

        // Look at lower layer
        if let Some(lower_layer) = proof_v0.root_layer.lower_layers.get(&b"verified".to_vec()) {
            println!("\n=== LOWER LAYER PROOF (verified subtree) ===");
            let ops = Decoder::new(&lower_layer.merk_proof);
            let mut node_count = 0;
            for op in ops {
                if let Ok(op) = op {
                    match &op {
                        Op::Push(node) => {
                            node_count += 1;
                            match node {
                                Node::Hash(h) => {
                                    println!("  Node {}: Hash({})", node_count, hex::encode(h));
                                }
                                Node::KVDigest(k, h) => {
                                    println!(
                                        "  Node {}: KVDigest(key={}, hash={})",
                                        node_count,
                                        hex::encode(k),
                                        hex::encode(h)
                                    );
                                }
                                Node::KV(k, v) => {
                                    println!(
                                        "  Node {}: KV(key={}, value={})",
                                        node_count,
                                        hex::encode(k),
                                        hex::encode(v)
                                    );
                                }
                                Node::KVHash(h) => {
                                    println!(
                                        "  Node {}: KVHash(hash={})",
                                        node_count,
                                        hex::encode(h)
                                    );
                                }
                                Node::KVValueHash(k, v, h) => {
                                    println!(
                                        "  Node {}: KVValueHash(key={}, value={}, hash={})",
                                        node_count,
                                        hex::encode(k),
                                        hex::encode(v),
                                        hex::encode(h)
                                    );
                                }
                                Node::KVHashCount(h, count) => {
                                    println!(
                                        "  Node {}: KVHashCount(hash={}, count={})",
                                        node_count,
                                        hex::encode(h),
                                        count
                                    );
                                }
                                Node::KVCount(k, v, count) => {
                                    println!(
                                        "  Node {}: KVCount(key={}, value={}, count={})",
                                        node_count,
                                        hex::encode(k),
                                        hex::encode(v),
                                        count
                                    );
                                }
                                Node::KVValueHashFeatureType(k, v, h, ft) => {
                                    println!(
                                        "  Node {}: KVValueHashFeatureType(key={}, value={}, \
                                         hash={}, feature_type={:?})",
                                        node_count,
                                        hex::encode(k),
                                        hex::encode(v),
                                        hex::encode(h),
                                        ft
                                    );
                                }
                                _ => {
                                    println!("  Node {}: Other node type", node_count);
                                }
                            }
                        }
                        Op::Parent => println!("  Op: Parent"),
                        Op::Child => println!("  Op: Child"),
                        _ => println!("  Op: Other"),
                    }
                }
            }

            // Layer proof doesn't have root_key or count fields
        }
    }

    // Now try to verify to see where it fails
    println!("\n=== ATTEMPTING VERIFICATION ===");
    match GroveDb::verify_query_raw(&proof, &path_query, grove_version) {
        Ok((root_hash, _)) => {
            println!(
                "Verification succeeded! Root hash: {}",
                hex::encode(&root_hash)
            );
        }
        Err(e) => {
            println!("Verification failed: {:?}", e);
        }
    }
}
