use grovedb_merk::tree::{combine_hash, kv_digest_to_kv_hash, node_hash_with_count, value_hash};

use crate::{tests::make_empty_grovedb, Element};

#[test]
fn debug_layered_reference_hash_calculation() {
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

    // Add an item to nested tree
    let item_key = b"alice";
    let item_value = b"value1";
    db.insert::<&[u8], _>(
        [parent_tree_key.as_slice(), nested_tree_key.as_slice()].as_ref(),
        item_key,
        Element::new_item(item_value.to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert item");

    // Get the provable count tree element
    let elem = db
        .get::<&[u8], _>([].as_ref(), parent_tree_key, None, grove_version)
        .unwrap()
        .expect("should get element");

    println!("ProvableCountTree element: {:?}", elem);

    // Serialize the element to see what's stored
    let serialized = elem.serialize(grove_version).unwrap();
    println!("Serialized element bytes: {}", hex::encode(&serialized));

    // Calculate value hash
    let val_hash = value_hash(&serialized).value().to_owned();
    println!("Value hash: {}", hex::encode(&val_hash));

    // For now, let's skip getting the nested tree hash directly

    // Now let's see how the hash is calculated for a ProvableCountTree with
    // LayeredReference When we have Element::Tree(Some(root_hash), type), the
    // serialization includes:
    // - Element type (8 for Tree)
    // - Tree type (7 for ProvableCountTree)
    // - The root hash of the subtree

    // Let's check if this is a layered reference
    if let Element::ProvableCountTree(root_hash, count, _) = elem {
        println!("\nProvableCountTree has:");
        if let Some(ref hash) = root_hash {
            println!("  Root hash: {}", hex::encode(hash));
        } else {
            println!("  Root hash: None");
        }
        println!("  Count: {}", count);

        // For layered references, the value bytes include:
        // - Element type: 8 (Tree)
        // - Tree type: 7 (ProvableCountTree)
        // - Root hash: 32 bytes
        // - Count encoding

        // Let's manually calculate what the hash should be
        let kv_hash = kv_digest_to_kv_hash(parent_tree_key, &val_hash).unwrap();
        println!("\nKV hash (key + value): {}", hex::encode(&kv_hash));

        // For ProvableCountTree, we should use node_hash_with_count
        // But in proof verification, it's using combine_hash
        if let Some(ref hash) = root_hash {
            // Check if hash is 32 bytes
            if hash.len() == 32 {
                let hash_array: [u8; 32] =
                    hash.clone().try_into().expect("hash should be 32 bytes");
                let combined_hash = combine_hash(&val_hash, &hash_array).value().to_owned();
                println!(
                    "Combined hash (value + subtree): {}",
                    hex::encode(&combined_hash)
                );
            } else {
                println!(
                    "Warning: Root hash is not 32 bytes, it's {} bytes: {}",
                    hash.len(),
                    hex::encode(hash)
                );
                // This is likely the raw key, not a proper hash
            }
        }

        // Let's also try node_hash_with_count to see the difference
        let empty_hash = [0u8; 32];
        let count_hash = node_hash_with_count(&kv_hash, &empty_hash, &empty_hash, count).unwrap();
        println!("Node hash with count: {}", hex::encode(&count_hash));
    }

    // Let's also check how the proof verification works
    // Generate a proof for the nested tree
    let query = crate::PathQuery::new_unsized(
        vec![parent_tree_key.to_vec()],
        crate::Query::new_single_key(nested_tree_key.to_vec()),
    );

    let proof = db
        .prove_query(&query, None, grove_version)
        .unwrap()
        .unwrap();
    println!("\nProof generated, length: {}", proof.len());

    // Try to verify it
    let (root_hash, results) = crate::GroveDb::verify_query(&proof, &query, grove_version).unwrap();
    println!("Verified root hash: {}", hex::encode(&root_hash));
    println!("Results: {:?}", results);
}
