use grovedb_merk::tree::value_hash;

use crate::{tests::make_empty_grovedb, Element};

#[test]
fn debug_empty_tree_hash() {
    let grove_version = &grovedb_version::version::GroveVersion::latest();
    let db = make_empty_grovedb();

    // Create a simple tree
    let tree_key = b"testtree";
    db.insert::<&[u8], _>(
        &[],
        tree_key,
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert tree");

    // Get the tree's root hash by getting the root hash at that path
    // Note: We can't easily access the merk directly in tests, so let's skip this
    // for now

    // Get the element and calculate its value hash
    let elem = db
        .get::<&[u8], _>(&[], tree_key, None, grove_version)
        .unwrap()
        .expect("should get element");

    let serialized = elem.serialize(grove_version).unwrap();
    println!(
        "Serialized empty tree element: {}",
        hex::encode(&serialized)
    );

    let elem_value_hash = value_hash(&serialized).unwrap();
    println!(
        "Empty tree element value hash: {}",
        hex::encode(&elem_value_hash)
    );

    // Now let's see what hash is in a KVDigest node for this tree
    let query =
        crate::PathQuery::new_unsized(vec![], crate::Query::new_single_key(tree_key.to_vec()));

    let proof_bytes = db
        .prove_query(&query, None, grove_version)
        .unwrap()
        .unwrap();

    // Decode and examine the proof
    let config = bincode::config::standard()
        .with_big_endian()
        .with_no_limit();
    let decoded_proof: crate::operations::proof::GroveDBProof =
        bincode::decode_from_slice(&proof_bytes, config).unwrap().0;

    match decoded_proof {
        crate::operations::proof::GroveDBProof::V0(proof_v0) => {
            println!("\nProof for empty tree:");
            for op in &proof_v0.root_layer.merk_proof {
                println!("  {:?}", op);
            }
        }
    }
}
