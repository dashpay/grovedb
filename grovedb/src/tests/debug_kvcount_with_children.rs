use grovedb_merk::proofs::Query;
use grovedb_version::version::GroveVersion;

use crate::{tests::make_test_grovedb, Element, GroveDb, PathQuery};

#[test]
fn debug_kvcount_with_children() {
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

    // Insert test data - enough to create a tree structure
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

    // Generate proof for alice and carol
    let mut query = Query::new();
    query.insert_key(b"alice".to_vec());
    query.insert_key(b"carol".to_vec());

    let path_query = PathQuery::new_unsized(vec![b"verified".to_vec()], query);

    let proof = db
        .prove_query(&path_query, None, grove_version)
        .unwrap()
        .expect("should generate proof");

    // Decode and analyze the proof structure
    use grovedb_merk::proofs::{Decoder, Node, Op};

    use crate::operations::proof::GroveDBProof;

    let config = bincode::config::standard()
        .with_big_endian()
        .with_no_limit();
    let decoded_proof: GroveDBProof = bincode::decode_from_slice(&proof, config)
        .expect("should decode proof")
        .0;

    if let GroveDBProof::V0(proof_v0) = decoded_proof {
        if let Some(lower_layer) = proof_v0.root_layer.lower_layers.get(&b"verified".to_vec()) {
            println!("=== ANALYZING LOWER LAYER PROOF ===");

            // Manually execute the proof to understand the tree structure

            let ops = Decoder::new(&lower_layer.merk_proof);
            let mut stack: Vec<String> = Vec::new();

            for (i, op) in ops.enumerate() {
                if let Ok(op) = op {
                    match &op {
                        Op::Push(node) => {
                            println!("\nOp {}: Push", i);
                            match node {
                                Node::KVCount(k, v, count) => {
                                    println!(
                                        "  KVCount: key={}, value={}, count={}",
                                        hex::encode(k),
                                        hex::encode(v),
                                        count
                                    );

                                    // Calculate what hash this node should have
                                    use grovedb_merk::tree::{
                                        kv_digest_to_kv_hash, node_hash_with_count, value_hash,
                                    };
                                    let val_hash = value_hash(v).unwrap();
                                    let kv_hash = kv_digest_to_kv_hash(k, &val_hash).unwrap();

                                    // This might not be a leaf - it might have children
                                    println!("  Stack depth before push: {}", stack.len());
                                }
                                Node::KVHashCount(h, count) => {
                                    println!(
                                        "  KVHashCount: hash={}, count={}",
                                        hex::encode(h),
                                        count
                                    );
                                }
                                Node::Hash(h) => {
                                    println!("  Hash: {}", hex::encode(h));
                                }
                                _ => {
                                    println!("  Other node type");
                                }
                            }
                        }
                        Op::Parent => {
                            println!("\nOp {}: Parent", i);
                            println!("  Stack depth: {}", stack.len());
                        }
                        Op::Child => {
                            println!("\nOp {}: Child", i);
                            println!("  Stack depth: {}", stack.len());
                        }
                        _ => {
                            println!("\nOp {}: Other", i);
                        }
                    }
                }
            }

            // Now let's verify and see what happens
            println!("\n=== VERIFICATION ATTEMPT ===");
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
    }
}
