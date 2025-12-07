use grovedb_merk::tree::value_hash;

use crate::{tests::make_empty_grovedb, Element};

#[test]
fn debug_hash_calculation_trace() {
    let grove_version = &grovedb_version::version::GroveVersion::latest();
    let db = make_empty_grovedb();

    // Create a provable count tree at root
    let parent_tree_key = b"verified";
    db.insert::<&[u8], _>(
        &[],
        parent_tree_key,
        Element::empty_provable_count_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert tree");

    // Create a nested tree inside the provable count tree
    let nested_tree_key = b"accounts";
    db.insert::<&[u8], _>(
        [parent_tree_key.as_slice()].as_ref(),
        nested_tree_key,
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert nested tree");

    // Let's see what the element looks like after insertion
    let elem = db
        .get::<&[u8], _>([].as_ref(), parent_tree_key, None, grove_version)
        .unwrap()
        .expect("should get element");

    println!("Element after insertion: {:?}", elem);

    // Let's manually calculate what the hash should be
    // The element stores Some(b"accounts") in the ProvableCountTree
    let serialized = elem.serialize(grove_version).unwrap();
    println!("Serialized element: {}", hex::encode(&serialized));

    let value_bytes_hash = value_hash(&serialized).unwrap();
    println!(
        "Value hash of serialized element: {}",
        hex::encode(&value_bytes_hash)
    );

    // Now let's trace what happens when we generate a proof
    let query = crate::PathQuery::new_unsized(
        vec![parent_tree_key.to_vec()],
        crate::Query::new_single_key(b"alice".to_vec()),
    );

    // Enable detailed tracing by getting the proof bytes
    let proof_bytes = db
        .prove_query(&query, None, grove_version)
        .unwrap()
        .unwrap();

    // Decode the proof to see what's in it
    let config = bincode::config::standard()
        .with_big_endian()
        .with_no_limit();
    let decoded_proof: crate::operations::proof::GroveDBProof =
        bincode::decode_from_slice(&proof_bytes, config).unwrap().0;

    println!("\nDecoded proof structure:");
    if let crate::operations::proof::GroveDBProof::V0(proof_v0) = decoded_proof {
        println!("Root layer merk proof:");
        for (i, op) in proof_v0.root_layer.merk_proof.iter().enumerate() {
            println!("  {}: {:?}", i, op);
        }
    }

    // Let's check what the proof says about the value_hash
    // The expected hash in the proof should be the value_hash stored in the
    // tree node
}
